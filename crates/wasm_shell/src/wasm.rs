//! WASM / JavaScript bindings — enabled with the `wasm` Cargo feature.
//!
//! Build with:
//! ```sh
//! wasm-pack build crates/wasm_shell \
//!   --target bundler \
//!   --features wasm \
//!   --out-dir ../../npm/wasm_shell/pkg
//! ```
#![allow(unsafe_code)] // SendMe / SendFut require unsafe — safe on single-threaded WASM

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use js_sys::{Array, Function, Object, Promise, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::error::{ShellError, VfsError};
use crate::shell::{ExitCode, ProgramContext, Shell};
use crate::vfs::{MountPoint, Stat};

// ── Safety wrappers ───────────────────────────────────────────────────────────
//
// WASM is always single-threaded; these wrappers implement Send/Sync so that
// JS values can satisfy the `Send` bounds required by the program registry.

struct SendMe<T>(T);
// SAFETY: WASM targets have exactly one thread; no concurrent access is possible.
unsafe impl<T> Send for SendMe<T> {}
unsafe impl<T> Sync for SendMe<T> {}

/// A future wrapper that asserts `Send`.  Safe on single-threaded WASM.
struct SendFut<F>(F);
// SAFETY: same as above.
unsafe impl<F: Future> Send for SendFut<F> {}

impl<F: Future> Future for SendFut<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: we do not move `F` out of the Pin projection.
        unsafe { Pin::map_unchecked_mut(self, |s| &mut s.0) }.poll(cx)
    }
}

// ── JsMount ───────────────────────────────────────────────────────────────────

/// A `MountPoint` that delegates all I/O to JavaScript async callbacks.
struct JsMount {
    read_fn:   SendMe<Function>,
    write_fn:  SendMe<Function>,
    list_fn:   SendMe<Function>,
    stat_fn:   SendMe<Function>,
    remove_fn: SendMe<Function>,
}

// We implement MountPoint manually instead of using #[async_trait] so that we
// can wrap each JsFuture in SendFut, satisfying the `+ Send` bound without
// requiring JsFuture (which is !Send) to actually be Send.
impl MountPoint for JsMount {
    fn read<'life0, 'life1, 'async_trait>(
        &'life0 self,
        path: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, VfsError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let f = SendMe(self.read_fn.0.clone());
        let p = path.to_owned();
        Box::pin(SendFut(async move {
            let promise = f.0
                .call1(&JsValue::NULL, &JsValue::from_str(&p))
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            let result = JsFuture::from(Promise::from(promise))
                .await
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            Ok(Uint8Array::from(result).to_vec())
        }))
    }

    fn write<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        path: &'life1 str,
        data: &'life2 [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), VfsError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        let f = SendMe(self.write_fn.0.clone());
        let p = path.to_owned();
        // Copy the slice into a Uint8Array synchronously before entering async;
        // then wrap in SendMe so that it can be captured by a Send async block.
        let d = SendMe(Uint8Array::from(data));
        Box::pin(SendFut(async move {
            let promise = f.0
                .call2(&JsValue::NULL, &JsValue::from_str(&p), &d.0)
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            JsFuture::from(Promise::from(promise))
                .await
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            Ok(())
        }))
    }

    fn list<'life0, 'life1, 'async_trait>(
        &'life0 self,
        path: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, VfsError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let f = SendMe(self.list_fn.0.clone());
        let p = path.to_owned();
        Box::pin(SendFut(async move {
            let promise = f.0
                .call1(&JsValue::NULL, &JsValue::from_str(&p))
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            let result = JsFuture::from(Promise::from(promise))
                .await
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            Ok(Array::from(&result)
                .iter()
                .filter_map(|v| v.as_string())
                .collect())
        }))
    }

    fn stat<'life0, 'life1, 'async_trait>(
        &'life0 self,
        path: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = Result<Stat, VfsError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let f = SendMe(self.stat_fn.0.clone());
        let p = path.to_owned();
        Box::pin(SendFut(async move {
            let promise = f.0
                .call1(&JsValue::NULL, &JsValue::from_str(&p))
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            let result = JsFuture::from(Promise::from(promise))
                .await
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            let get_bool = |key: &str| -> bool {
                js_sys::Reflect::get(&result, &JsValue::from_str(key))
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            };
            let get_f64 = |key: &str| -> f64 {
                js_sys::Reflect::get(&result, &JsValue::from_str(key))
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
            };
            Ok(Stat {
                is_file:   get_bool("isFile"),
                is_dir:    get_bool("isDir"),
                is_device: false,
                size:      get_f64("size") as u64,
            })
        }))
    }

    fn remove<'life0, 'life1, 'async_trait>(
        &'life0 self,
        path: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = Result<(), VfsError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let f = SendMe(self.remove_fn.0.clone());
        let p = path.to_owned();
        Box::pin(SendFut(async move {
            let promise = f.0
                .call1(&JsValue::NULL, &JsValue::from_str(&p))
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            JsFuture::from(Promise::from(promise))
                .await
                .map_err(|e| VfsError::Mount(format!("{e:?}")))?;
            Ok(())
        }))
    }
}

