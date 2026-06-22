//! PEP 508 environment markers — port of the marker parts of `_parser.py` and `markers.py`.
//!
//! This step covers parsing a marker string into an AST and rendering it back to canonical
//! form. Evaluation against an environment is a separate step.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::Specifier;
use crate::tokenizer::{ParserSyntaxError, Tokenizer, enclosing};
use crate::utils::canonicalize_name;

/// A marker operand: either an environment variable or a string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MarkerVar {
    Variable(String),
    Value(String),
}

/// One element of a marker expression tree. Mirrors `_parser.py`'s `MarkerList` elements:
/// a comparison item, a nested list, or a boolean joiner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MarkerExpr {
    Item {
        lhs: MarkerVar,
        op: String,
        rhs: MarkerVar,
    },
    List(Vec<MarkerExpr>),
    And,
    Or,
}

// --- Recursive-descent marker parser (port of the marker half of `_parser.py`). ---

fn parse_full_marker(tok: &mut Tokenizer) -> Result<Vec<MarkerExpr>, ParserSyntaxError> {
    let retval = parse_marker(tok)?;
    tok.expect("END", "end of marker expression")?;
    Ok(retval)
}

/// `marker = marker_atom (BOOLOP marker_atom)*`. `pub(crate)` so the requirement parser can
/// parse the marker after a `;`.
pub(crate) fn parse_marker(tok: &mut Tokenizer) -> Result<Vec<MarkerExpr>, ParserSyntaxError> {
    let mut expression = vec![parse_marker_atom(tok)?];
    while tok.check("BOOLOP") {
        let token = tok.read();
        let right = parse_marker_atom(tok)?;
        expression.push(if token.text == "or" {
            MarkerExpr::Or
        } else {
            MarkerExpr::And
        });
        expression.push(right);
    }
    Ok(expression)
}

/// `marker_atom = WS? ( '(' WS? marker WS? ')' | marker_item ) WS?`
fn parse_marker_atom(tok: &mut Tokenizer) -> Result<MarkerExpr, ParserSyntaxError> {
    tok.consume("WS");
    let result = if tok.peek("LEFT_PARENTHESIS") {
        let inner = enclosing(
            tok,
            "LEFT_PARENTHESIS",
            "RIGHT_PARENTHESIS",
            "marker expression",
            |tok| {
                tok.consume("WS");
                let marker = parse_marker(tok)?;
                tok.consume("WS");
                Ok(marker)
            },
        )?;
        MarkerExpr::List(inner)
    } else {
        parse_marker_item(tok)?
    };
    tok.consume("WS");
    Ok(result)
}

/// `marker_item = WS? marker_var WS? marker_op WS? marker_var WS?`
fn parse_marker_item(tok: &mut Tokenizer) -> Result<MarkerExpr, ParserSyntaxError> {
    tok.consume("WS");
    let lhs = parse_marker_var(tok)?;
    tok.consume("WS");
    let op = parse_marker_op(tok)?;
    tok.consume("WS");
    let rhs = parse_marker_var(tok)?;
    tok.consume("WS");
    Ok(MarkerExpr::Item { lhs, op, rhs })
}

/// `marker_var = VARIABLE | QUOTED_STRING`
fn parse_marker_var(tok: &mut Tokenizer) -> Result<MarkerVar, ParserSyntaxError> {
    if tok.check("VARIABLE") {
        let text = tok.read().text.replace('.', "_");
        Ok(process_env_var(&text))
    } else if tok.check("QUOTED_STRING") {
        let token = tok.read();
        let span = (token.position, token.position + token.text.len());
        process_python_str(&token.text).map_err(|_| {
            tok.syntax_error("Invalid quoted string", Some(span.0), Some(span.1))
        })
    } else {
        Err(tok.syntax_error("Expected a marker variable or quoted string", None, None))
    }
}

fn process_env_var(env_var: &str) -> MarkerVar {
    if env_var == "platform_python_implementation" || env_var == "python_implementation" {
        MarkerVar::Variable("platform_python_implementation".to_string())
    } else {
        MarkerVar::Variable(env_var.to_string())
    }
}

