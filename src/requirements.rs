//! PEP 508 requirements — port of `requirements.py` plus the requirement half of `_parser.py`.

use std::collections::BTreeSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::SpecifierSet;
use crate::markers::{Marker, MarkerExpr, parse_marker};
use crate::tokenizer::{ParserSyntaxError, Tokenizer, enclosing};
use crate::utils::canonicalize_name;

/// Raised when a requirement string cannot be parsed (PEP 508). Holds the parser error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRequirement(pub String);

impl fmt::Display for InvalidRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for InvalidRequirement {}

/// A parsed PEP 508 dependency specifier, e.g. `requests[security]>=2.0; python_version<"3.9"`.
/// Port of `requirements.Requirement`.
#[derive(Debug, Clone)]
pub struct Requirement {
    /// The project name, as written.
    pub name: String,
    /// A direct-reference URL (`name @ url`), if any.
    pub url: Option<String>,
    /// The requested extras, e.g. `{security, socks}`. Stored sorted.
    pub extras: BTreeSet<String>,
    /// The version constraints.
    pub specifier: SpecifierSet,
    /// The environment marker after `;`, if any.
    pub marker: Option<Marker>,
}

impl Requirement {
    /// Parse a requirement string. Equivalent to `Requirement(requirement_string)`.
    pub fn parse(requirement: &str) -> Result<Requirement, InvalidRequirement> {
        let mut tok = Tokenizer::new(requirement);
        let parsed =
            parse_requirement(&mut tok).map_err(|e| InvalidRequirement(e.to_string()))?;

        let specifier =
            SpecifierSet::parse(&parsed.specifier).map_err(|e| InvalidRequirement(e.to_string()))?;

        Ok(Requirement {
            name: parsed.name,
            url: if parsed.url.is_empty() {
                None
            } else {
                Some(parsed.url)
            },
            extras: parsed.extras.into_iter().collect(),
            specifier,
            marker: parsed.marker.map(Marker::from_tree),
        })
    }
}

impl fmt::Display for Requirement {
    /// Round-trip form, matching `Requirement.__str__` (extras sorted, specifier canonical).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;

        if !self.extras.is_empty() {
            let extras: Vec<&str> = self.extras.iter().map(String::as_str).collect();
            write!(f, "[{}]", extras.join(","))?;
        }

        if !self.specifier.is_empty() {
            write!(f, "{}", self.specifier)?;
        }

        if let Some(url) = &self.url {
            write!(f, "@ {url}")?;
            if self.marker.is_some() {
                write!(f, " ")?;
            }
        }

        if let Some(marker) = &self.marker {
            write!(f, "; {marker}")?;
        }
        Ok(())
    }
}

impl FromStr for Requirement {
    type Err = InvalidRequirement;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Requirement::parse(s)
    }
}

// Equality and hashing compare canonicalized names and ignore extras ordering.
impl PartialEq for Requirement {
    fn eq(&self, other: &Self) -> bool {
        canonicalize_name(&self.name) == canonicalize_name(&other.name)
            && self.extras == other.extras
            && self.specifier == other.specifier
            && self.url == other.url
            && self.marker == other.marker
    }
}

impl Eq for Requirement {}

impl Hash for Requirement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        canonicalize_name(&self.name).hash(state);
        self.extras.hash(state);
        self.specifier.hash(state);
        self.url.hash(state);
        self.marker.hash(state);
    }
}

// --- Recursive-descent requirement parser (the requirement half of `_parser.py`). ---

struct ParsedRequirement {
    name: String,
    url: String,
    extras: Vec<String>,
    specifier: String,
    marker: Option<Vec<MarkerExpr>>,
}

