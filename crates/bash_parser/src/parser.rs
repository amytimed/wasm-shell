//! Recursive-descent bash parser.
//!
//! Grammar (simplified):
//!
//! ```text
//! script        := list? EOF
//! list          := pipeline_cmd (list_op pipeline_cmd)*
//! list_op       := '&&' | '||' | ';' | '&' | newline
//! pipeline_cmd  := '!'? pipeline
//! pipeline      := command ('|' command)*
//! command       := simple_cmd | subshell | group
//! subshell      := '(' list ')'
//! group         := '{' list '}'
//! simple_cmd    := (assignment | word | redirect)*
//! ```

use crate::ast::{
    Command, DQPart, Heredoc, HeredocBody, Pipeline, Redirect, RedirectKind, RedirectTarget,
    Script, SimpleCommand, SpecialVar, VarExpr, Word,
};
use crate::error::{ParseError, Span};
use crate::lexer::{Lexer, RedirectOp, Token, TokenKind};

// ── Public entry point ────────────────────────────────────────────────────────

pub fn parse(src: &str) -> Result<Script, ParseError> {
    let mut p = Parser::new(src);
    let mut script = p.parse_script()?;
    fill_heredoc_bodies(&mut script);
    Ok(script)
}

// ── Parser ────────────────────────────────────────────────────────────────────

struct Parser<'src> {
    lexer: Lexer<'src>,
    peeked: Option<Token>,
    in_dq: bool,
    /// Pending heredocs on the current line: (delimiter, quoted, strip_tabs).
    pending_heredocs: Vec<(String, bool, bool)>,
}

impl<'src> Parser<'src> {
    fn new(src: &'src str) -> Self {
        Self { lexer: Lexer::new(src), peeked: None, in_dq: false, pending_heredocs: vec![] }
    }

    // ── Token management ─────────────────────────────────────────────────────