/// Port of `_parser.py`'s `process_python_str`, which runs `ast.literal_eval` on the quoted
/// token. The QUOTED_STRING rule guarantees matching outer quotes with no embedded quote of
/// the same kind, so only the backslash escapes inside need decoding. Returns `Err` on a
/// malformed escape (truncated `\x`/`\u`/`\U`, a lone trailing backslash, a surrogate, or
/// `\N{name}` which would need the Unicode-name database), matching Python raising on
/// `literal_eval`. Unknown escapes (e.g. `\d`) are kept literally, as CPython does.
fn process_python_str(quoted: &str) -> Result<MarkerVar, ()> {
    let inner = &quoted[1..quoted.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().ok_or(())? {
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            '\n' => {} // backslash-newline is a line continuation: drop both
            'a' => out.push('\u{07}'),
            'b' => out.push('\u{08}'),
            'f' => out.push('\u{0C}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{0B}'),
            d @ '0'..='7' => {
                let mut val = d.to_digit(8).expect("octal digit");
                for _ in 0..2 {
                    match chars.peek().and_then(|n| n.to_digit(8)) {
                        Some(n) => {
                            val = val * 8 + n;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(val).ok_or(())?);
            }
            'x' => out.push(decode_hex(&mut chars, 2)?),
            'u' => out.push(decode_hex(&mut chars, 4)?),
            'U' => out.push(decode_hex(&mut chars, 8)?),
            'N' => return Err(()), // \N{name} needs the Unicode-name database; unsupported
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(MarkerVar::Value(out))
}

/// Read exactly `n` hex digits and turn them into a `char`. `Err` if fewer than `n` digits
/// are present or the value is not a valid scalar (e.g. a surrogate).
fn decode_hex(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    n: usize,
) -> Result<char, ()> {
    let mut val: u32 = 0;
    for _ in 0..n {
        let d = chars.next().and_then(|c| c.to_digit(16)).ok_or(())?;
        val = val * 16 + d;
    }
    char::from_u32(val).ok_or(())
}

/// `marker_op = IN | NOT WS IN | OP`
fn parse_marker_op(tok: &mut Tokenizer) -> Result<String, ParserSyntaxError> {
    if tok.check("IN") {
        tok.read();
        Ok("in".to_string())
    } else if tok.check("NOT") {
        tok.read();
        tok.expect("WS", "whitespace after 'not'")?;
        tok.expect("IN", "'in' after 'not'")?;
        Ok("not in".to_string())
    } else if tok.check("OP") {
        Ok(tok.read().text)
    } else {
        Err(tok.syntax_error(
            "Expected marker operator, one of <=, <, !=, ==, >=, >, ~=, ===, in, not in",
            None,
            None,
        ))
    }
}

// --- Extra-value normalization (port of `_normalize_extra_values`). ---

fn normalize_extra_values(markers: Vec<MarkerExpr>) -> Vec<MarkerExpr> {
    markers.into_iter().map(normalize_expr).collect()
}

fn normalize_expr(expr: MarkerExpr) -> MarkerExpr {
    match expr {
        MarkerExpr::List(inner) => MarkerExpr::List(normalize_extra_values(inner)),
        MarkerExpr::Item { lhs, op, rhs } => {
            let (lhs, rhs) = normalize_item(lhs, rhs);
            MarkerExpr::Item { lhs, op, rhs }
        }
        other => other,
    }
}

fn normalize_item(lhs: MarkerVar, rhs: MarkerVar) -> (MarkerVar, MarkerVar) {
    match (&lhs, &rhs) {
        (MarkerVar::Variable(v), MarkerVar::Value(value)) if v == "extra" => {
            (lhs.clone(), MarkerVar::Value(canonicalize_name(value)))
        }
        (MarkerVar::Value(value), MarkerVar::Variable(v)) if v == "extra" => {
            (MarkerVar::Value(canonicalize_name(value)), rhs.clone())
        }
        _ => (lhs, rhs),
    }
}

// --- Display (port of `_format_marker`). ---

fn format_list(list: &[MarkerExpr], first: bool) -> String {
    // A single-element list wrapping another list/item drops the redundant parentheses.
    if list.len() == 1 {
        match &list[0] {
            MarkerExpr::List(inner) => return format_list(inner, true),
            MarkerExpr::Item { .. } => return format_expr(&list[0], true),
            _ => {}
        }
    }
    let parts: Vec<String> = list.iter().map(|e| format_expr(e, false)).collect();
    if first {
        parts.join(" ")
    } else {
        format!("({})", parts.join(" "))
    }
}

fn format_expr(expr: &MarkerExpr, first: bool) -> String {
    match expr {
        MarkerExpr::List(inner) => format_list(inner, first),
        MarkerExpr::Item { lhs, op, rhs } => {
            format!("{} {} {}", serialize_var(lhs), op, serialize_var(rhs))
        }
        MarkerExpr::And => "and".to_string(),
        MarkerExpr::Or => "or".to_string(),
    }
}

fn serialize_var(var: &MarkerVar) -> String {
    match var {
        MarkerVar::Variable(s) => s.clone(),
        MarkerVar::Value(s) => format!("\"{s}\""),
    }
}

// --- Public types. ---

/// Raised when a marker string cannot be parsed. Holds the formatted parser error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidMarker(pub String);

impl fmt::Display for InvalidMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for InvalidMarker {}

/// A parsed PEP 508 environment marker expression. Port of `markers.Marker`.
#[derive(Debug, Clone)]
pub struct Marker {
    markers: Vec<MarkerExpr>,
}

impl Marker {
    /// Parse a marker string. Equivalent to `Marker(marker)`.
    pub fn parse(marker: &str) -> Result<Marker, InvalidMarker> {
        let mut tok = Tokenizer::new(marker);
        let tree = parse_full_marker(&mut tok).map_err(|e| InvalidMarker(e.to_string()))?;
        Ok(Marker {
            markers: normalize_extra_values(tree),
        })
    }

    /// Build a `Marker` from an already-parsed marker tree (applying extra normalization).
    /// Used by the requirement parser, mirroring `Requirement.__init__`'s `Marker.__new__`.
    pub(crate) fn from_tree(tree: Vec<MarkerExpr>) -> Marker {
        Marker {
            markers: normalize_extra_values(tree),
        }
    }

    /// Evaluate this marker against `environment` (a map of marker variable to string value).
    /// Port of `markers.py`'s `Marker.evaluate`, minus default-environment synthesis.
    ///
    /// Because pyreq is not running inside the Python interpreter it describes, it cannot
    /// build a default environment (`python_version`, `implementation_name`, etc. have no
    /// source). The caller supplies the environment; a marker variable missing from it is an
    /// `UndefinedEnvironmentName` error rather than a synthesized default.
    ///
    /// The faithful, data-only transforms packaging applies are ported: `extra` is
    /// canonicalized (PEP 685) and `python_full_version` is repaired for non-tagged builds (a
    /// trailing `+` gets `local` appended, per `_repair_python_full_version`).
    ///
    /// This entry takes string-only values, so it cannot express the set-valued `extras` /
    /// `dependency_groups` variables. Use [`Marker::evaluate_with_context`] for those.
    pub fn evaluate(&self, environment: &HashMap<String, String>) -> Result<bool, MarkerError> {
        let env: HashMap<String, EnvValue> = environment
            .iter()
            .map(|(k, v)| (k.clone(), EnvValue::Str(v.clone())))
            .collect();
        self.eval_prepared(env)
    }

    /// Evaluate against an environment whose values may be sets, in a given context. Full port
    /// of `markers.py`'s `Marker.evaluate` evaluation semantics (still minus host default
    /// synthesis).
    ///
    /// `context` injects the defaults packaging adds before the caller's environment overlays
    /// them: [`EvaluateContext::Metadata`] supplies an empty `extra`; [`EvaluateContext::LockFile`]
    /// supplies empty `extras` and `dependency_groups` sets (so a marker like `"x" in extras`
    /// evaluates to `false` rather than erroring when the caller omits them);
    /// [`EvaluateContext::Requirement`] injects nothing. Host-derived variables are still the
    /// caller's responsibility.
    pub fn evaluate_with_context(
        &self,
        environment: &HashMap<String, EnvValue>,
        context: EvaluateContext,
    ) -> Result<bool, MarkerError> {
        let mut env: HashMap<String, EnvValue> = HashMap::new();
        match context {
            EvaluateContext::LockFile => {
                env.insert("extras".to_string(), EnvValue::Set(HashSet::new()));
                env.insert("dependency_groups".to_string(), EnvValue::Set(HashSet::new()));
            }
            EvaluateContext::Metadata => {
                env.insert("extra".to_string(), EnvValue::Str(String::new()));
            }
            EvaluateContext::Requirement => {}
        }
        for (k, v) in environment {
            env.insert(k.clone(), v.clone());
        }
        self.eval_prepared(env)
    }

    /// Shared evaluation tail: apply the `extra` canonicalization and `python_full_version`
    /// repair, then run the core. Used by both public entry points.
    fn eval_prepared(&self, mut env: HashMap<String, EnvValue>) -> Result<bool, MarkerError> {
        if let Some(EnvValue::Str(extra)) = env.get("extra").cloned() {
            let canon = if extra.is_empty() {
                String::new()
            } else {
                canonicalize_name(&extra)
            };
            env.insert("extra".to_string(), EnvValue::Str(canon));
        }
        if let Some(EnvValue::Str(full)) = env.get("python_full_version").cloned()
            && let Some(stripped) = full.strip_suffix('+')
        {
            env.insert(
                "python_full_version".to_string(),
                EnvValue::Str(format!("{stripped}+local")),
            );
        }
        evaluate_markers(&self.markers, &env)
    }
}

/// A marker environment value: a single string (most variables) or a set of strings (the
/// `extras` / `dependency_groups` variables). Mirrors packaging's `str | AbstractSet[str]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvValue {
    Str(String),
    Set(HashSet<String>),
}

/// The context a marker is evaluated in, controlling which empty defaults are injected.
/// Port of `markers.py`'s `EvaluateContext` literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluateContext {
    /// Core metadata (packaging's default): an empty `extra` is supplied.
    Metadata,
    /// Lock files: empty `extras` and `dependency_groups` sets are supplied.
    LockFile,
    /// Any other situation: no defaults are injected.
    Requirement,
}

/// An error raised while evaluating a marker. Mirrors `UndefinedComparison` and a missing
/// environment key (Python raises `KeyError` / `UndefinedEnvironmentName`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerError {
    UndefinedComparison(String),
    UndefinedEnvironmentName(String),
}

impl fmt::Display for MarkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarkerError::UndefinedComparison(m) => write!(f, "{m}"),
            MarkerError::UndefinedEnvironmentName(k) => {
                write!(f, "{k:?} does not exist in evaluation environment.")
            }
        }
    }
}

