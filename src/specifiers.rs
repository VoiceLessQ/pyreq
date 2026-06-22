//! PEP 440 version specifiers — port of `specifiers.py` (`Specifier` / `SpecifierSet`).
//!
//! Strategy: upstream's current internals match versions with an interval engine
//! (`_ranges.py`). This port instead implements the PEP 440 operator comparison semantics
//! directly (the approach packaging used before the range engine), which yields identical
//! `contains`/`filter` results — verified by differential testing — with far less code.
//! Each operator's behaviour was cross-checked against `_ranges.py`'s range builders,
//! including the pre/post/local "family" edge cases for `<`, `>`, `~=`, and `==V.*`.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;

use crate::Version;

/// Raised when a specifier string is not valid. Holds the offending string.
///
/// Mirrors `specifiers.py`'s `InvalidSpecifier`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSpecifier(pub String);

impl fmt::Display for InvalidSpecifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid specifier: '{}'", self.0)
    }
}

impl std::error::Error for InvalidSpecifier {}

/// The specifier grammar, translated from `specifiers.py`'s `_specifier_regex_str`.
///
/// Four alternatives by operator family:
/// - `===` arbitrary: an almost-anything version string.
/// - `==` / `!=`: epoch + release, then *either* a `.*` wildcard *or* the optional
///   pre/post/dev/local suffixes (the only operators that allow wildcards and locals).
/// - `~=` compatible: requires at least two release components (`+` not `*`), no local/wildcard.
/// - `<= >= < >`: like equality but no wildcard and no local.
///
/// Used only to validate; the operator/version split is done afterwards by prefix. Inner
/// `(alpha|…)` / `(post|…)` groups are made non-capturing (the captures are unused).
const SPECIFIER_PATTERN: &str = r"(?xi)
    \A\s*
    (?:
        (?:
            ===
            \s*
            [^\s;)]*
        )
        |
        (?:
            (?:==|!=)
            \s*
            v?
            (?:[0-9]+!)?
            [0-9]+(?:\.[0-9]+)*
            (?:
                \.\*
                |
                (?:
                    (?:[-_.]?(?:alpha|beta|preview|pre|a|b|c|rc)[-_.]?[0-9]*)?
                    (?:(?:-[0-9]+)|(?:[-_.]?(?:post|rev|r)[-_.]?[0-9]*))?
                    (?:[-_.]?dev[-_.]?[0-9]*)?
                    (?:\+[a-z0-9]+(?:[-_.][a-z0-9]+)*)?
                )
            )?
        )
        |
        (?:
            ~=
            \s*
            v?
            (?:[0-9]+!)?
            [0-9]+(?:\.[0-9]+)+
            (?:[-_.]?(?:alpha|beta|preview|pre|a|b|c|rc)[-_.]?[0-9]*)?
            (?:(?:-[0-9]+)|(?:[-_.]?(?:post|rev|r)[-_.]?[0-9]*))?
            (?:[-_.]?dev[-_.]?[0-9]*)?
        )
        |
        (?:
            (?:<=|>=|<|>)
            \s*
            v?
            (?:[0-9]+!)?
            [0-9]+(?:\.[0-9]+)*
            (?:[-_.]?(?:alpha|beta|preview|pre|a|b|c|rc)[-_.]?[0-9]*)?
            (?:(?:-[0-9]+)|(?:[-_.]?(?:post|rev|r)[-_.]?[0-9]*))?
            (?:[-_.]?dev[-_.]?[0-9]*)?
        )
    )
    \s*\z
";

static SPECIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SPECIFIER_PATTERN).expect("specifier regex is valid"));

/// A single PEP 440 version specifier, e.g. `>=1.2.3`, `==1.0.*`, or `~=2.2`.
#[derive(Debug, Clone)]
pub struct Specifier {
    /// One of `~=`, `==`, `!=`, `<=`, `>=`, `<`, `>`, `===`.
    operator: String,
    /// The version part as written (whitespace-stripped), e.g. `1.2.3` or `1.0.*`.
    version: String,
    /// Explicit pre-release policy (`None` = autodetect). Mirrors `_prereleases`.
    prereleases: Option<bool>,
}

