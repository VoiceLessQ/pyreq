//! Distribution name/version/filename helpers: port of `utils.py`.
//!
//! Port target: `../Reference/packaging/src/packaging/utils.py`.
//!
//! Covers `canonicalize_name`, `is_normalized_name`, `canonicalize_version`,
//! `parse_wheel_filename`, and `parse_sdist_filename`. These are pure string/version logic,
//! the first thing an installer or index mirror reaches for ("what package and version is
//! this file?"), and port faithfully with no host dependence.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::Version;
use crate::tags::{Tag, TagParseError, parse_tag};

/// PEP 503 name normalization: lowercase, `_`/`.` become `-`, runs of `-` collapse.
/// Port of `utils.canonicalize_name` (the non-validating path).
pub fn canonicalize_name(name: &str) -> String {
    let mut value = name.to_lowercase().replace(['_', '.'], "-");
    while value.contains("--") {
        value = value.replace("--", "-");
    }
    value
}

/// Shape half of `utils.py`'s `_normalized_regex` (`^([a-z0-9]|[a-z0-9]([a-z0-9-](?!--))*
/// [a-z0-9])$`): alnum start/end, alnum-or-dash interior. The regex's `(?!--)` lookahead
/// (no Rust equivalent) is enforced separately in `is_normalized_name`.
static NORMALIZED_SHAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u)\A[a-z0-9]([a-z0-9-]*[a-z0-9])?\z").expect("normalized shape regex")
});

/// `_wheel_name_regex` from `utils.py`: valid characters for an escaped project name in a
/// wheel filename. `\w` is Unicode-aware here, matching the source's `re.UNICODE`.
static WHEEL_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[\w._]*\z").expect("wheel name regex"));

/// Whether `name` matches `utils.is_normalized_name`. Note this is looser than "equals the
/// canonical form": the source regex's `(?!--)` permits a doubled dash, but only right after
/// the single leading character (so `a--b` is "normalized" but `aa--b` and `a---b` are not).
pub fn is_normalized_name(name: &str) -> bool {
    if !NORMALIZED_SHAPE_RE.is_match(name) {
        return false;
    }
    // `(?!--)` applies after each interior char, so a `--` survives only at index 1.
    let b = name.as_bytes();
    !(1..b.len().saturating_sub(1)).any(|i| i != 1 && b[i] == b'-' && b[i + 1] == b'-')
}

/// Return a canonical form of a version string. By default strips trailing zeros from the
/// release segment (PEP 625). Invalid versions are returned unaltered. Port of
/// `utils.canonicalize_version`.
pub fn canonicalize_version(version: &str, strip_trailing_zero: bool) -> String {
    let parsed = match Version::parse(version) {
        Ok(v) => v,
        Err(_) => return version.to_string(),
    };
    if !strip_trailing_zero {
        return parsed.to_string();
    }
    // `_TrimmedRelease` renders the version with trailing-zero-trimmed release (always
    // keeping at least one component); `release_trimmed` is exactly that trim.
    let mut parts = parsed.to_parts();
    parts.release = parsed.release_trimmed();
    Version::from_parts(parts)
        .expect("trimmed release is non-empty and local is unchanged")
        .to_string()
}

/// An invalid wheel filename (PEP 427). Mirrors `utils.InvalidWheelFilename`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidWheelFilename(pub String);

