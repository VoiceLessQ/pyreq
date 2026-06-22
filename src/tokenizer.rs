//! PEP 508 tokenizer — port of `_tokenizer.py`. Shared by markers and requirements.
//!
//! Each rule is a regex anchored with `\A` and matched against `source[position..]`, which
//! reproduces Python's `pattern.match(source, position)` (anchored at the current offset).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// A matched token: rule name, matched text, and byte start position.
// `name`/`position` are part of the token model; `position` is used by the requirements
// parser (error spans). Allow them to sit unread until that layer lands.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Token {
    pub name: &'static str,
    pub text: String,
    pub position: usize,
}

/// A parse error carrying enough information to render `_tokenizer.py`'s caret message.
#[derive(Debug, Clone)]
pub struct ParserSyntaxError {
    pub message: String,
    pub source: String,
    pub span: (usize, usize),
}

impl std::fmt::Display for ParserSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (start, end) = self.span;
        let marker = " ".repeat(start) + &"~".repeat(end.saturating_sub(start)) + "^";
        write!(f, "{}\n    {}\n    {}", self.message, self.source, marker)
    }
}

impl std::error::Error for ParserSyntaxError {}

// The specifier operator grammar (the 4 operator families), reused from the version
// specifier regex but anchored at the start for token matching.
const SPEC_CORE: &str = concat!(
    r"(?:",
    r"(?:===\s*[^\s;)]*)",
    r"|",
    r"(?:(?:==|!=)\s*v?(?:[0-9]+!)?[0-9]+(?:\.[0-9]+)*(?:\.\*|(?:",
    r"(?:[-_.]?(?:alpha|beta|preview|pre|a|b|c|rc)[-_.]?[0-9]*)?",
    r"(?:(?:-[0-9]+)|(?:[-_.]?(?:post|rev|r)[-_.]?[0-9]*))?",
    r"(?:[-_.]?dev[-_.]?[0-9]*)?",
    r"(?:\+[a-z0-9]+(?:[-_.][a-z0-9]+)*)?",
    r"))?)",
    r"|",
    r"(?:~=\s*v?(?:[0-9]+!)?[0-9]+(?:\.[0-9]+)+",
    r"(?:[-_.]?(?:alpha|beta|preview|pre|a|b|c|rc)[-_.]?[0-9]*)?",
    r"(?:(?:-[0-9]+)|(?:[-_.]?(?:post|rev|r)[-_.]?[0-9]*))?",
    r"(?:[-_.]?dev[-_.]?[0-9]*)?)",
    r"|",
    r"(?:(?:<=|>=|<|>)\s*v?(?:[0-9]+!)?[0-9]+(?:\.[0-9]+)*",
    r"(?:[-_.]?(?:alpha|beta|preview|pre|a|b|c|rc)[-_.]?[0-9]*)?",
    r"(?:(?:-[0-9]+)|(?:[-_.]?(?:post|rev|r)[-_.]?[0-9]*))?",
    r"(?:[-_.]?dev[-_.]?[0-9]*)?)",
    r")",
);

static RULES: LazyLock<HashMap<&'static str, Regex>> = LazyLock::new(|| {
    let mut m: HashMap<&'static str, Regex> = HashMap::new();
    let mut add = |name: &'static str, pat: String| {
        m.insert(name, Regex::new(&pat).expect("tokenizer rule compiles"));
    };
    add("LEFT_PARENTHESIS", r"\A\(".into());
    add("RIGHT_PARENTHESIS", r"\A\)".into());
    add("LEFT_BRACKET", r"\A\[".into());
    add("RIGHT_BRACKET", r"\A\]".into());
    add("SEMICOLON", r"\A;".into());
    add("COMMA", r"\A,".into());
    add("QUOTED_STRING", "\\A(?:'[^']*'|\"[^\"]*\")".into());
    add("OP", r"\A(?:===|==|~=|!=|<=|>=|<|>)".into());
    add("BOOLOP", r"\A\b(?:or|and)\b".into());
    add("IN", r"\A\bin\b".into());
    add("NOT", r"\A\bnot\b".into());
    add(
        "VARIABLE",
        concat!(
            r"\A\b(?:python_version|python_full_version|os[._]name|sys[._]platform",
            r"|platform_(?:release|system)|platform[._](?:version|machine|python_implementation)",
            r"|python_implementation|implementation_(?:name|version)|extras?|dependency_groups)\b",
        )
        .into(),
    );
    add("SPECIFIER", format!(r"(?i)\A{SPEC_CORE}"));
    add("AT", r"\A@".into());
    add("URL", r"\A[^ \t]+".into());
    add("IDENTIFIER", r"\A\b[a-zA-Z0-9][a-zA-Z0-9._-]*\b".into());
    add("VERSION_PREFIX_TRAIL", r"\A\.\*".into());
    add("VERSION_LOCAL_LABEL_TRAIL", r"\A\+[a-z0-9]+(?:[-_.][a-z0-9]+)*".into());
    add("WS", r"\A[ \t]+".into());
    // END is matched specially (end of source); see `Tokenizer::match_len`.
    m
});