impl std::error::Error for MarkerError {}

/// The inner string of a marker operand (a variable's name or a value's text).
fn operand_str(var: &MarkerVar) -> String {
    match var {
        MarkerVar::Variable(s) | MarkerVar::Value(s) => s.clone(),
    }
}

const MARKERS_REQUIRING_VERSION: [&str; 4] = [
    "implementation_version",
    "platform_release",
    "python_full_version",
    "python_version",
];

fn evaluate_markers(
    markers: &[MarkerExpr],
    env: &HashMap<String, EnvValue>,
) -> Result<bool, MarkerError> {
    // Groups split on `or`; within a group every item must hold (`and`). Any group holding
    // makes the whole expression true.
    let mut groups: Vec<Vec<bool>> = vec![Vec::new()];

    for marker in markers {
        match marker {
            MarkerExpr::List(inner) => {
                let value = evaluate_markers(inner, env)?;
                groups.last_mut().expect("group exists").push(value);
            }
            MarkerExpr::Item { lhs, op, rhs } => {
                let result = evaluate_item(lhs, op, rhs, env)?;
                groups.last_mut().expect("group exists").push(result);
            }
            MarkerExpr::Or => groups.push(Vec::new()),
            MarkerExpr::And => {}
        }
    }

    Ok(groups.iter().any(|group| group.iter().all(|&b| b)))
}

