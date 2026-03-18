use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::env::EnvMap;
use crate::error::VfsError;
use crate::expand::eval_arith;
use crate::io::Io;
use crate::registry::ProgramRegistry;
use crate::shell::{ExitCode, ProgramContext, Shell};
use crate::vfs::{MountPoint, Stat, Vfs, normalize_path, resolve_path};

// ═════════════════════════════════════════════════════════════════════════════
// Stage 2 — VFS + Program Registry
// ═════════════════════════════════════════════════════════════════════════════

// ── Path utilities ────────────────────────────────────────────────────────────

#[test]
fn normalize_absolute() {
    assert_eq!(normalize_path("/usr/bin/../bin/./foo"), "/usr/bin/foo");
    assert_eq!(normalize_path("/"), "/");
    assert_eq!(normalize_path("///a//b//"), "/a/b");
    assert_eq!(normalize_path("/a/b/c/../../d"), "/a/d");
}

#[test]
fn normalize_root_dotdot() {
    assert_eq!(normalize_path("/../../etc"), "/etc");
}

#[test]
fn resolve_relative() {
    assert_eq!(resolve_path("/home/user", "docs/file.txt"), "/home/user/docs/file.txt");
    assert_eq!(resolve_path("/home/user", "../other"), "/home/other");
    assert_eq!(resolve_path("/home/user", "/etc/passwd"), "/etc/passwd");
}

// ── VFS: basic in-memory operations ──────────────────────────────────────────

#[tokio::test]
async fn vfs_init_dirs_exist() {
    let vfs = Vfs::new();
    for path in &["/usr/bin", "/bin", "/tmp", "/home", "/dev", "/usr/local/bin"] {
        let s = vfs.stat(path).await.unwrap_or_else(|_| panic!("{path} should exist"));
        assert!(s.is_dir, "{path} should be a dir");
    }
}

#[tokio::test]
async fn vfs_init_devices_exist() {
    let vfs = Vfs::new();
    for path in &["/dev/null", "/dev/stdin", "/dev/stdout", "/dev/stderr"] {
        let s = vfs.stat(path).await.unwrap_or_else(|_| panic!("{path} should exist"));
        assert!(s.is_device, "{path} should be a device");
    }
}

#[tokio::test]
async fn vfs_write_read_file() {
    let mut vfs = Vfs::new();
    vfs.write_file("/tmp/hello.txt", b"hello world".to_vec()).await.unwrap();
    let data = vfs.read_file("/tmp/hello.txt").await.unwrap();
    assert_eq!(data, b"hello world");
}

#[tokio::test]
async fn vfs_overwrite_file() {
    let mut vfs = Vfs::new();
    vfs.write_file("/tmp/f.txt", b"v1".to_vec()).await.unwrap();
    vfs.write_file("/tmp/f.txt", b"v2".to_vec()).await.unwrap();
    assert_eq!(vfs.read_file("/tmp/f.txt").await.unwrap(), b"v2");
}

#[tokio::test]
async fn vfs_stat_file() {
    let mut vfs = Vfs::new();
    vfs.write_file("/tmp/data.bin", vec![0u8; 42]).await.unwrap();
    let s = vfs.stat("/tmp/data.bin").await.unwrap();
    assert!(s.is_file);
    assert!(!s.is_dir);
    assert_eq!(s.size, 42);
}

#[tokio::test]
async fn vfs_read_nonexistent() {
    let vfs = Vfs::new();
    assert!(matches!(vfs.read_file("/tmp/nope.txt").await, Err(VfsError::NotFound(_))));
}

#[tokio::test]
async fn vfs_read_dir_errors() {
    let vfs = Vfs::new();
    assert!(matches!(vfs.read_file("/tmp").await, Err(VfsError::IsADir(_))));
}

#[tokio::test]
async fn vfs_mkdir_and_list() {
    let mut vfs = Vfs::new();
    vfs.mkdir("/tmp/subdir", false).await.unwrap();
    vfs.write_file("/tmp/subdir/a.txt", b"a".to_vec()).await.unwrap();
    vfs.write_file("/tmp/subdir/b.txt", b"b".to_vec()).await.unwrap();
    let mut entries = vfs.list_dir("/tmp/subdir").await.unwrap();
    entries.sort();
    assert_eq!(entries, vec!["a.txt", "b.txt"]);
}

#[tokio::test]
async fn vfs_mkdir_parents() {
    let mut vfs = Vfs::new();
    vfs.mkdir("/tmp/a/b/c", true).await.unwrap();
    let s = vfs.stat("/tmp/a/b/c").await.unwrap();
    assert!(s.is_dir);
}

