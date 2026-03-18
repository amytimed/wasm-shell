use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use bash_parser::ast::{Command, Pipeline, Redirect, RedirectKind, RedirectTarget, Script, SimpleCommand, Word};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::ShellError;
use crate::expand::{expand_word, expand_words};
use crate::io::{BytesReader, Io, VecWriter};
use crate::shell::{CtxShared, ExitCode, ProgramContext, Shell};
use crate::vfs::resolve_path;

// ── Top-level helpers ─────────────────────────────────────────────────────────

/// Execute a script and return its exit code.
pub(crate) async fn execute_script(
    shell: &mut Shell,
    script: &Script,
    io: Io,
) -> Result<ExitCode, ShellError> {
    let mut code = ExitCode::SUCCESS;
    for cmd in script {
        code = execute_command(shell, cmd, io.share()).await?;
        shell.last_exit = code;
    }
    Ok(code)
}

/// Execute a script, capturing its stdout. Used by command substitution.
pub(crate) async fn exec_capture(
    shell: &mut Shell,
    script: &Script,
) -> Result<Vec<u8>, ShellError> {
    let stdout = VecWriter::new();
    let io = Io {
        stdin: BytesReader::empty(),
        stdout: stdout.clone(),
        stderr: VecWriter::new(), // discard stderr in substitution
    };
    execute_script(shell, script, io).await?;
    Ok(stdout.bytes())
}

// ── Command dispatch ──────────────────────────────────────────────────────────

fn execute_command<'a>(
    shell: &'a mut Shell,
    cmd: &'a Command,
    io: Io,
) -> Pin<Box<dyn Future<Output = Result<ExitCode, ShellError>> + 'a>> {
    Box::pin(execute_command_inner(shell, cmd, io))
}