/// Evaluate a single `lhs op rhs` comparison. The variable side picks the environment key; the
/// other side is its string operand. When the looked-up value is a set, packaging asserts the
/// left operand is a string (so the set must sit on the right), then does membership.
fn evaluate_item(
    lhs: &MarkerVar,
    op: &str,
    rhs: &MarkerVar,
    env: &HashMap<String, EnvValue>,
) -> Result<bool, MarkerError> {
    let var_is_lhs = matches!(lhs, MarkerVar::Variable(_));
    let key = if var_is_lhs {
        operand_str(lhs)
    } else {
        operand_str(rhs)
    };
    let value = env
        .get(&key)
        .ok_or_else(|| MarkerError::UndefinedEnvironmentName(key.clone()))?;
    // The operand on the non-variable side is always a plain string.
    let other = if var_is_lhs {
        operand_str(rhs)
    } else {
        operand_str(lhs)
    };

    match value {
        EnvValue::Str(s) => {
            let (lhs_value, rhs_value) = if var_is_lhs {
                (s.clone(), other)
            } else {
                (other, s.clone())
            };
            let (lhs_value, rhs_value) = normalize_operands(&lhs_value, &rhs_value, &key);
            eval_op(&lhs_value, op, &rhs_value, &key)
        }
        EnvValue::Set(set) => {
            if var_is_lhs {
                // The set landed on the left; packaging's `assert isinstance(lhs_value, str)`
                // would fail. A set-valued variable is only usable on the right of `in`/`not in`.
                return Err(MarkerError::UndefinedComparison(format!(
                    "{key:?} is set-valued and can only appear on the right of a comparison"
                )));
            }
            eval_set_op(&other, op, set, &key)
        }
    }
}