#[tokio::test]
async fn vfs_mkdir_duplicate_error() {
    let mut vfs = Vfs::new();
    assert!(matches!(vfs.mkdir("/tmp", false).await, Err(VfsError::AlreadyExists(_))));
}

#[tokio::test]
async fn vfs_remove_file() {
    let mut vfs = Vfs::new();
    vfs.write_file("/tmp/bye.txt", b"bye".to_vec()).await.unwrap();
    vfs.remove("/tmp/bye.txt", false).await.unwrap();
    assert!(matches!(vfs.stat("/tmp/bye.txt").await, Err(VfsError::NotFound(_))));
}

#[tokio::test]
async fn vfs_remove_empty_dir() {
    let mut vfs = Vfs::new();
    vfs.mkdir("/tmp/empty", false).await.unwrap();
    vfs.remove("/tmp/empty", false).await.unwrap();
    assert!(matches!(vfs.stat("/tmp/empty").await, Err(VfsError::NotFound(_))));
}

#[tokio::test]
async fn vfs_remove_nonempty_dir_requires_recursive() {
    let mut vfs = Vfs::new();
    vfs.mkdir("/tmp/full", false).await.unwrap();
    vfs.write_file("/tmp/full/x.txt", b"x".to_vec()).await.unwrap();
    assert!(matches!(vfs.remove("/tmp/full", false).await, Err(VfsError::NotEmpty(_))));
    vfs.remove("/tmp/full", true).await.unwrap();
    assert!(matches!(vfs.stat("/tmp/full").await, Err(VfsError::NotFound(_))));
}

#[tokio::test]
async fn vfs_rename() {
    let mut vfs = Vfs::new();
    vfs.write_file("/tmp/old.txt", b"data".to_vec()).await.unwrap();
    vfs.rename("/tmp/old.txt", "/tmp/new.txt").await.unwrap();
    assert!(matches!(vfs.stat("/tmp/old.txt").await, Err(VfsError::NotFound(_))));
    assert_eq!(vfs.read_file("/tmp/new.txt").await.unwrap(), b"data");
}

#[tokio::test]
async fn vfs_copy() {
    let mut vfs = Vfs::new();
    vfs.write_file("/tmp/src.txt", b"copy me".to_vec()).await.unwrap();
    vfs.copy("/tmp/src.txt", "/tmp/dst.txt").await.unwrap();
    assert_eq!(vfs.read_file("/tmp/src.txt").await.unwrap(), b"copy me");
    assert_eq!(vfs.read_file("/tmp/dst.txt").await.unwrap(), b"copy me");
}

#[tokio::test]
async fn dev_null_read_returns_empty() {
    let vfs = Vfs::new();
    assert_eq!(vfs.read_file("/dev/null").await.unwrap(), b"");
}

#[tokio::test]
async fn dev_null_write_discards() {
    let mut vfs = Vfs::new();
    vfs.write_file("/dev/null", b"anything".to_vec()).await.unwrap();
    let s = vfs.stat("/dev/null").await.unwrap();
    assert!(s.is_device);
}

#[tokio::test]
async fn vfs_accepts_unnormalized_paths() {
    let mut vfs = Vfs::new();
    vfs.write_file("/tmp/../tmp/./test.txt", b"ok".to_vec()).await.unwrap();
    assert_eq!(vfs.read_file("/tmp/test.txt").await.unwrap(), b"ok");
}

// ── VFS: mount points ─────────────────────────────────────────────────────────

struct MapMount(Mutex<HashMap<String, Vec<u8>>>);

impl MapMount {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(HashMap::new())))
    }
    fn insert(&self, path: &str, data: &[u8]) {
        self.0.lock().unwrap().insert(path.to_string(), data.to_vec());
    }
}

#[async_trait]
impl MountPoint for MapMount {
    async fn read(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        self.0
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| VfsError::NotFound(path.to_string()))
    }
    async fn write(&self, path: &str, data: &[u8]) -> Result<(), VfsError> {
        self.0.lock().unwrap().insert(path.to_string(), data.to_vec());
        Ok(())
    }
    async fn list(&self, path: &str) -> Result<Vec<String>, VfsError> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let names: Vec<String> = self
            .0
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .map(|k| k[prefix.len()..].splitn(2, '/').next().unwrap().to_string())
            .collect();
        Ok(names)
    }
    async fn stat(&self, path: &str) -> Result<Stat, VfsError> {
        let map = self.0.lock().unwrap();
        map.get(path)
            .map(|d| Stat { is_file: true, is_dir: false, is_device: false, size: d.len() as u64 })
            .ok_or_else(|| VfsError::NotFound(path.to_string()))
    }
    async fn remove(&self, path: &str) -> Result<(), VfsError> {
        self.0
            .lock()
            .unwrap()
            .remove(path)
            .ok_or_else(|| VfsError::NotFound(path.to_string()))?;
        Ok(())
    }
}