    fn peek(&mut self) -> Result<&Token, ParseError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lexer.next_token(self.in_dq)?);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    fn next(&mut self) -> Result<Token, ParseError> {
        if let Some(t) = self.peeked.take() {
            return Ok(t);
        }
        self.lexer.next_token(self.in_dq)
    }

    fn skip_newlines(&mut self) -> Result<bool, ParseError> {
        let mut any = false;
        loop {
            if matches!(self.peek()?.kind, TokenKind::Newline) {
                self.next()?;
                self.flush_heredocs()?;
                any = true;
            } else {
                break;
            }
        }
        Ok(any)
    }

    fn flush_heredocs(&mut self) -> Result<(), ParseError> {
        if self.pending_heredocs.is_empty() {
            return Ok(());
        }
        let heredocs = std::mem::take(&mut self.pending_heredocs);
        let bodies = self.lexer.collect_heredoc_bodies(&heredocs)?;
        HEREDOC_QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            for (i, body) in bodies.into_iter().enumerate() {
                let (_, quoted, _) = &heredocs[i];
                q.push((*quoted, body));
            }
        });
        Ok(())
    }

    // ── Grammar ───────────────────────────────────────────────────────────────

    fn parse_script(&mut self) -> Result<Script, ParseError> {
        self.skip_newlines()?;
        let mut cmds = Vec::new();
        loop {
            match self.peek()?.kind {
                TokenKind::Eof | TokenKind::RParen | TokenKind::RBrace => break,
                _ => {}
            }
            cmds.push(self.parse_list_item()?);
            self.skip_list_separator()?;
            self.skip_newlines()?;
        }
        Ok(cmds)
    }

    fn parse_list_item(&mut self) -> Result<Command, ParseError> {
        let mut left = self.parse_pipeline_cmd()?;

        loop {
            match self.peek()?.kind {
                TokenKind::AndAnd => {
                    self.next()?;
                    self.skip_newlines()?;
                    let right = self.parse_pipeline_cmd()?;
                    left = Command::And(Box::new(left), Box::new(right));
                }
                TokenKind::OrOr => {
                    self.next()?;
                    self.skip_newlines()?;
                    let right = self.parse_pipeline_cmd()?;
                    left = Command::Or(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }

        if matches!(self.peek()?.kind, TokenKind::Amp) {
            self.next()?;
            left = Command::Background(Box::new(left));
        }

        Ok(left)
    }

    fn skip_list_separator(&mut self) -> Result<bool, ParseError> {
        if matches!(self.peek()?.kind, TokenKind::Semi) {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_pipeline_cmd(&mut self) -> Result<Command, ParseError> {
        let negated = if matches!(self.peek()?.kind, TokenKind::Bang) {
            self.next()?;
            true
        } else {
            false
        };
        self.parse_pipeline(negated)
    }

    fn parse_pipeline(&mut self, negated: bool) -> Result<Command, ParseError> {
        let first = self.parse_command()?;

        if !matches!(self.peek()?.kind, TokenKind::Pipe) {
            if negated {
                if let Command::Simple(sc) = first {
                    return Ok(Command::Pipeline(Pipeline { commands: vec![sc], negated: true }));
                }
            }
            return Ok(first);
        }

        let first_sc = self.command_to_simple(first)?;
        let mut stages = vec![first_sc];

        while matches!(self.peek()?.kind, TokenKind::Pipe) {
            self.next()?;
            self.skip_newlines()?;
            let cmd = self.parse_command()?;
            stages.push(self.command_to_simple(cmd)?);
        }

        Ok(Command::Pipeline(Pipeline { commands: stages, negated }))
    }

    fn command_to_simple(&self, cmd: Command) -> Result<SimpleCommand, ParseError> {
        match cmd {
            Command::Simple(sc) => Ok(sc),
            _ => Err(ParseError::new(
                Span::eof(self.lexer.pos()),
                "only simple commands are supported as pipeline stages",
            )),
        }
    }

    fn parse_command(&mut self) -> Result<Command, ParseError> {
        match &self.peek()?.kind {
            TokenKind::LParen => self.parse_subshell(),
            TokenKind::LBrace => self.parse_group(),
            TokenKind::Word(w) => {
                match w.as_str() {
                    "if" => {
                        let span = self.peek()?.span;
                        Err(ParseError::new(
                            span,
                            "if statements are not supported — use && / || for conditionals",
                        ))
                    }
                    "for" | "while" | "until" => {
                        let span = self.peek()?.span;
                        Err(ParseError::new(span, "loops are not supported in this shell"))
                    }
                    "case" => {
                        let span = self.peek()?.span;
                        Err(ParseError::new(span, "case statements are not supported"))
                    }
                    "select" => {
                        let span = self.peek()?.span;
                        Err(ParseError::new(span, "select is not supported"))
                    }
                    _ => self.parse_simple_command(),
                }
            }
            _ => self.parse_simple_command(),
        }
    }

    fn parse_subshell(&mut self) -> Result<Command, ParseError> {
        let start = self.peek()?.span;
        self.next()?; // consume `(`
        self.skip_newlines()?;
        let body = self.parse_script()?;
        match self.peek()?.kind {
            TokenKind::RParen => { self.next()?; }
            TokenKind::Eof => return Err(ParseError::new(start, "unclosed subshell, missing `)`")),
            _ => return Err(ParseError::new(self.peek()?.span, "expected `)` to close subshell")),
        }
        Ok(Command::Subshell(body))
    }

    fn parse_group(&mut self) -> Result<Command, ParseError> {
        let start = self.peek()?.span;
        self.next()?; // consume `{`
        self.skip_newlines()?;
        let body = self.parse_script()?;
        self.skip_list_separator()?;
        self.skip_newlines()?;
        match self.peek()?.kind {
            TokenKind::RBrace => { self.next()?; }
            TokenKind::Eof => return Err(ParseError::new(start, "unclosed group, missing `}`")),
            _ => return Err(ParseError::new(self.peek()?.span, "expected `}` to close group")),
        }
        Ok(Command::Group(body))
    }

    fn parse_simple_command(&mut self) -> Result<Command, ParseError> {
        let mut sc = SimpleCommand::empty();
        let mut saw_name = false;

        loop {
            match &self.peek()?.kind {
                TokenKind::Eof
                | TokenKind::Newline
                | TokenKind::Semi
                | TokenKind::Amp
                | TokenKind::AndAnd
                | TokenKind::OrOr
                | TokenKind::Pipe
                | TokenKind::RParen
                | TokenKind::RBrace => break,

                TokenKind::Redirect(_) | TokenKind::HereDoc { .. } => {
                    let redir = self.parse_redirect()?;
                    sc.redirects.push(redir);
                }

                TokenKind::Word(_) | TokenKind::SingleQuote(_) | TokenKind::DoubleQuoteOpen => {
                    let word_span = self.peek()?.span;

                    // Check for assignment `FOO=bar` (only valid before the command name).
                    if !saw_name {
                        if let TokenKind::Word(w) = &self.peek()?.kind {
                            if let Some((name, value_str)) = split_assignment(w) {
                                let offset = word_span.start + name.len() + 1;
                                self.next()?;
                                let value = parse_word_str(&value_str, offset)?;
                                sc.assignments.push((name, value));
                                continue;
                            }
                        }
                    }

                    // Check for shell function definition `name()` — unsupported.
                    // A function def looks like: bare-word followed by `(`.
                    // Only a plain literal name (no `$`) can be a function name.
                    if !saw_name {
                        if let TokenKind::Word(w) = &self.peek()?.kind {
                            let is_plain_name = !w.contains('$')
                                && !w.contains('(')
                                && !w.contains('{')
                                && !w.contains('`');
                            if is_plain_name {
                                let saved_peeked = self.peeked.clone();
                                let saved_pos = self.lexer.pos();
                                self.next()?; // consume the name word
                                if matches!(self.peek()?.kind, TokenKind::LParen) {
                                    return Err(ParseError::new(
                                        word_span,
                                        "shell function definitions are not supported",
                                    ));
                                }
                                // Not a function definition — restore.
                                self.peeked = saved_peeked;
                                self.lexer.set_pos(saved_pos);
                            }
                        }
                    }

                    let word = self.parse_word()?;
                    if !saw_name {
                        sc.name = Some(word);
                        saw_name = true;
                    } else {
                        sc.args.push(word);
                    }
                }

                _ => {
                    let span = self.peek()?.span;
                    let kind_str = format!("{:?}", self.peek()?.kind);
                    return Err(ParseError::new(
                        span,
                        format!("unexpected token {kind_str} in command"),
                    ));
                }
            }
        }

        Ok(Command::Simple(sc))
    }

    // ── Word parsing ──────────────────────────────────────────────────────────

    fn parse_word(&mut self) -> Result<Word, ParseError> {
        let mut parts: Vec<Word> = Vec::new();

        loop {
            match &self.peek()?.kind {
                TokenKind::SingleQuote(s) => {
                    let s = s.clone();
                    self.next()?;
                    parts.push(Word::SingleQuoted(s));
                }
                TokenKind::DoubleQuoteOpen => {
                    parts.push(self.parse_double_quoted()?);
                }
                TokenKind::Word(w) => {
                    let w = w.clone();
                    let span = self.peek()?.span;
                    self.next()?;
                    let word = parse_word_str_inner(&w, span.start)?;
                    // Flatten single literal parts.
                    match word {
                        Word::Concat(mut ps) => parts.append(&mut ps),
                        other => parts.push(other),
                    }
                }
                _ => break,
            }
            // In word context, stop after the first complete token unless we're
            // doing concatenation (e.g. `'foo'"bar"baz`). We continue only if
            // the next token is immediately adjacent (no whitespace).
            // Since the lexer already skips whitespace before each token, any
            // adjacent quote/word would follow without a space token — but our
            // lexer doesn't track adjacency. For now, break after one token;
            // the parser's word-start position handles concatenation when
            // called in a loop from parse_simple_command.
            break;
        }

        Ok(simplify_word_parts(parts))
    }

    fn parse_double_quoted(&mut self) -> Result<Word, ParseError> {
        let open_span = self.peek()?.span;
        self.next()?; // consume `"`
        let old_in_dq = self.in_dq;
        self.in_dq = true;

        let mut parts: Vec<DQPart> = Vec::new();

        loop {
            match self.peek()?.kind.clone() {
                TokenKind::DoubleQuoteClose => {
                    self.next()?;
                    break;
                }
                TokenKind::DQText(s) => {
                    self.next()?;
                    parts.push(DQPart::Literal(s));
                }
                TokenKind::Word(w) => {
                    let span = self.peek()?.span;
                    let w = w.clone();
                    self.next()?;
                    let dq_parts = parse_dq_word_parts(&w, span.start)?;
                    parts.extend(dq_parts);
                }
                TokenKind::Eof => {
                    return Err(ParseError::new(open_span, "unterminated double-quoted string"))
                }
                ref kind => {
                    let span = self.peek()?.span;
                    let kind_str = format!("{kind:?}");
                    return Err(ParseError::new(
                        span,
                        format!("unexpected token {kind_str} inside double-quoted string"),
                    ));
                }
            }
        }

        self.in_dq = old_in_dq;
        Ok(Word::DoubleQuoted(parts))
    }

    // ── Redirections ──────────────────────────────────────────────────────────

    fn parse_redirect(&mut self) -> Result<Redirect, ParseError> {
        let tok = self.next()?;

        match tok.kind {
            TokenKind::Redirect(rt) => {
                let (default_fd, kind) = match rt.op {
                    RedirectOp::Less => (0, RedirectKind::Read),
                    RedirectOp::Greater => (1, RedirectKind::Write),
                    RedirectOp::GreaterGreater => (1, RedirectKind::Append),
                    RedirectOp::LessGreater => (0, RedirectKind::ReadWrite),
                    RedirectOp::GreaterPipe => (1, RedirectKind::Clobber),
                    RedirectOp::GreaterAmp => {
                        let fd = rt.fd.or(Some(1));
                        return self.parse_dup_redirect(fd, RedirectKind::Write);
                    }
                    RedirectOp::LessAmp => {
                        let fd = rt.fd.or(Some(0));
                        return self.parse_dup_redirect(fd, RedirectKind::Read);
                    }
                };
                let fd = rt.fd.or(Some(default_fd));
                let target_word = self.parse_word()?;
                Ok(Redirect { fd, kind, target: RedirectTarget::Word(target_word) })
            }

            TokenKind::HereDoc { fd, delimiter, quoted, strip_tabs } => {
                self.pending_heredocs.push((delimiter.clone(), quoted, strip_tabs));
                let heredoc = Heredoc {
                    delimiter,
                    quoted,
                    strip_tabs,
                    body: if quoted {
                        HeredocBody::Literal(String::new())
                    } else {
                        HeredocBody::Parts(vec![])
                    },
                };
                let default_fd = fd.or(Some(0));
                Ok(Redirect {
                    fd: default_fd,
                    kind: RedirectKind::Read,
                    target: RedirectTarget::Heredoc(heredoc),
                })
            }

            _ => Err(ParseError::new(tok.span, "expected redirect operator")),
        }
    }

    fn parse_dup_redirect(
        &mut self,
        fd: Option<u32>,
        kind: RedirectKind,
    ) -> Result<Redirect, ParseError> {
        match self.peek()?.kind.clone() {
            TokenKind::Word(w) => {
                if w == "-" {
                    self.next()?;
                    return Ok(Redirect { fd, kind, target: RedirectTarget::CloseFd });
                }
                if let Ok(target_fd) = w.parse::<u32>() {
                    self.next()?;
                    return Ok(Redirect { fd, kind, target: RedirectTarget::Fd(target_fd) });
                }
                let target_word = self.parse_word()?;
                Ok(Redirect { fd, kind, target: RedirectTarget::Word(target_word) })
            }
            _ => {
                let target_word = self.parse_word()?;
                Ok(Redirect { fd, kind, target: RedirectTarget::Word(target_word) })
            }
        }
    }
}

// ── Word part parsing (free functions) ───────────────────────────────────────

fn parse_word_str(s: &str, base: usize) -> Result<Word, ParseError> {
    parse_word_str_inner(s, base)
}

fn parse_word_str_inner(s: &str, base: usize) -> Result<Word, ParseError> {
    let parts = parse_word_parts(s, base, false)?;
    Ok(simplify_word_parts(parts))
}

fn parse_word_parts(s: &str, base: usize, inside_dq: bool) -> Result<Vec<Word>, ParseError> {
    let mut parts = Vec::new();
    let mut chars = s.char_indices().peekable();
    let mut literal = String::new();

    while let Some((i, c)) = chars.next() {
        match c {
            '\\' if !inside_dq => {
                if let Some((_, nc)) = chars.next() {
                    if nc != '\n' {
                        literal.push(nc);
                    }
                }
            }
            '$' => {
                if !literal.is_empty() {
                    parts.push(Word::Literal(std::mem::take(&mut literal)));
                }
                let rest = &s[i..];
                let (var_part, consumed) = parse_dollar(rest, base + i)?;
                parts.push(var_part);
                // Skip `consumed - 1` more chars (the `$` was already counted as `i`).
                advance_chars(&mut chars, consumed - 1);
            }
            '`' => {
                if !literal.is_empty() {
                    parts.push(Word::Literal(std::mem::take(&mut literal)));
                }
                let rest = &s[i + 1..];
                let (script, consumed) = parse_backtick(rest, base + i + 1)?;
                parts.push(Word::CommandSubst(script));
                advance_chars(&mut chars, consumed);
            }
            _ => literal.push(c),
        }
    }

    if !literal.is_empty() {
        parts.push(Word::Literal(literal));
    }
    Ok(parts)
}

fn parse_dq_word_parts(s: &str, base: usize) -> Result<Vec<DQPart>, ParseError> {
    let mut parts = Vec::new();
    let mut chars = s.char_indices().peekable();
    let mut literal = String::new();

    while let Some((i, c)) = chars.next() {
        match c {
            '$' => {
                if !literal.is_empty() {
                    parts.push(DQPart::Literal(std::mem::take(&mut literal)));
                }
                let rest = &s[i..];
                let (word_part, consumed) = parse_dollar(rest, base + i)?;
                parts.push(word_to_dq_part(word_part));
                advance_chars(&mut chars, consumed - 1);
            }
            '`' => {
                if !literal.is_empty() {
                    parts.push(DQPart::Literal(std::mem::take(&mut literal)));
                }
                let rest = &s[i + 1..];
                let (script, consumed) = parse_backtick(rest, base + i + 1)?;
                parts.push(DQPart::CommandSubst(script));
                advance_chars(&mut chars, consumed);
            }
            _ => literal.push(c),
        }
    }

    if !literal.is_empty() {
        parts.push(DQPart::Literal(literal));
    }
    Ok(parts)
}

/// Advance a peekable char-index iterator by `n` bytes (not chars).
fn advance_chars(
    chars: &mut std::iter::Peekable<std::str::CharIndices>,
    mut n: usize,
) {
    while n > 0 {
        match chars.next() {
            Some((_, c)) => n = n.saturating_sub(c.len_utf8()),
            None => break,
        }
    }
}

fn word_to_dq_part(w: Word) -> DQPart {
    match w {
        Word::Literal(s) => DQPart::Literal(s),
        Word::Variable(v) => DQPart::Variable(v),
        Word::CommandSubst(s) => DQPart::CommandSubst(s),
        Word::ArithSubst(s) => DQPart::ArithSubst(s),
        _ => DQPart::Literal(String::new()),
    }
}

/// Parse a `$…` expansion. `s` starts with `$`.
/// Returns (Word, bytes_consumed_from_s).
fn parse_dollar(s: &str, base: usize) -> Result<(Word, usize), ParseError> {
    debug_assert!(s.starts_with('$'));
    let after = &s[1..];

    if after.is_empty() {
        return Ok((Word::Literal("$".to_owned()), 1));
    }

    match after.as_bytes()[0] {
        b'(' => {
            if after.starts_with("((") {
                // Arithmetic `$(( ... ))`.
                let inner_start = 3; // skip `$((`
                let (inner, consumed) =
                    extract_balanced(&s[inner_start..], '(', ')', base + inner_start, "))")?;
                Ok((Word::ArithSubst(inner.to_owned()), inner_start + consumed))
            } else {
                // Command substitution `$( ... )`.
                let inner_start = 2; // skip `$(`
                let (inner, consumed) =
                    extract_balanced(&s[inner_start..], '(', ')', base + inner_start, ")")?;
                let script = parse(inner.trim())?;
                Ok((Word::CommandSubst(script), inner_start + consumed))
            }
        }
        b'{' => {
            let (var_expr, consumed) = parse_brace_var(&s[2..], base + 2)?;
            // consumed does not include the closing `}`; add 2 for `${` and 1 for `}`
            Ok((Word::Variable(var_expr), 2 + consumed + 1))
        }
        b'0'..=b'9' => {
            let n = after.as_bytes()[0] - b'0';
            let special =
                if n == 0 { SpecialVar::Zero } else { SpecialVar::Positional(n) };
            Ok((Word::Variable(VarExpr::Special(special)), 2))
        }
        b'*' => Ok((Word::Variable(VarExpr::Special(SpecialVar::Star)), 2)),
        b'@' => Ok((Word::Variable(VarExpr::Special(SpecialVar::At)), 2)),
        b'#' => Ok((Word::Variable(VarExpr::Special(SpecialVar::Hash)), 2)),
        b'$' => Ok((Word::Variable(VarExpr::Special(SpecialVar::Pid)), 2)),
        b'?' => Ok((Word::Variable(VarExpr::Special(SpecialVar::LastExit)), 2)),
        b'!' => Ok((Word::Variable(VarExpr::Special(SpecialVar::LastBgPid)), 2)),
        _ => {
            let name_len = after
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            if name_len == 0 {
                return Ok((Word::Literal("$".to_owned()), 1));
            }
            let name = after[..name_len].to_owned();
            Ok((Word::Variable(VarExpr::Simple(name)), 1 + name_len))
        }
    }
}

/// Extract the content inside balanced delimiters.
/// `s` starts after the opening delimiter.
/// Returns (inner_str, bytes_consumed_including_closing).
fn extract_balanced<'a>(
    s: &'a str,
    open: char,
    close: char,
    base: usize,
    close_str: &str,
) -> Result<(&'a str, usize), ParseError> {
    let mut depth = 1usize;
    let mut i = 0;

    while i < s.len() {
        if depth == 1 && s[i..].starts_with(close_str) {
            return Ok((&s[..i], i + close_str.len()));
        }
        let b = s.as_bytes()[i];
        if b == open as u8 {
            depth += 1;
        } else if b == close as u8 {
            depth -= 1;
            if depth == 0 {
                return Ok((&s[..i], i + 1));
            }
        }
        i += 1;
    }
    Err(ParseError::new(Span::eof(base + i), format!("unmatched `{close_str}`")))
}

/// Parse inside `${…}`. `s` starts after `${`.
/// Returns (VarExpr, bytes consumed NOT including the `}`).
fn parse_brace_var(s: &str, base: usize) -> Result<(VarExpr, usize), ParseError> {
    let close = s.find('}').ok_or_else(|| {
        ParseError::new(Span::eof(base + s.len()), "unclosed `${`")
    })?;
    let inner = &s[..close];

    if inner.is_empty() {
        return Err(ParseError::new(
            Span::new(base, close + 1),
            "empty variable expression `${}`",
        ));
    }

    // `${#VAR}` — length.
    if let Some(name) = inner.strip_prefix('#') {
        if is_valid_name(name) {
            return Ok((VarExpr::Length(name.to_owned()), close));
        }
    }

    // Operator forms: `${VAR:-…}` etc.
    if let Some(op_pos) = find_var_operator(inner) {
        let name = inner[..op_pos].to_owned();
        let has_colon = inner.as_bytes()[op_pos] == b':';
        let op_byte_offset = if has_colon { op_pos + 1 } else { op_pos };
        let op_char = inner.as_bytes()[op_byte_offset] as char;
        let word_str = &inner[op_byte_offset + 1..];
        let word = parse_word_str_inner(word_str, base + op_byte_offset + 1)?;
        let var_expr = match op_char {
            '-' => VarExpr::DefaultIfUnset(name, Box::new(word)),
            '=' => VarExpr::AssignIfUnset(name, Box::new(word)),
            '+' => VarExpr::AlternateIfSet(name, Box::new(word)),
            '?' => VarExpr::ErrorIfUnset(name, Box::new(word)),
            _ => unreachable!(),
        };
        return Ok((var_expr, close));
    }

    Ok((VarExpr::Simple(inner.to_owned()), close))
}

fn find_var_operator(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b':' && i + 1 < bytes.len() {
            if matches!(bytes[i + 1], b'-' | b'=' | b'+' | b'?') {
                return Some(i);
            }
        } else if matches!(b, b'-' | b'=' | b'+' | b'?') && i > 0 {
            return Some(i);
        }
    }
    None
}

fn parse_backtick(s: &str, base: usize) -> Result<(Script, usize), ParseError> {
    let close = s.find('`').ok_or_else(|| {
        ParseError::new(
            Span::eof(base + s.len()),
            "unterminated backtick command substitution",
        )
    })?;
    let script = parse(s[..close].trim())?;
    Ok((script, close + 1))
}

fn simplify_word_parts(parts: Vec<Word>) -> Word {
    match parts.len() {
        0 => Word::Literal(String::new()),
        1 => parts.into_iter().next().unwrap(),
        _ => Word::Concat(parts),
    }
}

fn split_assignment(s: &str) -> Option<(String, String)> {
    let eq = s.find('=')?;
    let name = &s[..eq];
    if name.is_empty() {
        return None;
    }
    let first = name.chars().next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((name.to_owned(), s[eq + 1..].to_owned()))
}

fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ── Heredoc body post-processing ─────────────────────────────────────────────

use std::cell::RefCell;

thread_local! {
    static HEREDOC_QUEUE: RefCell<Vec<(bool, String)>> = RefCell::new(Vec::new());
}

fn fill_heredoc_bodies(script: &mut Script) {
    HEREDOC_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let mut iter = q.drain(..);
        fill_heredocs_in_script(script, &mut iter);
    });
}