const MARKERS_ALLOWING_SET: [&str; 2] = ["extras", "dependency_groups"];

/// Evaluate a `string op set` comparison. For the set-allowing keys both sides are
/// canonicalized (PEP 685). Mirrors packaging's `_operators` applied with a set right operand:
/// `in`/`not in` test membership; `==` is always false and `!=` always true (a string never
/// equals a set); the ordering operators error (Python's `operator.lt(str, set)` raises
/// `TypeError`, surfaced here as `UndefinedComparison`).
fn eval_set_op(
    lhs: &str,
    op: &str,
    set: &HashSet<String>,
    key: &str,
) -> Result<bool, MarkerError> {
    let (needle, members): (String, HashSet<String>) = if MARKERS_ALLOWING_SET.contains(&key) {
        (
            canonicalize_name(lhs),
            set.iter().map(|m| canonicalize_name(m)).collect(),
        )
    } else {
        (lhs.to_string(), set.clone())
    };

    match op {
        "in" => Ok(members.contains(&needle)),
        "not in" => Ok(!members.contains(&needle)),
        "==" => Ok(false),
        "!=" => Ok(true),
        "<" | "<=" | ">=" | ">" => Err(MarkerError::UndefinedComparison(format!(
            "'{op}' not supported between a string and a set ({lhs:?})"
        ))),
        _ => Err(MarkerError::UndefinedComparison(format!(
            "Undefined {op:?} on {lhs:?} and a set."
        ))),
    }
}

/// Port of `_normalize`: canonicalize both sides for set-style keys; leave others as-is.
fn normalize_operands(lhs: &str, rhs: &str, key: &str) -> (String, String) {
    if key == "extra" {
        return (lhs.to_string(), rhs.to_string());
    }
    if key == "extras" || key == "dependency_groups" {
        return (canonicalize_name(lhs), canonicalize_name(rhs));
    }
    (lhs.to_string(), rhs.to_string())
}

/// Port of `_eval_op`: version-key comparisons go through a `Specifier`; everything else uses
/// packaging's `_operators` table, where the ordering operators do real string comparison.
fn eval_op(lhs: &str, op: &str, rhs: &str, key: &str) -> Result<bool, MarkerError> {
    if MARKERS_REQUIRING_VERSION.contains(&key)
        && let Ok(spec) = Specifier::parse(&format!("{op}{rhs}"))
    {
        return Ok(spec.contains(lhs, Some(true)));
    }

    // Non-version keys map to Python's operator functions: lexicographic compare, not equality.
    match op {
        "in" => Ok(rhs.contains(lhs)),
        "not in" => Ok(!rhs.contains(lhs)),
        "<" => Ok(lhs < rhs),
        "<=" => Ok(lhs <= rhs),
        "==" => Ok(lhs == rhs),
        "!=" => Ok(lhs != rhs),
        ">=" => Ok(lhs >= rhs),
        ">" => Ok(lhs > rhs),
        _ => Err(MarkerError::UndefinedComparison(format!(
            "Undefined {op:?} on {lhs:?} and {rhs:?}."
        ))),
    }
}

