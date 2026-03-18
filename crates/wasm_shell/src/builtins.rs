use std::pin::Pin;

use regex::{Regex, RegexBuilder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::ShellError;
use crate::shell::{ExitCode, ProgramContext, Shell};
use crate::vfs::resolve_path;

// ── Registration ──────────────────────────────────────────────────────────────

/// Register all built-in commands into the shell's program registry.
pub(crate) fn register(shell: &mut Shell) {
    // Filesystem
    shell.add_program("/usr/bin/cat",    |ctx| builtin_cat(ctx));
    shell.add_program("/usr/bin/ls",     |ctx| builtin_ls(ctx));
    shell.add_program("/usr/bin/mkdir",  |ctx| builtin_mkdir(ctx));
    shell.add_program("/usr/bin/rm",     |ctx| builtin_rm(ctx));
    shell.add_program("/usr/bin/mv",     |ctx| builtin_mv(ctx));
    shell.add_program("/usr/bin/cp",     |ctx| builtin_cp(ctx));
    shell.add_program("/usr/bin/touch",  |ctx| builtin_touch(ctx));
    shell.add_program("/usr/bin/find",   |ctx| builtin_find(ctx));
    // Text processing
    shell.add_program("/usr/bin/head",   |ctx| builtin_head(ctx));
    shell.add_program("/usr/bin/tail",   |ctx| builtin_tail(ctx));
    shell.add_program("/usr/bin/wc",     |ctx| builtin_wc(ctx));
    shell.add_program("/usr/bin/sort",   |ctx| builtin_sort(ctx));
    shell.add_program("/usr/bin/uniq",   |ctx| builtin_uniq(ctx));
    shell.add_program("/usr/bin/cut",    |ctx| builtin_cut(ctx));
    shell.add_program("/usr/bin/tr",     |ctx| builtin_tr(ctx));
    shell.add_program("/usr/bin/grep",   |ctx| builtin_grep(ctx));
    shell.add_program("/usr/bin/sed",    |ctx| builtin_sed(ctx));
    // Shell environment
    shell.add_program("/usr/bin/cd",     |ctx| builtin_cd(ctx));
    shell.add_program("/usr/bin/pwd",    |ctx| builtin_pwd(ctx));
    shell.add_program("/usr/bin/env",    |ctx| builtin_env(ctx));
    shell.add_program("/usr/bin/export", |ctx| builtin_export(ctx));
    shell.add_program("/usr/bin/unset",  |ctx| builtin_unset(ctx));
    // Utility
    shell.add_program("/usr/bin/echo",   |ctx| builtin_echo(ctx));
    shell.add_program("/usr/bin/printf", |ctx| builtin_printf(ctx));
    shell.add_program("/usr/bin/true",   |_|   async { Ok(ExitCode::SUCCESS) });
    shell.add_program("/usr/bin/false",  |_|   async { Ok(ExitCode::FAILURE) });
    shell.add_program("/usr/bin/test",   |ctx| builtin_test(ctx));
    shell.add_program("/usr/bin/sleep",  |ctx| builtin_sleep(ctx));
    shell.add_program("/usr/bin/read",   |ctx| builtin_read(ctx));
    shell.add_program("/usr/bin/awk",    |ctx| builtin_awk(ctx));
    shell.add_program("/usr/bin/wait",   |_|   async { Ok(ExitCode::SUCCESS) });
    shell.add_program("/usr/bin/kill",   |ctx| builtin_kill(ctx));
    // Alias management
    shell.add_program("/usr/bin/alias",   |ctx| builtin_alias(ctx));
    shell.add_program("/usr/bin/unalias", |ctx| builtin_unalias(ctx));
    // System info
    shell.add_program("/usr/bin/uname",    |ctx| builtin_uname(ctx));
    shell.add_program("/usr/bin/neofetch", |ctx| builtin_neofetch(ctx));
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Interpret `\n`, `\t`, `\\`, `\r`, `\a`, `\b`, `\e`, `\f`, `\v` in a string.
fn interpret_escapes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('n')  => result.push('\n'),
            Some('t')  => result.push('\t'),
            Some('r')  => result.push('\r'),
            Some('\\') => result.push('\\'),
            Some('a')  => result.push('\x07'),
            Some('b')  => result.push('\x08'),
            Some('e') | Some('E') => result.push('\x1b'),
            Some('f')  => result.push('\x0c'),
            Some('v')  => result.push('\x0b'),
            Some('0')  => result.push('\0'),
            Some(other) => { result.push('\\'); result.push(other); }
            None => result.push('\\'),
        }
    }
    result
}

/// Expand a character set for `tr`, supporting `a-z` ranges and `\n`, `\t`.
fn expand_charset(s: &str) -> Vec<char> {
    let mut result = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                'n' => result.push('\n'),
                't' => result.push('\t'),
                '\\' => result.push('\\'),
                c => result.push(c),
            }
            i += 2;
        } else if i + 2 < chars.len() && chars[i + 1] == '-' {
            let start = chars[i] as u32;
            let end = chars[i + 2] as u32;
            for cp in start..=end {
                if let Some(ch) = char::from_u32(cp) {
                    result.push(ch);
                }
            }
            i += 3;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    result
}

/// Parse a field spec like `1`, `2-4`, `1,3`, `2-`, `-3` into a list of
/// 0-based field indices (or `None` = "to end").
fn parse_fields(spec: &str) -> Vec<(usize, Option<usize>)> {
    let mut ranges = Vec::new();
    for part in spec.split(',') {
        if let Some(dash) = part.find('-') {
            let lo: usize = part[..dash].parse::<usize>().unwrap_or(1).saturating_sub(1);
            let hi: Option<usize> = if dash + 1 < part.len() {
                part[dash + 1..].parse().ok().map(|n: usize| n.saturating_sub(1))
            } else {
                None
            };
            ranges.push((lo, hi));
        } else {
            let n: usize = part.parse::<usize>().unwrap_or(1).saturating_sub(1);
            ranges.push((n, Some(n)));
        }
    }
    ranges
}

fn field_in_range(idx: usize, ranges: &[(usize, Option<usize>)]) -> bool {
    ranges.iter().any(|&(lo, hi)| match hi {
        Some(hi) => idx >= lo && idx <= hi,
        None => idx >= lo,
    })
}

/// A simple glob matcher supporting `*` and `?`.
fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_bytes(pat: &[u8], text: &[u8]) -> bool {
    match (pat.first(), text.first()) {
        (None, None) => true,
        (None, _) => false,
        (Some(&b'*'), _) => {
            glob_match_bytes(&pat[1..], text)
                || (!text.is_empty() && glob_match_bytes(pat, &text[1..]))
        }
        (Some(&b'?'), Some(_)) => glob_match_bytes(&pat[1..], &text[1..]),
        (Some(&b'?'), None) => false,
        (Some(p), Some(t)) => p == t && glob_match_bytes(&pat[1..], &text[1..]),
        (Some(_), None) => false,
    }
}

/// Parse a number for `test` comparisons.
fn parse_num(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
}

/// Read all stdin bytes.
async fn read_stdin(ctx: &mut ProgramContext) -> Vec<u8> {
    let mut buf = Vec::new();
    ctx.stdin().read_to_end(&mut buf).await.ok();
    buf
}

