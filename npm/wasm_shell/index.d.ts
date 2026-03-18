/**
 * wasm_shell — TypeScript type declarations
 *
 * These types describe the public API of the `WasmShell` and
 * `WasmProgramContext` types exported by the compiled WASM module.
 *
 * Import after initialising the WASM module:
 *
 * ```ts
 * import { WasmShell } from 'wasm-shell';
 * const shell = new WasmShell();
 * ```
 */

// ── Supporting types ──────────────────────────────────────────────────────────

/** Filesystem entry metadata returned by a mount's `stat` callback. */
export interface StatInfo {
  isFile: boolean;
  isDir: boolean;
  size: number;
}

/**
 * Options passed to {@link WasmShell.mount}.
 *
 * All paths supplied to the callbacks are **relative to `virtualPath`**.
 * For example, if mounted at `/home/agent`, reading `/home/agent/foo.txt`
 * passes `"/foo.txt"` to `read`.
 */
export interface MountOptions {
  /** The virtual path in the VFS where the host filesystem is overlaid. */
  virtualPath: string;
  /** Read a file.  Must resolve to a `Uint8Array`. */
  read(path: string): Promise<Uint8Array>;
  /** Write a file. */
  write(path: string, data: Uint8Array): Promise<void>;
  /** List a directory.  Must resolve to an array of entry names (not full paths). */
  list(path: string): Promise<string[]>;
  /** Stat a path. */
  stat(path: string): Promise<StatInfo>;
  /** Remove a file or directory. */
  remove(path: string): Promise<void>;
}

/**
 * Context object passed as the sole argument to a program callback registered
 * via {@link WasmShell.addProgram}.
 *
 * ```ts
 * shell.addProgram('greet', async (ctx) => {
 *   const enc = new TextEncoder();
 *   const name = ctx.args[1] ?? 'world';
 *   await ctx.writeStdout(enc.encode(`Hello, ${name}!\n`));
 *   return 0;
 * });
 * ```
 */
export interface ProgramContext {
  /**
   * Full argument vector.  `args[0]` is the program name,
   * `args[1]` onwards are the user-supplied arguments.
   */
  readonly args: string[];

  /** Current working directory at the time of invocation. */
  readonly cwd: string;

  /**
   * Snapshot of the environment as a plain object.
   * Mutations here do **not** propagate back; use {@link setEnv} instead.
   */
  readonly env: Record<string, string>;

  /** Get the value of a single environment variable. */
  getEnv(key: string): string | undefined;

  /**
   * Set an environment variable.
   * The change is visible to the parent shell after the callback returns.
   */
  setEnv(key: string, value: string): void;

  /** Unset an environment variable. */
  unsetEnv(key: string): void;

  /** Write bytes to the program's stdout. */
  writeStdout(data: Uint8Array): Promise<void>;

  /** Write bytes to the program's stderr. */
  writeStderr(data: Uint8Array): Promise<void>;

  /**
   * Read all available stdin bytes.
   * Returns an empty `Uint8Array` if no stdin was provided.
   */
  readStdin(): Promise<Uint8Array>;
}

/**
 * The callback signature for programs registered via
 * {@link WasmShell.addProgram}.
 *
 * @returns The exit code.  `0` means success; any other value is treated as
 *          failure and affects `&&`/`||` chaining in scripts.
 */
export type ProgramCallback = (ctx: ProgramContext) => Promise<number>;

/**
 * Result returned by {@link WasmShell.exec}.
 *
 * This is a plain JS object — it does **not** need to be `free()`'d.
 */
export interface ExecResult {
  /** Exit code of the last command in the script. */
  code: number;
  /** Bytes written to stdout. */
  stdout: Uint8Array;
  /** Bytes written to stderr. */
  stderr: Uint8Array;
}

// ── WasmShell ─────────────────────────────────────────────────────────────────

/**
 * A sandboxed, embeddable shell instance.
 *
 * Each `WasmShell` has its own isolated environment: virtual filesystem,
 * environment variables, current working directory, and program registry.
 *
 * @example Basic usage
 * ```ts
 * import { WasmShell } from 'wasm-shell';
 *
 * const shell = new WasmShell();
 * const dec = new TextDecoder();
 *
 * const result = await shell.exec('echo hello | tr a-z A-Z');
 * console.log(dec.decode(result.stdout)); // "HELLO\n"
 * ```
 *
 * @example Registering a custom program
 * ```ts
 * const enc = new TextEncoder();
 * shell.addProgram('fetch-text', async (ctx) => {
 *   const url = ctx.args[1];
 *   if (!url) { await ctx.writeStderr(enc.encode('Usage: fetch-text <url>\n')); return 1; }
 *   const text = await fetch(url).then(r => r.text());
 *   await ctx.writeStdout(enc.encode(text));
 *   return 0;
 * });
 * const r = await shell.exec('fetch-text https://example.com | grep -i title');
 * ```
 */
