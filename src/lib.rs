//! A Rust port of Python's `packaging` library — PEP 440 versions.
//!
//! Port target: `../Reference/packaging/src/packaging/version.py`.
//! Spec: <https://peps.python.org/pep-0440/>.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;

mod specifiers;
pub use specifiers::{InvalidSpecifier, Specifier, SpecifierSet};

mod tokenizer;

mod markers;
pub use markers::{EnvValue, EvaluateContext, InvalidMarker, Marker, MarkerError};

mod requirements;
pub use requirements::{InvalidRequirement, Requirement};

mod tags;
pub use tags::{InvalidTag, Tag, TagParseError, UnsortedTagsError, parse_tag};

mod utils;
pub use utils::{
    BuildTag, InvalidSdistFilename, InvalidWheelFilename, WheelFilename, canonicalize_name,
    canonicalize_version, is_normalized_name, parse_sdist_filename, parse_wheel_filename,
};

/// The letter of a pre-release segment, already normalized to its canonical form.
///
/// PEP 440 spells these many ways (`alpha`, `a`, `beta`, `b`, `c`, `pre`, `preview`,
/// `rc`), but they all normalize to exactly three values. Mirrors the `"a" | "b" | "rc"`
/// `Literal` that `version.py` uses for `Version._pre`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreLetter {
    A,
    B,
    Rc,
}

impl PreLetter {
    /// The canonical lowercase spelling, as it appears in a normalized version string.
    fn as_str(self) -> &'static str {
        match self {
            PreLetter::A => "a",
            PreLetter::B => "b",
            PreLetter::Rc => "rc",
        }
    }
}

/// One segment of a local version label (the part after `+`).
///
/// `version.py` stores each dot/underscore/dash-separated part as either an `int` (when
/// all-digits) or a lowercased `str`. Python's dynamic typing lets it mix the two in one
/// tuple; in Rust that mix becomes an enum. See `_parse_local_version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalSegment {
    /// An all-digit segment, parsed as a number (e.g. the `1` in `+ubuntu.1`).
    Number(u64),
    /// A non-numeric segment, lowercased (e.g. the `ubuntu` in `+ubuntu.1`).
    String(String),
}

/// A parsed PEP 440 version.
///
/// This is the structured form of a version string, matching the six fields
/// `version.py` parses into (`_epoch`, `_release`, `_pre`, `_post`, `_dev`, `_local`).
/// Parsing populates it (step 2); ordering is derived from it via a comparison key
/// (step 3), *not* from these fields directly — equality and order both run through that
/// key, because e.g. `1.0.0` must compare equal to `1`.
///
/// Example: `1!2.3.4a5.post6.dev7+ubuntu.1` parses to
/// `epoch=1`, `release=[2,3,4]`, `pre=(A,5)`, `post=6`, `dev=7`, `local=[ubuntu, 1]`.
#[derive(Debug, Clone)]
pub struct Version {
    /// The epoch — the `N!` prefix. Defaults to `0` when absent.
    epoch: u64,

    /// The release segment, e.g. `[1, 2, 3]` for `1.2.3`. Always at least one element.
    release: Vec<u64>,

    /// Pre-release: the normalized letter and its number. A bare `1.0a` gets an
    /// implicit `0`, so this is `Some((PreLetter::A, 0))`. `None` for final releases.
    pre: Option<(PreLetter, u64)>,

    /// Post-release number (the `N` in `.postN`). `None` if absent.
    post: Option<u64>,

    /// Development-release number (the `N` in `.devN`). `None` if absent.
    dev: Option<u64>,

    /// Local version label, already split into segments. `None` if there is no `+`.
    local: Option<Vec<LocalSegment>>,
}

/// Raised when a string is not a valid PEP 440 version. Holds the offending string.
///
/// Mirrors `version.py`'s `InvalidVersion`. The message format approximates Python's
/// `f"Invalid version: {version!r}"`; exact `repr` quoting parity is deferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidVersion(pub String);

impl std::fmt::Display for InvalidVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid version: '{}'", self.0)
    }
}

impl std::error::Error for InvalidVersion {}