impl fmt::Display for Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_list(&self.markers, true))
    }
}

impl FromStr for Marker {
    type Err = InvalidMarker;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Marker::parse(s)
    }
}

// Equality and hashing are by canonical string form, matching `markers.Marker`.
impl PartialEq for Marker {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

impl Eq for Marker {}

impl Hash for Marker {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.to_string().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Marker {
        Marker::parse(s).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parses_and_roundtrips_simple() {
        assert_eq!(m("python_version > \"3.6\"").to_string(), "python_version > \"3.6\"");
        assert_eq!(m("os_name == \"posix\"").to_string(), "os_name == \"posix\"");
        assert_eq!(m("\"3.6\" == python_version").to_string(), "\"3.6\" == python_version");
    }

    #[test]
    fn normalizes_variable_aliases() {
        // `os.name` -> `os_name`, `platform.python_implementation` -> canonical name.
        assert_eq!(m("os.name == \"nt\"").to_string(), "os_name == \"nt\"");
        assert_eq!(
            m("platform.python_implementation == \"CPython\"").to_string(),
            "platform_python_implementation == \"CPython\""
        );
        assert_eq!(
            m("python_implementation == \"PyPy\"").to_string(),
            "platform_python_implementation == \"PyPy\""
        );
    }

    #[test]
    fn boolean_and_grouping() {
        assert_eq!(
            m("python_version > \"3.6\" and os_name == \"posix\"").to_string(),
            "python_version > \"3.6\" and os_name == \"posix\""
        );
        // Redundant outer parens are stripped; needed inner ones kept.
        assert_eq!(
            m("(python_version == \"3.6\")").to_string(),
            "python_version == \"3.6\""
        );
        assert_eq!(
            m("python_version > \"3.6\" or (python_version == \"3.6\" and os_name == \"unix\")")
                .to_string(),
            "python_version > \"3.6\" or (python_version == \"3.6\" and os_name == \"unix\")"
        );
    }

    #[test]
    fn in_and_not_in() {
        assert_eq!(
            m("\"arm\" in platform_machine").to_string(),
            "\"arm\" in platform_machine"
        );
        assert_eq!(
            m("platform_machine not in \"x86_64\"").to_string(),
            "platform_machine not in \"x86_64\""
        );
    }

    #[test]
    fn extra_value_is_normalized() {
        assert_eq!(m("extra == \"Foo.Bar\"").to_string(), "extra == \"foo-bar\"");
    }

    #[test]
    fn equality_is_canonical() {
        assert_eq!(m("(os_name == \"nt\")"), m("os_name == \"nt\""));
        assert_ne!(m("os_name == \"nt\""), m("os_name == \"posix\""));
    }

    #[test]
    fn rejects_invalid() {
        for bad in [
            "",
            "python_version",
            "python_version ==",
            "== \"3.6\"",
            "python_version = \"3.6\"",
            "(python_version == \"3.6\"",
            "foo == \"bar\"",
        ] {
            assert!(Marker::parse(bad).is_err(), "expected {bad:?} invalid");
        }
    }