impl std::fmt::Display for InvalidWheelFilename {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for InvalidWheelFilename {}

/// An invalid sdist filename. Mirrors `utils.InvalidSdistFilename`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSdistFilename(pub String);

impl std::fmt::Display for InvalidSdistFilename {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for InvalidSdistFilename {}

/// The build tag of a wheel: `None` when absent, else `(leading digits, rest)`.
/// Mirrors `utils.BuildTag` (`()` vs `(int, str)`).
pub type BuildTag = Option<(u64, String)>;

/// What a parsed wheel filename yields: normalized name, version, build tag, and the set of
/// supported [`Tag`] triples.
pub type WheelFilename = (String, Version, BuildTag, HashSet<Tag>);

/// Parse a wheel filename `{name}-{version}(-{build})?-{python}-{abi}-{platform}.whl`.
/// Port of `utils.parse_wheel_filename`. With `validate_order`, compressed tag-set components
/// must be in sorted order (PEP 425).
pub fn parse_wheel_filename(
    filename: &str,
    validate_order: bool,
) -> Result<WheelFilename, InvalidWheelFilename> {
    let stem = filename.strip_suffix(".whl").ok_or_else(|| {
        InvalidWheelFilename(format!(
            "Invalid wheel filename (extension must be '.whl'): {filename:?}"
        ))
    })?;

    let dashes = stem.matches('-').count();
    if dashes != 4 && dashes != 5 {
        return Err(InvalidWheelFilename(format!(
            "Invalid wheel filename (wrong number of parts): {stem:?}"
        )));
    }

    // Python: stem.split("-", dashes - 2) keeps the trailing tag fields un-split. In Rust,
    // splitn takes the part *count* (maxsplit + 1).
    let parts: Vec<&str> = stem.splitn(dashes - 1, '-').collect();

    let name_part = parts[0];
    if name_part.contains("__") || !WHEEL_NAME_RE.is_match(name_part) {
        return Err(InvalidWheelFilename(format!(
            "Invalid project name: {stem:?}"
        )));
    }
    let name = canonicalize_name(name_part);

    let version = Version::parse(parts[1]).map_err(|_| {
        InvalidWheelFilename(format!(
            "Invalid wheel filename (invalid version): {stem:?}"
        ))
    })?;

    let build: BuildTag = if dashes == 5 {
        let build_part = parts[2];
        let digits: String = build_part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return Err(InvalidWheelFilename(format!(
                "Invalid build number: {build_part} in {stem:?}"
            )));
        }
        let rest = build_part[digits.len()..].to_string();
        let num = digits.parse::<u64>().map_err(|_| {
            InvalidWheelFilename(format!("Invalid build number: {build_part} in {stem:?}"))
        })?;
        Some((num, rest))
    } else {
        None
    };

    let tag_str = parts[parts.len() - 1];
    let tags = parse_tag(tag_str, validate_order).map_err(|e| match e {
        TagParseError::Unsorted(_) => InvalidWheelFilename(format!(
            "Invalid wheel filename (compressed tag set components must be in sorted order \
             per PEP 425): {stem:?}"
        )),
        TagParseError::Invalid(_) => InvalidWheelFilename(format!(
            "Invalid wheel filename (empty tag component): {stem:?}"
        )),
    })?;