impl Specifier {
    /// Parse a specifier string. Equivalent to `specifiers.py`'s `Specifier.__init__`.
    pub fn parse(spec: &str) -> Result<Specifier, InvalidSpecifier> {
        if !SPECIFIER_RE.is_match(spec) {
            return Err(InvalidSpecifier(spec.to_string()));
        }

        let spec = spec.trim();
        // The regex guarantees the operator; split it off and strip the version.
        let (operator, version) = if let Some(rest) = spec.strip_prefix("===") {
            ("===", rest.trim())
        } else if spec.starts_with("~=")
            || spec.starts_with("==")
            || spec.starts_with("!=")
            || spec.starts_with("<=")
            || spec.starts_with(">=")
        {
            (&spec[..2], spec[2..].trim())
        } else {
            // Remaining single-char operators: `<` or `>`.
            (&spec[..1], spec[1..].trim())
        };

        Ok(Specifier {
            operator: operator.to_string(),
            version: version.to_string(),
            prereleases: None,
        })
    }

    /// Return a copy with an explicit pre-release policy (`Some(true)`/`Some(false)`), or
    /// `None` to restore autodetection. Mirrors the `prereleases=` constructor argument.
    pub fn with_prereleases(mut self, value: Option<bool>) -> Self {
        self.prereleases = value;
        self
    }

    /// The operator, e.g. `">="`.
    pub fn operator(&self) -> &str {
        &self.operator
    }

    /// The version part, e.g. `"1.2.3"`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Whether pre-releases are accepted by default for this specifier.
    ///
    /// An explicit setting wins; otherwise `!=` and `==V.*` imply `false`, an unparsable
    /// (`===`) version is unknown (`None`), and any other operator follows whether its own
    /// version is a pre-release. Port of `specifiers.py`'s `prereleases` property.
    pub fn prereleases(&self) -> Option<bool> {
        if let Some(p) = self.prereleases {
            return Some(p);
        }
        if self.operator == "!=" {
            return Some(false);
        }
        if self.operator == "==" && self.version.ends_with(".*") {
            return Some(false);
        }
        coerce_version(&self.version).map(|v| v.is_prerelease())
    }

    /// Whether `item` satisfies this specifier.
    ///
    /// `prereleases`: `Some(true)`/`Some(false)` force the policy; `None` uses this specifier's
    /// own `prereleases()`, which defaults to excluding pre-releases (only a spec whose own
    /// version is a pre-release admits them). Port of `specifiers.py`'s `Specifier.contains`.
    ///
    /// Divergence: an `===` specifier whose version is not a valid PEP 440 string (e.g.
    /// `===foobar`) returns `false` here for any item it does not match; packaging instead
    /// raises (computing `self.prereleases` parses the arbitrary string and fails). A
    /// `bool`-returning API cannot reproduce that, and returning `false` is the safer behaviour.
    pub fn contains(&self, item: &str, prereleases: Option<bool>) -> bool {
        // `===` coerces the item to a Version, then compares its normalized string to the raw
        // spec version case-insensitively (so `===0` matches `0!0`, and `===1.0` rejects
        // `1.0.0`). An unparsable item never matches.
        if self.operator == "===" {
            let item_v = match coerce_version(item) {
                Some(v) => v,
                None => return false,
            };
            // packaging's `contains` gates on `self.prereleases` (the property) directly, which
            // defaults to false; only an explicit `prereleases=` argument overrides it.
            let effective = prereleases.or_else(|| self.prereleases());
            if effective == Some(false) && item_v.is_prerelease() {
                return false;
            }
            return item_v.to_string().to_lowercase() == self.version.to_lowercase();
        }

        let parsed = match coerce_version(item) {
            Some(v) => v,
            None => return false, // standard operators never match an unparsable input
        };

        let effective = prereleases.or_else(|| self.prereleases());
        if effective == Some(false) && parsed.is_prerelease() {
            return false;
        }

        operator_match(&self.operator, &parsed, &self.version)
    }