/// PEP 440's version grammar, translated from `version.py`'s `VERSION_PATTERN`.
///
/// Differences from the Python source, all behaviour-preserving:
/// - Possessive quantifiers (`*+`, `?+`) become plain greedy ones — this is exactly the
///   `_VERSION_PATTERN_OLD` form upstream itself uses on regex engines without possessive
///   support, so it's the reference behaviour.
/// - Inline flags `(?xi)` = verbose + case-insensitive (Python's `re.VERBOSE | re.IGNORECASE`).
/// - Anchored with `\A\s*` … `\s*\z` to emulate `fullmatch` over a whitespace-trimmed string.
/// - Outer `pre`/`post`/`dev` capture groups are dropped (unused); only the inner
///   `_l`/`_n` groups are kept.
///
/// Caveat: case-insensitivity here is Unicode-aware, where Python scopes ASCII via `(?a:)`.
/// Version strings are ASCII in practice; revisit only if differential testing flags it.
const VERSION_PATTERN: &str = r"(?xi)
    \A\s*
    v?
    (?:(?P<epoch>[0-9]+)!)?
    (?P<release>[0-9]+(?:\.[0-9]+)*)
    (?:
        [._-]?
        (?P<pre_l>alpha|a|beta|b|preview|pre|c|rc)
        [._-]?
        (?P<pre_n>[0-9]+)?
    )?
    (?:
        (?:-(?P<post_n1>[0-9]+))
        |
        (?:
            [._-]?
            (?P<post_l>post|rev|r)
            [._-]?
            (?P<post_n2>[0-9]+)?
        )
    )?
    (?:
        [._-]?
        (?P<dev_l>dev)
        [._-]?
        (?P<dev_n>[0-9]+)?
    )?
    (?:\+
        (?P<local>[a-z0-9]+(?:[._-][a-z0-9]+)*)
    )?
    \s*\z
";

/// Compiled once, lazily, on first use. `LazyLock` is the std equivalent of `once_cell`.
static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(VERSION_PATTERN).expect("version regex is valid"));

/// `frozenset(".0123456789")` — the chars that allow `version.py`'s regex-free fast path.
fn is_simple(version: &str) -> bool {
    version.bytes().all(|b| b == b'.' || b.is_ascii_digit())
}

/// Normalize a pre-release letter to its canonical form, per `_LETTER_NORMALIZATION`.
/// The regex guarantees the input is one of the known spellings.
fn normalize_pre_letter(letter: &str) -> PreLetter {
    match letter.to_ascii_lowercase().as_str() {
        "alpha" | "a" => PreLetter::A,
        "beta" | "b" => PreLetter::B,
        "c" | "pre" | "preview" | "rc" => PreLetter::Rc,
        other => unreachable!("regex restricts pre letters; got {other:?}"),
    }
}

/// `int(number or 0)` — a missing numeric capture means an implicit `0`.
/// The capture, when present, is `[0-9]+`, so the parse cannot fail (modulo the u64 cap).
fn cap_int(m: Option<regex::Match>) -> u64 {
    m.map(|x| x.as_str().parse().expect("digit group fits u64"))
        .unwrap_or(0)
}

/// `_parse_local_version`: split on `. _ -`; numeric parts become ints, others lowercase.
fn parse_local(local: &str) -> Vec<LocalSegment> {
    local
        .split(['.', '_', '-'])
        .map(|part| {
            if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
                LocalSegment::Number(part.parse().expect("digit segment fits u64"))
            } else {
                LocalSegment::String(part.to_ascii_lowercase())
            }
        })
        .collect()
}