async fn execute_command_inner(
    shell: &mut Shell,
    cmd: &Command,
    io: Io,
) -> Result<ExitCode, ShellError> {
    match cmd {
        Command::Simple(sc) => execute_simple(shell, sc, io).await,
        Command::Pipeline(p) => execute_pipeline(shell, p, io).await,
        Command::And(left, right) => {
            let code = execute_command(shell, left, io.share()).await?;
            shell.last_exit = code;
            if code.0 == 0 {
                execute_command(shell, right, io).await
            } else {
                Ok(code)
            }
        }
        Command::Or(left, right) => {
            let code = execute_command(shell, left, io.share()).await?;
            shell.last_exit = code;
            if code.0 != 0 {
                execute_command(shell, right, io).await
            } else {
                Ok(code)
            }
        }
        Command::Sequence(cmds) => {
            let mut code = ExitCode::SUCCESS;
            for c in cmds {
                code = execute_command(shell, c, io.share()).await?;
                shell.last_exit = code;
            }
            Ok(code)
        }
        Command::Background(cmd) => {
            // Stage 6: run detached. For now run synchronously.
            execute_command(shell, cmd, io).await
        }
        Command::Subshell(script) => {
            // True subshell isolation (clone env+cwd, shared VFS) is Stage 6.
            // For now: run in-place with a cloned env so mutations don't leak.
            let saved_env = shell.env.clone();
            let saved_cwd = shell.cwd.clone();
            let code = execute_script(shell, script, io).await?;
            shell.env = saved_env;
            shell.cwd = saved_cwd;
            Ok(code)
        }
        Command::Group(script) => execute_script(shell, script, io).await,
    }
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

async fn execute_pipeline(
    shell: &mut Shell,
    pipeline: &Pipeline,
    io: Io,
) -> Result<ExitCode, ShellError> {
    let Io { stdin, stdout: parent_stdout, stderr } = io;
    let stages = &pipeline.commands;
    let n = stages.len();

    let mut stage_stdin: Option<BytesReader> = Some(stdin);
    let mut last_code = ExitCode::SUCCESS;

    for (i, stage) in stages.iter().enumerate() {
        let is_last = i == n - 1;
        // Last stage writes to the parent's stdout; others to a temporary buffer.
        let stage_stdout = if is_last { parent_stdout.clone() } else { VecWriter::new() };
        let stage_io = Io {
            stdin: stage_stdin.take().unwrap(),
            stdout: stage_stdout.clone(),
            stderr: stderr.clone(),
        };

        last_code = execute_simple(shell, stage, stage_io).await?;
        shell.last_exit = last_code;

        if !is_last {
            stage_stdin = Some(BytesReader::new(stage_stdout.bytes()));
        }
    }

    if pipeline.negated {
        last_code = ExitCode(if last_code.0 == 0 { 1 } else { 0 });
    }
    Ok(last_code)
}

// ── Simple command ────────────────────────────────────────────────────────────

async fn execute_simple(
    shell: &mut Shell,
    cmd: &SimpleCommand,
    io: Io,
) -> Result<ExitCode, ShellError> {
    // Process variable assignments (VAR=value before command).
    // For assignment-only commands (no name), apply directly to the shell env.
    // For commands with a name, assignments are scoped: applied for the duration
    // of the command, then restored afterward.
    let mut saved_assignments: Vec<(String, Option<String>)> = Vec::new();
    for (var_name, value_word) in &cmd.assignments {
        let value = expand_word(value_word, shell).await?;
        if cmd.name.is_none() {
            shell.env.set(var_name.clone(), value);
        } else {
            let old = shell.env.get(var_name).map(|s| s.to_owned());
            saved_assignments.push((var_name.clone(), old));
            shell.env.set(var_name.clone(), value);
        }
    }

    let name_word = match &cmd.name {
        Some(w) => w,
        None => return Ok(ExitCode::SUCCESS),
    };

    let raw_name = expand_word(name_word, shell).await?;

    // Alias expansion: if the command name matches an alias, prepend the alias
    // value's words to the argument list.
    let (name, alias_prefix): (String, Vec<String>) =
        if let Some(alias_val) = shell.aliases.get(&raw_name).cloned() {
            let parts: Vec<String> =
                alias_val.split_ascii_whitespace().map(|s| s.to_string()).collect();
            if parts.is_empty() {
                (raw_name, vec![])
            } else {
                (parts[0].clone(), parts[1..].to_vec())
            }
        } else {
            (raw_name, vec![])
        };

    let mut args = vec![name.clone()];
    args.extend(alias_prefix);
    args.extend(expand_words(&cmd.args, shell).await?);

    // Apply redirections to the I/O bundle.
    let (io, deferred) = apply_redirections(&cmd.redirects, io, shell).await?;

    let code = dispatch(shell, &name, args, io).await;

    // Flush deferred output-redirect buffers to VFS.
    for dw in deferred {
        let data = if dw.append {
            let existing = shell.fs.lock().await.read_file(&dw.path).await.unwrap_or_default();
            let mut combined = existing;
            combined.extend_from_slice(&dw.buf.bytes());
            combined
        } else {
            dw.buf.bytes()
        };
        shell.fs.lock().await.write_file(&dw.path, data).await?;
    }

    // Restore assignment-scoped variables (after dispatch may have propagated
    // env back, so program mutations to *other* vars are preserved).
    for (var_name, old_val) in saved_assignments {
        match old_val {
            Some(v) => shell.env.set(var_name, v),
            None => shell.env.unset(&var_name),
        }
    }

    code
}

// ── Redirections ──────────────────────────────────────────────────────────────

struct DeferredWrite {
    path: String,
    buf: VecWriter,
    append: bool,
}

async fn apply_redirections(
    redirects: &[Redirect],
    mut io: Io,
    shell: &mut Shell,
) -> Result<(Io, Vec<DeferredWrite>), ShellError> {
    let mut deferred = Vec::new();

    for redir in redirects {
        // Default fd: 0 for input, 1 for output.
        let fd = redir.fd.unwrap_or(match redir.kind {
            RedirectKind::Read => 0,
            _ => 1,
        });

        match (&redir.kind, &redir.target) {
            // ── Input redirections ────────────────────────────────────────────
            (RedirectKind::Read, RedirectTarget::Word(w)) => {
                let expanded = expand_word(w, shell).await?;
                let path = resolve_path(&shell.cwd, &expanded);
                let data = shell.fs.lock().await.read_file(&path).await?;
                io.stdin = BytesReader::new(data);
            }
            (RedirectKind::Read, RedirectTarget::Heredoc(hd)) => {
                let body = expand_word(&Word::Heredoc(hd.clone()), shell).await?;
                io.stdin = BytesReader::new(body.into_bytes());
            }

            // ── Output redirections ───────────────────────────────────────────
            (
                RedirectKind::Write | RedirectKind::Clobber | RedirectKind::Append,
                RedirectTarget::Word(w),
            ) => {
                let expanded = expand_word(w, shell).await?;
                let path = resolve_path(&shell.cwd, &expanded);
                let append = matches!(redir.kind, RedirectKind::Append);
                let buf = VecWriter::new();
                match fd {
                    1 => io.stdout = buf.clone(),
                    2 => io.stderr = buf.clone(),
                    _ => {}
                }
                deferred.push(DeferredWrite { path, buf, append });
            }

            // &> file — both stdout and stderr to the same file.
            (RedirectKind::ReadWrite, RedirectTarget::Word(w)) => {
                let expanded = expand_word(w, shell).await?;
                let path = resolve_path(&shell.cwd, &expanded);
                let buf = VecWriter::new();
                io.stdout = buf.clone();
                io.stderr = buf.clone();
                deferred.push(DeferredWrite { path, buf, append: false });
            }

            // ── Fd duplication (2>&1 etc.) ────────────────────────────────────
            (_, RedirectTarget::Fd(target_fd)) => match (fd, target_fd) {
                (2, 1) => io.stderr = io.stdout.clone(),
                (1, 2) => io.stdout = io.stderr.clone(),
                _ => {}
            },

            // >&- (close fd) — ignore for now.
            (_, RedirectTarget::CloseFd) => {}

            _ => {}
        }
    }

    Ok((io, deferred))
}

// ── Program dispatch ──────────────────────────────────────────────────────────

/// Handle built-ins that need direct shell access (exit, source, xargs, which, type).
/// Returns `Some(result)` if handled, `None` if the name is not a shell-intrinsic.
async fn dispatch_builtin(
    shell: &mut Shell,
    name: &str,
    args: &[String],
    mut io: Io,
) -> Option<Result<ExitCode, ShellError>> {
    match name {
        // ── exit ─────────────────────────────────────────────────────────────
        "exit" => {
            let code = args.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            Some(Err(ShellError::Exit(code)))
        }

        // ── source / . ───────────────────────────────────────────────────────
        "source" | "." => {
            let path = match args.get(1) {
                Some(p) => p.clone(),
                None => {
                    io.stderr.write_all(b"source: filename required\n").await.ok();
                    return Some(Ok(ExitCode::FAILURE));
                }
            };
            let resolved = resolve_path(&shell.cwd, &path);
            let data = match shell.fs.lock().await.read_file(&resolved).await {
                Ok(d) => d,
                Err(e) => {
                    let msg = format!("source: {}: {}\n", path, e);
                    io.stderr.write_all(msg.as_bytes()).await.ok();
                    return Some(Ok(ExitCode::FAILURE));
                }
            };
            let src = match std::str::from_utf8(&data) {
                Ok(s) => s.to_string(),
                Err(_) => {
                    io.stderr.write_all(b"source: file is not valid UTF-8\n").await.ok();
                    return Some(Ok(ExitCode::FAILURE));
                }
            };
            let script = match bash_parser::parse(&src) {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("source: {}\n", e);
                    io.stderr.write_all(msg.as_bytes()).await.ok();
                    return Some(Ok(ExitCode::FAILURE));
                }
            };
            Some(execute_script(shell, &script, io).await)
        }

        // ── xargs ────────────────────────────────────────────────────────────
        "xargs" => {
            let mut buf = Vec::new();
            io.stdin.read_to_end(&mut buf).await.ok();
            let text = String::from_utf8_lossy(&buf);
            let stdin_tokens: Vec<String> =
                text.split_whitespace().map(|s| s.to_string()).collect();

            if stdin_tokens.is_empty() {
                return Some(Ok(ExitCode::SUCCESS));
            }

            let cmd_name = args.get(1).map(|s| s.as_str()).unwrap_or("echo");
            let mut cmd_args = vec![cmd_name.to_string()];
            cmd_args.extend(args[2.min(args.len())..].iter().cloned());
            cmd_args.extend(stdin_tokens);

            Some(dispatch(shell, cmd_name, cmd_args, io).await)
        }

        // ── which ─────────────────────────────────────────────────────────────
        "which" => {
            if args.len() < 2 {
                io.stderr.write_all(b"which: missing argument\n").await.ok();
                return Some(Ok(ExitCode::FAILURE));
            }
            let path_var = shell.path_var().to_string();
            let mut code = ExitCode::SUCCESS;
            for name_arg in &args[1..] {
                match shell.registry.find_path(name_arg, &path_var) {
                    Some(p) => {
                        let msg = format!("{}\n", p);
                        io.stdout.write_all(msg.as_bytes()).await.ok();
                    }
                    None => {
                        let msg = format!("{}: not found\n", name_arg);
                        io.stderr.write_all(msg.as_bytes()).await.ok();
                        code = ExitCode::FAILURE;
                    }
                }
            }
            Some(Ok(code))
        }

        // ── type ──────────────────────────────────────────────────────────────
        "type" => {
            if args.len() < 2 {
                io.stderr.write_all(b"type: missing argument\n").await.ok();
                return Some(Ok(ExitCode::FAILURE));
            }
            const SHELL_INTRINSICS: &[&str] =
                &["exit", "source", ".", "xargs", "which", "type"];
            let path_var = shell.path_var().to_string();
            for name_arg in &args[1..] {
                let line = if let Some(val) = shell.aliases.get(name_arg.as_str()) {
                    format!("{} is aliased to `{}'\n", name_arg, val)
                } else if SHELL_INTRINSICS.contains(&name_arg.as_str()) {
                    format!("{} is a shell builtin\n", name_arg)
                } else if let Some(p) = shell.registry.find_path(name_arg, &path_var) {
                    format!("{} is {}\n", name_arg, p)
                } else {
                    format!("{}: not found\n", name_arg)
                };
                io.stdout.write_all(line.as_bytes()).await.ok();
            }
            Some(Ok(ExitCode::SUCCESS))
        }

        _ => None,
    }
}