    /// Keep the items that match this specifier, honouring the pre-release policy. Port of
    /// `Specifier.filter`. Returned items preserve input order.
    pub fn filter<'a>(&self, items: &[&'a str], prereleases: Option<bool>) -> Vec<&'a str> {
        let effective =
            prereleases.or_else(|| resolve_prereleases(self.prereleases, self.prereleases()));
        let matched: Vec<&'a str> = items
            .iter()
            .copied()
            .filter(|item| self.contains(item, Some(true)))
            .collect();
        apply_prereleases_filter(matched, effective)
    }

    /// The canonical `(operator, version)` pair used for equality, hashing, and
    /// de-duplication. `===` and wildcard versions compare verbatim; other versions are
    /// canonicalized (trailing zeros stripped, except for `~=`). Port of `_canonical_spec`.
    fn canonical_spec(&self) -> (String, String) {
        if self.operator == "===" || self.version.ends_with(".*") {
            return (self.operator.clone(), self.version.clone());
        }
        let v = parse_spec(&self.version);
        (
            self.operator.clone(),
            canonicalize_version(&v, self.operator != "~="),
        )
    }
}

impl fmt::Display for Specifier {
    /// Round-trip form: operator directly followed by version, with no spaces.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.operator, self.version)
    }
}

impl FromStr for Specifier {
    type Err = InvalidSpecifier;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Specifier::parse(s)
    }
}

// --- Operator comparison engine (PEP 440 semantics). ---

/// Parse a string to a version, returning `None` on failure. Port of `coerce_version`.
fn coerce_version(item: &str) -> Option<Version> {
    Version::parse(item).ok()
}

/// Resolve a specifier's effective default pre-release policy. Port of `resolve_prereleases`.
fn resolve_prereleases(configured: Option<bool>, autodetected: Option<bool>) -> Option<bool> {
    if configured.is_some() {
        return configured;
    }
    if autodetected == Some(true) {
        return Some(true);
    }
    None
}

/// The version with its local label removed (`Version(v.public)`).
fn public_of(v: &Version) -> Version {
    Version::parse(&v.public()).expect("public() yields a valid version")
}

/// Whether two versions share a base version (epoch + release, ignoring pre/post/dev/local).
/// Mirrors packaging's `Version(a.base_version) == Version(b.base_version)`.
fn base_version_eq(a: &Version, b: &Version) -> bool {
    parse_spec(&a.base_version()) == parse_spec(&b.base_version())
}

/// `^([0-9]+)((?:a|b|c|rc)[0-9]+)$`: splits a release/pre run like `0rc1` into `0`, `rc1`.
/// Mirrors `specifiers.py`'s `_prefix_regex`.
static PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)\A([0-9]+)((?:a|b|c|rc)[0-9]+)\z").expect("prefix regex"));

/// Split a version *string* into comparison components. Direct port of `_version_split`: works
/// on the raw text (not a parsed `Version`), so quirks like a leading `v` survive into the
/// components, matching packaging exactly.
fn version_split(version: &str) -> Vec<String> {
    let mut result = Vec::new();
    let (epoch, rest) = match version.rsplit_once('!') {
        Some((e, r)) => (e, r),
        None => ("", version),
    };
    result.push(if epoch.is_empty() {
        "0".to_string()
    } else {
        epoch.to_string()
    });
    for item in rest.split('.') {
        if let Some(caps) = PREFIX_RE.captures(item) {
            result.push(caps[1].to_string());
            result.push(caps[2].to_string());
        } else {
            result.push(item.to_string());
        }
    }
    result
}

/// Re-join split components as `epoch!rest`. Port of `_version_join`.
fn version_join(components: &[String]) -> String {
    let (epoch, rest) = components.split_first().expect("components include the epoch");
    format!("{epoch}!{}", rest.join("."))
}

/// Whether `segment` is not a pre/post/dev suffix marker. Port of `_is_not_suffix`.
fn is_not_suffix(segment: &str) -> bool {
    !["dev", "a", "b", "rc", "post"]
        .iter()
        .any(|p| segment.starts_with(p))
}