// ── TypeScript extras ────────────────────────────────────────────────────────
// Injected verbatim into the generated .d.ts by wasm-bindgen.

#[wasm_bindgen(typescript_custom_section)]
const TYPESCRIPT_EXTRAS: &str = r#"
/** Metadata returned by a mount's {@link MountOptions.stat} callback. */
export interface MountStat {
    isFile: boolean;
    isDir: boolean;
    isDevice: boolean;
    size: number;
}

/**
 * Callbacks for a host-provided virtual filesystem overlaid via
 * {@link WasmShell.mount}.  All `path` arguments are relative to the mount
 * root (e.g. `""` = root, `"/foo.txt"` = a file at the root).
 */
export interface MountOptions {
    read(path: string): Promise<Uint8Array>;
    write(path: string, data: Uint8Array): Promise<void>;
    list(path: string): Promise<string[]>;
    stat(path: string): Promise<MountStat>;
    remove(path: string): Promise<void>;
}

/** Result object resolved by {@link WasmShell.exec}. */
export interface ExecResult {
    /** Shell exit code — 0 means success. */
    code: number;
    /** Bytes written to stdout during the command. */
    stdout: Uint8Array;
    /** Bytes written to stderr during the command. */
    stderr: Uint8Array;
}

// ── Declaration merges — refine the generated class types ────────────────────

interface WasmShell {
    /** Overlay a host filesystem onto `virtualPath`. */
    mount(virtualPath: string, options: MountOptions): void;
    /** Execute a shell script string. */
    exec(src: string): Promise<ExecResult>;
    /** Register a JavaScript program callback. */
    addProgram(name: string, callback: (ctx: WasmProgramContext) => Promise<number>): void;
    /** Register a callback invoked with stdout bytes after each `exec` call. */
    onStdout(callback: (data: Uint8Array) => void): void;
    /** Register a callback invoked with stderr bytes after each `exec` call. */
    onStderr(callback: (data: Uint8Array) => void): void;
    /** Returns `true` when no `exec` is currently in progress. */
    isAvailable(): boolean;
}

interface WasmProgramContext {
    /** Full argument vector — `args[0]` is the program name. */
    readonly args: string[];
    /** Snapshot of the shell environment. */
    readonly env: Record<string, string>;
}
"#;

// ── WasmProgramContext ────────────────────────────────────────────────────────

/// Context given to a JavaScript program registered via `Shell.addProgram`.
///
/// Passed as the single argument to the callback:
/// ```js
/// shell.addProgram('my-tool', async (ctx) => {
///   const enc = new TextEncoder();
///   await ctx.writeStdout(enc.encode('hello\n'));
///   return 0;  // exit code
/// });
/// ```
#[wasm_bindgen]
pub struct WasmProgramContext {
    ctx: ProgramContext,
}

#[wasm_bindgen]
impl WasmProgramContext {
    /// Full argument vector.  `args()[0]` is the program name.
    #[wasm_bindgen(getter, skip_typescript)]
    pub fn args(&self) -> Array {
        self.ctx.args.iter().map(|s| JsValue::from_str(s)).collect()
    }

    /// Current working directory.
    #[wasm_bindgen(getter)]
    pub fn cwd(&self) -> String {
        self.ctx.cwd()
    }