/// Context-sensitive tokenizer. The parser drives it via `check`/`read`/`expect`.
pub struct Tokenizer {
    pub source: String,
    pub position: usize,
    next_token: Option<Token>,
}

impl Tokenizer {
    pub fn new(source: &str) -> Tokenizer {
        Tokenizer {
            source: source.to_string(),
            position: 0,
            next_token: None,
        }
    }

    /// Length of the match for `name` at the current position, if any.
    fn match_len(&self, name: &str) -> Option<usize> {
        if name == "END" {
            return (self.position >= self.source.len()).then_some(0);
        }
        let re = RULES.get(name).expect("known token rule");
        re.find(&self.source[self.position..]).map(|m| m.len())
    }

    /// Whether the next token is `name`. Loads it for a following `read` unless this is a
    /// `peek` (which leaves the stream untouched).
    pub fn check(&mut self, name: &'static str) -> bool {
        match self.match_len(name) {
            Some(len) => {
                let text = self.source[self.position..self.position + len].to_string();
                self.next_token = Some(Token {
                    name,
                    text,
                    position: self.position,
                });
                true
            }
            None => false,
        }
    }

    /// Whether the next token is `name`, without loading it.
    pub fn peek(&self, name: &str) -> bool {
        self.match_len(name).is_some()
    }

    /// Consume and return the token loaded by the most recent successful `check`.
    pub fn read(&mut self) -> Token {
        let token = self.next_token.take().expect("read without a checked token");
        self.position += token.text.len();
        token
    }

    /// Read the next token if it is `name`.
    pub fn consume(&mut self, name: &'static str) {
        if self.check(name) {
            self.read();
        }
    }

    /// Require `name` next, or fail with a syntax error.
    pub fn expect(&mut self, name: &'static str, expected: &str) -> Result<Token, ParserSyntaxError> {
        if !self.check(name) {
            return Err(self.syntax_error(&format!("Expected {expected}"), None, None));
        }
        Ok(self.read())
    }

    pub fn syntax_error(
        &self,
        message: &str,
        span_start: Option<usize>,
        span_end: Option<usize>,
    ) -> ParserSyntaxError {
        ParserSyntaxError {
            message: message.to_string(),
            source: self.source.clone(),
            span: (
                span_start.unwrap_or(self.position),
                span_end.unwrap_or(self.position),
            ),
        }
    }
}

/// Run `body`, optionally wrapped in matching `open`/`close` tokens. Port of
/// `Tokenizer.enclosing_tokens`. Used by both the marker and requirement parsers.
pub(crate) fn enclosing<T>(
    tok: &mut Tokenizer,
    open: &'static str,
    close: &'static str,
    around: &str,
    body: impl FnOnce(&mut Tokenizer) -> Result<T, ParserSyntaxError>,
) -> Result<T, ParserSyntaxError> {
    let open_pos = if tok.check(open) {
        let pos = tok.position;
        tok.read();
        Some(pos)
    } else {
        None
    };

    let result = body(tok)?;

    if let Some(open_pos) = open_pos {
        if !tok.check(close) {
            return Err(tok.syntax_error(
                &format!("Expected matching {close} for {open}, after {around}"),
                Some(open_pos),
                None,
            ));
        }
        tok.read();
    }
    Ok(result)
}