fn fill_heredocs_in_script(script: &mut Script, queue: &mut dyn Iterator<Item = (bool, String)>) {
    for cmd in script.iter_mut() {
        fill_heredocs_in_cmd(cmd, queue);
    }
}

fn fill_heredocs_in_cmd(cmd: &mut Command, queue: &mut dyn Iterator<Item = (bool, String)>) {
    match cmd {
        Command::Simple(sc) => fill_heredocs_in_simple(sc, queue),
        Command::Pipeline(p) => {
            for sc in &mut p.commands {
                fill_heredocs_in_simple(sc, queue);
            }
        }
        Command::And(l, r) | Command::Or(l, r) => {
            fill_heredocs_in_cmd(l, queue);
            fill_heredocs_in_cmd(r, queue);
        }
        Command::Sequence(cmds) => {
            for c in cmds {
                fill_heredocs_in_cmd(c, queue);
            }
        }
        Command::Background(c) => fill_heredocs_in_cmd(c, queue),
        Command::Subshell(s) | Command::Group(s) => fill_heredocs_in_script(s, queue),
    }
}

fn fill_heredocs_in_simple(
    sc: &mut SimpleCommand,
    queue: &mut dyn Iterator<Item = (bool, String)>,
) {
    for r in &mut sc.redirects {
        if let RedirectTarget::Heredoc(ref mut hd) = r.target {
            if let Some((quoted, body)) = queue.next() {
                if quoted {
                    hd.body = HeredocBody::Literal(body);
                } else {
                    let parts = parse_dq_word_parts(&body, 0)
                        .unwrap_or_else(|_| vec![DQPart::Literal(body.clone())]);
                    hd.body = HeredocBody::Parts(parts);
                }
            }
        }
    }
}