    /// Snapshot of the environment as a plain JS object (`{ KEY: "value", … }`).
    #[wasm_bindgen(getter, skip_typescript)]
    pub fn env(&self) -> Object {
        let obj = Object::new();
        for (k, v) in self.ctx.env_snapshot().iter() {
            js_sys::Reflect::set(&obj, &JsValue::from_str(k), &JsValue::from_str(v)).ok();
        }
        obj
    }

    /// Get a single environment variable.
    #[wasm_bindgen(js_name = getEnv)]
    pub fn get_env(&self, key: &str) -> Option<String> {
        self.ctx.get_env(key)
    }

    /// Set an environment variable.  Change is visible to the shell after the
    /// program callback returns.
    #[wasm_bindgen(js_name = setEnv)]
    pub fn set_env(&self, key: &str, value: &str) {
        self.ctx.set_env(key, value);
    }

    /// Unset an environment variable.
    #[wasm_bindgen(js_name = unsetEnv)]
    pub fn unset_env(&self, key: &str) {
        self.ctx.unset_env(key);
    }

    /// Write bytes to the program's stdout.
    #[wasm_bindgen(js_name = writeStdout)]
    pub async fn write_stdout(&self, data: &[u8]) -> Result<(), JsValue> {
        use tokio::io::AsyncWriteExt;
        self.ctx
            .stdout()
            .write_all(data)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Write bytes to the program's stderr.
    #[wasm_bindgen(js_name = writeStderr)]
    pub async fn write_stderr(&self, data: &[u8]) -> Result<(), JsValue> {
        use tokio::io::AsyncWriteExt;
        self.ctx
            .stderr()
            .write_all(data)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Read all available stdin bytes.
    #[wasm_bindgen(js_name = readStdin)]
    pub async fn read_stdin(&mut self) -> Result<Uint8Array, JsValue> {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        self.ctx
            .stdin()
            .read_to_end(&mut buf)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Uint8Array::from(buf.as_slice()))
    }
}

// ── WasmShell ─────────────────────────────────────────────────────────────────

/// A sandboxed shell instance exposed to JavaScript.
///
/// ```js
/// import initWasmShell, { WasmShell } from 'wasm_shell';
///
/// await initWasmShell();
/// const shell = new WasmShell();
/// shell.onStdout(chunk => process.stdout.write(chunk));
/// const result = await shell.exec('echo hello | tr a-z A-Z');
/// console.log(result.code); // 0
/// ```
#[wasm_bindgen]
pub struct WasmShell {
    // Held behind RefCell<Option<>> so that `exec` can move the Shell out
    // before awaiting — letting wasm-bindgen use a shared (&self) borrow for
    // the whole async span.  While exec is running, `inner` is `None`; any
    // re-entrant call sees `None` and returns a graceful error instead of the
    // cryptic "recursive use of an object" panic from wasm-bindgen.
    inner: RefCell<Option<Shell>>,
    stdout_cb: Option<Function>,
    stderr_cb: Option<Function>,
    stdin_bytes: RefCell<Vec<u8>>,
}

#[wasm_bindgen]
impl WasmShell {
    /// Create a new shell with a clean environment and empty VFS.
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmShell {
        WasmShell {
            inner: RefCell::new(Some(Shell::new())),
            stdout_cb: None,
            stderr_cb: None,
            stdin_bytes: RefCell::new(Vec::new()),
        }
    }

    // ── Availability ─────────────────────────────────────────────────────────

    /// Returns `true` when no `exec` is currently in progress.
    ///
    /// ```js
    /// if (!shell.isAvailable()) {
    ///   console.error("shell is busy");
    ///   return;
    /// }
    /// await shell.exec("echo hi");
    /// ```
    #[wasm_bindgen(js_name = isAvailable)]
    pub fn is_available(&self) -> bool {
        self.inner.borrow().is_some()
    }

    // ── Program registration ──────────────────────────────────────────────────