#[tokio::test]
async fn mount_read_routes_to_host() {
    let m = MapMount::new();
    m.insert("/hello.txt", b"from mount");
    let mut vfs = Vfs::new();
    vfs.add_mount("/home/agent", m);
    assert_eq!(vfs.read_file("/home/agent/hello.txt").await.unwrap(), b"from mount");
}

#[tokio::test]
async fn mount_write_routes_to_host() {
    let m = MapMount::new();
    let m_ref = Arc::clone(&m);
    let mut vfs = Vfs::new();
    vfs.add_mount("/home/agent", m_ref);
    vfs.write_file("/home/agent/out.txt", b"written".to_vec()).await.unwrap();
    assert_eq!(m.0.lock().unwrap().get("/out.txt").unwrap(), b"written");
}

#[tokio::test]
async fn mount_does_not_affect_other_paths() {
    let m = MapMount::new();
    let mut vfs = Vfs::new();
    vfs.add_mount("/home/agent", m);
    vfs.write_file("/tmp/local.txt", b"local".to_vec()).await.unwrap();
    assert_eq!(vfs.read_file("/tmp/local.txt").await.unwrap(), b"local");
}

#[tokio::test]
async fn mount_exact_path_read() {
    let m = MapMount::new();
    m.insert("", b"root content");
    let mut vfs = Vfs::new();
    vfs.add_mount("/home/agent", m);
    assert_eq!(vfs.read_file("/home/agent").await.unwrap(), b"root content");
}

// ── Program registry ──────────────────────────────────────────────────────────

#[test]
fn registry_bare_name_stored_at_usr_bin() {
    let mut reg = ProgramRegistry::new();
    reg.add("mytool", |_ctx| async { Ok(ExitCode::SUCCESS) });
    assert!(reg.resolve("mytool", "/usr/bin:/bin").is_some());
}

#[test]
fn registry_full_path_stored_correctly() {
    let mut reg = ProgramRegistry::new();
    reg.add("/usr/local/bin/mytool", |_ctx| async { Ok(ExitCode::SUCCESS) });
    assert!(reg.resolve("/usr/local/bin/mytool", "").is_some());
    assert!(reg.resolve("mytool", "/usr/bin").is_none());
}

#[test]
fn registry_path_resolution_order() {
    let mut reg = ProgramRegistry::new();
    reg.add("/usr/bin/tool", |_ctx| async { Ok(ExitCode(0)) });
    reg.add("/usr/local/bin/tool", |_ctx| async { Ok(ExitCode(42)) });
    assert!(reg.resolve("tool", "/usr/bin:/usr/local/bin").is_some());
    assert!(reg.resolve("tool", "/usr/local/bin:/usr/bin").is_some());
}

#[test]
fn registry_unregistered_name_returns_none() {
    let reg = ProgramRegistry::new();
    assert!(reg.resolve("nope", "/usr/bin:/bin").is_none());
}

#[tokio::test]
async fn registry_callback_invocable() {
    let mut reg = ProgramRegistry::new();
    reg.add("echo-test", |ctx| async move {
        assert_eq!(ctx.args[0], "echo-test");
        Ok(ExitCode(7))
    });
    let cb = reg.resolve("echo-test", "/usr/bin").unwrap();
    let ctx = ProgramContext::new(
        vec!["echo-test".to_string()],
        EnvMap::new(),
        "/".to_string(),
        std::sync::Arc::new(tokio::sync::Mutex::new(Vfs::new())),
        Io::new(),
    );
    let code = cb(ctx).await.unwrap();
    assert_eq!(code, ExitCode(7));
}

// ── Shell ─────────────────────────────────────────────────────────────────────

#[test]
fn shell_default_env() {
    let shell = Shell::new();
    assert_eq!(shell.get_env("PATH"), Some("/usr/bin:/bin:/usr/local/bin"));
    assert_eq!(shell.get_env("HOME"), Some("/home"));
}

#[test]
fn shell_default_cwd() {
    let shell = Shell::new();
    assert_eq!(shell.cwd, "/");
}

#[tokio::test]
async fn shell_vfs_initialized() {
    let shell = Shell::new();
    let s = shell.fs.lock().await.stat("/usr/bin").await.unwrap();
    assert!(s.is_dir);
}

