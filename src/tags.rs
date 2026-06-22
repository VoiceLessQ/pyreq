//! Wheel platform tags: the parsing slice of `tags.py`.
//!
//! Port target: `../Reference/packaging/src/packaging/tags.py` (`Tag`, `parse_tag`).
//!
//! Only the *parsing* half is ported: turning a compressed tag string such as
//! `cp39.cp310-abi3-manylinux1_x86_64` into a set of [`Tag`] triples. Tag *generation*
//! (`sys_tags`, `cpython_tags`, manylinux/musllinux/mac platform enumeration) introspects
//! the running interpreter via `sysconfig`/`platform` and cannot exist in a standalone Rust
//! crate, so it is deliberately out of scope.

use std::collections::HashSet;
use std::fmt;

/// A wheel tag triple (interpreter, ABI, platform), e.g. `cp39-abi3-manylinux1_x86_64`.
///
/// Port of `tags.Tag`. The three components are lowercased on construction, matching
/// `Tag.__init__`, so equality and hashing are case-insensitive.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tag {
    interpreter: String,
    abi: String,
    platform: String,
}

impl Tag {
    /// Build a tag, lowercasing each component (as `Tag.__init__` does).
    pub fn new(interpreter: &str, abi: &str, platform: &str) -> Tag {
        Tag {
            interpreter: interpreter.to_lowercase(),
            abi: abi.to_lowercase(),
            platform: platform.to_lowercase(),
        }
    }

    /// The interpreter component, e.g. `cp39`.
    pub fn interpreter(&self) -> &str {
        &self.interpreter
    }

    /// The ABI component, e.g. `abi3`.
    pub fn abi(&self) -> &str {
        &self.abi
    }

    /// The platform component, e.g. `manylinux1_x86_64`.
    pub fn platform(&self) -> &str {
        &self.platform
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}-{}", self.interpreter, self.abi, self.platform)
    }
}

/// Raised when a tag has an empty interpreter, ABI, or platform component, or the wrong
/// number of dash-separated components. Mirrors `tags.InvalidTag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTag(pub String);

impl fmt::Display for InvalidTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for InvalidTag {}

/// Raised when a compressed tag-set component is not in sorted order (PEP 425), and the
/// caller asked for `validate_order`. Mirrors `tags.UnsortedTagsError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsortedTagsError(pub String);

impl fmt::Display for UnsortedTagsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for UnsortedTagsError {}

/// Either failure parse_tag can raise. Lets the wheel parser collapse both into one error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagParseError {
    Invalid(InvalidTag),
    Unsorted(UnsortedTagsError),
}

/// Parse a compressed tag string into the set of [`Tag`] triples it expands to.
///
/// Port of `tags.parse_tag`. A component may itself be dot-joined (`py2.py3`), in which case
/// every combination is produced (the Cartesian product). With `validate_order`, each
/// dot-joined component must already be in sorted order (PEP 425).
pub fn parse_tag(tag: &str, validate_order: bool) -> Result<HashSet<Tag>, TagParseError> {
    let components: Vec<Vec<&str>> = tag.split('-').map(|c| c.split('.').collect()).collect();

    for parts in &components {
        if parts.iter().any(|p| p.is_empty()) {
            let component = parts.join(".");
            return Err(TagParseError::Invalid(InvalidTag(format!(
                "Tag {tag:?} has an empty component: {component:?}"
            ))));
        }
        if validate_order {
            let mut sorted = parts.clone();
            sorted.sort_unstable();
            if *parts != sorted {
                let component = parts.join(".");
                return Err(TagParseError::Unsorted(UnsortedTagsError(format!(
                    "Tag component {component:?} is not in sorted order per PEP 425"
                ))));
            }
        }
    }

    // Python unpacks `interpreters, abis, platforms = component_parts`, which requires exactly
    // three components; anything else is a (ValueError) failure.
    if components.len() != 3 {
        return Err(TagParseError::Invalid(InvalidTag(format!(
            "Tag {tag:?} must have exactly three dash-separated components"
        ))));
    }

    let mut tags = HashSet::new();
    for interp in &components[0] {
        for abi in &components[1] {
            for plat in &components[2] {
                tags.insert(Tag::new(interp, abi, plat));
            }
        }
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_tag() {
        let tags = parse_tag("py3-none-any", false).unwrap();
        assert_eq!(tags.len(), 1);
        assert!(tags.contains(&Tag::new("py3", "none", "any")));
    }

    #[test]
    fn compressed_set_expands() {
        let tags = parse_tag("py2.py3-none-any", false).unwrap();
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&Tag::new("py2", "none", "any")));
        assert!(tags.contains(&Tag::new("py3", "none", "any")));
    }

    #[test]
    fn cartesian_product() {
        let tags = parse_tag("cp39.cp310-abi3.none-linux_x86_64", false).unwrap();
        assert_eq!(tags.len(), 4);
    }

    #[test]
    fn lowercases_components() {
        let tags = parse_tag("CP39-ABI3-Linux_X86_64", false).unwrap();
        assert!(tags.contains(&Tag::new("cp39", "abi3", "linux_x86_64")));
        assert_eq!(Tag::new("CP39", "abi3", "any").to_string(), "cp39-abi3-any");
    }

    #[test]
    fn empty_component_is_invalid() {
        assert!(matches!(
            parse_tag("py3--any", false),
            Err(TagParseError::Invalid(_))
        ));
        assert!(matches!(
            parse_tag("py3.-none-any", false),
            Err(TagParseError::Invalid(_))
        ));
    }

    #[test]
    fn wrong_component_count_is_invalid() {
        assert!(matches!(
            parse_tag("py3-none", false),
            Err(TagParseError::Invalid(_))
        ));
        assert!(matches!(
            parse_tag("py3-none-any-extra", false),
            Err(TagParseError::Invalid(_))
        ));
    }

    #[test]
    fn validate_order_rejects_unsorted() {
        assert!(parse_tag("py2.py3-none-any", true).is_ok());
        assert!(parse_tag("py3.py2-none-any", false).is_ok());
        assert!(matches!(
            parse_tag("py3.py2-none-any", true),
            Err(TagParseError::Unsorted(_))
        ));
    }
}