/// Collect input: read all named files into lines (or stdin if none).
async fn collect_lines(ctx: &mut ProgramContext, paths: &[String]) -> (Vec<String>, ExitCode) {
    let mut lines = Vec::new();
    let mut code = ExitCode::SUCCESS;
    if paths.is_empty() {
        let data = read_stdin(ctx).await;
        let text = String::from_utf8_lossy(&data);
        for line in text.lines() {
            lines.push(line.to_string());
        }
    } else {
        for path in paths {
            match ctx.read_file(path).await {
                Ok(data) => {
                    let text = String::from_utf8_lossy(&data);
                    for line in text.lines() {
                        lines.push(line.to_string());
                    }
                }
                Err(e) => {
                    let msg = format!("{}: {}: {}\n", "cat", path, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                }
            }
        }
    }
    (lines, code)
}

/// Recursively walk a directory, returning all paths (relative to `root`).
async fn walk_dir(ctx: &ProgramContext, root: &str) -> Vec<String> {
    let mut result = Vec::new();
    walk_dir_inner(ctx, root, root, &mut result).await;
    result
}

fn walk_dir_inner<'a>(
    ctx: &'a ProgramContext,
    base: &'a str,
    dir: &'a str,
    out: &'a mut Vec<String>,
) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let entries = match ctx.list_dir(dir).await {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut sorted = entries;
        sorted.sort();
        for entry in sorted {
            let full = if dir == base {
                format!("{}/{}", dir, entry)
            } else {
                format!("{}/{}", dir, entry)
            };
            out.push(full.clone());
            if let Ok(s) = ctx.stat(&full).await {
                if s.is_dir {
                    walk_dir_inner(ctx, base, &full, out).await;
                }
            }
        }
    })
}

// ── cat ───────────────────────────────────────────────────────────────────────

async fn builtin_cat(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut number_lines = false;
    let mut paths = Vec::new();
    for arg in ctx.args.iter().skip(1) {
        match arg.as_str() {
            "-n" => number_lines = true,
            a if a.starts_with('-') => {
                let msg = format!("cat: unknown option: {}\n", a);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                return Ok(ExitCode::FAILURE);
            }
            _ => paths.push(arg.clone()),
        }
    }

    let mut code = ExitCode::SUCCESS;
    let mut line_num: usize = 1;

    let sources: Vec<Option<String>> = if paths.is_empty() {
        vec![None]
    } else {
        paths.iter().map(|p| Some(p.clone())).collect()
    };

    for source in sources {
        let data = match source {
            None => read_stdin(&mut ctx).await,
            Some(ref path) => match ctx.read_file(path).await {
                Ok(d) => d,
                Err(e) => {
                    let msg = format!("cat: {}: {}\n", path, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                    continue;
                }
            },
        };

        if number_lines {
            let text = String::from_utf8_lossy(&data);
            for line in text.lines() {
                let out = format!("{:6}\t{}\n", line_num, line);
                ctx.stdout().write_all(out.as_bytes()).await.ok();
                line_num += 1;
            }
        } else {
            ctx.stdout().write_all(&data).await.ok();
        }
    }
    Ok(code)
}

// ── ls ────────────────────────────────────────────────────────────────────────

async fn builtin_ls(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut long = false;
    let mut all = false;
    let mut recursive = false;
    let mut paths: Vec<String> = Vec::new();

    for arg in ctx.args.iter().skip(1) {
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            for c in arg.chars().skip(1) {
                match c {
                    'l' => long = true,
                    'a' => all = true,
                    'R' => recursive = true,
                    _ => {
                        let msg = format!("ls: invalid option -- '{}'\n", c);
                        ctx.stderr().write_all(msg.as_bytes()).await.ok();
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
        } else {
            paths.push(arg.clone());
        }
    }
    if paths.is_empty() {
        paths.push(ctx.cwd());
    }

    let mut code = ExitCode::SUCCESS;
    let multi = paths.len() > 1;

    for path in &paths {
        match ctx.stat(path).await {
            Ok(s) if s.is_dir => {
                if multi || recursive {
                    let header = format!("{}:\n", path);
                    ctx.stdout().write_all(header.as_bytes()).await.ok();
                }
                if let Err(e) = list_dir_recursive(&ctx, path, long, all, recursive, multi).await {
                    let msg = format!("ls: {}: {}\n", path, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                }
            }
            Ok(_) => {
                // Single file
                let entry_line = ls_format_entry(&ctx, path, long).await;
                ctx.stdout().write_all(entry_line.as_bytes()).await.ok();
            }
            Err(e) => {
                let msg = format!("ls: {}: {}\n", path, e);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                code = ExitCode::FAILURE;
            }
        }
    }
    Ok(code)
}

fn list_dir_recursive<'a>(
    ctx: &'a ProgramContext,
    dir: &'a str,
    long: bool,
    all: bool,
    recursive: bool,
    print_header: bool,
) -> Pin<Box<dyn std::future::Future<Output = Result<(), crate::error::VfsError>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = ctx.list_dir(dir).await?;
        entries.sort();
        let mut subdirs = Vec::new();
        for entry in &entries {
            if !all && entry.starts_with('.') {
                continue;
            }
            let full = format!("{}/{}", dir, entry);
            let line = ls_format_entry(ctx, &full, long).await;
            ctx.stdout().write_all(line.as_bytes()).await.ok();
            if recursive {
                if let Ok(s) = ctx.stat(&full).await {
                    if s.is_dir {
                        subdirs.push(full);
                    }
                }
            }
        }
        for sub in subdirs {
            ctx.stdout().write_all(b"\n").await.ok();
            let header = format!("{}:\n", sub);
            ctx.stdout().write_all(header.as_bytes()).await.ok();
            list_dir_recursive(ctx, &sub, long, all, recursive, print_header).await?;
        }
        Ok(())
    })
}

async fn ls_format_entry(ctx: &ProgramContext, path: &str, long: bool) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if long {
        match ctx.stat(path).await {
            Ok(s) => {
                let type_char = if s.is_dir { 'd' } else if s.is_device { 'c' } else { '-' };
                format!("{}rwxr-xr-x  {:>8}  {}\n", type_char, s.size, name)
            }
            Err(_) => format!("?---------  {:>8}  {}\n", 0, name),
        }
    } else {
        format!("{}\n", name)
    }
}

// ── mkdir ─────────────────────────────────────────────────────────────────────

async fn builtin_mkdir(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut parents = false;
    let mut paths = Vec::new();
    for arg in ctx.args.iter().skip(1) {
        match arg.as_str() {
            "-p" => parents = true,
            a if a.starts_with('-') => {
                let msg = format!("mkdir: invalid option: {}\n", a);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                return Ok(ExitCode::FAILURE);
            }
            _ => paths.push(arg.clone()),
        }
    }
    if paths.is_empty() {
        ctx.stderr().write_all(b"mkdir: missing operand\n").await.ok();
        return Ok(ExitCode::FAILURE);
    }
    let mut code = ExitCode::SUCCESS;
    for path in &paths {
        if let Err(e) = ctx.mkdir(path, parents).await {
            let msg = format!("mkdir: {}: {}\n", path, e);
            ctx.stderr().write_all(msg.as_bytes()).await.ok();
            code = ExitCode::FAILURE;
        }
    }
    Ok(code)
}

// ── rm ────────────────────────────────────────────────────────────────────────

