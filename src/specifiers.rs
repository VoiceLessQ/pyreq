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

use crate::{Version, VersionParts};

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
    /// `prereleases`: `Some(true)`/`Some(false)` force the policy; `None` autodetects.
    /// Port of `specifiers.py`'s `Specifier.contains`. Note: unlike `filter`, a single
    /// `contains` does *not* apply the "buffer pre-releases until a final" rule — a
    /// matching pre-release is included unless the policy is explicitly `false`.
    pub fn contains(&self, item: &str, prereleases: Option<bool>) -> bool {
        // `===` matches the raw string case-insensitively; a parse would be wasted.
        if self.operator == "===" {
            if item.to_lowercase() != self.version.to_lowercase() {
                return false;
            }
            let effective = prereleases.or_else(|| resolve_prereleases(self.prereleases, self.prereleases()));
            if effective == Some(false) && coerce_version(item).is_some_and(|v| v.is_prerelease()) {
                return false;
            }
            return true;
        }

        let parsed = match coerce_version(item) {
            Some(v) => v,
            None => return false, // standard operators never match an unparsable input
        };

        let effective = prereleases.or_else(|| resolve_prereleases(self.prereleases, self.prereleases()));
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

/// `V` with `dev` forced to 0 and any local removed (`V.__replace__(dev=0, local=None)`).
fn with_dev0_no_local(v: &Version) -> Version {
    let mut parts = v.to_parts();
    parts.dev = Some(0);
    parts.local = None;
    Version::from_parts(parts).expect("a .dev0 version is valid")
}

/// Whether `p` shares `v`'s "family": same epoch, same trimmed release, and same
/// pre-release. Used to carve `V` and its post/local variants out of `>V`.
fn same_family(p: &Version, v: &Version) -> bool {
    if p.epoch() != v.epoch() {
        return false;
    }
    let trimmed = v.release_trimmed();
    let release = p.release();
    if release.len() < trimmed.len() {
        return false;
    }
    if release[..trimmed.len()] != trimmed[..] {
        return false;
    }
    if release[trimmed.len()..].iter().any(|&x| x != 0) {
        return false;
    }
    p.pre() == v.pre()
}

/// Construct `epoch!release.dev0`, the smallest version with that epoch+release.
fn dev0(epoch: u64, release: Vec<u64>) -> Version {
    Version::from_parts(VersionParts {
        epoch,
        release,
        dev: Some(0),
        ..Default::default()
    })
    .expect("a .dev0 boundary is always a valid version")
}

/// Dispatch a standard (non-`===`) operator. The version string is a valid PEP 440
/// version (possibly with a trailing `.*` for `==`/`!=`).
fn operator_match(op: &str, parsed: &Version, version: &str) -> bool {
    match op {
        "~=" => compare_compatible(parsed, version),
        "==" => compare_equal(parsed, version),
        "!=" => !compare_equal(parsed, version),
        "<=" => public_of(parsed) <= parse_spec(version),
        ">=" => *parsed >= parse_spec(version),
        "<" => compare_less_than(parsed, version),
        ">" => compare_greater_than(parsed, version),
        other => unreachable!("unexpected operator {other:?}"),
    }
}

/// Parse a specifier's version part (guaranteed valid by the grammar).
fn parse_spec(version: &str) -> Version {
    Version::parse(version).expect("specifier version is a valid PEP 440 version")
}

/// `~=V`: `>=V` and below the next release of `V`'s prefix (`V` with its last release
/// component dropped, then incremented). `~=` requires at least two release components.
fn compare_compatible(parsed: &Version, version: &str) -> bool {
    let spec = parse_spec(version);
    let mut prefix = spec.release().to_vec();
    prefix.pop(); // drop last component; grammar guarantees >= 2 remained
    *prefix.last_mut().expect("compatible prefix is non-empty") += 1;
    let upper = dev0(spec.epoch(), prefix);
    *parsed >= spec && *parsed < upper
}

/// `==V` or `==V.*`. Wildcard means the half-open prefix range `[V.dev0, next-prefix.dev0)`;
/// otherwise exact equality, comparing public versions unless the spec carries a local.
fn compare_equal(parsed: &Version, version: &str) -> bool {
    if let Some(base) = version.strip_suffix(".*") {
        let spec = parse_spec(base);
        let lower = dev0(spec.epoch(), spec.release().to_vec());
        let mut upper_release = spec.release().to_vec();
        *upper_release.last_mut().expect("release is non-empty") += 1;
        let upper = dev0(spec.epoch(), upper_release);
        *parsed >= lower && *parsed < upper
    } else {
        let spec = parse_spec(version);
        if spec.local().is_none() {
            public_of(parsed) == spec
        } else {
            *parsed == spec
        }
    }
}

/// `<V`. The upper bound is `V` itself when `V` is a pre-release, otherwise `V.dev0` — so
/// `<V` of a final/post release excludes `V` and all of `V`'s own pre/post/local versions.
fn compare_less_than(parsed: &Version, version: &str) -> bool {
    let spec = parse_spec(version);
    let bound = if spec.is_prerelease() {
        spec
    } else {
        with_dev0_no_local(&spec)
    };
    *parsed < bound
}

/// `>V`. For a dev/post spec the lower bound is the next dev/post release; otherwise `>V`
/// excludes `V` and its whole family (same epoch, release, and pre — i.e. `V`, `V+local`,
/// and every `V.postN`).
fn compare_greater_than(parsed: &Version, version: &str) -> bool {
    let spec = parse_spec(version);

    if let Some(dev) = spec.dev() {
        // >V.devN: dev releases have no posts, so the next real version is V.dev(N+1).
        let mut parts = spec.to_parts();
        parts.dev = Some(dev + 1);
        parts.local = None;
        return *parsed >= Version::from_parts(parts).expect("valid lower bound");
    }
    if let Some(post) = spec.post() {
        // >V.postN: the next real version is V.post(N+1).dev0.
        let mut parts = spec.to_parts();
        parts.post = Some(post + 1);
        parts.dev = Some(0);
        parts.local = None;
        return *parsed >= Version::from_parts(parts).expect("valid lower bound");
    }

    *parsed > spec && !same_family(parsed, &spec)
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

        if effective == Some(false) && coerce_version(item).is_some_and(|v| v.is_prerelease()) {
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
    fn greater_equal_includes_prereleases_by_default() {
        let spec = s(">=1.2.3");
        assert!(spec.contains("1.2.3", None));
        assert!(!spec.contains("1.0.0", None));
        assert!(spec.contains("1.3.0a1", None)); // matching pre-release allowed by default
        assert!(!spec.contains("1.3.0a1", Some(false))); // ...but not when forced off
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
        let spec = s("===foobar");
        assert!(spec.contains("foobar", None));
        assert!(spec.contains("FOOBAR", None)); // case-insensitive
        assert!(!spec.contains("foobaz", None));
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
        assert!(ss.contains("1.3.0a1", None)); // pre-release included by default
        assert!(!ss.contains("1.3.0a1", Some(false)));
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
        assert!(ss.contains("1.5a1", None)); // a lone pre-release is included
        assert!(!ss.contains("1.5a1", Some(false)));
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