/// Render local segments back to canonical form: parts joined by `.`, the inverse of
/// `parse_local`. Mirrors the body of `version.py`'s `local` property.
fn render_local(segments: &[LocalSegment]) -> String {
    segments
        .iter()
        .map(|seg| match seg {
            LocalSegment::Number(n) => n.to_string(),
            LocalSegment::String(s) => s.clone(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

impl Version {
    /// Parse a version string into structured form. Equivalent to `version.py`'s
    /// `Version.__init__` / the module-level `parse()`.
    pub fn parse(version: &str) -> Result<Version, InvalidVersion> {
        // Fast path: only digits and dots → the release segment, nothing else.
        if is_simple(version) {
            let mut release = Vec::new();
            for part in version.split('.') {
                // Empty parts (from "1..2", ".1", "1.", "") are invalid versions.
                let n = part
                    .parse::<u64>()
                    .map_err(|_| InvalidVersion(version.to_string()))?;
                release.push(n);
            }
            return Ok(Version {
                epoch: 0,
                release,
                pre: None,
                post: None,
                dev: None,
                local: None,
            });
        }

        let caps = VERSION_RE
            .captures(version)
            .ok_or_else(|| InvalidVersion(version.to_string()))?;

        let epoch = cap_int(caps.name("epoch"));

        let release = caps
            .name("release")
            .expect("release is mandatory in the regex")
            .as_str()
            .split('.')
            .map(|p| p.parse::<u64>().expect("digit group fits u64"))
            .collect();

        let pre = caps
            .name("pre_l")
            .map(|l| (normalize_pre_letter(l.as_str()), cap_int(caps.name("pre_n"))));

        // Post has two spellings: the explicit `.postN` (letter present, implicit-0
        // number) and the implicit `-N` form (no letter, number required).
        let post = if caps.name("post_l").is_some() {
            Some(cap_int(caps.name("post_n2")))
        } else {
            caps.name("post_n1")
                .map(|n| n.as_str().parse::<u64>().expect("digit group fits u64"))
        };

        let dev = caps.name("dev_l").map(|_| cap_int(caps.name("dev_n")));

        let local = caps.name("local").map(|l| parse_local(l.as_str()));

        Ok(Version {
            epoch,
            release,
            pre,
            post,
            dev,
            local,
        })
    }
}

impl FromStr for Version {
    type Err = InvalidVersion;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Version::parse(s)
    }
}

// --- Accessors: the read-only `@property` surface from `version.py`. ---

impl Version {
    /// The epoch (the `N!` prefix), `0` when absent.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The release segment, e.g. `[1, 2, 3]`. Borrowed — no copy.
    pub fn release(&self) -> &[u64] {
        &self.release
    }

    /// The pre-release `(letter, number)`, or `None` for a final release.
    pub fn pre(&self) -> Option<(PreLetter, u64)> {
        self.pre
    }

    /// The post-release number, or `None`.
    pub fn post(&self) -> Option<u64> {
        self.post
    }

    /// The development-release number, or `None`.
    pub fn dev(&self) -> Option<u64> {
        self.dev
    }

    /// The local label in canonical string form (e.g. `"ubuntu.1"`), or `None`.
    pub fn local(&self) -> Option<String> {
        self.local.as_deref().map(render_local)
    }

    /// The public portion — the full version minus any local label.
    pub fn public(&self) -> String {
        let s = self.to_string();
        match s.split_once('+') {
            Some((head, _)) => head.to_string(),
            None => s,
        }
    }

    /// The base version — epoch and release only, no pre/post/dev/local markers.
    pub fn base_version(&self) -> String {
        let release: Vec<String> = self.release.iter().map(|n| n.to_string()).collect();
        let release = release.join(".");
        if self.epoch != 0 {
            format!("{}!{}", self.epoch, release)
        } else {
            release
        }
    }

    /// Whether this is a pre-release — a pre-release *or* development release.
    pub fn is_prerelease(&self) -> bool {
        self.dev.is_some() || self.pre.is_some()
    }

    /// Whether this is a post-release.
    pub fn is_postrelease(&self) -> bool {
        self.post.is_some()
    }

    /// Whether this is a development release.
    pub fn is_devrelease(&self) -> bool {
        self.dev.is_some()
    }

    /// First release component, or `0` if absent.
    pub fn major(&self) -> u64 {
        self.release.first().copied().unwrap_or(0)
    }

    /// Second release component, or `0` if absent.
    pub fn minor(&self) -> u64 {
        self.release.get(1).copied().unwrap_or(0)
    }

    /// Third release component, or `0` if absent.
    pub fn micro(&self) -> u64 {
        self.release.get(2).copied().unwrap_or(0)
    }

    /// The release with trailing zeros removed but at least one component kept, mirroring
    /// `version.py`'s `_TrimmedRelease.release`. Distinct from the comparison key's trim,
    /// which can drop *every* component; this one always leaves one. Used by specifiers.
    pub fn release_trimmed(&self) -> Vec<u64> {
        let mut end = self.release.len();
        while end > 1 && self.release[end - 1] == 0 {
            end -= 1;
        }
        self.release[..end].to_vec()
    }
}

// --- Builder API: construct/modify without a string, ported from `from_parts`. ---

/// Validation pattern for a local label, from `version.py`'s `_LOCAL_PATTERN`.
static LOCAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\A[a-z0-9]+(?:[._-][a-z0-9]+)*\z").expect("local regex"));

/// The pieces of a [`Version`], for building one without parsing a string.
///
/// Mirrors the keyword arguments of `version.py`'s `from_parts` / `__replace__`. `release`
/// is required (must be non-empty); everything else defaults to "absent". Because the field
/// types are already constrained (`u64` can't be negative, `PreLetter` can't be a bad
/// spelling), the only runtime validation left is "release non-empty" and "local well-formed".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionParts {
    pub epoch: u64,
    pub release: Vec<u64>,
    pub pre: Option<(PreLetter, u64)>,
    pub post: Option<u64>,
    pub dev: Option<u64>,
    /// A local label string (e.g. `"ubuntu.1"`); validated and split on build.
    pub local: Option<String>,
}

impl Version {
    /// Build a [`Version`] directly from its parts, skipping the string parser.
    pub fn from_parts(parts: VersionParts) -> Result<Version, InvalidVersion> {
        if parts.release.is_empty() {
            return Err(InvalidVersion(
                "release must be a non-empty list of components".to_string(),
            ));
        }
        let local = match &parts.local {
            None => None,
            Some(s) if LOCAL_RE.is_match(s) => Some(parse_local(s)),
            Some(s) => return Err(InvalidVersion(format!("invalid local label: {s:?}"))),
        };
        Ok(Version {
            epoch: parts.epoch,
            release: parts.release,
            pre: parts.pre,
            post: parts.post,
            dev: parts.dev,
            local,
        })
    }

    /// Decompose into [`VersionParts`]. Together with [`Version::from_parts`] this covers
    /// `__replace__`: `to_parts()`, tweak a field, `from_parts()`.
    pub fn to_parts(&self) -> VersionParts {
        VersionParts {
            epoch: self.epoch,
            release: self.release.clone(),
            pre: self.pre,
            post: self.post,
            dev: self.dev,
            local: self.local(),
        }
    }
}

/// Module-level helper mirroring `packaging.version.parse`.
pub fn parse(version: &str) -> Result<Version, InvalidVersion> {
    Version::parse(version)
}

// --- Ordering (step 3): the PEP 440 comparison key, ported from `_cmpkey`. ---

/// A single local-version segment in *comparison* form.
///
/// Per PEP 440, within a local label strings sort before numbers, strings compare
/// lexicographically, and numbers compare numerically. `version.py` encodes that with
/// `(-1, s)` for strings and `(n, "")` for ints so plain tuple comparison works; in Rust
/// an enum whose `Str` variant is declared *before* `Int` gives the same order for free —
/// derived `Ord` ranks an earlier-declared variant as smaller.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum LocalCmp {
    Str(String),
    Int(u64),
}

/// The flattened comparison key. Derived `Ord` compares fields top-to-bottom,
/// lexicographically — exactly how Python compares the `(epoch, release, suffix, local)`
/// tuple that `_cmpkey` returns. The field order here *is* the precedence order.
///
/// `local` is an `Option`: `None` sorts before `Some`, reproducing "a version with no
/// local label sorts before one that has it" (Python relies on a 3-tuple being < a
/// 4-tuple for the same effect).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CmpKey {
    epoch: u64,
    release: Vec<u64>,
    pre_rank: i8,
    pre_n: u64,
    post_rank: u8,
    post_n: u64,
    dev_rank: u8,
    dev_n: u64,
    local: Option<Vec<LocalCmp>>,
}

