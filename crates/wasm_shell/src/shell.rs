use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::env::EnvMap;
use crate::error::{ShellError, VfsError};
use crate::io::{BytesReader, Io, VecWriter};
use crate::registry::ProgramRegistry;
use crate::vfs::{MountPoint, Stat, Vfs, resolve_path};

// ── ExitCode ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub i32);

impl ExitCode {
    pub const SUCCESS: ExitCode = ExitCode(0);
    pub const FAILURE: ExitCode = ExitCode(1);
}

// ── ProgramContext ────────────────────────────────────────────────────────────

/// Shared mutable shell state for a running program.
/// Held behind `Arc<Mutex<>>` so mutations propagate back to the shell.
pub(crate) struct CtxShared {
    pub env: EnvMap,
    pub cwd: String,
    pub aliases: HashMap<String, String>,
}

/// Context given to a registered program when it is invoked.
pub struct ProgramContext {
    /// `args[0]` is the program name, `args[1..]` are the arguments.
    pub args: Vec<String>,
    pub(crate) shared: Arc<Mutex<CtxShared>>,
    /// Shared virtual filesystem — same instance as `Shell.fs`.
    pub(crate) fs: Arc<tokio::sync::Mutex<Vfs>>,
    pub(crate) io: Io,
}

impl ProgramContext {
    #[allow(dead_code)]
    pub(crate) fn new(
        args: Vec<String>,
        env: EnvMap,
        cwd: String,
        fs: Arc<tokio::sync::Mutex<Vfs>>,
        io: Io,
    ) -> Self {
        Self {
            args,
            shared: Arc::new(Mutex::new(CtxShared {
                env,
                cwd,
                aliases: HashMap::new(),
            })),
            fs,
            io,
        }
    }

    pub(crate) fn new_shared(
        args: Vec<String>,
        shared: Arc<Mutex<CtxShared>>,
        fs: Arc<tokio::sync::Mutex<Vfs>>,
        io: Io,
    ) -> Self {
        Self { args, shared, fs, io }
    }

    // ── Environment ───────────────────────────────────────────────────────────

    /// Get an environment variable. Reflects mutations made via `set_env`.
    pub fn get_env(&self, key: &str) -> Option<String> {
        self.shared.lock().unwrap().env.get(key).map(|s| s.to_owned())
    }

    /// Set an environment variable. Change propagates back to the shell.
    pub fn set_env(&self, key: &str, value: &str) {
        self.shared.lock().unwrap().env.set(key, value);
    }

    /// Unset an environment variable. Change propagates back to the shell.
    pub fn unset_env(&self, key: &str) {
        self.shared.lock().unwrap().env.unset(key);
    }

    /// Snapshot of the full environment at the time of the call.
    pub fn env_snapshot(&self) -> EnvMap {
        self.shared.lock().unwrap().env.clone()
    }

    // ── Working directory ─────────────────────────────────────────────────────

    /// Current working directory. Reflects mutations made via `set_cwd`.
    pub fn cwd(&self) -> String {
        self.shared.lock().unwrap().cwd.clone()
    }

    /// Change the working directory. Change propagates back to the shell.
    pub fn set_cwd(&self, path: &str) {
        self.shared.lock().unwrap().cwd = path.to_string();
    }

    // ── Aliases ───────────────────────────────────────────────────────────────

    pub fn get_alias(&self, name: &str) -> Option<String> {
        self.shared.lock().unwrap().aliases.get(name).cloned()
    }

    pub fn set_alias(&self, name: &str, value: &str) {
        self.shared.lock().unwrap().aliases.insert(name.to_string(), value.to_string());
    }

    pub fn unset_alias(&self, name: &str) {
        self.shared.lock().unwrap().aliases.remove(name);
    }

    pub fn aliases_snapshot(&self) -> HashMap<String, String> {
        self.shared.lock().unwrap().aliases.clone()
    }

    // ── Filesystem ───────────────────────────────────────────────────────────
    // Paths are resolved relative to the program's current cwd.

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        let resolved = resolve_path(&self.cwd(), path);
        self.fs.lock().await.read_file(&resolved).await
    }

    pub async fn write_file(&self, path: &str, data: Vec<u8>) -> Result<(), VfsError> {
        let resolved = resolve_path(&self.cwd(), path);
        self.fs.lock().await.write_file(&resolved, data).await
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>, VfsError> {
        let resolved = resolve_path(&self.cwd(), path);
        self.fs.lock().await.list_dir(&resolved).await
    }

    pub async fn stat(&self, path: &str) -> Result<Stat, VfsError> {
        let resolved = resolve_path(&self.cwd(), path);
        self.fs.lock().await.stat(&resolved).await
    }

    pub async fn mkdir(&self, path: &str, parents: bool) -> Result<(), VfsError> {
        let resolved = resolve_path(&self.cwd(), path);
        self.fs.lock().await.mkdir(&resolved, parents).await
    }

    pub async fn remove(&self, path: &str, recursive: bool) -> Result<(), VfsError> {
        let resolved = resolve_path(&self.cwd(), path);
        self.fs.lock().await.remove(&resolved, recursive).await
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), VfsError> {
        let from = resolve_path(&self.cwd(), from);
        let to = resolve_path(&self.cwd(), to);
        self.fs.lock().await.rename(&from, &to).await
    }

    pub async fn copy(&self, from: &str, to: &str) -> Result<(), VfsError> {
        let from = resolve_path(&self.cwd(), from);
        let to = resolve_path(&self.cwd(), to);
        self.fs.lock().await.copy(&from, &to).await
    }

    // ── I/O ───────────────────────────────────────────────────────────────────

    /// Returns a handle to stdout. Multiple handles share the same buffer.
    /// Implements `AsyncWrite` — use with `tokio::io::AsyncWriteExt`.
    pub fn stdout(&self) -> VecWriter {
        self.io.stdout.clone()
    }

    /// Returns a handle to stderr.
    pub fn stderr(&self) -> VecWriter {
        self.io.stderr.clone()
    }

    /// Returns a mutable reference to stdin.
    /// Implements `AsyncRead` — use with `tokio::io::AsyncReadExt`.
    pub fn stdin(&mut self) -> &mut BytesReader {
        &mut self.io.stdin
    }
}