/// 0-pad the release segments of two split versions to equal length. Port of `_pad_version`.
fn pad_version(left: &[String], right: &[String]) -> (Vec<String>, Vec<String>) {
    let is_digit = |x: &&String| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let l_rel: Vec<String> = left.iter().take_while(is_digit).cloned().collect();
    let r_rel: Vec<String> = right.iter().take_while(is_digit).cloned().collect();
    let l_rest = &left[l_rel.len()..];
    let r_rest = &right[r_rel.len()..];
    let mut l = l_rel.clone();
    l.extend(vec!["0".to_string(); r_rel.len().saturating_sub(l_rel.len())]);
    l.extend_from_slice(l_rest);
    let mut r = r_rel.clone();
    r.extend(vec!["0".to_string(); l_rel.len().saturating_sub(r_rel.len())]);
    r.extend_from_slice(r_rest);
    (l, r)
}

/// Dispatch a standard (non-`===`) operator. The version string is a valid PEP 440
/// version (possibly with a trailing `.*` for `==`/`!=`).
fn operator_match(op: &str, parsed: &Version, version: &str) -> bool {
    match op {
        "~=" => compare_compatible(parsed, version),
        "==" => compare_equal(parsed, version),
        "!=" => !compare_equal(parsed, version),
        "<=" => public_of(parsed) <= parse_spec(version),
        ">=" => public_of(parsed) >= parse_spec(version),
        "<" => compare_less_than(parsed, version),
        ">" => compare_greater_than(parsed, version),
        other => unreachable!("unexpected operator {other:?}"),
    }
}

/// Parse a specifier's version part (guaranteed valid by the grammar).
fn parse_spec(version: &str) -> Version {
    Version::parse(version).expect("specifier version is a valid PEP 440 version")
}

/// `~=V`. Direct port of `_compare_compatible`: equivalent to `>=V` and `==prefix.*`, where the
/// prefix is `V`'s components (sans suffixes) with the last one dropped, built from the raw
/// spec string.
fn compare_compatible(parsed: &Version, version: &str) -> bool {
    let kept: Vec<String> = version_split(version)
        .into_iter()
        .take_while(|s| is_not_suffix(s))
        .collect();
    let prefix_parts = &kept[..kept.len().saturating_sub(1)];
    let mut prefix = version_join(prefix_parts);
    prefix.push_str(".*");
    public_of(parsed) >= parse_spec(version) && compare_equal(parsed, &prefix)
}

/// `==V` or `==V.*`. Direct port of `_compare_equal`: wildcard does prefix matching over the
/// 0-padded, normalized component lists; otherwise exact equality, comparing public versions
/// unless the spec carries a local label.
fn compare_equal(parsed: &Version, version: &str) -> bool {
    if let Some(base) = version.strip_suffix(".*") {
        let normalized_prospective = crate::canonicalize_version(&parsed.public(), false);
        let normalized_spec = crate::canonicalize_version(base, false);
        let split_spec = version_split(&normalized_spec);
        let split_prospective = version_split(&normalized_prospective);
        let (padded_prospective, _) = pad_version(&split_prospective, &split_spec);
        let shortened: Vec<String> = padded_prospective.into_iter().take(split_spec.len()).collect();
        shortened == split_spec
    } else {
        let spec = parse_spec(version);
        if spec.local().is_none() {
            public_of(parsed) == spec
        } else {
            *parsed == spec
        }
    }
}

/// `<V`. Direct port of packaging's `_compare_less_than`: below `V`, but a pre-release of `V`'s
/// own base version is excluded unless `V` is itself a pre-release.
fn compare_less_than(parsed: &Version, version: &str) -> bool {
    let spec = parse_spec(version);
    if !(*parsed < spec) {
        return false;
    }
    if !spec.is_prerelease() && parsed.is_prerelease() && base_version_eq(parsed, &spec) {
        return false;
    }
    true
}