/// Strip trailing zeros from a release so `1.0.0`, `1.0` and `1` share one key.
/// Matches `_cmpkey`'s `while i and release[i-1] == 0` — note it can strip *every*
/// element, so `0`, `0.0`, `0.0.0` all reduce to an empty release and compare equal.
fn trim_release(release: &[u64]) -> Vec<u64> {
    let mut end = release.len();
    while end > 0 && release[end - 1] == 0 {
        end -= 1;
    }
    release[..end].to_vec()
}

impl Version {
    /// Build the PEP 440 comparison key. Pure port of `_cmpkey`.
    fn cmp_key(&self) -> CmpKey {
        // pre_rank: dev-only=-1, a=0, b=1, rc=2, no-pre=3.
        let (pre_rank, pre_n): (i8, u64) = match self.pre {
            Some((PreLetter::A, n)) => (0, n),
            Some((PreLetter::B, n)) => (1, n),
            Some((PreLetter::Rc, n)) => (2, n),
            None => {
                if self.post.is_none() && self.dev.is_some() {
                    // A dev-only release (e.g. 1.0.dev1) sorts before any pre-release.
                    (-1, 0)
                } else {
                    // No pre-release tag sorts after rc.
                    (3, 0)
                }
            }
        };

        // post_rank: no-post=0 (sorts before), post=1.
        let post_rank: u8 = if self.post.is_some() { 1 } else { 0 };
        let post_n = self.post.unwrap_or(0);

        // dev_rank: dev=0 (sorts before), no-dev=1.
        let dev_rank: u8 = if self.dev.is_some() { 0 } else { 1 };
        let dev_n = self.dev.unwrap_or(0);

        let local = self.local.as_ref().map(|segs| {
            segs.iter()
                .map(|seg| match seg {
                    LocalSegment::Number(n) => LocalCmp::Int(*n),
                    LocalSegment::String(s) => LocalCmp::Str(s.clone()),
                })
                .collect()
        });

        CmpKey {
            epoch: self.epoch,
            release: trim_release(&self.release),
            pre_rank,
            pre_n,
            post_rank,
            post_n,
            dev_rank,
            dev_n,
            local,
        }
    }
}

