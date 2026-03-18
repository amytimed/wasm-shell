//! Bash lexer.
//!
//! Produces a flat stream of `Token`s from a source string.
//! The parser drives the lexer and requests re-lexing of the same position
//! with different context (e.g. inside `$(…)` vs top-level).

use crate::error::{ParseError, Span};

// ── Token types ──────────────────────────────────────────────────────────────

/// A redirect operator token. Carries the file-descriptor prefix (e.g. `2>` → fd=Some(2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectToken {
    /// The fd prefix, if any (e.g. `2>` → Some(2)).
    pub fd: Option<u32>,
    pub op: RedirectOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectOp {
    /// `<`
    Less,
    /// `>`
    Greater,
    /// `>>`
    GreaterGreater,
    /// `<>`
    LessGreater,
    /// `>|`
    GreaterPipe,
    /// `>&`
    GreaterAmp,
    /// `<&`
    LessAmp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Literals / words
    Word(String),
    /// `'...'`
    SingleQuote(String),
    /// Opening `"` — contents are yielded as sub-tokens by the parser.
    DoubleQuoteOpen,
    DoubleQuoteClose,
    /// Raw text fragment inside `"..."` (no special meaning).
    DQText(String),

    // Operators
    /// `|`
    Pipe,
    /// `||`
    OrOr,
    /// `&&`
    AndAnd,
    /// `&`
    Amp,
    /// `;`
    Semi,
    /// `;;` (only meaningful in case, but we lex it anyway)
    SemiSemi,
    /// `(` — subshell open or arithmetic/command-subst
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `!` at start of pipeline
    Bang,
    /// Newline
    Newline,

    /// Any redirect operator, with optional fd prefix.
    Redirect(RedirectToken),

    /// `<<` heredoc operator.
    HereDoc { fd: Option<u32>, delimiter: String, quoted: bool, strip_tabs: bool },

    // EOF
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

// ── Lexer ────────────────────────────────────────────────────────────────────

pub struct Lexer<'src> {
    src: &'src str,
    pos: usize,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Self { src, pos: 0 }
    }

    pub fn source(&self) -> &'src str {
        self.src
    }

    // ── Primitives ───────────────────────────────────────────────────────────

    fn remaining(&self) -> &str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn skip_whitespace_inline(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.advance();
        }
    }

    fn skip_comment(&mut self) {
        while !matches!(self.peek(), None | Some('\n')) {
            self.advance();
        }
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(start, self.pos - start)
    }

    // ── Public interface ─────────────────────────────────────────────────────

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// Lex the next token at the current position.
    ///
    /// `in_dq` — true if we're inside a `"..."` string.
    pub fn next_token(&mut self, in_dq: bool) -> Result<Token, ParseError> {
        if in_dq {
            return self.lex_dq_fragment();
        }

        self.skip_whitespace_inline();

        let start = self.pos;
        match self.peek() {
            None => Ok(Token::new(TokenKind::Eof, Span::eof(self.pos))),

            Some('#') => {
                self.skip_comment();
                self.next_token(false)
            }

            Some('\n') => {
                self.advance();
                Ok(Token::new(TokenKind::Newline, self.span_from(start)))
            }

            Some(';') => {
                self.advance();
                if self.eat(';') {
                    Ok(Token::new(TokenKind::SemiSemi, self.span_from(start)))
                } else {
                    Ok(Token::new(TokenKind::Semi, self.span_from(start)))
                }
            }

            Some('&') => {
                self.advance();
                if self.eat('&') {
                    Ok(Token::new(TokenKind::AndAnd, self.span_from(start)))
                } else {
                    Ok(Token::new(TokenKind::Amp, self.span_from(start)))
                }
            }

            Some('|') => {
                self.advance();
                if self.eat('|') {
                    Ok(Token::new(TokenKind::OrOr, self.span_from(start)))
                } else {
                    Ok(Token::new(TokenKind::Pipe, self.span_from(start)))
                }
            }

            Some('(') => {
                self.advance();
                Ok(Token::new(TokenKind::LParen, self.span_from(start)))
            }
            Some(')') => {
                self.advance();
                Ok(Token::new(TokenKind::RParen, self.span_from(start)))
            }
            Some('{') => {
                self.advance();
                Ok(Token::new(TokenKind::LBrace, self.span_from(start)))
            }
            Some('}') => {
                self.advance();
                Ok(Token::new(TokenKind::RBrace, self.span_from(start)))
            }

            Some('!') => {
                self.advance();
                Ok(Token::new(TokenKind::Bang, self.span_from(start)))
            }

            Some('"') => {
                self.advance();
                Ok(Token::new(TokenKind::DoubleQuoteOpen, self.span_from(start)))
            }

            Some('\'') => {
                self.advance();
                let text_start = self.pos;
                loop {
                    match self.peek() {
                        None => {
                            return Err(ParseError::new(
                                self.span_from(start),
                                "unterminated single-quoted string",
                            ))
                        }
                        Some('\'') => {
                            let text = self.src[text_start..self.pos].to_owned();
                            self.advance();
                            return Ok(Token::new(
                                TokenKind::SingleQuote(text),
                                self.span_from(start),
                            ));
                        }
                        _ => {
                            self.advance();
                        }
                    }
                }
            }

            Some('<') | Some('>') => self.lex_redirect(start, None),

            Some(c) if c.is_ascii_digit() => {
                // Peek ahead: if digits are immediately followed by `<` or `>`, this is
                // a fd-prefixed redirect (e.g. `2>`, `2>>`, `2>&1`).
                if self.is_fd_redirect() {
                    self.lex_redirect(start, None)
                } else {
                    self.lex_word(start)
                }
            }

            _ => self.lex_word(start),
        }
    }

    /// Returns true if the current position looks like `N<` or `N>`.
    fn is_fd_redirect(&self) -> bool {
        let rem = self.remaining();
        let digits_end = rem
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rem.len());
        if digits_end == 0 {
            return false;
        }
        matches!(rem.as_bytes().get(digits_end), Some(&b'<') | Some(&b'>'))
    }

    fn lex_redirect(&mut self, start: usize, pre_fd: Option<u32>) -> Result<Token, ParseError> {
        // Optionally consume a numeric fd prefix.
        let fd_start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.advance();
        }
        let fd_str = &self.src[fd_start..self.pos];

        let fd: Option<u32> = if !fd_str.is_empty() {
            fd_str.parse().ok()
        } else {
            pre_fd
        };

        match self.peek() {
            Some('<') => {
                self.advance();
                if self.eat('<') {
                    let strip_tabs = self.eat('-');
                    self.skip_whitespace_inline();
                    let (delim, quoted) = self.lex_heredoc_delimiter()?;
                    Ok(Token::new(
                        TokenKind::HereDoc { fd, delimiter: delim, quoted, strip_tabs },
                        self.span_from(start),
                    ))
                } else if self.eat('>') {
                    Ok(Token::new(
                        TokenKind::Redirect(RedirectToken { fd, op: RedirectOp::LessGreater }),
                        self.span_from(start),
                    ))
                } else if self.eat('&') {
                    Ok(Token::new(
                        TokenKind::Redirect(RedirectToken { fd, op: RedirectOp::LessAmp }),
                        self.span_from(start),
                    ))
                } else {
                    Ok(Token::new(
                        TokenKind::Redirect(RedirectToken { fd, op: RedirectOp::Less }),
                        self.span_from(start),
                    ))
                }
            }
            Some('>') => {
                self.advance();
                if self.eat('>') {
                    Ok(Token::new(
                        TokenKind::Redirect(RedirectToken { fd, op: RedirectOp::GreaterGreater }),
                        self.span_from(start),
                    ))
                } else if self.eat('|') {
                    Ok(Token::new(
                        TokenKind::Redirect(RedirectToken { fd, op: RedirectOp::GreaterPipe }),
                        self.span_from(start),
                    ))
                } else if self.eat('&') {
                    Ok(Token::new(
                        TokenKind::Redirect(RedirectToken { fd, op: RedirectOp::GreaterAmp }),
                        self.span_from(start),
                    ))
                } else {
                    Ok(Token::new(
                        TokenKind::Redirect(RedirectToken { fd, op: RedirectOp::Greater }),
                        self.span_from(start),
                    ))
                }
            }
            _ => {
                // Digits not followed by redirect operator — backtrack and lex as word.
                self.pos = fd_start;
                self.lex_word(start)
            }
        }
    }

    /// Lex a heredoc delimiter word (may be quoted or unquoted).
    fn lex_heredoc_delimiter(&mut self) -> Result<(String, bool), ParseError> {
        let start = self.pos;
        if self.eat('\'') {
            let text_start = self.pos;
            loop {
                match self.peek() {
                    None | Some('\n') => {
                        return Err(ParseError::new(
                            self.span_from(start),
                            "unterminated heredoc delimiter quote",
                        ))
                    }
                    Some('\'') => {
                        let delim = self.src[text_start..self.pos].to_owned();
                        self.advance();
                        return Ok((delim, true));
                    }
                    _ => {
                        self.advance();
                    }
                }
            }
        } else if self.eat('"') {
            let text_start = self.pos;
            loop {
                match self.peek() {
                    None | Some('\n') => {
                        return Err(ParseError::new(
                            self.span_from(start),
                            "unterminated heredoc delimiter quote",
                        ))
                    }
                    Some('"') => {
                        let delim = self.src[text_start..self.pos].to_owned();
                        self.advance();
                        return Ok((delim, true));
                    }
                    _ => {
                        self.advance();
                    }
                }
            }
        } else {
            let text_start = self.pos;
            while !matches!(self.peek(), None | Some('\n') | Some(' ') | Some('\t')) {
                self.advance();
            }
            if self.pos == text_start {
                return Err(ParseError::new(
                    Span::eof(self.pos),
                    "expected heredoc delimiter after <<",
                ));
            }
            let delim = self.src[text_start..self.pos].to_owned();
            Ok((delim, false))
        }
    }

    /// Lex a word token (unquoted, possibly containing `$` expansions).
    ///
    /// Crucially, `${...}`, `$(...)`, `$((...))`, and `` `...` `` are consumed
    /// in full so they arrive as a single Word token for the parser to expand.
    fn lex_word(&mut self, start: usize) -> Result<Token, ParseError> {
        loop {
            match self.peek() {
                None => break,
                Some('\\') => {
                    self.advance();
                    if self.peek().is_some() {
                        self.advance();
                    }
                }
                Some('$') => {
                    self.advance(); // consume `$`
                    match self.peek() {
                        Some('{') => {
                            self.advance(); // consume `{`
                            self.consume_balanced('{', '}')?;
                        }
                        Some('(') => {
                            self.advance(); // consume first `(`
                            let start_depth = if self.peek() == Some('(') {
                                self.advance(); // consume second `(`
                                2 // `$((`
                            } else {
                                1 // `$(`
                            };
                            self.consume_balanced_depth('(', ')', start_depth)?;
                        }
                        Some(c) if c.is_ascii_alphanumeric() || c == '_'
                            || matches!(c, '@' | '*' | '#' | '?' | '$' | '!' | '0'..='9') =>
                        {
                            self.advance(); // consume the single special/digit char
                            // For identifier chars, consume the rest of the name.
                            if c.is_ascii_alphabetic() || c == '_' {
                                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_')
                                {
                                    self.advance();
                                }
                            }
                        }
                        _ => {} // lone `$` — leave it in the word
                    }
                }
                Some('`') => {
                    self.advance(); // opening backtick
                    loop {
                        match self.peek() {
                            None | Some('\n') => break,
                            Some('\\') => {
                                self.advance();
                                if self.peek().is_some() {
                                    self.advance();
                                }
                            }
                            Some('`') => {
                                self.advance();
                                break;
                            }
                            _ => {
                                self.advance();
                            }
                        }
                    }
                }
                Some(c) if is_word_char(c) => {
                    self.advance();
                }
                _ => break,
            }
        }
        let word = self.src[start..self.pos].to_owned();
        Ok(Token::new(TokenKind::Word(word), self.span_from(start)))
    }

    /// Consume characters until the matching `close` delimiter, tracking depth
    /// for `open`. Starts at depth 1. Errors on unterminated input.
    fn consume_balanced(&mut self, open: char, close: char) -> Result<(), ParseError> {
        self.consume_balanced_depth(open, close, 1)
    }

    fn consume_balanced_depth(
        &mut self,
        open: char,
        close: char,
        start_depth: usize,
    ) -> Result<(), ParseError> {
        let err_pos = self.pos;
        let mut depth = start_depth;
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError::new(
                        Span::eof(err_pos),
                        format!("unterminated `{open}...{close}`"),
                    ))
                }
                Some('\'') => {
                    // Single-quoted strings inside substitutions — consume raw.
                    self.advance();
                    while !matches!(self.peek(), None | Some('\'')) {
                        self.advance();
                    }
                    self.eat('\'');
                }
                Some(c) if c == open => {
                    self.advance();
                    depth += 1;
                }
                Some(c) if c == close => {
                    self.advance();
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {
                    self.advance();
                }
            }
        }
        Ok(())
    }

    /// Lex a fragment inside `"..."`.
    fn lex_dq_fragment(&mut self) -> Result<Token, ParseError> {
        let start = self.pos;
        match self.peek() {
            None => Err(ParseError::new(
                Span::eof(self.pos),
                "unterminated double-quoted string",
            )),
            Some('"') => {
                self.advance();
                Ok(Token::new(TokenKind::DoubleQuoteClose, self.span_from(start)))
            }
            Some('$') | Some('`') => {
                // Consume the full substitution as a single Word token so the
                // parser can expand it with parse_dq_word_parts.
                self.lex_word(start)
            }
            _ => {
                while !matches!(self.peek(), None | Some('"') | Some('$') | Some('`')) {
                    if self.peek() == Some('\\') {
                        self.advance();
                        if self.peek().is_some() {
                            self.advance();
                        }
                    } else {
                        self.advance();
                    }
                }
                let text = self.src[start..self.pos].to_owned();
                Ok(Token::new(TokenKind::DQText(unescape_dq(&text)), self.span_from(start)))
            }
        }
    }

    // ── Heredoc body collection ──────────────────────────────────────────────

    /// After a newline on a line containing `<<` redirects, consume the heredoc
    /// bodies. Returns the raw body strings in order.
    pub fn collect_heredoc_bodies(
        &mut self,
        heredocs: &[(String, bool, bool)],
    ) -> Result<Vec<String>, ParseError> {
        let mut bodies = Vec::with_capacity(heredocs.len());

        for (delimiter, _quoted, strip_tabs) in heredocs {
            let body_start = self.pos;
            let mut body = String::new();

            loop {
                let line_start = self.pos;
                while !matches!(self.peek(), None | Some('\n')) {
                    self.advance();
                }
                let line = &self.src[line_start..self.pos];
                let had_newline = self.eat('\n');

                let stripped = if *strip_tabs { line.trim_start_matches('\t') } else { line };

                if stripped == delimiter {
                    break;
                }

                body.push_str(line);
                if had_newline {
                    body.push('\n');
                }

                if self.is_eof() && stripped != delimiter {
                    return Err(ParseError::new(
                        Span::new(body_start, self.pos - body_start),
                        format!("unterminated heredoc: missing delimiter `{delimiter}`"),
                    ));
                }
            }

            bodies.push(body);
        }

        Ok(bodies)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn is_word_char(c: char) -> bool {
    !matches!(
        c,
        ' ' | '\t'
            | '\n'
            | '|'
            | '&'
            | ';'
            | '('
            | ')'
            | '{'
            | '}'
            | '<'
            | '>'
            | '"'
            | '\''
    )
}

fn unescape_dq(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('$') => out.push('$'),
                Some('`') => out.push('`'),
                Some('\n') => {}
                Some(c) => {
                    out.push('\\');
                    out.push(c);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