async fn builtin_rm(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut recursive = false;
    let mut force = false;
    let mut paths = Vec::new();
    for arg in ctx.args.iter().skip(1) {
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            for c in arg.chars().skip(1) {
                match c {
                    'r' | 'R' => recursive = true,
                    'f' => force = true,
                    _ => {
                        let msg = format!("rm: invalid option -- '{}'\n", c);
                        ctx.stderr().write_all(msg.as_bytes()).await.ok();
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
        } else {
            paths.push(arg.clone());
        }
    }
    if paths.is_empty() {
        ctx.stderr().write_all(b"rm: missing operand\n").await.ok();
        return Ok(if force { ExitCode::SUCCESS } else { ExitCode::FAILURE });
    }
    let mut code = ExitCode::SUCCESS;
    for path in &paths {
        match ctx.remove(path, recursive).await {
            Ok(()) => {}
            Err(e) => {
                if !force {
                    let msg = format!("rm: {}: {}\n", path, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                }
            }
        }
    }
    Ok(code)
}

// ── mv ────────────────────────────────────────────────────────────────────────

async fn builtin_mv(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let args: Vec<&str> = ctx.args.iter().skip(1).map(|s| s.as_str()).collect();
    if args.len() < 2 {
        ctx.stderr().write_all(b"mv: missing operand\n").await.ok();
        return Ok(ExitCode::FAILURE);
    }
    let (srcs, dst) = args.split_at(args.len() - 1);
    let dst = dst[0];

    // If dst is a directory, move each src into it.
    let dst_is_dir = ctx.stat(dst).await.map(|s| s.is_dir).unwrap_or(false);
    let mut code = ExitCode::SUCCESS;
    for src in srcs {
        let target = if dst_is_dir {
            let name = src.rsplit('/').next().unwrap_or(src);
            format!("{}/{}", dst, name)
        } else {
            dst.to_string()
        };
        if let Err(e) = ctx.rename(src, &target).await {
            let msg = format!("mv: {}: {}\n", src, e);
            ctx.stderr().write_all(msg.as_bytes()).await.ok();
            code = ExitCode::FAILURE;
        }
    }
    Ok(code)
}

// ── cp ────────────────────────────────────────────────────────────────────────

async fn builtin_cp(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut recursive = false;
    let mut rest: Vec<String> = Vec::new();
    for arg in ctx.args.iter().skip(1) {
        match arg.as_str() {
            "-r" | "-R" => recursive = true,
            a if a.starts_with('-') && a.len() > 1 => {
                for c in a.chars().skip(1) {
                    match c {
                        'r' | 'R' => recursive = true,
                        _ => {
                            let msg = format!("cp: invalid option -- '{}'\n", c);
                            ctx.stderr().write_all(msg.as_bytes()).await.ok();
                            return Ok(ExitCode::FAILURE);
                        }
                    }
                }
            }
            _ => rest.push(arg.clone()),
        }
    }
    if rest.len() < 2 {
        ctx.stderr().write_all(b"cp: missing operand\n").await.ok();
        return Ok(ExitCode::FAILURE);
    }
    let (srcs, dst_slice) = rest.split_at(rest.len() - 1);
    let dst = &dst_slice[0];

    let dst_is_dir = ctx.stat(dst).await.map(|s| s.is_dir).unwrap_or(false);
    let mut code = ExitCode::SUCCESS;

    for src in srcs {
        let target = if dst_is_dir {
            let name = src.rsplit('/').next().unwrap_or(src.as_str());
            format!("{}/{}", dst, name)
        } else {
            dst.clone()
        };

        let src_is_dir = ctx.stat(src).await.map(|s| s.is_dir).unwrap_or(false);
        if src_is_dir {
            if !recursive {
                let msg = format!("cp: {}: is a directory (use -r)\n", src);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                code = ExitCode::FAILURE;
                continue;
            }
            if let Err(e) = cp_recursive(&ctx, src, &target).await {
                let msg = format!("cp: {}: {}\n", src, e);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                code = ExitCode::FAILURE;
            }
        } else if let Err(e) = ctx.copy(src, &target).await {
            let msg = format!("cp: {}: {}\n", src, e);
            ctx.stderr().write_all(msg.as_bytes()).await.ok();
            code = ExitCode::FAILURE;
        }
    }
    Ok(code)
}

fn cp_recursive<'a>(
    ctx: &'a ProgramContext,
    src: &'a str,
    dst: &'a str,
) -> Pin<Box<dyn std::future::Future<Output = Result<(), crate::error::VfsError>> + Send + 'a>> {
    Box::pin(async move {
        ctx.mkdir(dst, true).await.ok();
        let entries = ctx.list_dir(src).await?;
        for entry in entries {
            let src_child = format!("{}/{}", src, entry);
            let dst_child = format!("{}/{}", dst, entry);
            let s = ctx.stat(&src_child).await?;
            if s.is_dir {
                cp_recursive(ctx, &src_child, &dst_child).await?;
            } else {
                ctx.copy(&src_child, &dst_child).await?;
            }
        }
        Ok(())
    })
}

// ── touch ─────────────────────────────────────────────────────────────────────

async fn builtin_touch(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let paths: Vec<&str> = ctx.args.iter().skip(1).map(|s| s.as_str()).collect();
    if paths.is_empty() {
        ctx.stderr().write_all(b"touch: missing operand\n").await.ok();
        return Ok(ExitCode::FAILURE);
    }
    let mut code = ExitCode::SUCCESS;
    for path in paths {
        // Create file if it doesn't exist; if it does exist, no-op (no mtime).
        match ctx.stat(path).await {
            Ok(_) => {} // already exists
            Err(_) => {
                if let Err(e) = ctx.write_file(path, Vec::new()).await {
                    let msg = format!("touch: {}: {}\n", path, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                }
            }
        }
    }
    Ok(code)
}

// ── find ──────────────────────────────────────────────────────────────────────

async fn builtin_find(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    // find [path] [-name pat] [-type f|d] [-maxdepth N]
    let args = &ctx.args[1..];
    let mut root = ctx.cwd();
    let mut name_pat: Option<String> = None;
    let mut type_filter: Option<char> = None;
    let mut maxdepth: Option<usize> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-name" => {
                i += 1;
                name_pat = args.get(i).cloned();
            }
            "-type" => {
                i += 1;
                type_filter = args.get(i).and_then(|s| s.chars().next());
            }
            "-maxdepth" => {
                i += 1;
                maxdepth = args.get(i).and_then(|s| s.parse().ok());
            }
            a if !a.starts_with('-') => root = a.to_string(),
            _ => {}
        }
        i += 1;
    }

    // Resolve root relative to cwd.
    let root = resolve_path(&ctx.cwd(), &root);
    find_recursive(&ctx, &root, &root, 0, maxdepth, &name_pat, type_filter).await;
    Ok(ExitCode::SUCCESS)
}

fn find_recursive<'a>(
    ctx: &'a ProgramContext,
    base: &'a str,
    dir: &'a str,
    depth: usize,
    maxdepth: Option<usize>,
    name_pat: &'a Option<String>,
    type_filter: Option<char>,
) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        // Print the directory itself if it matches.
        let rel = if dir == base {
            ".".to_string()
        } else {
            format!(".{}", &dir[base.len()..])
        };

        let dir_stat = ctx.stat(dir).await;
        let should_print = match (&dir_stat, type_filter) {
            (Ok(s), Some('f')) => s.is_file,
            (Ok(s), Some('d')) => s.is_dir,
            (Ok(_), _) => true,
            _ => false,
        };
        let name_matches = name_pat.as_ref().map_or(true, |pat| {
            let basename = rel.rsplit('/').next().unwrap_or(&rel);
            glob_match(pat, basename)
        });

        if should_print && name_matches {
            ctx.stdout()
                .write_all(format!("{}\n", rel).as_bytes())
                .await
                .ok();
        }

        // Don't recurse if not a dir or depth exceeded.
        if !dir_stat.as_ref().map(|s| s.is_dir).unwrap_or(false) {
            return;
        }
        if maxdepth.map_or(false, |d| depth >= d) {
            return;
        }

        let mut entries = ctx.list_dir(dir).await.unwrap_or_default();
        entries.sort();
        for entry in entries {
            let child = format!("{}/{}", dir, entry);
            find_recursive(ctx, base, &child, depth + 1, maxdepth, name_pat, type_filter).await;
        }
    })
}

