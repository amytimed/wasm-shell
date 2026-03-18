use std::future::Future;
use std::pin::Pin;

use bash_parser::ast::{DQPart, Heredoc, HeredocBody, SpecialVar, VarExpr, Word};

use crate::env::EnvMap;
use crate::error::ShellError;
use crate::shell::Shell;

type WordFuture<'a> = Pin<Box<dyn Future<Output = Result<String, ShellError>> + 'a>>;

// ── Public entry points ───────────────────────────────────────────────────────

/// Expand a single `Word` to a `String`.
/// Command substitutions execute against the current shell state.
/// Undefined variables expand to `""`.
///
/// This function returns a boxed future to break the mutual recursion between
/// word expansion, double-quoted part expansion, and variable expansion.
pub(crate) fn expand_word<'a>(word: &'a Word, shell: &'a mut Shell) -> WordFuture<'a> {
    Box::pin(expand_word_inner(word, shell))
}

async fn expand_word_inner(word: &Word, shell: &mut Shell) -> Result<String, ShellError> {
    match word {
        Word::Literal(s) => {
            // Tilde expansion: `~` and `~/path` expand to $HOME.
            if s == "~" || s.starts_with("~/") {
                let home = shell.get_env("HOME");
                let home_str = home.as_deref().unwrap_or("/");
                Ok(format!("{}{}", home_str, &s[1..]))
            } else {
                Ok(s.clone())
            }
        }
        Word::SingleQuoted(s) => Ok(s.clone()),
        Word::DoubleQuoted(parts) => {
            let mut out = String::new();
            for part in parts {
                out.push_str(&expand_dq_part(part, shell).await?);
            }
            Ok(out)
        }
        Word::Variable(var) => expand_var(var, shell).await,
        Word::CommandSubst(script) => {
            let bytes = crate::exec::exec_capture(shell, script).await?;
            let s = String::from_utf8_lossy(&bytes).into_owned();
            // Trim trailing newlines, matching bash behaviour.
            Ok(s.trim_end_matches('\n').to_string())
        }
        Word::ArithSubst(expr) => {
            let val = eval_arith(expr, &shell.env)?;
            Ok(val.to_string())
        }
        Word::Heredoc(hd) => expand_heredoc(hd, shell).await,
        Word::Concat(parts) => {
            let mut out = String::new();
            for part in parts {
                out.push_str(&expand_word(part, shell).await?);
            }
            Ok(out)
        }
    }
}

/// Expand a slice of words to a `Vec<String>` (argv-style).
pub(crate) async fn expand_words(
    words: &[Word],
    shell: &mut Shell,
) -> Result<Vec<String>, ShellError> {
    let mut result = Vec::with_capacity(words.len());
    for w in words {
        result.push(expand_word(w, shell).await?);
    }
    Ok(result)
}

// ── Double-quoted parts ───────────────────────────────────────────────────────

async fn expand_dq_part(part: &DQPart, shell: &mut Shell) -> Result<String, ShellError> {
    match part {
        DQPart::Literal(s) => Ok(s.clone()),
        DQPart::Variable(var) => expand_var(var, shell).await,
        DQPart::CommandSubst(script) => {
            let bytes = crate::exec::exec_capture(shell, script).await?;
            let s = String::from_utf8_lossy(&bytes).into_owned();
            Ok(s.trim_end_matches('\n').to_string())
        }
        DQPart::ArithSubst(expr) => {
            let val = eval_arith(expr, &shell.env)?;
            Ok(val.to_string())
        }
    }
}

// ── Variable expressions ──────────────────────────────────────────────────────

async fn expand_var(var: &VarExpr, shell: &mut Shell) -> Result<String, ShellError> {
    match var {
        VarExpr::Simple(name) => Ok(shell.env.get(name).unwrap_or("").to_string()),

        VarExpr::DefaultIfUnset(name, default) => {
            let val = shell.env.get(name).map(str::to_string);
            if val.as_deref().map_or(true, str::is_empty) {
                expand_word(default, shell).await
            } else {
                Ok(val.unwrap())
            }
        }

        VarExpr::AssignIfUnset(name, default) => {
            let val = shell.env.get(name).map(str::to_string);
            if val.as_deref().map_or(true, str::is_empty) {
                let expanded = expand_word(default, shell).await?;
                shell.env.set(name.clone(), expanded.clone());
                Ok(expanded)
            } else {
                Ok(val.unwrap())
            }
        }

        VarExpr::AlternateIfSet(name, alt) => {
            let val = shell.env.get(name).map(str::to_string);
            if val.as_deref().map_or(false, |v| !v.is_empty()) {
                expand_word(alt, shell).await
            } else {
                Ok(String::new())
            }
        }

        VarExpr::ErrorIfUnset(name, msg) => {
            let val = shell.env.get(name).map(str::to_string);
            if val.as_deref().map_or(true, str::is_empty) {
                let msg_str = expand_word(msg, shell).await?;
                let msg_str = if msg_str.is_empty() {
                    format!("{}: parameter null or not set", name)
                } else {
                    format!("{}: {}", name, msg_str)
                };
                Err(ShellError::Io(msg_str))
            } else {
                Ok(val.unwrap())
            }
        }

        VarExpr::Length(name) => {
            Ok(shell.env.get(name).unwrap_or("").len().to_string())
        }

        VarExpr::Special(special) => expand_special(special, shell),
    }
}

fn expand_special(special: &SpecialVar, shell: &Shell) -> Result<String, ShellError> {
    Ok(match special {
        SpecialVar::Zero => "wasm_shell".to_string(),
        SpecialVar::Positional(_) => String::new(),
        SpecialVar::Star | SpecialVar::At => String::new(),
        SpecialVar::Hash => "0".to_string(),
        SpecialVar::Pid => "1".to_string(),
        SpecialVar::LastExit => shell.last_exit.0.to_string(),
        SpecialVar::LastBgPid => "0".to_string(),
    })
}

