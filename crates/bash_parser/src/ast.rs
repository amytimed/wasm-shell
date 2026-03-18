/// A complete parsed script is a list of commands (separated by newlines/semicolons).
pub type Script = Vec<Command>;

/// Top-level command node.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A simple command: `cmd arg1 arg2 >file`
    Simple(SimpleCommand),
    /// `cmd1 | cmd2 | cmd3` — run concurrently, stdout→stdin chained.
    Pipeline(Pipeline),
    /// `left && right` — run right only if left succeeds (exit 0).
    And(Box<Command>, Box<Command>),
    /// `left || right` — run right only if left fails (exit != 0).
    Or(Box<Command>, Box<Command>),
    /// `cmd1; cmd2; cmd3` — run sequentially.
    Sequence(Vec<Command>),
    /// `cmd &` — run detached in background.
    Background(Box<Command>),
    /// `(cmd1; cmd2)` — run in a subshell (forked env+cwd).
    Subshell(Script),
    /// `{ cmd1; cmd2; }` — run in current shell context (grouping only).
    Group(Script),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    /// All stages. Must have at least 2 elements.
    pub commands: Vec<SimpleCommand>,
    /// If true the whole pipeline is negated (! pipeline).
    pub negated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    /// Variable assignments that precede the command (e.g. `FOO=bar cmd`).
    /// If `name` is None these are environment-only assignments (no command run).
    pub assignments: Vec<(String, Word)>,
    /// The command name word (argv[0]), if present.
    pub name: Option<Word>,
    /// Arguments (argv[1..]).
    pub args: Vec<Word>,
    /// Redirections.
    pub redirects: Vec<Redirect>,
}

impl SimpleCommand {
    pub fn empty() -> Self {
        Self { assignments: vec![], name: None, args: vec![], redirects: vec![] }
    }
}

// ── Words ────────────────────────────────────────────────────────────────────

/// A word is a sequence of parts that expand to a string at runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum Word {
    /// A literal unquoted string (no expansion).
    Literal(String),
    /// `'text'` — no expansion at all.
    SingleQuoted(String),
    /// `"parts..."` — variable + command substitution inside.
    DoubleQuoted(Vec<DQPart>),
    /// `$VAR` or `${VAR}` or `${VAR:-default}` etc.
    Variable(VarExpr),
    /// `$(cmd)` or `` `cmd` ``
    CommandSubst(Script),
    /// `$((expr))` — arithmetic; expr is the raw inner string.
    ArithSubst(String),
    /// A heredoc reference resolved during parsing.
    Heredoc(Heredoc),
    /// Multiple parts concatenated: `foo${BAR}baz`
    Concat(Vec<Word>),
}

/// Parts allowed inside double-quoted strings.
#[derive(Debug, Clone, PartialEq)]
pub enum DQPart {
    Literal(String),
    Variable(VarExpr),
    CommandSubst(Script),
    ArithSubst(String),
}

/// Variable expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum VarExpr {
    /// `$VAR` or `${VAR}`
    Simple(String),
    /// `${VAR:-word}` — use default if unset or empty.
    DefaultIfUnset(String, Box<Word>),
    /// `${VAR:=word}` — assign default if unset or empty.
    AssignIfUnset(String, Box<Word>),
    /// `${VAR:+word}` — use alternate if set.
    AlternateIfSet(String, Box<Word>),
    /// `${VAR:?msg}` — error if unset.
    ErrorIfUnset(String, Box<Word>),
    /// `${#VAR}` — string length.
    Length(String),
    /// `$0`, `$1`, … `$9`, `$*`, `$@`, `$#`, `$$`, `$?`, `$!`
    Special(SpecialVar),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecialVar {
    /// `$0` — script/shell name.
    Zero,
    /// `$1`..`$9` — positional parameters.
    Positional(u8),
    /// `$*` — all positional params as single word.
    Star,
    /// `$@` — all positional params as separate words.
    At,
    /// `$#` — number of positional params.
    Hash,
    /// `$$` — current shell PID (virtual).
    Pid,
    /// `$?` — last exit code.
    LastExit,
    /// `$!` — PID of last background job (virtual).
    LastBgPid,
}

// ── Heredocs ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Heredoc {
    /// The delimiter word as written (e.g. `EOF`).
    pub delimiter: String,
    /// If true the delimiter was quoted → no expansion in the body.
    pub quoted: bool,
    /// Strip leading tabs if `<<-` was used.
    pub strip_tabs: bool,
    /// The body text (everything between the opening line and the delimiter line).
    /// Collected during parsing.
    pub body: HeredocBody,
}

/// The body of a heredoc — either a raw string (quoted heredoc) or a list of
/// parts to be expanded (unquoted heredoc).
#[derive(Debug, Clone, PartialEq)]
pub enum HeredocBody {
    /// No expansion (quoted delimiter).
    Literal(String),
    /// Variable + command substitution (unquoted delimiter).
    Parts(Vec<DQPart>),
}

// ── Redirections ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    /// The source file descriptor. None = default (1 for output, 0 for input).
    pub fd: Option<u32>,
    pub kind: RedirectKind,
    pub target: RedirectTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    /// `>`
    Write,
    /// `>>`
    Append,
    /// `<`
    Read,
    /// `<>`
    ReadWrite,
    /// `>|` (write, clobber even if noclobber set)
    Clobber,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RedirectTarget {
    Word(Word),
    /// `>&2` — duplicate fd.
    Fd(u32),
    /// `>&-` — close fd.
    CloseFd,
    Heredoc(Heredoc),
}