// Equality, ordering and hashing all route through the comparison key, so that e.g.
// `1.0.0 == 1` and equal versions hash alike (mirrors `_BaseVersion`).
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_key() == other.cmp_key()
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_key().cmp(&other.cmp_key())
    }
}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.cmp_key().hash(state);
    }
}

// --- Display (step 4): the normalized round-trip string, ported from `__str__`. ---

impl std::fmt::Display for Version {
    /// Render the canonical PEP 440 string. This *normalizes*: spellings collapse
    /// (`ALPHA`→`a`, `c`→`rc`), the implicit `1.0-1` post form becomes `1.0.post1`, and
    /// local separators all become `.`. Trailing release zeros are *kept* (`1.0.0` stays
    /// `1.0.0`) — only the comparison key trims them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Release segment, e.g. [1, 2, 3] -> "1.2.3", prefixed by the epoch if non-zero.
        if self.epoch != 0 {
            write!(f, "{}!", self.epoch)?;
        }
        let release: Vec<String> = self.release.iter().map(|n| n.to_string()).collect();
        write!(f, "{}", release.join("."))?;

        // Pre-release: letter immediately followed by its number, e.g. "a5".
        if let Some((letter, n)) = self.pre {
            write!(f, "{}{}", letter.as_str(), n)?;
        }
        if let Some(post) = self.post {
            write!(f, ".post{post}")?;
        }
        if let Some(dev) = self.dev {
            write!(f, ".dev{dev}")?;
        }
        // Local label: segments joined by ".", whatever the original separators were.
        if let Some(local) = &self.local {
            write!(f, "+{}", render_local(local))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn simple_release() {
        let ver = v("1.2.3");
        assert_eq!(ver.epoch, 0);
        assert_eq!(ver.release, vec![1, 2, 3]);
        assert_eq!(ver.pre, None);
        assert_eq!(ver.post, None);
        assert_eq!(ver.dev, None);
        assert_eq!(ver.local, None);
    }

    #[test]
    fn the_kitchen_sink() {
        let ver = v("1!2.3.4a5.post6.dev7+ubuntu.1");
        assert_eq!(ver.epoch, 1);
        assert_eq!(ver.release, vec![2, 3, 4]);
        assert_eq!(ver.pre, Some((PreLetter::A, 5)));
        assert_eq!(ver.post, Some(6));
        assert_eq!(ver.dev, Some(7));
        assert_eq!(
            ver.local,
            Some(vec![
                LocalSegment::String("ubuntu".to_string()),
                LocalSegment::Number(1),
            ])
        );
    }

    #[test]
    fn implicit_pre_number_is_zero() {
        assert_eq!(v("1.0a").pre, Some((PreLetter::A, 0)));
    }

    #[test]
    fn implicit_post_dash_form() {
        // "1.0-1" is a POST release (number, no letter), not a pre-release.
        let ver = v("1.0-1");
        assert_eq!(ver.pre, None);
        assert_eq!(ver.post, Some(1));
    }

    #[test]
    fn implicit_post_number_is_zero() {
        assert_eq!(v("1.0.post").post, Some(0));
    }

    #[test]
    fn pre_letter_normalization() {
        assert_eq!(v("1.0C1").pre, Some((PreLetter::Rc, 1)));
        assert_eq!(v("1.0preview2").pre, Some((PreLetter::Rc, 2)));
        assert_eq!(v("1.0ALPHA").pre, Some((PreLetter::A, 0)));
        assert_eq!(v("1.0beta3").pre, Some((PreLetter::B, 3)));
    }

    #[test]
    fn leading_v_and_surrounding_whitespace() {
        assert_eq!(v("v1.0").release, vec![1, 0]);
        assert_eq!(v("  1.0  ").release, vec![1, 0]);
    }

    #[test]
    fn local_segments_mixed() {
        let ver = v("1.0+Foo.1.BAR-2");
        assert_eq!(
            ver.local,
            Some(vec![
                LocalSegment::String("foo".to_string()),
                LocalSegment::Number(1),
                LocalSegment::String("bar".to_string()),
                LocalSegment::Number(2),
            ])
        );
    }

    #[test]
    fn invalid_versions_are_rejected() {
        for bad in ["foo", "1..2", ".1", "1.", "", "1.0+", "1.0.0bad"] {
            assert!(
                Version::parse(bad).is_err(),
                "expected {bad:?} to be invalid"
            );
        }
    }

    #[test]
    fn equality_ignores_trailing_zeros() {
        assert_eq!(v("1.0.0"), v("1"));
        assert_eq!(v("1.0"), v("1"));
        assert_eq!(v("0.0.0"), v("0"));
        assert_eq!(v("1.0alpha1"), v("1.0a1")); // spelling normalization
        assert_ne!(v("1.0"), v("1.0+abc"));
    }

    #[test]
    fn pre_post_dev_ordering() {
        assert!(v("1.0.dev1") < v("1.0a1")); // dev-only before alpha
        assert!(v("1.0a1") < v("1.0b1"));
        assert!(v("1.0b1") < v("1.0rc1"));
        assert!(v("1.0rc1") < v("1.0")); // pre-release before final
        assert!(v("1.0.dev1") < v("1.0")); // dev-only before final
        assert!(v("1.0") < v("1.0.post1")); // final before post
        assert!(v("1.0a1") < v("1.0a1.post1")); // post after no-post
        assert!(v("1.0.post1.dev1") < v("1.0.post1")); // dev before non-dev
        assert!(v("1.0.post1") < v("1.0.post2"));
    }

    #[test]
    fn epoch_dominates() {
        assert!(v("2.0") < v("1!1.0"));
        assert!(v("1!1.0") < v("2!0.1"));
    }

    #[test]
    fn local_ordering() {
        assert!(v("1.0") < v("1.0+abc")); // no-local before local
        assert!(v("1.0+abc") < v("1.0+1")); // strings sort before numbers
        assert!(v("1.0+abc.1") < v("1.0+abc.2"));
        assert!(v("1.0+1") < v("1.0+1.0")); // shorter prefix sorts first
    }

    #[test]
    fn sorts_into_canonical_order() {
        let ascending = [
            "1.0.dev1", "1.0a1", "1.0a2", "1.0b1", "1.0rc1", "1.0", "1.0.post1", "1.1",
            "2.0", "1!1.0",
        ];
        for pair in ascending.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} should be < {}", pair[0], pair[1]);
        }
        // Sorting a scrambled copy reproduces the canonical order.
        let scrambled = [
            "1!1.0", "1.0a1", "2.0", "1.0", "1.0.dev1", "1.1", "1.0rc1", "1.0.post1",
            "1.0b1", "1.0a2",
        ];
        let mut versions: Vec<Version> = scrambled.iter().map(|s| v(s)).collect();
        versions.sort();
        let expected: Vec<Version> = ascending.iter().map(|s| v(s)).collect();
        assert_eq!(versions, expected);
    }

    #[test]
    fn equal_versions_hash_alike() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(v("1.0.0"));
        set.insert(v("1.0"));
        set.insert(v("1"));
        assert_eq!(set.len(), 1, "1.0.0, 1.0 and 1 are one version");
    }

    #[test]
    fn display_roundtrips_canonical_strings() {
        // Already-canonical inputs render back unchanged.
        for s in [
            "1.2.3",
            "1.0.0", // trailing zeros are preserved by Display
            "1.0a5",
            "1.0rc1",
            "1!2.0",
            "1.0.post1",
            "1.0.dev1",
            "1.0+ubuntu.1",
            "2!1.0a1.post2.dev3+abc.4",
        ] {
            assert_eq!(v(s).to_string(), s, "{s} should round-trip");
        }
    }

    #[test]
    fn display_normalizes() {
        let cases = [
            ("1.0ALPHA5", "1.0a5"),     // letter spelling + case
            ("1.0C", "1.0rc0"),         // c -> rc, implicit 0
            ("1.0preview", "1.0rc0"),   // preview -> rc
            ("1.0-1", "1.0.post1"),     // implicit dash post form
            ("v1.0", "1.0"),            // leading v dropped
            ("  1.0  ", "1.0"),         // whitespace trimmed
            ("1.0+Ubuntu_1-x", "1.0+ubuntu.1.x"), // local: lowercased, separators -> "."
        ];
        for (input, expected) in cases {
            assert_eq!(v(input).to_string(), expected, "{input} -> {expected}");
        }
    }

    #[test]
    fn parse_display_is_idempotent() {
        // Re-parsing a rendered version yields an equal version with the same string.
        for s in ["1.0ALPHA5", "1.0-1", "v1!2.3.4.post5.dev6+FOO_1"] {
            let once = v(s);
            let twice = v(&once.to_string());
            assert_eq!(once, twice);
            assert_eq!(once.to_string(), twice.to_string());
        }
    }

    #[test]
    fn accessors_full() {
        let ver = v("1!2.3.4a5.post6.dev7+ubuntu.1");
        assert_eq!(ver.epoch(), 1);
        assert_eq!(ver.release(), &[2u64, 3, 4]);
        assert_eq!(ver.pre(), Some((PreLetter::A, 5)));
        assert_eq!(ver.post(), Some(6));
        assert_eq!(ver.dev(), Some(7));
        assert_eq!(ver.local(), Some("ubuntu.1".to_string()));
        assert_eq!(ver.public(), "1!2.3.4a5.post6.dev7");
        assert_eq!(ver.base_version(), "1!2.3.4");
        assert!(ver.is_prerelease());
        assert!(ver.is_postrelease());
        assert!(ver.is_devrelease());
        assert_eq!((ver.major(), ver.minor(), ver.micro()), (2, 3, 4));
    }

    #[test]
    fn accessors_defaults_and_flags() {
        let ver = v("1");
        assert_eq!(ver.local(), None);
        assert_eq!(ver.public(), "1");
        assert_eq!(ver.base_version(), "1");
        assert!(!ver.is_prerelease());
        assert!(!ver.is_postrelease());
        assert!(!ver.is_devrelease());
        assert_eq!((ver.major(), ver.minor(), ver.micro()), (1, 0, 0));
        // dev-only counts as a pre-release; post does not.
        assert!(v("1.0.dev1").is_prerelease());
        assert!(!v("1.0.post1").is_prerelease());
    }

    #[test]
    fn release_trimmed_keeps_one() {
        assert_eq!(v("1.0.0").release_trimmed(), vec![1]);
        assert_eq!(v("0.0").release_trimmed(), vec![0]);
        assert_eq!(v("1.2.0").release_trimmed(), vec![1, 2]);
        assert_eq!(v("1.2.3").release_trimmed(), vec![1, 2, 3]);
    }

    #[test]
    fn from_parts_builds_and_validates() {
        let simple = VersionParts {
            release: vec![1, 2, 3],
            ..Default::default()
        };
        assert_eq!(Version::from_parts(simple).unwrap(), v("1.2.3"));

        let full = VersionParts {
            epoch: 1,
            release: vec![2, 3, 4],
            pre: Some((PreLetter::A, 5)),
            post: Some(6),
            dev: Some(7),
            local: Some("ubuntu.1".to_string()),
        };
        assert_eq!(
            Version::from_parts(full).unwrap().to_string(),
            "1!2.3.4a5.post6.dev7+ubuntu.1"
        );

        // Empty release and malformed local are rejected.
        assert!(Version::from_parts(VersionParts::default()).is_err());
        let bad_local = VersionParts {
            release: vec![1],
            local: Some("bad!local".to_string()),
            ..Default::default()
        };
        assert!(Version::from_parts(bad_local).is_err());
    }

    #[test]
    fn to_parts_roundtrips() {
        for s in ["1.2.3", "1!2.0a1.post3.dev4+abc.5", "1.0", "1.0.0"] {
            let ver = v(s);
            let rebuilt = Version::from_parts(ver.to_parts()).unwrap();
            assert_eq!(ver, rebuilt);
            assert_eq!(ver.to_string(), rebuilt.to_string());
        }
    }
}