// ── Heredocs ──────────────────────────────────────────────────────────────────

async fn expand_heredoc(hd: &Heredoc, shell: &mut Shell) -> Result<String, ShellError> {
    match &hd.body {
        HeredocBody::Literal(s) => Ok(s.clone()),
        HeredocBody::Parts(parts) => {
            let mut out = String::new();
            for part in parts {
                out.push_str(&expand_dq_part(part, shell).await?);
            }
            Ok(out)
        }
    }
}

// ── Arithmetic evaluator ──────────────────────────────────────────────────────

/// Evaluate `$((expr))`. Variables inside are substituted before parsing.
pub(crate) fn eval_arith(expr: &str, env: &EnvMap) -> Result<i64, ShellError> {
    let expanded = expand_arith_vars(expr, env);
    parse_arith(expanded.trim()).map_err(|e| ShellError::Io(format!("arithmetic: {}", e)))
}

fn expand_arith_vars(expr: &str, env: &EnvMap) -> String {
    let bytes = expr.as_bytes();
    let mut result = String::with_capacity(expr.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'{' {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'}' {
                    i += 1;
                }
                let name = &expr[start..i];
                if i < bytes.len() {
                    i += 1; // consume '}'
                }
                result.push_str(env.get(name).unwrap_or("0"));
            } else {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                {
                    i += 1;
                }
                if start == i {
                    result.push('$');
                } else {
                    let name = &expr[start..i];
                    result.push_str(env.get(name).unwrap_or("0"));
                }
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

// -- Arithmetic token --

#[derive(Debug, Clone, PartialEq)]
enum ATok {
    Num(i64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    LParen,
    RParen,
}

fn tokenize_arith(input: &str) -> Result<Vec<ATok>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '0'..='9' => {
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        num.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(ATok::Num(num.parse().map_err(|e: std::num::ParseIntError| e.to_string())?));
            }
            '+' => {
                chars.next();
                tokens.push(ATok::Plus);
            }
            '-' => {
                chars.next();
                tokens.push(ATok::Minus);
            }
            '*' => {
                chars.next();
                if chars.peek() == Some(&'*') {
                    chars.next();
                    tokens.push(ATok::StarStar);
                } else {
                    tokens.push(ATok::Star);
                }
            }
            '/' => {
                chars.next();
                tokens.push(ATok::Slash);
            }
            '%' => {
                chars.next();
                tokens.push(ATok::Percent);
            }
            '(' => {
                chars.next();
                tokens.push(ATok::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(ATok::RParen);
            }
            _ => return Err(format!("unexpected char in arithmetic: {:?}", c)),
        }
    }
    Ok(tokens)
}

fn parse_arith(input: &str) -> Result<i64, String> {
    let tokens = tokenize_arith(input)?;
    let mut pos = 0;
    let val = parse_expr(&tokens, &mut pos, 0)?;
    if pos != tokens.len() {
        return Err(format!("unexpected token at position {}", pos));
    }
    Ok(val)
}

fn parse_expr(tokens: &[ATok], pos: &mut usize, min_bp: u8) -> Result<i64, String> {
    let mut lhs = parse_prefix(tokens, pos)?;
    loop {
        let Some(op) = tokens.get(*pos) else { break };
        // (left binding power, right binding power)
        let (lbp, rbp): (u8, u8) = match op {
            ATok::Plus | ATok::Minus => (10, 11),
            ATok::Star | ATok::Slash | ATok::Percent => (20, 21),
            ATok::StarStar => (30, 29), // right-associative
            _ => break,
        };
        if lbp < min_bp {
            break;
        }
        *pos += 1;
        let rhs = parse_expr(tokens, pos, rbp)?;
        lhs = apply_binop(op, lhs, rhs)?;
    }
    Ok(lhs)
}

fn parse_prefix(tokens: &[ATok], pos: &mut usize) -> Result<i64, String> {
    match tokens.get(*pos) {
        Some(ATok::Num(n)) => {
            *pos += 1;
            Ok(*n)
        }
        Some(ATok::Minus) => {
            *pos += 1;
            let val = parse_prefix(tokens, pos)?;
            Ok(-val)
        }
        Some(ATok::Plus) => {
            *pos += 1;
            parse_prefix(tokens, pos)
        }
        Some(ATok::LParen) => {
            *pos += 1;
            let val = parse_expr(tokens, pos, 0)?;
            if tokens.get(*pos) != Some(&ATok::RParen) {
                return Err("expected closing parenthesis".to_string());
            }
            *pos += 1;
            Ok(val)
        }
        other => Err(format!("expected number or expression, got {:?}", other)),
    }
}

fn apply_binop(op: &ATok, lhs: i64, rhs: i64) -> Result<i64, String> {
    match op {
        ATok::Plus => Ok(lhs + rhs),
        ATok::Minus => Ok(lhs - rhs),
        ATok::Star => Ok(lhs * rhs),
        ATok::Slash => {
            if rhs == 0 {
                Err("division by zero".to_string())
            } else {
                Ok(lhs / rhs)
            }
        }
        ATok::Percent => {
            if rhs == 0 {
                Err("modulo by zero".to_string())
            } else {
                Ok(lhs % rhs)
            }
        }
        ATok::StarStar => {
            if rhs < 0 {
                Ok(0)
            } else {
                Ok(lhs.pow(rhs as u32))
            }
        }
        _ => unreachable!(),
    }
}