/// `>V`. Direct port of packaging's `_compare_greater_than`: above `V`, but a post-release of
/// `V`'s base version is excluded unless `V` is itself a post-release, and a local of `V`'s base
/// version is always excluded.
fn compare_greater_than(parsed: &Version, version: &str) -> bool {
    let spec = parse_spec(version);
    if !(*parsed > spec) {
        return false;
    }
    if !spec.is_postrelease() && parsed.is_postrelease() && base_version_eq(parsed, &spec) {
        return false;
    }
    if parsed.local().is_some() && base_version_eq(parsed, &spec) {
        return false;
    }
    true
}

/// Apply the three-way pre-release policy to an already-matched, order-preserving list.
/// `Some(true)` keeps everything; `Some(false)` drops parsable pre-releases; `None` is the
/// PEP 440 default. Port of `_apply_prereleases_filter` / `_pep440_filter_prereleases`.
fn apply_prereleases_filter(matched: Vec<&str>, effective: Option<bool>) -> Vec<&str> {
    match effective {
        Some(true) => matched,
        Some(false) => matched
            .into_iter()
            .filter(|item| !coerce_version(item).is_some_and(|v| v.is_prerelease()))
            .collect(),
        None => pep440_buffer(matched),
    }
}

/// PEP 440 default filtering: stream finals immediately; hold pre-releases (and arbitrary
/// strings seen before the first final) back, emitting them only if no final ever appears.
fn pep440_buffer(matched: Vec<&str>) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut all_nonfinal: Vec<&str> = Vec::new();
    let mut arbitrary: Vec<&str> = Vec::new();
    let mut found_final = false;

    for item in matched {
        match coerce_version(item) {
            // Unparsable (arbitrary string): yield once a final has appeared, else buffer.
            None => {
                if found_final {
                    out.push(item);
                } else {
                    arbitrary.push(item);
                    all_nonfinal.push(item);
                }
            }
            // Final release: on the first one, flush the leading arbitrary strings.
            Some(v) if !v.is_prerelease() => {
                if !found_final {
                    out.append(&mut arbitrary);
                    found_final = true;
                }
                out.push(item);
            }
            // Pre-release: buffer until a final appears; dropped if one ever does.
            Some(_) => {
                if !found_final {
                    all_nonfinal.push(item);
                }
            }
        }
    }

    if !found_final {
        out.append(&mut all_nonfinal);
    }
    out
}

// --- Canonicalization, equality, and SpecifierSet. ---

/// Canonical string form of a version (port of `canonicalize_version`). With
/// `strip_trailing_zero` the release segment loses trailing zeros (keeping at least one
/// component); otherwise it is the plain normalized string.
fn canonicalize_version(v: &Version, strip_trailing_zero: bool) -> String {
    if !strip_trailing_zero {
        return v.to_string();
    }
    let mut parts = v.to_parts();
    parts.release = v.release_trimmed();
    Version::from_parts(parts)
        .expect("a trimmed release is still a valid version")
        .to_string()
}

// Specifier equality and hashing compare canonical specs and ignore the pre-release setting.
impl PartialEq for Specifier {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_spec() == other.canonical_spec()
    }
}

impl Eq for Specifier {}

impl Hash for Specifier {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.canonical_spec().hash(state);
    }
}

/// A set of version specifiers, e.g. `>=1.0,!=1.1,<2.0`. Port of `SpecifierSet`.
#[derive(Debug, Clone)]
pub struct SpecifierSet {
    specs: Vec<Specifier>,
    prereleases: Option<bool>,
}