// ── ExecOutput ────────────────────────────────────────────────────────────────

/// Result of `Shell::exec`.
pub struct ExecOutput {
    pub code: ExitCode,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

// ── Shell ─────────────────────────────────────────────────────────────────────

pub struct Shell {
    /// The virtual filesystem, shared with running programs via `ProgramContext`.
    pub fs: Arc<tokio::sync::Mutex<Vfs>>,
    pub registry: ProgramRegistry,
    pub env: EnvMap,
    pub cwd: String,
    pub(crate) aliases: HashMap<String, String>,
    /// Exit code of the last completed command (exposed as `$?`).
    pub(crate) last_exit: ExitCode,
}

impl Shell {
    pub fn new() -> Self {
        let mut env = EnvMap::new();
        env.set("PATH", "/usr/bin:/bin:/usr/local/bin");
        env.set("HOME", "/home");
        env.set("TMPDIR", "/tmp");

        let mut shell = Self {
            fs: Arc::new(tokio::sync::Mutex::new(Vfs::new())),
            registry: ProgramRegistry::new(),
            env,
            cwd: "/".to_string(),
            aliases: HashMap::new(),
            last_exit: ExitCode::SUCCESS,
        };
        crate::builtins::register(&mut shell);
        shell
    }

    // ── Program registration ──────────────────────────────────────────────────

    /// Register a program callback.
    /// Bare names are installed at `/usr/bin/<name>`.
    /// Names containing `/` are stored at the given path.
    pub fn add_program<F, Fut>(&mut self, name: &str, f: F)
    where
        F: Fn(ProgramContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ExitCode, ShellError>> + Send + 'static,
    {
        self.registry.add(name, f);
    }

    // ── Mount API ─────────────────────────────────────────────────────────────

    /// Overlay a host mount point onto `virtual_path`.
    pub fn mount(&mut self, virtual_path: &str, point: Arc<dyn MountPoint>) {
        self.fs.try_lock().expect("VFS lock unavailable during mount").add_mount(virtual_path, point);
    }

    // ── Execution ─────────────────────────────────────────────────────────────

    /// Parse and execute a shell script string.
    /// Returns the exit code and captured stdout/stderr.
    pub async fn exec(&mut self, src: &str) -> Result<ExecOutput, ShellError> {
        let script = bash_parser::parse(src)
            .map_err(|e| ShellError::Parse(e.to_string()))?;

        let stdout = VecWriter::new();
        let stderr = VecWriter::new();
        let io = Io {
            stdin: crate::io::BytesReader::empty(),
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        };

        let code = match crate::exec::execute_script(self, &script, io).await {
            Ok(c) => c,
            Err(ShellError::Exit(n)) => ExitCode(n),
            Err(e) => return Err(e),
        };
        self.last_exit = code;

        Ok(ExecOutput {
            code,
            stdout: stdout.bytes(),
            stderr: stderr.bytes(),
        })
    }

    /// Like [`exec`] but with pre-loaded stdin bytes (consumed by the script).
    pub async fn exec_with_stdin(&mut self, src: &str, stdin: Vec<u8>) -> Result<ExecOutput, ShellError> {
        let script = bash_parser::parse(src)
            .map_err(|e| ShellError::Parse(e.to_string()))?;

        let stdout = VecWriter::new();
        let stderr = VecWriter::new();
        let io = Io {
            stdin: BytesReader::new(stdin),
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        };

        let code = match crate::exec::execute_script(self, &script, io).await {
            Ok(c) => c,
            Err(ShellError::Exit(n)) => ExitCode(n),
            Err(e) => return Err(e),
        };
        self.last_exit = code;

        Ok(ExecOutput {
            code,
            stdout: stdout.bytes(),
            stderr: stderr.bytes(),
        })
    }

    // ── Environment ───────────────────────────────────────────────────────────

    pub fn set_env(&mut self, key: &str, value: &str) {
        self.env.set(key, value);
    }

    pub fn get_env(&self, key: &str) -> Option<&str> {
        self.env.get(key)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// The current `$PATH` value.
    pub fn path_var(&self) -> &str {
        self.env.get("PATH").unwrap_or("/usr/bin:/bin:/usr/local/bin")
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}