/// `requirement = WS? IDENTIFIER WS? extras WS? requirement_details END`
fn parse_requirement(tok: &mut Tokenizer) -> Result<ParsedRequirement, ParserSyntaxError> {
    tok.consume("WS");
    let name = tok
        .expect("IDENTIFIER", "package name at the start of dependency specifier")?
        .text;
    tok.consume("WS");

    let extras = parse_extras(tok)?;
    tok.consume("WS");

    let (url, specifier, marker) = parse_requirement_details(tok)?;
    tok.expect("END", "end of dependency specifier")?;

    Ok(ParsedRequirement {
        name,
        url,
        extras,
        specifier,
        marker,
    })
}

/// `requirement_details = AT URL (WS requirement_marker?)? | specifier WS? requirement_marker?`
fn parse_requirement_details(
    tok: &mut Tokenizer,
) -> Result<(String, String, Option<Vec<MarkerExpr>>), ParserSyntaxError> {
    let mut specifier = String::new();
    let mut url = String::new();
    let mut marker = None;

    if tok.check("AT") {
        tok.read();
        tok.consume("WS");

        let url_start = tok.position;
        url = tok.expect("URL", "URL after @")?.text;
        if tok.peek("END") {
            return Ok((url, specifier, marker));
        }

        tok.expect("WS", "whitespace after URL")?;
        if tok.peek("END") {
            return Ok((url, specifier, marker));
        }

        marker = Some(parse_requirement_marker(
            tok,
            url_start,
            "semicolon (after URL and whitespace)",
        )?);
    } else {
        let specifier_start = tok.position;
        specifier = parse_specifier(tok)?;
        tok.consume("WS");

        if tok.peek("END") {
            return Ok((url, specifier, marker));
        }

        let expected = if specifier.is_empty() {
            "semicolon (after name with no version specifier)"
        } else {
            "comma (within version specifier), semicolon (after version specifier)"
        };
        marker = Some(parse_requirement_marker(tok, specifier_start, expected)?);
    }

    Ok((url, specifier, marker))
}

/// `requirement_marker = SEMICOLON marker WS?`
fn parse_requirement_marker(
    tok: &mut Tokenizer,
    span_start: usize,
    expected: &str,
) -> Result<Vec<MarkerExpr>, ParserSyntaxError> {
    if !tok.check("SEMICOLON") {
        return Err(tok.syntax_error(&format!("Expected {expected} or end"), Some(span_start), None));
    }
    tok.read();

    let marker = parse_marker(tok)?;
    tok.consume("WS");
    Ok(marker)
}

/// `extras = (LEFT_BRACKET WS? extras_list? WS? RIGHT_BRACKET)?`
fn parse_extras(tok: &mut Tokenizer) -> Result<Vec<String>, ParserSyntaxError> {
    if !tok.peek("LEFT_BRACKET") {
        return Ok(Vec::new());
    }
    enclosing(tok, "LEFT_BRACKET", "RIGHT_BRACKET", "extras", |tok| {
        tok.consume("WS");
        let extras = parse_extras_list(tok)?;
        tok.consume("WS");
        Ok(extras)
    })
}

/// `extras_list = IDENTIFIER (WS? COMMA WS? IDENTIFIER)*`
fn parse_extras_list(tok: &mut Tokenizer) -> Result<Vec<String>, ParserSyntaxError> {
    let mut extras = Vec::new();
    if !tok.check("IDENTIFIER") {
        return Ok(extras);
    }
    extras.push(tok.read().text);

    loop {
        tok.consume("WS");
        if tok.peek("IDENTIFIER") {
            return Err(tok.syntax_error("Expected comma between extra names", None, None));
        } else if !tok.check("COMMA") {
            break;
        }
        tok.read();
        tok.consume("WS");
        extras.push(tok.expect("IDENTIFIER", "extra name after comma")?.text);
    }
    Ok(extras)
}