impl SpecifierSet {
    /// Parse a comma-separated specifier string. Empty pieces are ignored, so `""` yields an
    /// empty set that matches everything.
    pub fn parse(specifiers: &str) -> Result<SpecifierSet, InvalidSpecifier> {
        let mut specs = Vec::new();
        for part in specifiers.split(',') {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                specs.push(Specifier::parse(trimmed)?);
            }
        }
        Ok(SpecifierSet {
            specs,
            prereleases: None,
        })
    }

    /// Return a copy with an explicit pre-release policy (`None` restores autodetection).
    pub fn with_prereleases(mut self, value: Option<bool>) -> Self {
        self.prereleases = value;
        self
    }

    /// The specifiers in this set, in their original (unsorted) order.
    pub fn specifiers(&self) -> &[Specifier] {
        &self.specs
    }

    /// The number of specifiers.
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Whether the set has no specifiers (matches everything).
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Whether pre-releases are accepted by default. An explicit setting wins; otherwise the
    /// set accepts them if any of its specifiers does. Port of the `prereleases` property.
    pub fn prereleases(&self) -> Option<bool> {
        if let Some(p) = self.prereleases {
            return Some(p);
        }
        if self.specs.is_empty() {
            return None;
        }
        if self.specs.iter().any(|s| s.prereleases() == Some(true)) {
            return Some(true);
        }
        None
    }

    /// Whether `item` satisfies every specifier in the set, under a unified pre-release
    /// policy. Port of `SpecifierSet.contains` (single-item semantics; the `installed`
    /// argument is not modelled).
    pub fn contains(&self, item: &str, prereleases: Option<bool>) -> bool {
        let effective = prereleases.or_else(|| self.prereleases());

        let matches = self.matches_all(item);

        // packaging gates with `not prereleases`, so a prerelease is excluded unless the policy
        // is explicitly true (a `None`/unknown policy still excludes, unlike a single specifier).
        if effective != Some(true) && coerce_version(item).is_some_and(|v| v.is_prerelease()) {
            return false;
        }
        matches
    }

    /// Whether every specifier matches `item` (bare operator match; an empty set matches
    /// everything). Pre-release gating is applied by the set's `contains`/`filter`, not here.
    fn matches_all(&self, item: &str) -> bool {
        self.specs.iter().all(|s| s.contains(item, Some(true)))
    }

    /// Keep the items that satisfy every specifier, honouring the unified pre-release
    /// policy. Port of `SpecifierSet.filter`.
    pub fn filter<'a>(&self, items: &[&'a str], prereleases: Option<bool>) -> Vec<&'a str> {
        let effective = prereleases.or_else(|| self.prereleases());
        let matched: Vec<&'a str> = items
            .iter()
            .copied()
            .filter(|item| self.matches_all(item))
            .collect();
        apply_prereleases_filter(matched, effective)
    }

    /// Deduplicated, string-sorted specifiers, for canonical display/equality.
    fn canonical_specs(&self) -> Vec<Specifier> {
        let mut sorted = self.specs.clone();
        sorted.sort_by_key(|s| s.to_string());
        let mut result: Vec<Specifier> = Vec::new();
        for spec in sorted {
            if !result.contains(&spec) {
                result.push(spec);
            }
        }
        result
    }
}

impl fmt::Display for SpecifierSet {
    /// Canonical round-trip form: deduplicated specifiers, sorted, joined by `,`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<String> = self
            .canonical_specs()
            .iter()
            .map(|s| s.to_string())
            .collect();
        write!(f, "{}", rendered.join(","))
    }
}

impl FromStr for SpecifierSet {
    type Err = InvalidSpecifier;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SpecifierSet::parse(s)
    }
}

// Equality and hashing ignore the pre-release setting and the original ordering.
impl PartialEq for SpecifierSet {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_specs() == other.canonical_specs()
    }
}

impl Eq for SpecifierSet {}

impl Hash for SpecifierSet {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.canonical_specs().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(spec: &str) -> Specifier {
        Specifier::parse(spec).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parses_each_operator() {
        let cases = [
            ("==1.2.3", "==", "1.2.3"),
            ("!=1.0", "!=", "1.0"),
            (">=1.0.0", ">=", "1.0.0"),
            ("<=2.0", "<=", "2.0"),
            ("<2", "<", "2"),
            (">3", ">", "3"),
            ("~=2.2", "~=", "2.2"),
            ("===foobar", "===", "foobar"),
            ("==1.0.*", "==", "1.0.*"),
        ];
        for (input, op, ver) in cases {
            let spec = s(input);
            assert_eq!(spec.operator(), op, "operator of {input:?}");
            assert_eq!(spec.version(), ver, "version of {input:?}");
        }
    }