// ── head ──────────────────────────────────────────────────────────────────────

async fn builtin_head(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut n = 10usize;
    let mut paths = Vec::new();
    let mut i = 1;
    while i < ctx.args.len() {
        match ctx.args[i].as_str() {
            "-n" => {
                i += 1;
                n = ctx.args.get(i).and_then(|s| s.parse().ok()).unwrap_or(10);
            }
            a if a.starts_with("-n") => {
                n = a[2..].parse().unwrap_or(10);
            }
            a if a.starts_with('-') && a.len() > 1 => {
                let msg = format!("head: unknown flag: {}\n", a);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                return Ok(ExitCode::FAILURE);
            }
            _ => paths.push(ctx.args[i].clone()),
        }
        i += 1;
    }

    let (lines, code) = collect_lines(&mut ctx, &paths).await;
    for line in lines.iter().take(n) {
        ctx.stdout().write_all(format!("{}\n", line).as_bytes()).await.ok();
    }
    Ok(code)
}

// ── tail ──────────────────────────────────────────────────────────────────────

async fn builtin_tail(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut n = 10usize;
    let mut paths = Vec::new();
    let mut i = 1;
    while i < ctx.args.len() {
        match ctx.args[i].as_str() {
            "-n" => {
                i += 1;
                n = ctx.args.get(i).and_then(|s| s.parse().ok()).unwrap_or(10);
            }
            a if a.starts_with("-n") => {
                n = a[2..].parse().unwrap_or(10);
            }
            a if a.starts_with('-') && a.len() > 1 => {
                let msg = format!("tail: unknown flag: {}\n", a);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                return Ok(ExitCode::FAILURE);
            }
            _ => paths.push(ctx.args[i].clone()),
        }
        i += 1;
    }

    let (lines, code) = collect_lines(&mut ctx, &paths).await;
    let start = lines.len().saturating_sub(n);
    for line in &lines[start..] {
        ctx.stdout().write_all(format!("{}\n", line).as_bytes()).await.ok();
    }
    Ok(code)
}

// ── wc ────────────────────────────────────────────────────────────────────────