/// `specifier = (LEFT_PARENTHESIS WS? version_many WS? RIGHT_PARENTHESIS) | (WS? version_many WS?)`
fn parse_specifier(tok: &mut Tokenizer) -> Result<String, ParserSyntaxError> {
    enclosing(
        tok,
        "LEFT_PARENTHESIS",
        "RIGHT_PARENTHESIS",
        "version specifier",
        |tok| {
            tok.consume("WS");
            let parsed = parse_version_many(tok)?;
            tok.consume("WS");
            Ok(parsed)
        },
    )
}

/// `version_many = (SPECIFIER (WS? COMMA WS? SPECIFIER)*)?`
fn parse_version_many(tok: &mut Tokenizer) -> Result<String, ParserSyntaxError> {
    let mut parsed = String::new();
    while tok.check("SPECIFIER") {
        let span_start = tok.position;
        parsed.push_str(&tok.read().text);

        if tok.peek("VERSION_PREFIX_TRAIL") {
            return Err(tok.syntax_error(
                ".* suffix can only be used with `==` or `!=` operators",
                Some(span_start),
                Some(tok.position + 1),
            ));
        }
        if tok.peek("VERSION_LOCAL_LABEL_TRAIL") {
            return Err(tok.syntax_error(
                "Local version label can only be used with `==` or `!=` operators",
                Some(span_start),
                Some(tok.position),
            ));
        }

        tok.consume("WS");
        if !tok.check("COMMA") {
            break;
        }
        parsed.push_str(&tok.read().text);
        tok.consume("WS");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> Requirement {
        Requirement::parse(s).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parses_fields() {
        let req = r("requests[security,socks]>=2.0,<3.0; python_version < \"3.9\"");
        assert_eq!(req.name, "requests");
        assert_eq!(
            req.extras.iter().cloned().collect::<Vec<_>>(),
            vec!["security", "socks"]
        );
        assert_eq!(req.specifier.to_string(), "<3.0,>=2.0");
        assert_eq!(req.url, None);
        assert_eq!(req.marker.unwrap().to_string(), "python_version < \"3.9\"");
    }

    #[test]
    fn display_roundtrips_and_normalizes() {
        assert_eq!(r("requests").to_string(), "requests");
        assert_eq!(r("requests>=2.0").to_string(), "requests>=2.0");
        // extras get sorted; the specifier's version is shown verbatim (not canonicalized).
        assert_eq!(r("foo[B,A]==1.0.0").to_string(), "foo[A,B]==1.0.0");
        assert_eq!(
            r("requests[security]>=2.0; python_version<\"3.9\"").to_string(),
            "requests[security]>=2.0; python_version < \"3.9\""
        );
    }

    #[test]
    fn parenthesized_specifier() {
        assert_eq!(r("foo (>=1.0)").to_string(), "foo>=1.0");
    }

    #[test]
    fn url_form() {
        let req = r("name @ https://example.com/pkg.tar.gz");
        assert_eq!(req.url.as_deref(), Some("https://example.com/pkg.tar.gz"));
        // packaging renders the URL form with no space before `@`.
        assert_eq!(req.to_string(), "name@ https://example.com/pkg.tar.gz");
        let with_marker = r("name @ https://example.com/p.tgz ; python_version > \"3.0\"");
        assert_eq!(
            with_marker.to_string(),
            "name@ https://example.com/p.tgz ; python_version > \"3.0\""
        );
    }

    #[test]
    fn equality_canonicalizes_name_and_specifier() {
        assert_eq!(r("Foo>=1.0"), r("foo>=1.0.0"));
        assert_eq!(r("foo[A,B]"), r("foo[B,A]"));
        assert_ne!(r("foo>=1.0"), r("foo>=2.0"));
    }

    #[test]
    fn rejects_invalid() {
        for bad in [
            "",
            "==1.0",
            "foo bar baz",
            "foo>=",
            "foo @",
            "(foo)",
            "foo[]extra",
            "foo; python_version",
        ] {
            assert!(Requirement::parse(bad).is_err(), "expected {bad:?} invalid");
        }
    }
}