    /// Register a JavaScript program.
    ///
    /// `callback` is called as `async (ctx: WasmProgramContext) => number`
    /// where the return value is the exit code (0 = success).
    ///
    /// Bare names are installed at `/usr/bin/<name>`.
    /// Names containing `/` are stored at that exact path.
    #[wasm_bindgen(js_name = addProgram, skip_typescript)]
    pub fn add_program(&mut self, name: &str, callback: Function) {
        let cb = SendMe(callback);
        self.inner.borrow_mut().as_mut()
            .expect("cannot addProgram while exec is running")
            .add_program(name, move |ctx: ProgramContext| {
                // Clone the JS function handle and wrap as Send (safe: WASM is single-threaded).
                let f = SendMe(cb.0.clone());
                let wasm_ctx = WasmProgramContext { ctx };
                // Convert to JsValue synchronously, then wrap as Send.
                let wasm_ctx_val = SendMe(JsValue::from(wasm_ctx));
                // Return a future that is Send (via SendFut) so it satisfies ProgramFuture.
                SendFut(async move {
                    let promise_val = f
                        .0
                        .call1(&JsValue::NULL, &wasm_ctx_val.0)
                        .map_err(|e| ShellError::Io(format!("addProgram callback error: {e:?}")))?;
                    let result =
                        JsFuture::from(Promise::from(promise_val))
                            .await
                            .map_err(|e| ShellError::Io(format!("addProgram callback failed: {e:?}")))?;
                    let code = result.as_f64().unwrap_or(0.0) as i32;
                    Ok(ExitCode(code))
                })
            });
    }

    // ── Mount API ─────────────────────────────────────────────────────────────

    /// Overlay a host filesystem onto `virtual_path`.
    ///
    /// `options` must be an object with the shape:
    /// ```ts
    /// {
    ///   read:   (path: string) => Promise<Uint8Array>,
    ///   write:  (path: string, data: Uint8Array) => Promise<void>,
    ///   list:   (path: string) => Promise<string[]>,
    ///   stat:   (path: string) => Promise<{ isFile: boolean, isDir: boolean, size: number }>,
    ///   remove: (path: string) => Promise<void>,
    /// }
    /// ```
    #[wasm_bindgen(skip_typescript)]
    pub fn mount(&mut self, virtual_path: &str, options: &JsValue) -> Result<(), JsValue> {
        let get_fn = |key: &str| -> Result<Function, JsValue> {
            let val = js_sys::Reflect::get(options, &JsValue::from_str(key))?;
            if val.is_function() {
                Ok(Function::from(val))
            } else {
                Err(JsValue::from_str(&format!("mount option `{key}` is not a function")))
            }
        };
        let point = Arc::new(JsMount {
            read_fn:   SendMe(get_fn("read")?),
            write_fn:  SendMe(get_fn("write")?),
            list_fn:   SendMe(get_fn("list")?),
            stat_fn:   SendMe(get_fn("stat")?),
            remove_fn: SendMe(get_fn("remove")?),
        });
        self.inner.borrow_mut().as_mut()
            .expect("cannot mount while exec is running")
            .mount(virtual_path, point);
        Ok(())
    }

    // ── Execution ─────────────────────────────────────────────────────────────

    /// Execute a shell script string.
    ///
    /// Resolves with a plain JS object `{ code: number, stdout: Uint8Array, stderr: Uint8Array }`.
    /// Any bytes pre-loaded via `setStdin` are consumed by this call.
    ///
    /// Rejects with an error if called while a previous `exec` is still running.
    /// Use `isAvailable()` to check before calling.
    #[wasm_bindgen(skip_typescript)]
    pub async fn exec(&self, src: &str) -> Result<JsValue, JsValue> {
        // Take the Shell out of the cell.  If it's already None, a previous
        // exec is still in flight — return a clean error instead of panicking.
        let mut shell = self.inner.borrow_mut().take()
            .ok_or_else(|| JsValue::from_str("shell is busy: exec is already running"))?;

        let stdin = std::mem::take(&mut *self.stdin_bytes.borrow_mut());

        // Await with only a local `shell` — no borrow of `self.inner` is held
        // across this point, so wasm-bindgen won't see re-entrant access.
        let result = shell.exec_with_stdin(src, stdin).await;

        // Always return the Shell before propagating any error.
        *self.inner.borrow_mut() = Some(shell);

        let output = result.map_err(|e| JsValue::from_str(&e.to_string()))?;

        // Deliver output to registered callbacks.
        if let Some(cb) = &self.stdout_cb {
            if !output.stdout.is_empty() {
                let data = Uint8Array::from(output.stdout.as_slice());
                cb.call1(&JsValue::NULL, &data).ok();
            }
        }
        if let Some(cb) = &self.stderr_cb {
            if !output.stderr.is_empty() {
                let data = Uint8Array::from(output.stderr.as_slice());
                cb.call1(&JsValue::NULL, &data).ok();
            }
        }

        // Return a plain JS object so callers don't need to manage Rust memory.
        let obj = Object::new();
        let set = |key: &str, val: JsValue| {
            js_sys::Reflect::set(&obj, &JsValue::from_str(key), &val).ok();
        };
        set("code",   JsValue::from_f64(output.code.0 as f64));
        set("stdout", Uint8Array::from(output.stdout.as_slice()).into());
        set("stderr", Uint8Array::from(output.stderr.as_slice()).into());

        Ok(obj.into())
    }