    #[test]
    fn strips_surrounding_and_inner_whitespace() {
        let spec = s("  >=  1.0  ");
        assert_eq!(spec.operator(), ">=");
        assert_eq!(spec.version(), "1.0");
        assert_eq!(spec.to_string(), ">=1.0");
    }

    #[test]
    fn display_roundtrips() {
        for input in ["==1.2.3", ">=1.0.0", "~=2.2", "!=1.0.*", "===foobar", "<2.0"] {
            assert_eq!(s(input).to_string(), input);
        }
    }

    #[test]
    fn rejects_invalid() {
        for bad in [
            "lolwat",    // no operator
            "1.0.0",     // missing operator
            "==",        // missing version
            "= =1.0",    // broken operator
            "~=1",       // compatible needs >= 2 release components
            "<1.0.*",    // wildcard only allowed for == / !=
            ">=1.0+abc", // local not allowed outside == / !=
            "~=1.0+abc", // local not allowed for ~=
        ] {
            assert!(
                Specifier::parse(bad).is_err(),
                "expected {bad:?} to be invalid"
            );
        }
    }

    #[test]
    fn greater_equal_excludes_prereleases_by_default() {
        let spec = s(">=1.2.3");
        assert!(spec.contains("1.2.3", None));
        assert!(!spec.contains("1.0.0", None));
        // A non-prerelease spec excludes matching pre-releases by default (packaging semantics).
        assert!(!spec.contains("1.3.0a1", None));
        assert!(spec.contains("1.3.0a1", Some(true))); // ...unless forced on
    }

    #[test]
    fn equal_and_not_equal() {
        assert!(s("==1.2.3").contains("1.2.3", None));
        assert!(s("==1.2.3").contains("1.2.3.0", None)); // trailing-zero equal
        assert!(!s("==1.2.3").contains("1.2.4", None));
        assert!(s("==1.2.3").contains("1.2.3+local", None)); // spec has no local -> local ignored
        assert!(!s("!=1.2.3").contains("1.2.3", None));
        assert!(s("!=1.2.3").contains("1.2.4", None));
    }

    #[test]
    fn wildcards() {
        let spec = s("==1.0.*");
        assert!(spec.contains("1.0", None));
        assert!(spec.contains("1.0.5", None));
        assert!(!spec.contains("1.1", None));
        assert!(spec.contains("1.0a1", Some(true)));
        let neq = s("!=1.0.*");
        assert!(!neq.contains("1.0.5", None));
        assert!(neq.contains("1.1", None));
    }

    #[test]
    fn compatible_release() {
        let spec = s("~=2.2");
        assert!(spec.contains("2.2", None));
        assert!(spec.contains("2.5", None));
        assert!(!spec.contains("3.0", None));
        assert!(!spec.contains("2.1", None));
        let three = s("~=1.4.5");
        assert!(three.contains("1.4.5", None));
        assert!(three.contains("1.4.99", None));
        assert!(!three.contains("1.5.0", None));
    }

    #[test]
    fn strict_less_and_greater_exclude_boundary_family() {
        // <1.0 excludes 1.0 and its own pre-releases.
        let lt = s("<1.0");
        assert!(lt.contains("0.9", None));
        assert!(!lt.contains("1.0", None));
        assert!(!lt.contains("1.0a1", Some(true))); // pre-release of 1.0 is excluded
        // >1.0 excludes 1.0, its posts, and its locals.
        let gt = s(">1.0");
        assert!(gt.contains("1.0.1", None));
        assert!(!gt.contains("1.0", None));
        assert!(!gt.contains("1.0.post1", None));
        assert!(!gt.contains("1.0+local", None));
        assert!(gt.contains("2.0", None));
    }