async fn builtin_wc(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut show_l = false;
    let mut show_w = false;
    let mut show_c = false;
    let mut paths = Vec::new();

    for arg in ctx.args.iter().skip(1) {
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg.chars().skip(1) {
                match c {
                    'l' => show_l = true,
                    'w' => show_w = true,
                    'c' => show_c = true,
                    _ => {
                        let msg = format!("wc: invalid option -- '{}'\n", c);
                        ctx.stderr().write_all(msg.as_bytes()).await.ok();
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
        } else {
            paths.push(arg.clone());
        }
    }
    if !show_l && !show_w && !show_c {
        show_l = true;
        show_w = true;
        show_c = true;
    }

    let sources: Vec<Option<String>> = if paths.is_empty() {
        vec![None]
    } else {
        paths.iter().map(|p| Some(p.clone())).collect()
    };

    let mut code = ExitCode::SUCCESS;
    for source in sources {
        let data = match source {
            None => read_stdin(&mut ctx).await,
            Some(ref path) => match ctx.read_file(path).await {
                Ok(d) => d,
                Err(e) => {
                    let msg = format!("wc: {}: {}\n", path, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                    continue;
                }
            },
        };

        let text = String::from_utf8_lossy(&data);
        let nlines = text.lines().count();
        let nwords = text.split_whitespace().count();
        let nbytes = data.len();

        let name = source.as_deref().unwrap_or("");
        let pad = paths.len() > 1;
        let mut parts = Vec::new();
        if show_l { if pad { parts.push(format!("{:>8}", nlines)); } else { parts.push(format!("{}", nlines)); } }
        if show_w { if pad { parts.push(format!("{:>8}", nwords)); } else { parts.push(format!(" {}", nwords)); } }
        if show_c { if pad { parts.push(format!("{:>8}", nbytes)); } else { parts.push(format!(" {}", nbytes)); } }
        if !name.is_empty() { parts.push(format!(" {}", name)); }

        ctx.stdout().write_all(format!("{}\n", parts.join("")).as_bytes()).await.ok();
    }
    Ok(code)
}

// ── sort ──────────────────────────────────────────────────────────────────────

async fn builtin_sort(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut reverse = false;
    let mut numeric = false;
    let mut unique = false;
    let mut paths = Vec::new();

    for arg in ctx.args.iter().skip(1) {
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg.chars().skip(1) {
                match c {
                    'r' => reverse = true,
                    'n' => numeric = true,
                    'u' => unique = true,
                    _ => {
                        let msg = format!("sort: invalid option -- '{}'\n", c);
                        ctx.stderr().write_all(msg.as_bytes()).await.ok();
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
        } else {
            paths.push(arg.clone());
        }
    }

    let (mut lines, code) = collect_lines(&mut ctx, &paths).await;

    if numeric {
        lines.sort_by(|a, b| parse_num(a).cmp(&parse_num(b)));
    } else {
        lines.sort();
    }
    if reverse {
        lines.reverse();
    }
    if unique {
        lines.dedup();
    }

    for line in &lines {
        ctx.stdout().write_all(format!("{}\n", line).as_bytes()).await.ok();
    }
    Ok(code)
}

// ── uniq ──────────────────────────────────────────────────────────────────────

async fn builtin_uniq(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut count = false;
    let mut dups_only = false;
    let mut paths = Vec::new();

    for arg in ctx.args.iter().skip(1) {
        match arg.as_str() {
            "-c" => count = true,
            "-d" => dups_only = true,
            a if a.starts_with('-') => {
                let msg = format!("uniq: invalid option: {}\n", a);
                ctx.stderr().write_all(msg.as_bytes()).await.ok();
                return Ok(ExitCode::FAILURE);
            }
            _ => paths.push(arg.clone()),
        }
    }

    let (lines, code) = collect_lines(&mut ctx, &paths).await;
    let mut groups: Vec<(String, usize)> = Vec::new();
    for line in lines {
        if groups.last().map(|(l, _)| l == &line).unwrap_or(false) {
            groups.last_mut().unwrap().1 += 1;
        } else {
            groups.push((line, 1));
        }
    }

    for (line, n) in groups {
        if dups_only && n < 2 {
            continue;
        }
        let out = if count {
            format!("{:>7} {}\n", n, line)
        } else {
            format!("{}\n", line)
        };
        ctx.stdout().write_all(out.as_bytes()).await.ok();
    }
    Ok(code)
}

// ── cut ───────────────────────────────────────────────────────────────────────

async fn builtin_cut(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut delim = '\t';
    let mut fields: Option<Vec<(usize, Option<usize>)>> = None;
    let mut chars_mode: Option<Vec<(usize, Option<usize>)>> = None;
    let mut paths = Vec::new();

    let mut i = 1;
    while i < ctx.args.len() {
        match ctx.args[i].as_str() {
            "-d" => {
                i += 1;
                delim = ctx.args.get(i).and_then(|s| s.chars().next()).unwrap_or('\t');
            }
            "-f" => {
                i += 1;
                fields = ctx.args.get(i).map(|s| parse_fields(s));
            }
            "-c" => {
                i += 1;
                chars_mode = ctx.args.get(i).map(|s| parse_fields(s));
            }
            a if a.starts_with("-d") => {
                delim = a.chars().nth(2).unwrap_or('\t');
            }
            a if a.starts_with("-f") => {
                fields = Some(parse_fields(&a[2..]));
            }
            a if a.starts_with("-c") => {
                chars_mode = Some(parse_fields(&a[2..]));
            }
            _ => paths.push(ctx.args[i].clone()),
        }
        i += 1;
    }

    let (lines, code) = collect_lines(&mut ctx, &paths).await;
    for line in &lines {
        let out = if let Some(ref ranges) = chars_mode {
            line.chars()
                .enumerate()
                .filter(|(i, _)| field_in_range(*i, ranges))
                .map(|(_, c)| c)
                .collect::<String>()
        } else if let Some(ref ranges) = fields {
            let parts: Vec<&str> = line.split(delim).collect();
            parts
                .iter()
                .enumerate()
                .filter(|(i, _)| field_in_range(*i, ranges))
                .map(|(_, s)| *s)
                .collect::<Vec<_>>()
                .join(&delim.to_string())
        } else {
            line.clone()
        };
        ctx.stdout().write_all(format!("{}\n", out).as_bytes()).await.ok();
    }
    Ok(code)
}

// ── tr ────────────────────────────────────────────────────────────────────────

async fn builtin_tr(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut delete = false;
    let mut squeeze = false;
    let mut sets: Vec<String> = Vec::new();

    for arg in ctx.args.iter().skip(1) {
        match arg.as_str() {
            "-d" => delete = true,
            "-s" => squeeze = true,
            a if a.starts_with('-') && a.len() > 1 => {
                for c in a.chars().skip(1) {
                    match c {
                        'd' => delete = true,
                        's' => squeeze = true,
                        _ => {
                            let msg = format!("tr: invalid option -- '{}'\n", c);
                            ctx.stderr().write_all(msg.as_bytes()).await.ok();
                            return Ok(ExitCode::FAILURE);
                        }
                    }
                }
            }
            _ => sets.push(arg.clone()),
        }
    }

    let data = read_stdin(&mut ctx).await;
    let text = String::from_utf8_lossy(&data);

    let result = if delete {
        let del_set = sets.first().map(|s| expand_charset(s)).unwrap_or_default();
        text.chars()
            .filter(|c| !del_set.contains(c))
            .collect::<String>()
    } else if sets.len() >= 2 {
        let set1 = expand_charset(&sets[0]);
        let set2 = expand_charset(&sets[1]);
        text.chars()
            .map(|c| {
                if let Some(i) = set1.iter().position(|&x| x == c) {
                    *set2.get(i).or(set2.last()).unwrap_or(&c)
                } else {
                    c
                }
            })
            .collect::<String>()
    } else {
        text.into_owned()
    };

    // Apply squeeze if requested.
    let result = if squeeze {
        let sq_set: Vec<char> = sets.first().map(|s| expand_charset(s)).unwrap_or_default();
        let mut out = String::new();
        let mut prev: Option<char> = None;
        for c in result.chars() {
            if sq_set.contains(&c) && prev == Some(c) {
                continue;
            }
            out.push(c);
            prev = Some(c);
        }
        out
    } else {
        result
    };

    ctx.stdout().write_all(result.as_bytes()).await.ok();
    Ok(ExitCode::SUCCESS)
}

// ── grep ──────────────────────────────────────────────────────────────────────

async fn builtin_grep(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut recursive = false;
    let mut case_insensitive = false;
    let mut show_line_numbers = false;
    let mut files_only = false;
    let mut invert = false;
    let mut pattern: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();

    let mut i = 1;
    while i < ctx.args.len() {
        let a = &ctx.args[i];
        if a.starts_with('-') && a.len() > 1 && !a.starts_with("--") {
            for c in a.chars().skip(1) {
                match c {
                    'r' | 'R' => recursive = true,
                    'i' => case_insensitive = true,
                    'n' => show_line_numbers = true,
                    'l' => files_only = true,
                    'v' => invert = true,
                    'E' => {} // extended regex — we always use it
                    'e' => {
                        i += 1;
                        pattern = ctx.args.get(i).cloned();
                    }
                    _ => {
                        let msg = format!("grep: invalid option -- '{}'\n", c);
                        ctx.stderr().write_all(msg.as_bytes()).await.ok();
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
        } else if pattern.is_none() {
            pattern = Some(a.clone());
        } else {
            paths.push(a.clone());
        }
        i += 1;
    }

    let pattern = match pattern {
        Some(p) => p,
        None => {
            ctx.stderr().write_all(b"grep: missing pattern\n").await.ok();
            return Ok(ExitCode::FAILURE);
        }
    };

    let re = match RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("grep: invalid regex: {}\n", e);
            ctx.stderr().write_all(msg.as_bytes()).await.ok();
            return Ok(ExitCode::FAILURE);
        }
    };

    // Expand recursive paths.
    let mut file_paths: Vec<String> = Vec::new();
    if paths.is_empty() {
        if recursive {
            // `grep -r pattern` with no paths defaults to `.` like real grep.
            let walked = walk_dir(&ctx, &ctx.cwd()).await;
            file_paths.extend(walked);
        } else {
            file_paths.push(String::new()); // stdin sentinel
        }
    } else {
        for path in &paths {
            if recursive {
                let s = ctx.stat(path).await;
                if s.as_ref().map(|s| s.is_dir).unwrap_or(false) {
                    let walked = walk_dir(&ctx, path).await;
                    file_paths.extend(walked);
                } else {
                    file_paths.push(path.clone());
                }
            } else {
                file_paths.push(path.clone());
            }
        }
    }

    let multi = file_paths.len() > 1;
    let mut matched = false;

    for fp in &file_paths {
        let data = if fp.is_empty() {
            read_stdin(&mut ctx).await
        } else {
            match ctx.read_file(fp).await {
                Ok(d) => d,
                Err(e) => {
                    let msg = format!("grep: {}: {}\n", fp, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    continue;
                }
            }
        };

        let text = String::from_utf8_lossy(&data);
        let mut file_matched = false;

        for (ln, line) in text.lines().enumerate() {
            let is_match = re.is_match(line);
            let show = if invert { !is_match } else { is_match };
            if show {
                matched = true;
                file_matched = true;
                if !files_only {
                    let mut out = String::new();
                    if multi && !fp.is_empty() {
                        out.push_str(fp);
                        out.push(':');
                    }
                    if show_line_numbers {
                        out.push_str(&format!("{}:", ln + 1));
                    }
                    out.push_str(line);
                    out.push('\n');
                    ctx.stdout().write_all(out.as_bytes()).await.ok();
                }
            }
        }

        if files_only && file_matched && !fp.is_empty() {
            ctx.stdout().write_all(format!("{}\n", fp).as_bytes()).await.ok();
        }
    }

    Ok(if matched { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

// ── sed ───────────────────────────────────────────────────────────────────────

enum SedAddr {
    Line(usize),
    Regex(Regex),
}

enum SedOp {
    Substitute { re: Regex, repl: String, global: bool, print_after: bool },
    Delete,
    Print,
    Quit,
}

struct SedCmd {
    addr: Option<SedAddr>,
    negate: bool,
    op: SedOp,
}

fn parse_sed_commands(script: &str) -> Result<Vec<SedCmd>, String> {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    let mut cmds = Vec::new();

    while i < chars.len() {
        // Skip whitespace/semicolons between commands.
        while i < chars.len() && matches!(chars[i], ' ' | '\t' | ';' | '\n') {
            i += 1;
        }
        if i >= chars.len() { break; }

        // Optional address.
        let addr = if chars[i].is_ascii_digit() {
            let mut n = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                n.push(chars[i]);
                i += 1;
            }
            Some(SedAddr::Line(n.parse::<usize>().map_err(|_| "invalid line number")?))
        } else if chars[i] == '/' {
            i += 1;
            let mut pat = String::new();
            while i < chars.len() && chars[i] != '/' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    pat.push('\\');
                }
                pat.push(chars[i]);
                i += 1;
            }
            if i < chars.len() { i += 1; } // closing /
            let re = Regex::new(&pat).map_err(|e| format!("bad regex /{}/: {}", pat, e))?;
            Some(SedAddr::Regex(re))
        } else {
            None
        };

        // Optional negation.
        let negate = if i < chars.len() && chars[i] == '!' { i += 1; true } else { false };

        // Skip optional whitespace before command letter.
        while i < chars.len() && matches!(chars[i], ' ' | '\t') { i += 1; }
        if i >= chars.len() { break; }

        let op = match chars[i] {
            'd' => { i += 1; SedOp::Delete }
            'p' => { i += 1; SedOp::Print }
            'q' => { i += 1; SedOp::Quit }
            's' => {
                i += 1;
                if i >= chars.len() { return Err("s: missing delimiter".into()); }
                let delim = chars[i]; i += 1;

                let read_field = |chars: &[char], pos: &mut usize, delim: char| {
                    let mut field = String::new();
                    while *pos < chars.len() && chars[*pos] != delim {
                        if chars[*pos] == '\\' && *pos + 1 < chars.len() {
                            *pos += 1;
                            if chars[*pos] == delim {
                                field.push(chars[*pos]);
                            } else {
                                field.push('\\');
                                field.push(chars[*pos]);
                            }
                        } else {
                            field.push(chars[*pos]);
                        }
                        *pos += 1;
                    }
                    if *pos < chars.len() { *pos += 1; } // closing delim
                    field
                };

                let pattern  = read_field(&chars, &mut i, delim);
                let repl_raw = read_field(&chars, &mut i, delim);

                let mut global      = false;
                let mut case_i      = false;
                let mut print_after = false;
                while i < chars.len() && !matches!(chars[i], ';' | '\n') {
                    match chars[i] {
                        'g' => global      = true,
                        'i' | 'I' => case_i = true,
                        'p' => print_after = true,
                        ' ' | '\t' => {}
                        _ => {}
                    }
                    i += 1;
                }

                let re = RegexBuilder::new(&pattern)
                    .case_insensitive(case_i)
                    .build()
                    .map_err(|e| format!("invalid regex '{}': {}", pattern, e))?;

                // Convert sed & / \N to regex-crate $0 / $N.
                let repl = repl_raw
                    .replace("\\&", "\x00AMP\x00")
                    .replace('&', "$0")
                    .replace("\x00AMP\x00", "\\&")
                    .replace("\\1", "$1").replace("\\2", "$2")
                    .replace("\\3", "$3").replace("\\4", "$4")
                    .replace("\\5", "$5").replace("\\n", "\n");

                SedOp::Substitute { re, repl, global, print_after }
            }
            c => return Err(format!("unsupported sed command: '{}'", c)),
        };

        cmds.push(SedCmd { addr, negate, op });
    }
    Ok(cmds)
}

/// Apply all sed commands to one line. Returns (lines to emit, should_quit).
fn sed_apply_line(pattern: &str, line_num: usize, cmds: &[SedCmd], suppress: bool) -> (Vec<String>, bool) {
    let mut current = pattern.to_string();
    let mut extra: Vec<String> = Vec::new();
    let mut deleted = false;
    let mut quit = false;

    'cmd: for cmd in cmds {
        let matches = match &cmd.addr {
            None                  => true,
            Some(SedAddr::Line(n)) => line_num == *n,
            Some(SedAddr::Regex(re)) => re.is_match(&current),
        };
        if (matches) == cmd.negate { continue; }

        match &cmd.op {
            SedOp::Delete => { deleted = true; break 'cmd; }
            SedOp::Print  => { extra.push(current.clone()); }
            SedOp::Quit   => { quit = true; }
            SedOp::Substitute { re, repl, global, print_after } => {
                current = if *global {
                    re.replace_all(&current, repl.as_str()).into_owned()
                } else {
                    re.replace(&current, repl.as_str()).into_owned()
                };
                if *print_after { extra.push(current.clone()); }
            }
        }
    }

    let mut out = extra;
    if !deleted && !suppress { out.push(current); }
    (out, quit)
}

async fn builtin_sed(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let args: Vec<String> = ctx.args[1..].to_vec();

    let mut in_place = false;
    let mut suppress = false;
    let mut scripts: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-i"            => in_place = true,
            "-n"            => suppress = true,
            "-e"            => { i += 1; if i < args.len() { scripts.push(args[i].clone()); } }
            a if a.starts_with("-e") => scripts.push(a[2..].to_string()),
            a if a.starts_with('-') && a.len() > 1 => {
                for c in a.chars().skip(1) {
                    match c {
                        'i' => in_place = true,
                        'n' => suppress = true,
                        other => {
                            let msg = format!("sed: invalid option -- '{}'\n", other);
                            ctx.stderr().write_all(msg.as_bytes()).await.ok();
                            return Ok(ExitCode::FAILURE);
                        }
                    }
                }
            }
            _ => {
                if scripts.is_empty() { scripts.push(args[i].clone()); }
                else                  { paths.push(args[i].clone()); }
            }
        }
        i += 1;
    }

    if scripts.is_empty() {
        ctx.stderr().write_all(b"sed: no script\n").await.ok();
        return Ok(ExitCode::FAILURE);
    }

    let full_script = scripts.join("\n");
    let cmds = match parse_sed_commands(&full_script) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("sed: {}\n", e);
            ctx.stderr().write_all(msg.as_bytes()).await.ok();
            return Ok(ExitCode::FAILURE);
        }
    };

    let sources: Vec<Option<String>> = if paths.is_empty() {
        vec![None]
    } else {
        paths.iter().map(|p| Some(p.clone())).collect()
    };

    let mut code = ExitCode::SUCCESS;
    for source in sources {
        let data = match &source {
            None => read_stdin(&mut ctx).await,
            Some(path) => match ctx.read_file(path).await {
                Ok(d) => d,
                Err(e) => {
                    let msg = format!("sed: {}: {}\n", path, e);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                    continue;
                }
            },
        };

        let text = String::from_utf8_lossy(&data);
        let mut out = String::new();
        for (idx, line) in text.lines().enumerate() {
            let (lines, quit) = sed_apply_line(line, idx + 1, &cmds, suppress);
            for l in lines { out.push_str(&l); out.push('\n'); }
            if quit { break; }
        }

        if in_place {
            if let Some(ref path) = source {
                ctx.write_file(path, out.into_bytes()).await.ok();
            }
        } else {
            ctx.stdout().write_all(out.as_bytes()).await.ok();
        }
    }
    Ok(code)
}

// ── cd ────────────────────────────────────────────────────────────────────────

async fn builtin_cd(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let target = ctx
        .args
        .get(1)
        .cloned()
        .unwrap_or_else(|| ctx.get_env("HOME").unwrap_or_else(|| "/".to_string()));

    let new_cwd = resolve_path(&ctx.cwd(), &target);

    // Verify the path exists and is a directory.
    match ctx.stat(&new_cwd).await {
        Ok(s) if s.is_dir => {
            ctx.set_cwd(&new_cwd);
            Ok(ExitCode::SUCCESS)
        }
        Ok(_) => {
            let msg = format!("cd: {}: not a directory\n", target);
            ctx.stderr().write_all(msg.as_bytes()).await.ok();
            Ok(ExitCode::FAILURE)
        }
        Err(e) => {
            let msg = format!("cd: {}: {}\n", target, e);
            ctx.stderr().write_all(msg.as_bytes()).await.ok();
            Ok(ExitCode::FAILURE)
        }
    }
}

// ── pwd ───────────────────────────────────────────────────────────────────────

async fn builtin_pwd(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    ctx.stdout()
        .write_all(format!("{}\n", ctx.cwd()).as_bytes())
        .await
        .ok();
    Ok(ExitCode::SUCCESS)
}

// ── env ───────────────────────────────────────────────────────────────────────

async fn builtin_env(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let env = ctx.env_snapshot();
    let mut pairs = env.to_vec();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in pairs {
        ctx.stdout()
            .write_all(format!("{}={}\n", k, v).as_bytes())
            .await
            .ok();
    }
    Ok(ExitCode::SUCCESS)
}

// ── export ────────────────────────────────────────────────────────────────────

async fn builtin_export(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    if ctx.args.len() < 2 {
        // Print exported variables (same as env in our model).
        return builtin_env(ctx).await;
    }
    for arg in ctx.args.iter().skip(1) {
        if let Some((k, v)) = arg.split_once('=') {
            ctx.set_env(k, v);
        }
        // `export NAME` with no value: no-op if already set, else set to "".
    }
    Ok(ExitCode::SUCCESS)
}

// ── unset ─────────────────────────────────────────────────────────────────────

async fn builtin_unset(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    for arg in ctx.args.iter().skip(1) {
        ctx.unset_env(arg);
    }
    Ok(ExitCode::SUCCESS)
}

// ── echo ──────────────────────────────────────────────────────────────────────

async fn builtin_echo(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut no_newline = false;
    let mut escape = false;
    let mut rest = &ctx.args[1..];

    loop {
        match rest.first().map(|s| s.as_str()) {
            Some("-n") => { no_newline = true; rest = &rest[1..]; }
            Some("-e") => { escape = true; rest = &rest[1..]; }
            Some("-E") => { escape = false; rest = &rest[1..]; }
            Some(a) if a == "-ne" || a == "-en" => {
                no_newline = true; escape = true; rest = &rest[1..];
            }
            _ => break,
        }
    }

    let text = rest.join(" ");
    let out = if escape { interpret_escapes(&text) } else { text };

    ctx.stdout().write_all(out.as_bytes()).await.ok();
    if !no_newline {
        ctx.stdout().write_all(b"\n").await.ok();
    }
    Ok(ExitCode::SUCCESS)
}

// ── printf ────────────────────────────────────────────────────────────────────

async fn builtin_printf(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    if ctx.args.len() < 2 {
        ctx.stderr().write_all(b"printf: missing format string\n").await.ok();
        return Ok(ExitCode::FAILURE);
    }
    let out = format_printf(&ctx.args[1], &ctx.args[2..]);
    ctx.stdout().write_all(out.as_bytes()).await.ok();
    Ok(ExitCode::SUCCESS)
}

fn format_printf(fmt: &str, args: &[String]) -> String {
    let mut result = String::new();
    let mut chars = fmt.chars().peekable();
    let mut idx = 0usize;

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some('0') => {} // null — skip in UTF-8 string
                Some(other) => { result.push('\\'); result.push(other); }
                None => result.push('\\'),
            }
        } else if c == '%' {
            match chars.next() {
                Some('s') => {
                    result.push_str(args.get(idx).map(|s| s.as_str()).unwrap_or(""));
                    idx += 1;
                }
                Some('d') | Some('i') => {
                    let n: i64 = args.get(idx).and_then(|s| s.parse().ok()).unwrap_or(0);
                    result.push_str(&n.to_string());
                    idx += 1;
                }
                Some('f') => {
                    let f: f64 = args.get(idx).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                    result.push_str(&format!("{:.6}", f));
                    idx += 1;
                }
                Some('e') => {
                    let f: f64 = args.get(idx).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                    result.push_str(&format!("{:e}", f));
                    idx += 1;
                }
                Some('c') => {
                    if let Some(s) = args.get(idx) {
                        if let Some(ch) = s.chars().next() { result.push(ch); }
                    }
                    idx += 1;
                }
                Some('x') => {
                    let n: i64 = args.get(idx).and_then(|s| s.parse().ok()).unwrap_or(0);
                    result.push_str(&format!("{:x}", n));
                    idx += 1;
                }
                Some('X') => {
                    let n: i64 = args.get(idx).and_then(|s| s.parse().ok()).unwrap_or(0);
                    result.push_str(&format!("{:X}", n));
                    idx += 1;
                }
                Some('o') => {
                    let n: i64 = args.get(idx).and_then(|s| s.parse().ok()).unwrap_or(0);
                    result.push_str(&format!("{:o}", n));
                    idx += 1;
                }
                Some('b') => {
                    let s = args.get(idx).map(|s| s.as_str()).unwrap_or("");
                    result.push_str(&interpret_escapes(s));
                    idx += 1;
                }
                Some('%') => result.push('%'),
                Some(other) => { result.push('%'); result.push(other); }
                None => result.push('%'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

// ── test ──────────────────────────────────────────────────────────────────────

async fn builtin_test(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    // Strip leading argv[0] and, for `[`, the trailing `]`.
    let mut args: &[String] = &ctx.args[1..];
    if ctx.args[0] == "[" {
        if args.last().map(|s| s.as_str()) == Some("]") {
            args = &args[..args.len() - 1];
        }
    }
    let result = eval_test(args, &ctx).await;
    Ok(if result { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn eval_test<'a>(
    args: &'a [String],
    ctx: &'a ProgramContext,
) -> Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
    Box::pin(async move {
        if args.is_empty() {
            return false;
        }

        // -o has lowest precedence — split at leftmost occurrence.
        if let Some(pos) = args.iter().position(|a| a == "-o") {
            let left = eval_test(&args[..pos], ctx).await;
            let right = eval_test(&args[pos + 1..], ctx).await;
            return left || right;
        }
        // -a has next precedence.
        if let Some(pos) = args.iter().position(|a| a == "-a") {
            let left = eval_test(&args[..pos], ctx).await;
            let right = eval_test(&args[pos + 1..], ctx).await;
            return left && right;
        }

        match args.len() {
            1 => !args[0].is_empty(),
            2 => {
                let operand = &args[1];
                match args[0].as_str() {
                    "!" => !eval_test(&args[1..], ctx).await,
                    "-n" => !operand.is_empty(),
                    "-z" => operand.is_empty(),
                    "-f" => ctx.stat(operand).await.map(|s| s.is_file).unwrap_or(false),
                    "-d" => ctx.stat(operand).await.map(|s| s.is_dir).unwrap_or(false),
                    "-e" => ctx.stat(operand).await.is_ok(),
                    "-r" | "-w" | "-x" => ctx.stat(operand).await.is_ok(),
                    "-s" => ctx.stat(operand).await.map(|s| s.size > 0).unwrap_or(false),
                    _ => !args[0].is_empty(),
                }
            }
            3 => {
                if args[0] == "!" {
                    return !eval_test(&args[1..], ctx).await;
                }
                let (left, op, right) = (&args[0], args[1].as_str(), &args[2]);
                match op {
                    "=" | "==" => left == right,
                    "!=" => left != right,
                    "-eq" => parse_num(left) == parse_num(right),
                    "-ne" => parse_num(left) != parse_num(right),
                    "-lt" => parse_num(left) < parse_num(right),
                    "-gt" => parse_num(left) > parse_num(right),
                    "-le" => parse_num(left) <= parse_num(right),
                    "-ge" => parse_num(left) >= parse_num(right),
                    _ => false,
                }
            }
            _ => {
                if args[0] == "!" {
                    !eval_test(&args[1..], ctx).await
                } else {
                    false
                }
            }
        }
    })
}

// ── sleep ─────────────────────────────────────────────────────────────────────

async fn builtin_sleep(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let secs: f64 = ctx
        .args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let millis = (secs.max(0.0) * 1000.0) as u32;

    #[cfg(target_arch = "wasm32")]
    {
        use std::future::Future;
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use wasm_bindgen::prelude::*;
        use wasm_bindgen_futures::JsFuture;

        // JsFuture is !Send; this wrapper is safe because WASM is single-threaded.
        struct SendFut<F>(F);
        unsafe impl<F: Future> Send for SendFut<F> {}
        impl<F: Future> Future for SendFut<F> {
            type Output = F::Output;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
            }
        }

        let promise = js_sys::Promise::new(&mut |resolve, _| {
            let global = js_sys::global();
            let set_timeout = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
                .unwrap();
            let set_timeout: js_sys::Function = set_timeout.dyn_into().unwrap();
            set_timeout
                .call2(&global, &resolve, &JsValue::from_f64(millis as f64))
                .unwrap();
        });
        SendFut(JsFuture::from(promise)).await.ok();
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let dur = std::time::Duration::from_millis(millis as u64);
        tokio::time::sleep(dur).await;
    }

    Ok(ExitCode::SUCCESS)
}

// ── read ──────────────────────────────────────────────────────────────────────

async fn builtin_read(mut ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let var = ctx.args.get(1).cloned().unwrap_or_else(|| "REPLY".to_string());
    let mut buf = Vec::new();
    ctx.stdin().read_to_end(&mut buf).await.ok();
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next().unwrap_or("").to_string();
    ctx.set_env(&var, &line);
    Ok(ExitCode::SUCCESS)
}

// ── awk ───────────────────────────────────────────────────────────────────────

async fn builtin_awk(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    ctx.stderr()
        .write_all(
            b"awk: not supported -- use grep, sed, cut, or tr as alternatives\n",
        )
        .await
        .ok();
    Ok(ExitCode::FAILURE)
}

// ── kill ──────────────────────────────────────────────────────────────────────

async fn builtin_kill(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    // Stage 6 will implement real job signals. For now, no-op.
    let _ = &ctx.args;
    Ok(ExitCode::SUCCESS)
}

// ── alias ─────────────────────────────────────────────────────────────────────

async fn builtin_alias(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    if ctx.args.len() < 2 {
        // Print all aliases.
        let mut aliases: Vec<(String, String)> = ctx.aliases_snapshot().into_iter().collect();
        aliases.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, val) in aliases {
            ctx.stdout()
                .write_all(format!("alias {}='{}'\n", name, val).as_bytes())
                .await
                .ok();
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut code = ExitCode::SUCCESS;
    for arg in ctx.args.iter().skip(1) {
        if let Some((name, val)) = arg.split_once('=') {
            ctx.set_alias(name, val);
        } else {
            // Print a single alias.
            match ctx.get_alias(arg) {
                Some(val) => {
                    let msg = format!("alias {}='{}'\n", arg, val);
                    ctx.stdout().write_all(msg.as_bytes()).await.ok();
                }
                None => {
                    let msg = format!("alias: {}: not found\n", arg);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                }
            }
        }
    }
    Ok(code)
}

// ── uname ────────────────────────────────────────────────────────────────────

async fn builtin_uname(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    // Flags: -s (kernel name), -n (nodename), -r (release), -v (version),
    //        -m (machine), -p (processor), -a (all)
    let args = &ctx.args[1..];

    // Default (no flags) behaves like -s.
    let all  = args.iter().any(|a| a == "-a");
    let s    = all || args.is_empty() || args.iter().any(|a| a.contains('s'));
    let n    = all || args.iter().any(|a| a.contains('n'));
    let r    = all || args.iter().any(|a| a.contains('r'));
    let v    = all || args.iter().any(|a| a.contains('v'));
    let m    = all || args.iter().any(|a| a.contains('m'));

    let mut parts: Vec<&str> = Vec::new();
    if s { parts.push("wasm_shell");  }
    if n { parts.push("wasm_shell"); }
    if r { parts.push(env!("CARGO_PKG_VERSION")); }
    if v { parts.push("#1"); }
    if m { parts.push("wasm32"); }

    let out = format!("{}
", parts.join(" "));
    ctx.stdout().write_all(out.as_bytes()).await.ok();
    Ok(ExitCode::SUCCESS)
}

// ── neofetch ──────────────────────────────────────────────────────────────────

async fn builtin_neofetch(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let version = env!("CARGO_PKG_VERSION");
    let mut out = format!("OS: wasm_shell {}
", version);

    if let Ok(data) = ctx.read_file("/etc/hostname").await {
        let hostname = String::from_utf8_lossy(&data);
        let hostname = hostname.trim();
        if !hostname.is_empty() {
            out.push_str(&format!("Hostname: {}
", hostname));
        }
    }

    if let Ok(data) = ctx.read_file("/etc/cpu").await {
        let cpu = String::from_utf8_lossy(&data);
        let cpu = cpu.trim();
        if !cpu.is_empty() {
            out.push_str(&format!("CPU: {}
", cpu));
        }
    }

    if let Ok(data) = ctx.read_file("/etc/gpu").await {
        let text = String::from_utf8_lossy(&data);
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() {
                out.push_str(&format!("GPU: {}
", line));
            }
        }
    }

    ctx.stdout().write_all(out.as_bytes()).await.ok();
    Ok(ExitCode::SUCCESS)
}

// ── unalias ───────────────────────────────────────────────────────────────────

async fn builtin_unalias(ctx: ProgramContext) -> Result<ExitCode, ShellError> {
    let mut code = ExitCode::SUCCESS;
    for arg in ctx.args.iter().skip(1) {
        if arg == "-a" {
            // Remove all aliases.
            let names: Vec<String> = ctx.aliases_snapshot().into_keys().collect();
            for name in names {
                ctx.unset_alias(&name);
            }
        } else {
            match ctx.get_alias(arg) {
                Some(_) => ctx.unset_alias(arg),
                None => {
                    let msg = format!("unalias: {}: not found\n", arg);
                    ctx.stderr().write_all(msg.as_bytes()).await.ok();
                    code = ExitCode::FAILURE;
                }
            }
        }
    }
    Ok(code)
}