#[tokio::test]
async fn shell_add_program_and_resolve() {
    let mut shell = Shell::new();
    shell.add_program("greet", |_ctx| async { Ok(ExitCode::SUCCESS) });
    assert!(shell.registry.resolve("greet", shell.path_var()).is_some());
}

#[tokio::test]
async fn shell_mount_routes_vfs() {
    let m = MapMount::new();
    m.insert("/README.md", b"hello");
    let mut shell = Shell::new();
    shell.mount("/workspace", Arc::clone(&m) as Arc<dyn MountPoint>);
    assert_eq!(shell.fs.lock().await.read_file("/workspace/README.md").await.unwrap(), b"hello");
}

// ═════════════════════════════════════════════════════════════════════════════
// Stage 3 — Execution Engine
// ═════════════════════════════════════════════════════════════════════════════

// ── Arithmetic ────────────────────────────────────────────────────────────────

#[test]
fn arith_basic_ops() {
    let env = EnvMap::new();
    assert_eq!(eval_arith("2+3", &env).unwrap(), 5);
    assert_eq!(eval_arith("10 - 4", &env).unwrap(), 6);
    assert_eq!(eval_arith("3 * 7", &env).unwrap(), 21);
    assert_eq!(eval_arith("15 / 4", &env).unwrap(), 3); // integer division
    assert_eq!(eval_arith("17 % 5", &env).unwrap(), 2);
}

#[test]
fn arith_precedence() {
    let env = EnvMap::new();
    assert_eq!(eval_arith("2 + 3 * 4", &env).unwrap(), 14);
    assert_eq!(eval_arith("(2 + 3) * 4", &env).unwrap(), 20);
}

#[test]
fn arith_power() {
    let env = EnvMap::new();
    assert_eq!(eval_arith("2**10", &env).unwrap(), 1024);
    // right-associative: 2**2**3 = 2**(2**3) = 2**8 = 256
    assert_eq!(eval_arith("2**2**3", &env).unwrap(), 256);
}

#[test]
fn arith_unary_minus() {
    let env = EnvMap::new();
    assert_eq!(eval_arith("-5 + 3", &env).unwrap(), -2);
    assert_eq!(eval_arith("-(2+3)", &env).unwrap(), -5);
}

#[test]
fn arith_variable_substitution() {
    let mut env = EnvMap::new();
    env.set("X", "6");
    env.set("Y", "7");
    assert_eq!(eval_arith("$X * $Y", &env).unwrap(), 42);
    assert_eq!(eval_arith("${X} + 1", &env).unwrap(), 7);
}

#[test]
fn arith_undefined_var_is_zero() {
    let env = EnvMap::new();
    assert_eq!(eval_arith("$UNDEF + 5", &env).unwrap(), 5);
}

#[test]
fn arith_division_by_zero() {
    let env = EnvMap::new();
    assert!(eval_arith("1/0", &env).is_err());
}

