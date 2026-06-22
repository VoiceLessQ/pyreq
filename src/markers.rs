//! PEP 508 environment markers — port of the marker parts of `_parser.py` and `markers.py`.
//!
//! This step covers parsing a marker string into an AST and rendering it back to canonical
//! form. Evaluation against an environment is a separate step.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::Specifier;
use crate::tokenizer::{ParserSyntaxError, Tokenizer, enclosing};

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

/// PEP 503 name normalization (`canonicalize_name`): lowercase; `_`/`.` become `-`; runs of
/// `-` collapse. Shared with the requirements layer.
pub(crate) fn canonicalize_name(name: &str) -> String {
    let mut value = name.to_lowercase().replace(['_', '.'], "-");
    while value.contains("--") {
        value = value.replace("--", "-");
    }
    value
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
        Ok(process_python_str(&token.text))
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

fn process_python_str(quoted: &str) -> MarkerVar {
    // QUOTED_STRING guarantees matching quotes with no embedded quote, so the content is the
    // slice between them. (Python's `ast.literal_eval` escape handling is not modelled.)
    MarkerVar::Value(quoted[1..quoted.len() - 1].to_string())
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

    /// Evaluate this marker against `environment` (a complete map of marker variable to
    /// string value). Port of `markers.py`'s `_evaluate_markers` (the evaluation core).
    ///
    /// Unlike `Marker.evaluate`, this does not synthesize a default environment, canonicalize
    /// `extra`, or repair `python_full_version` — the caller supplies a prepared environment.
    /// Set-valued environment entries (`extras`/`dependency_groups`) are not modelled.
    pub fn evaluate(&self, environment: &HashMap<String, String>) -> Result<bool, MarkerError> {
        evaluate_markers(&self.markers, environment)
    }
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
    env: &HashMap<String, String>,
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
                // The variable side picks the environment key; the other side is its operand.
                let (lhs_value, env_key, rhs_value) = if matches!(lhs, MarkerVar::Variable(_)) {
                    let key = operand_str(lhs);
                    let value = env
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| MarkerError::UndefinedEnvironmentName(key.clone()))?;
                    (value, key, operand_str(rhs))
                } else {
                    let key = operand_str(rhs);
                    let value = env
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| MarkerError::UndefinedEnvironmentName(key.clone()))?;
                    (operand_str(lhs), key, value)
                };

                let (lhs_value, rhs_value) = normalize_operands(&lhs_value, &rhs_value, &env_key);
                let result = eval_op(&lhs_value, op, &rhs_value, &env_key)?;
                groups.last_mut().expect("group exists").push(result);
            }
            MarkerExpr::Or => groups.push(Vec::new()),
            MarkerExpr::And => {}
        }
    }

    Ok(groups.iter().any(|group| group.iter().all(|&b| b)))
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
/// the degenerate PEP 508 string operators (`<`/`>` always false, `<=`/`==`/`>=` are equality).
fn eval_op(lhs: &str, op: &str, rhs: &str, key: &str) -> Result<bool, MarkerError> {
    if MARKERS_REQUIRING_VERSION.contains(&key)
        && let Ok(spec) = Specifier::parse(&format!("{op}{rhs}"))
    {
        return Ok(spec.contains(lhs, Some(true)));
    }

    match op {
        "in" => Ok(rhs.contains(lhs)),
        "not in" => Ok(!rhs.contains(lhs)),
        // Non-version keys use Python's string operators (lexicographic), per `_operators`.
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
        // String keys compare lexicographically (Python's str operators), not equality-only.
        assert!(m("os_name < \"z\"").evaluate(&e).unwrap()); // "posix" < "z"
        assert!(!m("os_name < \"a\"").evaluate(&e).unwrap());
        assert!(m("os_name <= \"z\"").evaluate(&e).unwrap()); // <= is a real compare, not ==
        assert!(m("os_name >= \"posix\"").evaluate(&e).unwrap());
        assert!(!m("os_name > \"z\"").evaluate(&e).unwrap());
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