    // ── I/O callbacks ─────────────────────────────────────────────────────────

    /// Register a callback invoked with stdout bytes after each `exec` call.
    /// Signature: `(data: Uint8Array) => void`
    #[wasm_bindgen(js_name = onStdout, skip_typescript)]
    pub fn on_stdout(&mut self, callback: Function) {
        self.stdout_cb = Some(callback);
    }

    /// Register a callback invoked with stderr bytes after each `exec` call.
    /// Signature: `(data: Uint8Array) => void`
    #[wasm_bindgen(js_name = onStderr, skip_typescript)]
    pub fn on_stderr(&mut self, callback: Function) {
        self.stderr_cb = Some(callback);
    }

    /// Pre-load bytes to be provided as stdin for the next `exec` call.
    /// The bytes are consumed on the next `exec`.
    #[wasm_bindgen(js_name = setStdin)]
    pub fn set_stdin(&mut self, data: &[u8]) {
        *self.stdin_bytes.borrow_mut() = data.to_vec();
    }

    // ── VFS access ────────────────────────────────────────────────────────────

    /// Read a file from the virtual filesystem. Returns a `Uint8Array`.
    #[wasm_bindgen(js_name = readFile)]
    pub async fn read_file(&self, path: &str) -> Result<Uint8Array, JsValue> {
        // Clone the Arc so we don't hold the RefCell borrow across the await.
        let fs = self.inner.borrow().as_ref()
            .ok_or_else(|| JsValue::from_str("shell is busy"))?
            .fs.clone();
        let data = fs.lock().await
            .read_file(path)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Uint8Array::from(data.as_slice()))
    }

    /// Write bytes to a file in the virtual filesystem.
    #[wasm_bindgen(js_name = writeFile)]
    pub async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), JsValue> {
        let fs = self.inner.borrow().as_ref()
            .ok_or_else(|| JsValue::from_str("shell is busy"))?
            .fs.clone();
        let result = fs.lock().await
            .write_file(path, data.to_vec())
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()));
        result
    }

    // ── Environment ───────────────────────────────────────────────────────────

    /// Set an environment variable on the shell.
    #[wasm_bindgen(js_name = setEnv)]
    pub fn set_env(&mut self, key: &str, value: &str) {
        self.inner.borrow_mut().as_mut()
            .expect("cannot setEnv while exec is running")
            .set_env(key, value);
    }

    /// Get an environment variable from the shell.
    #[wasm_bindgen(js_name = getEnv)]
    pub fn get_env(&self, key: &str) -> Option<String> {
        self.inner.borrow().as_ref()?.get_env(key).map(|s| s.to_owned())
    }

    /// Get the current working directory.
    #[wasm_bindgen(js_name = getCwd)]
    pub fn get_cwd(&self) -> String {
        self.inner.borrow().as_ref().map(|s| s.cwd.clone()).unwrap_or_default()
    }

    /// Set the current working directory (does not validate against VFS).
    #[wasm_bindgen(js_name = setCwd)]
    pub fn set_cwd(&mut self, path: &str) {
        if let Some(shell) = self.inner.borrow_mut().as_mut() {
            shell.cwd = crate::vfs::normalize_path(path);
        }
    }
}

impl Default for WasmShell {
    fn default() -> Self {
        Self::new()
    }
}