// ── Simple commands ───────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_simple_command() {
    let mut shell = Shell::new();
    shell.add_program("hello", |ctx| async move {
        ctx.stdout().write_all(b"hello world\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("hello").await.unwrap();
    assert_eq!(out.code, ExitCode::SUCCESS);
    assert_eq!(out.stdout, b"hello world\n");
}

#[tokio::test]
async fn exec_program_receives_args() {
    let mut shell = Shell::new();
    shell.add_program("cat-args", |ctx| async move {
        let result = ctx.args[1..].join(" ");
        ctx.stdout().write_all(result.as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("cat-args foo bar baz").await.unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "foo bar baz");
}

#[tokio::test]
async fn exec_program_reads_stdin() {
    let mut shell = Shell::new();
    shell.add_program("upper", |mut ctx| async move {
        let mut buf = Vec::new();
        ctx.stdin().read_to_end(&mut buf).await.unwrap();
        let upper = String::from_utf8_lossy(&buf).to_uppercase();
        ctx.stdout().write_all(upper.as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    // Feed stdin via heredoc
    let out = shell.exec("upper <<'EOF'\nhello\nEOF").await.unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "HELLO\n");
}

#[tokio::test]
async fn exec_exit_code_propagated() {
    let mut shell = Shell::new();
    shell.add_program("fail", |_ctx| async { Ok(ExitCode(42)) });
    let out = shell.exec("fail").await.unwrap();
    assert_eq!(out.code, ExitCode(42));
}

#[tokio::test]
async fn exec_command_not_found() {
    let mut shell = Shell::new();
    let out = shell.exec("no-such-program").await.unwrap();
    assert_eq!(out.code, ExitCode(127));
    assert!(out.stderr.windows(b"not found".len()).any(|w| w == b"not found"));
}

#[tokio::test]
async fn exec_stderr_separate() {
    let mut shell = Shell::new();
    shell.add_program("log", |ctx| async move {
        ctx.stdout().write_all(b"out\n").await.unwrap();
        ctx.stderr().write_all(b"err\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("log").await.unwrap();
    assert_eq!(out.stdout, b"out\n");
    assert_eq!(out.stderr, b"err\n");
}

// ── Variable expansion ────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_variable_expansion() {
    let mut shell = Shell::new();
    shell.set_env("GREETING", "hi");
    shell.add_program("print", |ctx| async move {
        ctx.stdout().write_all(ctx.args[1].as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("print $GREETING").await.unwrap();
    assert_eq!(out.stdout, b"hi");
}

#[tokio::test]
async fn exec_assignment_only() {
    let mut shell = Shell::new();
    shell.exec("MY_VAR=hello").await.unwrap();
    assert_eq!(shell.get_env("MY_VAR"), Some("hello"));
}

#[tokio::test]
async fn exec_arithmetic_expansion() {
    let mut shell = Shell::new();
    shell.add_program("print", |ctx| async move {
        ctx.stdout().write_all(ctx.args[1].as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("print $((3 * 7))").await.unwrap();
    assert_eq!(out.stdout, b"21");
}

// ── Command substitution ──────────────────────────────────────────────────────

#[tokio::test]
async fn exec_command_substitution() {
    let mut shell = Shell::new();
    shell.add_program("gen", |ctx| async move {
        ctx.stdout().write_all(b"world\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    shell.add_program("print", |ctx| async move {
        ctx.stdout().write_all(ctx.args[1].as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("print $(gen)").await.unwrap();
    assert_eq!(out.stdout, b"world"); // trailing newline stripped
}

// ── Sequences and logical operators ──────────────────────────────────────────

#[tokio::test]
async fn exec_sequence() {
    let mut shell = Shell::new();
    shell.add_program("w", |ctx| async move {
        ctx.stdout().write_all(ctx.args[1].as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("w a; w b; w c").await.unwrap();
    assert_eq!(out.stdout, b"abc");
}

#[tokio::test]
async fn exec_and_short_circuit() {
    let mut shell = Shell::new();
    shell.add_program("ok", |_| async { Ok(ExitCode(0)) });
    shell.add_program("fail", |_| async { Ok(ExitCode(1)) });
    shell.add_program("mark", |ctx| async move {
        ctx.stdout().write_all(b"ran").await.unwrap();
        Ok(ExitCode(0))
    });
    // fail && mark — mark should NOT run
    let out = shell.exec("fail && mark").await.unwrap();
    assert!(out.stdout.is_empty());
    // ok && mark — mark SHOULD run
    let out = shell.exec("ok && mark").await.unwrap();
    assert_eq!(out.stdout, b"ran");
}

#[tokio::test]
async fn exec_or_short_circuit() {
    let mut shell = Shell::new();
    shell.add_program("ok", |_| async { Ok(ExitCode(0)) });
    shell.add_program("fail", |_| async { Ok(ExitCode(1)) });
    shell.add_program("mark", |ctx| async move {
        ctx.stdout().write_all(b"ran").await.unwrap();
        Ok(ExitCode(0))
    });
    // ok || mark — mark should NOT run
    let out = shell.exec("ok || mark").await.unwrap();
    assert!(out.stdout.is_empty());
    // fail || mark — mark SHOULD run
    let out = shell.exec("fail || mark").await.unwrap();
    assert_eq!(out.stdout, b"ran");
}

// ── Pipelines ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_pipeline_two_stages() {
    let mut shell = Shell::new();
    shell.add_program("producer", |ctx| async move {
        ctx.stdout().write_all(b"hello\nworld\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    shell.add_program("upper", |mut ctx| async move {
        let mut buf = Vec::new();
        ctx.stdin().read_to_end(&mut buf).await.unwrap();
        let up = String::from_utf8_lossy(&buf).to_uppercase();
        ctx.stdout().write_all(up.as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("producer | upper").await.unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "HELLO\nWORLD\n");
}

#[tokio::test]
async fn exec_pipeline_three_stages() {
    let mut shell = Shell::new();
    shell.add_program("gen", |ctx| async move {
        ctx.stdout().write_all(b"abc").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    shell.add_program("dup", |mut ctx| async move {
        let mut buf = Vec::new();
        ctx.stdin().read_to_end(&mut buf).await.unwrap();
        let doubled = [buf.clone(), buf].concat();
        ctx.stdout().write_all(&doubled).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("gen | dup | dup").await.unwrap();
    assert_eq!(out.stdout, b"abcabcabcabc");
}

#[tokio::test]
async fn exec_negated_pipeline() {
    let mut shell = Shell::new();
    shell.add_program("ok", |_| async { Ok(ExitCode(0)) });
    let out = shell.exec("! ok").await.unwrap();
    assert_eq!(out.code, ExitCode(1));
}

// ── Redirections ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_redirect_stdout_to_file() {
    let mut shell = Shell::new();
    shell.add_program("hello", |ctx| async move {
        ctx.stdout().write_all(b"file content\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    shell.exec("hello > /tmp/out.txt").await.unwrap();
    let data = shell.fs.lock().await.read_file("/tmp/out.txt").await.unwrap();
    assert_eq!(data, b"file content\n");
}

#[tokio::test]
async fn exec_redirect_append() {
    let mut shell = Shell::new();
    shell.add_program("w", |ctx| async move {
        ctx.stdout().write_all(ctx.args[1].as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    shell.exec("w line1 >> /tmp/append.txt").await.unwrap();
    shell.exec("w line2 >> /tmp/append.txt").await.unwrap();
    let data = shell.fs.lock().await.read_file("/tmp/append.txt").await.unwrap();
    assert_eq!(data, b"line1line2");
}

#[tokio::test]
async fn exec_redirect_stdin_from_file() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/input.txt", b"from file\n".to_vec()).await.unwrap();
    shell.add_program("cat", |mut ctx| async move {
        let mut buf = Vec::new();
        ctx.stdin().read_to_end(&mut buf).await.unwrap();
        ctx.stdout().write_all(&buf).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("cat < /tmp/input.txt").await.unwrap();
    assert_eq!(out.stdout, b"from file\n");
}

#[tokio::test]
async fn exec_redirect_stderr_to_stdout() {
    let mut shell = Shell::new();
    shell.add_program("warn", |ctx| async move {
        ctx.stderr().write_all(b"warning\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("warn 2>&1").await.unwrap();
    assert_eq!(out.stdout, b"warning\n");
    assert!(out.stderr.is_empty());
}

// ── Subshell isolation ────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_subshell_env_isolated() {
    let mut shell = Shell::new();
    shell.set_env("X", "outer");
    shell.exec("(X=inner)").await.unwrap();
    // X should remain "outer" after the subshell exits.
    assert_eq!(shell.get_env("X"), Some("outer"));
}

// ── $? last exit code ─────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_last_exit_code() {
    let mut shell = Shell::new();
    shell.add_program("exit42", |_| async { Ok(ExitCode(42)) });
    shell.add_program("print", |ctx| async move {
        ctx.stdout().write_all(ctx.args[1].as_bytes()).await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    shell.exec("exit42").await.unwrap();
    let out = shell.exec("print $?").await.unwrap();
    assert_eq!(out.stdout, b"42");
}

// ═════════════════════════════════════════════════════════════════════════════
// Stage 4 — Built-ins
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn builtin_echo_basic() {
    let mut shell = Shell::new();
    let out = shell.exec("echo hello world").await.unwrap();
    assert_eq!(out.stdout, b"hello world\n");
}

#[tokio::test]
async fn builtin_echo_n() {
    let mut shell = Shell::new();
    let out = shell.exec("echo -n hello").await.unwrap();
    assert_eq!(out.stdout, b"hello");
}

#[tokio::test]
async fn builtin_echo_e() {
    let mut shell = Shell::new();
    let out = shell.exec("echo -e 'a\\nb'").await.unwrap();
    assert_eq!(out.stdout, b"a\nb\n");
}

#[tokio::test]
async fn builtin_pwd() {
    let mut shell = Shell::new();
    let out = shell.exec("pwd").await.unwrap();
    assert_eq!(out.stdout, b"/\n");
}

#[tokio::test]
async fn builtin_cd_and_pwd() {
    let mut shell = Shell::new();
    shell.exec("cd /tmp").await.unwrap();
    assert_eq!(shell.cwd, "/tmp");
    let out = shell.exec("pwd").await.unwrap();
    assert_eq!(out.stdout, b"/tmp\n");
}

#[tokio::test]
async fn builtin_mkdir_and_ls() {
    let mut shell = Shell::new();
    shell.exec("mkdir /tmp/mydir").await.unwrap();
    let out = shell.exec("ls /tmp").await.unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("mydir"));
}

#[tokio::test]
async fn builtin_touch_creates_file() {
    let mut shell = Shell::new();
    shell.exec("touch /tmp/f.txt").await.unwrap();
    let s = shell.fs.lock().await.stat("/tmp/f.txt").await.unwrap();
    assert!(s.is_file);
}

#[tokio::test]
async fn builtin_cat_file() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/hi.txt", b"hello\n".to_vec()).await.unwrap();
    let out = shell.exec("cat /tmp/hi.txt").await.unwrap();
    assert_eq!(out.stdout, b"hello\n");
}

#[tokio::test]
async fn builtin_cat_stdin() {
    let mut shell = Shell::new();
    shell.add_program("emit", |ctx| async move {
        ctx.stdout().write_all(b"piped\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("emit | cat").await.unwrap();
    assert_eq!(out.stdout, b"piped\n");
}

#[tokio::test]
async fn builtin_rm_file() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/del.txt", b"x".to_vec()).await.unwrap();
    shell.exec("rm /tmp/del.txt").await.unwrap();
    assert!(shell.fs.lock().await.stat("/tmp/del.txt").await.is_err());
}

#[tokio::test]
async fn builtin_rm_recursive() {
    let mut shell = Shell::new();
    shell.exec("mkdir -p /tmp/d/sub").await.unwrap();
    shell.exec("touch /tmp/d/sub/f.txt").await.unwrap();
    shell.exec("rm -r /tmp/d").await.unwrap();
    assert!(shell.fs.lock().await.stat("/tmp/d").await.is_err());
}

#[tokio::test]
async fn builtin_mv_rename() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/a.txt", b"data".to_vec()).await.unwrap();
    shell.exec("mv /tmp/a.txt /tmp/b.txt").await.unwrap();
    assert!(shell.fs.lock().await.stat("/tmp/b.txt").await.is_ok());
    assert!(shell.fs.lock().await.stat("/tmp/a.txt").await.is_err());
}

#[tokio::test]
async fn builtin_cp_file() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/src.txt", b"copy me".to_vec()).await.unwrap();
    shell.exec("cp /tmp/src.txt /tmp/dst.txt").await.unwrap();
    let data = shell.fs.lock().await.read_file("/tmp/dst.txt").await.unwrap();
    assert_eq!(data, b"copy me");
}

#[tokio::test]
async fn builtin_head() {
    let mut shell = Shell::new();
    let content = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n";
    shell.fs.lock().await.write_file("/tmp/nums.txt", content.as_bytes().to_vec()).await.unwrap();
    let out = shell.exec("head -n 3 /tmp/nums.txt").await.unwrap();
    assert_eq!(out.stdout, b"1\n2\n3\n");
}

#[tokio::test]
async fn builtin_tail() {
    let mut shell = Shell::new();
    let content = "1\n2\n3\n4\n5\n";
    shell.fs.lock().await.write_file("/tmp/t.txt", content.as_bytes().to_vec()).await.unwrap();
    let out = shell.exec("tail -n 2 /tmp/t.txt").await.unwrap();
    assert_eq!(out.stdout, b"4\n5\n");
}

#[tokio::test]
async fn builtin_wc_lines() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/wc.txt", b"a\nb\nc\n".to_vec()).await.unwrap();
    let out = shell.exec("wc -l /tmp/wc.txt").await.unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).trim().starts_with('3'));
}

#[tokio::test]
async fn builtin_sort_basic() {
    let mut shell = Shell::new();
    shell.add_program("emit", |ctx| async move {
        ctx.stdout().write_all(b"b\na\nc\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("emit | sort").await.unwrap();
    assert_eq!(out.stdout, b"a\nb\nc\n");
}

#[tokio::test]
async fn builtin_grep_basic() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/g.txt", b"foo\nbar\nfoo2\n".to_vec()).await.unwrap();
    let out = shell.exec("grep foo /tmp/g.txt").await.unwrap();
    assert_eq!(out.stdout, b"foo\nfoo2\n");
}

#[tokio::test]
async fn builtin_grep_v() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/g2.txt", b"foo\nbar\n".to_vec()).await.unwrap();
    let out = shell.exec("grep -v foo /tmp/g2.txt").await.unwrap();
    assert_eq!(out.stdout, b"bar\n");
}

#[tokio::test]
async fn builtin_sed_substitute() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/s.txt", b"hello world\n".to_vec()).await.unwrap();
    let out = shell.exec("sed 's/world/rust/' /tmp/s.txt").await.unwrap();
    assert_eq!(out.stdout, b"hello rust\n");
}

#[tokio::test]
async fn builtin_sed_global() {
    let mut shell = Shell::new();
    shell.add_program("emit", |ctx| async move {
        ctx.stdout().write_all(b"aaa\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("emit | sed 's/a/b/g'").await.unwrap();
    assert_eq!(out.stdout, b"bbb\n");
}

#[tokio::test]
async fn builtin_export_persists() {
    let mut shell = Shell::new();
    shell.exec("export FOO=bar").await.unwrap();
    assert_eq!(shell.get_env("FOO"), Some("bar"));
}

#[tokio::test]
async fn builtin_unset_removes() {
    let mut shell = Shell::new();
    shell.set_env("X", "1");
    shell.exec("unset X").await.unwrap();
    assert_eq!(shell.get_env("X"), None);
}

#[tokio::test]
async fn builtin_env_prints() {
    let mut shell = Shell::new();
    shell.set_env("MY_VAR", "hello");
    let out = shell.exec("env").await.unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("MY_VAR=hello"));
}

#[tokio::test]
async fn builtin_test_file_exists() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/x.txt", b"".to_vec()).await.unwrap();
    let out = shell.exec("test -f /tmp/x.txt && echo yes").await.unwrap();
    assert_eq!(out.stdout, b"yes\n");
}

#[tokio::test]
async fn builtin_test_string_eq() {
    let mut shell = Shell::new();
    let out = shell.exec("test hello = hello && echo yes").await.unwrap();
    assert_eq!(out.stdout, b"yes\n");
}

#[tokio::test]
async fn builtin_test_numeric() {
    let mut shell = Shell::new();
    let out = shell.exec("test 3 -gt 2 && echo yes").await.unwrap();
    assert_eq!(out.stdout, b"yes\n");
}

#[tokio::test]
async fn builtin_true_false() {
    let mut shell = Shell::new();
    let t = shell.exec("true").await.unwrap();
    assert_eq!(t.code, ExitCode::SUCCESS);
    let f = shell.exec("false").await.unwrap();
    assert_eq!(f.code, ExitCode::FAILURE);
}

#[tokio::test]
async fn builtin_printf_basic() {
    let mut shell = Shell::new();
    let out = shell.exec(r#"printf "%s: %d\n" hello 42"#).await.unwrap();
    assert_eq!(out.stdout, b"hello: 42\n");
}

#[tokio::test]
async fn builtin_exit_code() {
    let mut shell = Shell::new();
    let out = shell.exec("exit 5").await.unwrap();
    assert_eq!(out.code, ExitCode(5));
}

#[tokio::test]
async fn builtin_source() {
    let mut shell = Shell::new();
    shell.fs.lock().await.write_file("/tmp/script.sh", b"export SOURCED=1\n".to_vec()).await.unwrap();
    shell.exec("source /tmp/script.sh").await.unwrap();
    assert_eq!(shell.get_env("SOURCED"), Some("1"));
}

#[tokio::test]
async fn builtin_alias_set_and_use() {
    let mut shell = Shell::new();
    shell.exec("alias ll='ls -l'").await.unwrap();
    assert!(shell.aliases.contains_key("ll"));
}

#[tokio::test]
async fn builtin_cd_home_fallback() {
    let mut shell = Shell::new();
    shell.set_env("HOME", "/home");
    shell.cwd = "/tmp".to_string();
    shell.exec("cd").await.unwrap();
    assert_eq!(shell.cwd, "/home");
}

#[tokio::test]
async fn builtin_which_finds_builtin() {
    let mut shell = Shell::new();
    let out = shell.exec("which echo").await.unwrap();
    assert_eq!(out.stdout, b"/usr/bin/echo\n");
}

#[tokio::test]
async fn builtin_tr_basic() {
    let mut shell = Shell::new();
    shell.add_program("emit", |ctx| async move {
        ctx.stdout().write_all(b"hello").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("emit | tr a-z A-Z").await.unwrap();
    assert_eq!(out.stdout, b"HELLO");
}

#[tokio::test]
async fn builtin_cut_fields() {
    let mut shell = Shell::new();
    shell.add_program("emit", |ctx| async move {
        ctx.stdout().write_all(b"a:b:c\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("emit | cut -d: -f2").await.unwrap();
    assert_eq!(out.stdout, b"b\n");
}

#[tokio::test]
async fn builtin_read_from_stdin() {
    let mut shell = Shell::new();
    shell.add_program("emit", |ctx| async move {
        ctx.stdout().write_all(b"myvalue\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    shell.exec("emit | read MYVAR").await.unwrap();
    assert_eq!(shell.get_env("MYVAR"), Some("myvalue"));
}

#[tokio::test]
async fn builtin_xargs_basic() {
    let mut shell = Shell::new();
    shell.add_program("emit", |ctx| async move {
        ctx.stdout().write_all(b"foo bar baz\n").await.unwrap();
        Ok(ExitCode::SUCCESS)
    });
    let out = shell.exec("emit | xargs echo").await.unwrap();
    assert_eq!(out.stdout, b"foo bar baz\n");
}