    #[test]
    fn arbitrary_equality() {
        // `===V` compares the normalized item against the raw spec string.
        let spec = s("===1.0");
        assert!(spec.contains("1.0", None));
        assert!(spec.contains("v1.0", None)); // normalizes to "1.0"
        assert!(!spec.contains("1.0.0", None)); // normalizes to "1.0.0", not "1.0"
        assert!(!spec.contains("2.0", None));
        assert!(s("===0").contains("0!0", None)); // "0!0" normalizes to "0"
        // A non-PEP 440 spec/item raises in packaging; here it simply never matches (no panic).
        assert!(!s("===foobar").contains("foobar", None));
    }

    #[test]
    fn prerelease_spec_accepts_prereleases() {
        // A pre-release in the spec implies pre-releases are accepted by default.
        let spec = s(">=1.0a1");
        assert!(spec.contains("1.0a2", None));
        assert!(spec.contains("1.0b1", None));
    }

    fn set(s: &str) -> SpecifierSet {
        SpecifierSet::parse(s).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn set_contains_requires_all() {
        let ss = set(">=1.0.0,!=1.0.1");
        assert!(ss.contains("1.2.3", None));
        assert!(!ss.contains("1.0.1", None)); // excluded by !=
        assert!(!ss.contains("0.9", None)); // below >=
        assert!(!ss.contains("1.3.0a1", None)); // pre-release excluded by default
        assert!(ss.contains("1.3.0a1", Some(true)));
    }

    #[test]
    fn set_intersection_bounds() {
        let ss = set(">=1.0,<2.0");
        assert!(ss.contains("1.5", None));
        assert!(!ss.contains("2.0", None));
        assert!(!ss.contains("0.5", None));
    }

    #[test]
    fn empty_set_matches_everything() {
        let ss = set("");
        assert!(ss.is_empty());
        assert!(ss.contains("1.0", None));
        // An empty set has an unknown (None) prerelease policy, which still excludes prereleases.
        assert!(!ss.contains("1.5a1", None));
        assert!(ss.contains("1.5a1", Some(true)));
    }

    #[test]
    fn set_display_sorts_and_dedupes() {
        assert_eq!(set(">=1.0.0,!=2.0.0").to_string(), "!=2.0.0,>=1.0.0");
        assert_eq!(set("==1.0,==1.0.0").to_string(), "==1.0"); // canonical duplicate dropped
        assert_eq!(set("").to_string(), "");
    }

    #[test]
    fn set_equality_is_canonical() {
        assert_eq!(set(">=1.0,!=1.1"), set("!=1.1,>=1.0")); // order-independent
        assert_eq!(set("==1.0"), set("==1.0.0")); // canonical
        assert_ne!(set(">=1.0"), set(">=2.0"));
    }

    #[test]
    fn set_rejects_invalid_member() {
        assert!(SpecifierSet::parse(">=1.0,lolwat").is_err());
    }

    #[test]
    fn specifier_filter_buffers_prereleases() {
        let spec = s(">=1.2.3");
        // A final exists, so the matching pre-release is dropped.
        assert_eq!(spec.filter(&["1.2", "1.3", "1.5a1"], None), vec!["1.3"]);
        // No final matches, so the pre-release is kept.
        assert_eq!(spec.filter(&["1.2", "1.5a1"], None), vec!["1.5a1"]);
        // Forced on: keep everything that matches.
        assert_eq!(spec.filter(&["1.3", "1.5a1"], Some(true)), vec!["1.3", "1.5a1"]);
        // Forced off: drop the pre-release.
        assert_eq!(spec.filter(&["1.3", "1.5a1"], Some(false)), vec!["1.3"]);
    }

    #[test]
    fn set_filter() {
        let ss = set(">=1.2.3");
        assert_eq!(ss.filter(&["1.2", "1.3", "1.5a1"], None), vec!["1.3"]);

        let empty = set("");
        assert_eq!(empty.filter(&["1.3", "1.5a1"], None), vec!["1.3"]);
        assert_eq!(empty.filter(&["1.5a1"], None), vec!["1.5a1"]);
        assert_eq!(
            empty.filter(&["1.3", "1.5a1"], Some(true)),
            vec!["1.3", "1.5a1"]
        );
    }
}