fn dispatch<'a>(
    shell: &'a mut Shell,
    name: &'a str,
    args: Vec<String>,
    io: Io,
) -> Pin<Box<dyn Future<Output = Result<ExitCode, ShellError>> + 'a>> {
    Box::pin(async move {
        // Shell-intrinsic built-ins (need direct shell access).
        if let Some(result) = dispatch_builtin(shell, name, &args, io.share()).await {
            return result;
        }

        // PATH lookup in the registry.
        let path_var = shell.path_var().to_string();
        if let Some(cb) = shell.registry.resolve(name, &path_var) {
            // Share env+cwd+aliases with the callback so mutations propagate back.
            let shared = Arc::new(Mutex::new(CtxShared {
                env: shell.env.clone(),
                cwd: shell.cwd.clone(),
                aliases: shell.aliases.clone(),
            }));
            let ctx = ProgramContext::new_shared(args, shared.clone(), shell.fs.clone(), io);
            let code = cb(ctx).await?;
            // Propagate mutations back to the shell.
            let m = shared.lock().unwrap();
            shell.env = m.env.clone();
            shell.cwd = m.cwd.clone();
            shell.aliases = m.aliases.clone();
            return Ok(code);
        }

        // command not found
        // If the name looks like a path (contains '/'), give a POSIX-style error
        // distinguishing "no such file" from "not executable" (not in registry).
        if name.contains('/') {
            let resolved = crate::vfs::resolve_path(&shell.cwd, name);
            let exists = shell.fs.lock().await.stat(&resolved).await.is_ok();
            let msg = if exists {
                format!("wasm_shell: {}: Permission denied\n", name)
            } else {
                format!("wasm_shell: {}: No such file or directory\n", name)
            };
            io.stderr.clone().write_all(msg.as_bytes()).await.ok();
            return Ok(ExitCode(126));
        }
        let msg = format!("{}: command not found\n", name);
        io.stderr.clone().write_all(msg.as_bytes()).await.ok();
        Ok(ExitCode(127))
    })
}