export declare class WasmShell {
  constructor();

  // ── Program registration ────────────────────────────────────────────────────

  /**
   * Register a virtual program that can be invoked from shell scripts.
   *
   * - Bare names (e.g. `"my-tool"`) are installed at `/usr/bin/my-tool`.
   * - Names containing `/` (e.g. `"/usr/local/bin/my-tool"`) are stored at
   *   that exact path.
   *
   * The callback receives a {@link ProgramContext} and must return a `Promise`
   * that resolves to the exit code.
   */
  addProgram(name: string, callback: ProgramCallback): void;

  // ── Mount API ───────────────────────────────────────────────────────────────

  /**
   * Overlay a host-provided filesystem onto a virtual path.
   *
   * All five callback fields (`read`, `write`, `list`, `stat`, `remove`) are
   * required.  Paths passed to the callbacks are relative to `virtualPath`.
   *
   * @throws If any of the required callback fields are missing or not functions.
   */
  mount(virtualPath: string, options: Omit<MountOptions, "virtualPath">): void;

  // ── Execution ───────────────────────────────────────────────────────────────

  /**
   * Parse and execute a shell script string.
   *
   * Resolves with an {@link ExecResult} plain object containing the exit code
   * and captured stdout / stderr bytes.
   *
   * Any bytes previously supplied via {@link setStdin} are consumed as stdin
   * for this call.
   *
   * @throws A `string` error message if the script cannot be parsed.
   */
  exec(src: string): Promise<ExecResult>;

  // ── I/O callbacks ───────────────────────────────────────────────────────────

  /**
   * Register a callback that is invoked with the stdout bytes after each
   * {@link exec} call.  Called at most once per `exec` with the full output.
   *
   * @example
   * ```ts
   * const dec = new TextDecoder();
   * shell.onStdout(chunk => process.stdout.write(dec.decode(chunk)));
   * ```
   */
  onStdout(callback: (data: Uint8Array) => void): void;

  /**
   * Register a callback that is invoked with the stderr bytes after each
   * {@link exec} call.
   */
  onStderr(callback: (data: Uint8Array) => void): void;

  /**
   * Pre-load bytes to use as stdin for the **next** {@link exec} call.
   * The bytes are consumed on the next `exec` and do not persist beyond it.
   *
   * @example
   * ```ts
   * const enc = new TextEncoder();
   * shell.setStdin(enc.encode('hello world\n'));
   * const r = await shell.exec('wc -w');
   * ```
   */
  setStdin(data: Uint8Array): void;

  // ── VFS access ──────────────────────────────────────────────────────────────

  /**
   * Read a file from the virtual filesystem.
   * @throws If the file does not exist or is not readable.
   */
  readFile(path: string): Promise<Uint8Array>;

  /**
   * Write bytes to a file in the virtual filesystem, creating it if needed.
   * @throws On VFS errors (e.g. parent directory does not exist).
   */
  writeFile(path: string, data: Uint8Array): Promise<void>;

  // ── Environment ─────────────────────────────────────────────────────────────

  /** Set an environment variable on the shell. */
  setEnv(key: string, value: string): void;

  /** Get an environment variable from the shell.  Returns `undefined` if unset. */
  getEnv(key: string): string | undefined;

  /** Get the current working directory. */
  getCwd(): string;

  /** Set the current working directory (does not validate against VFS). */
  setCwd(path: string): void;

  // ── Lifecycle ───────────────────────────────────────────────────────────────

  /**
   * Release the WASM memory associated with this shell instance.
   * The object must not be used after calling `free()`.
   */
  free(): void;
}

// ── WasmProgramContext (exported for advanced use) ─────────────────────────────

/**
 * The concrete WASM-backed type passed to program callbacks.
 * Implements the {@link ProgramContext} interface.
 *
 * You normally don't need to import this type — annotate callbacks with
 * `ProgramContext` instead.  The runtime object you receive will have the
 * correct shape.
 */
export declare class WasmProgramContext implements ProgramContext {
  readonly args: string[];
  readonly cwd: string;
  readonly env: Record<string, string>;
  getEnv(key: string): string | undefined;
  setEnv(key: string, value: string): void;
  unsetEnv(key: string): void;
  writeStdout(data: Uint8Array): Promise<void>;
  writeStderr(data: Uint8Array): Promise<void>;
  readStdin(): Promise<Uint8Array>;
  free(): void;
}