    Ok((name, version, build, tags))
}

/// Parse an sdist filename (`.tar.gz` or `.zip`) into normalized name and version.
/// Port of `utils.parse_sdist_filename`.
pub fn parse_sdist_filename(filename: &str) -> Result<(String, Version), InvalidSdistFilename> {
    let stem = if let Some(s) = filename.strip_suffix(".tar.gz") {
        s
    } else if let Some(s) = filename.strip_suffix(".zip") {
        s
    } else {
        return Err(InvalidSdistFilename(format!(
            "Invalid sdist filename (extension must be '.tar.gz' or '.zip'): {filename:?}"
        )));
    };

    // A PEP 440 version cannot contain a dash, so the name/version split is the last dash.
    let (name_part, version_part) = stem
        .rsplit_once('-')
        .ok_or_else(|| InvalidSdistFilename(format!("Invalid sdist filename: {filename:?}")))?;

    let name = canonicalize_name(name_part);
    let version = Version::parse(version_part).map_err(|_| {
        InvalidSdistFilename(format!(
            "Invalid sdist filename (invalid version): {filename:?}"
        ))
    })?;

    Ok((name, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_name_normalizes() {
        assert_eq!(canonicalize_name("Django"), "django");
        assert_eq!(canonicalize_name("oslo.concurrency"), "oslo-concurrency");
        assert_eq!(canonicalize_name("foo__bar"), "foo-bar");
        assert_eq!(canonicalize_name("a---b.._c"), "a-b-c");
    }

    #[test]
    fn is_normalized_name_checks() {
        assert!(is_normalized_name("requests"));
        assert!(is_normalized_name("oslo-concurrency"));
        assert!(!is_normalized_name("Django"));
        assert!(!is_normalized_name("foo_bar"));
        assert!(!is_normalized_name("foo.bar"));
        assert!(!is_normalized_name("-ab"));
        assert!(!is_normalized_name("ab-"));
        // packaging quirk: a doubled dash is accepted only right after the leading char.
        assert!(is_normalized_name("a--b"));
        assert!(!is_normalized_name("aa--b"));
        assert!(!is_normalized_name("a---b"));
        assert!(!is_normalized_name("a--b--c"));
    }

    #[test]
    fn canonicalize_version_strips_trailing_zeros() {
        assert_eq!(canonicalize_version("1.0.1", true), "1.0.1");
        assert_eq!(canonicalize_version("1.0.0", true), "1");
        assert_eq!(canonicalize_version("1.0.0", false), "1.0.0");
        assert_eq!(canonicalize_version("1.4.0.0.0", true), "1.4");
        // Invalid versions pass through unchanged.
        assert_eq!(canonicalize_version("foo bar baz", true), "foo bar baz");
        // Epoch and suffixes are preserved around the trim.
        assert_eq!(canonicalize_version("1!2.0.0a1", true), "1!2a1");
    }

    #[test]
    fn wheel_filename_basic() {
        let (name, ver, build, tags) =
            parse_wheel_filename("foo-1.0-py3-none-any.whl", false).unwrap();
        assert_eq!(name, "foo");
        assert_eq!(ver, Version::parse("1.0").unwrap());
        assert_eq!(build, None);
        assert_eq!(tags.len(), 1);
        assert!(tags.contains(&Tag::new("py3", "none", "any")));
    }

    #[test]
    fn wheel_filename_with_build_tag() {
        let (name, ver, build, _tags) =
            parse_wheel_filename("foo-1.0-1bar-py3-none-any.whl", false).unwrap();
        assert_eq!(name, "foo");
        assert_eq!(ver, Version::parse("1.0").unwrap());
        assert_eq!(build, Some((1, "bar".to_string())));
    }

    #[test]
    fn wheel_filename_normalizes_name_and_expands_tags() {
        let (name, _ver, _build, tags) =
            parse_wheel_filename("Foo.Bar-2.0-py2.py3-none-any.whl", false).unwrap();
        assert_eq!(name, "foo-bar");
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn wheel_filename_rejects_bad_input() {
        assert!(parse_wheel_filename("foo-1.0-py3-none-any.tar.gz", false).is_err());
        assert!(parse_wheel_filename("foo-1.0-py3-none.whl", false).is_err()); // too few parts
        assert!(parse_wheel_filename("foo-bad!ver-py3-none-any.whl", false).is_err());
        assert!(parse_wheel_filename("foo bar-1.0-py3-none-any.whl", false).is_err()); // bad name
        assert!(parse_wheel_filename("foo-1.0-bar-py3-none-any.whl", false).is_err()); // build no digit
    }

    #[test]
    fn wheel_filename_validate_order() {
        assert!(parse_wheel_filename("foo-1.0-py3.py2-none-any.whl", false).is_ok());
        assert!(parse_wheel_filename("foo-1.0-py3.py2-none-any.whl", true).is_err());
    }

    #[test]
    fn sdist_filename_basic() {
        let (name, ver) = parse_sdist_filename("foo-1.0.tar.gz").unwrap();
        assert_eq!(name, "foo");
        assert_eq!(ver, Version::parse("1.0").unwrap());

        let (name, ver) = parse_sdist_filename("Foo.Bar-2.0.1.zip").unwrap();
        assert_eq!(name, "foo-bar");
        assert_eq!(ver, Version::parse("2.0.1").unwrap());
    }

    #[test]
    fn sdist_filename_rejects_bad_input() {
        assert!(parse_sdist_filename("foo-1.0.whl").is_err()); // wrong extension
        assert!(parse_sdist_filename("foo.tar.gz").is_err()); // no dash
        assert!(parse_sdist_filename("foo-bad!ver.tar.gz").is_err()); // bad version
    }
}