    fn env() -> HashMap<String, String> {
        [
            ("python_version", "3.9"),
            ("python_full_version", "3.9.7"),
            ("os_name", "posix"),
            ("sys_platform", "linux"),
            ("platform_machine", "x86_64"),
            ("platform_python_implementation", "CPython"),
            ("implementation_name", "cpython"),
            ("extra", "test"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn evaluate_version_keys_use_specifier() {
        let e = env();
        assert!(m("python_version >= \"3.6\"").evaluate(&e).unwrap());
        assert!(!m("python_version < \"3.8\"").evaluate(&e).unwrap());
        assert!(m("python_version <= \"3.9\"").evaluate(&e).unwrap()); // real <=, not string-eq
        assert!(m("python_full_version >= \"3.9.0\"").evaluate(&e).unwrap());
    }

    #[test]
    fn evaluate_string_keys_and_operators() {
        let e = env();
        assert!(m("os_name == \"posix\"").evaluate(&e).unwrap());
        assert!(!m("sys_platform == \"win32\"").evaluate(&e).unwrap());
        assert!(m("os_name != \"nt\"").evaluate(&e).unwrap());
        assert!(m("\"x86\" in platform_machine").evaluate(&e).unwrap());
        assert!(m("platform_machine not in \"arm64\"").evaluate(&e).unwrap());
        // Non-version keys follow packaging's `_operators`: ordering ops compare strings
        // lexicographically (os_name = "posix").
        assert!(m("os_name < \"z\"").evaluate(&e).unwrap());
        assert!(m("os_name > \"a\"").evaluate(&e).unwrap());
        assert!(m("os_name <= \"posix\"").evaluate(&e).unwrap());
        assert!(m("os_name <= \"z\"").evaluate(&e).unwrap());
        assert!(m("os_name >= \"posix\"").evaluate(&e).unwrap());
        assert!(!m("os_name >= \"z\"").evaluate(&e).unwrap());
    }

    #[test]
    fn evaluate_boolean_logic() {
        let e = env();
        assert!(
            m("python_version >= \"3.6\" and os_name == \"posix\"")
                .evaluate(&e)
                .unwrap()
        );
        assert!(
            !m("python_version < \"3.0\" and os_name == \"posix\"")
                .evaluate(&e)
                .unwrap()
        );
        assert!(
            m("python_version < \"3.0\" or os_name == \"posix\"")
                .evaluate(&e)
                .unwrap()
        );
    }

    #[test]
    fn string_escapes_are_decoded() {
        // \x2e is '.', so the literal becomes "3.6"; round-trips through Display too.
        assert_eq!(
            m("python_version == \"3\\x2e6\"").to_string(),
            "python_version == \"3.6\""
        );
        // \\ and \n decode to a backslash and a newline.
        assert_eq!(m("os_name == \"a\\\\b\"").to_string(), "os_name == \"a\\b\"");
        assert_eq!(
            m("os_name == \"a\\nb\"").to_string(),
            "os_name == \"a\nb\""
        );
        // Unknown escape \d keeps both characters literally, as CPython does.
        assert_eq!(
            m("os_name == \"a\\db\"").to_string(),
            "os_name == \"a\\db\""
        );
    }

    #[test]
    fn malformed_escapes_are_rejected() {
        // Truncated \x and an unsupported \N{...} fail to parse.
        assert!(Marker::parse("os_name == \"a\\x1\"").is_err());
        assert!(Marker::parse("os_name == \"\\N{BULLET}\"").is_err());
    }

    #[test]
    fn evaluate_canonicalizes_extra() {
        // The marker side is canonicalized at parse time; the env value at eval time.
        let mut e = HashMap::new();
        e.insert("extra".to_string(), "Foo.Bar".to_string());
        assert!(m("extra == \"foo-bar\"").evaluate(&e).unwrap());
    }

    #[test]
    fn evaluate_repairs_python_full_version() {
        // A non-tagged build reports "3.11.0+"; repair appends "local" so it parses as a
        // PEP 440 version and the comparison succeeds.
        let mut e = HashMap::new();
        e.insert("python_full_version".to_string(), "3.11.0+".to_string());
        assert!(m("python_full_version >= \"3.11.0\"").evaluate(&e).unwrap());
    }

    #[test]
    fn lock_file_context_supplies_empty_sets() {
        let empty = HashMap::new();
        // With no caller env, the lock-file context still defines extras as an empty set, so
        // membership is false (rather than UndefinedEnvironmentName).
        assert!(
            !m("\"cpu\" in extras")
                .evaluate_with_context(&empty, EvaluateContext::LockFile)
                .unwrap()
        );
        assert!(
            m("\"cpu\" not in extras")
                .evaluate_with_context(&empty, EvaluateContext::LockFile)
                .unwrap()
        );
    }

    #[test]
    fn set_membership_and_canonicalization() {
        let mut e = HashMap::new();
        e.insert(
            "extras".to_string(),
            EnvValue::Set(HashSet::from(["foo-bar".to_string(), "gpu".to_string()])),
        );
        assert!(
            m("\"gpu\" in extras")
                .evaluate_with_context(&e, EvaluateContext::LockFile)
                .unwrap()
        );
        // PEP 685: the literal is canonicalized before membership, so "Foo.Bar" matches "foo-bar".
        assert!(
            m("\"Foo.Bar\" in extras")
                .evaluate_with_context(&e, EvaluateContext::LockFile)
                .unwrap()
        );
        assert!(
            !m("\"missing\" in extras")
                .evaluate_with_context(&e, EvaluateContext::LockFile)
                .unwrap()
        );
    }

    #[test]
    fn dependency_groups_set() {
        let mut e = HashMap::new();
        e.insert(
            "dependency_groups".to_string(),
            EnvValue::Set(HashSet::from(["test".to_string()])),
        );
        assert!(
            m("\"test\" in dependency_groups")
                .evaluate_with_context(&e, EvaluateContext::LockFile)
                .unwrap()
        );
    }

    #[test]
    fn metadata_context_supplies_empty_extra() {
        let empty = HashMap::new();
        assert!(
            !m("extra == \"foo\"")
                .evaluate_with_context(&empty, EvaluateContext::Metadata)
                .unwrap()
        );
        let mut e = HashMap::new();
        e.insert("extra".to_string(), EnvValue::Str("Foo".to_string()));
        assert!(
            m("extra == \"foo\"")
                .evaluate_with_context(&e, EvaluateContext::Metadata)
                .unwrap()
        );
    }

    #[test]
    fn set_valued_variable_on_left_errors() {
        let mut e = HashMap::new();
        e.insert("extras".to_string(), EnvValue::Set(HashSet::new()));
        assert!(matches!(
            m("extras == \"foo\"").evaluate_with_context(&e, EvaluateContext::LockFile),
            Err(MarkerError::UndefinedComparison(_))
        ));
    }

    #[test]
    fn set_op_equality_and_ordering() {
        let mut e = HashMap::new();
        e.insert(
            "extras".to_string(),
            EnvValue::Set(HashSet::from(["gpu".to_string()])),
        );
        let ctx = EvaluateContext::LockFile;
        // A string never equals a set: == is always false, != always true (matches Python).
        assert!(!m("\"gpu\" == extras").evaluate_with_context(&e, ctx).unwrap());
        assert!(m("\"gpu\" != extras").evaluate_with_context(&e, ctx).unwrap());
        // Ordering a string against a set raises in Python (TypeError) -> UndefinedComparison.
        for op in ["<", "<=", ">=", ">"] {
            assert!(
                matches!(
                    m(&format!("\"gpu\" {op} extras")).evaluate_with_context(&e, ctx),
                    Err(MarkerError::UndefinedComparison(_))
                ),
                "op {op:?} should be an undefined comparison against a set"
            );
        }
    }

    #[test]
    fn requirement_context_injects_nothing() {
        let empty = HashMap::new();
        // No defaults, so a reference to extras is undefined.
        assert!(matches!(
            m("\"x\" in extras").evaluate_with_context(&empty, EvaluateContext::Requirement),
            Err(MarkerError::UndefinedEnvironmentName(_))
        ));
    }

    #[test]
    fn evaluate_errors() {
        let e = env();
        // Undefined comparison: `~=` on a non-version key.
        assert!(matches!(
            m("os_name ~= \"1.0\"").evaluate(&e),
            Err(MarkerError::UndefinedComparison(_))
        ));
        // Missing environment key.
        let mut sparse = HashMap::new();
        sparse.insert("os_name".to_string(), "posix".to_string());
        assert!(matches!(
            m("python_version == \"3.9\"").evaluate(&sparse),
            Err(MarkerError::UndefinedEnvironmentName(_))
        ));
    }
}
