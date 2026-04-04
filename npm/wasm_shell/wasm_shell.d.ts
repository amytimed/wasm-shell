/* tslint:disable */
/* eslint-disable */

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
}

interface WasmProgramContext {
    /** Full argument vector — `args[0]` is the program name. */
    readonly args: string[];
    /** Snapshot of the shell environment. */
    readonly env: Record<string, string>;
}



/**
 * Context given to a JavaScript program registered via `Shell.addProgram`.
 *
 * Passed as the single argument to the callback:
 * ```js
 * shell.addProgram('my-tool', async (ctx) => {
 *   const enc = new TextEncoder();
 *   await ctx.writeStdout(enc.encode('hello\n'));
 *   return 0;  // exit code
 * });
 * ```
 */
export class WasmProgramContext {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Get a single environment variable.
     */
    getEnv(key: string): string | undefined;
    /**
     * Read all available stdin bytes.
     */
    readStdin(): Promise<Uint8Array>;
    /**
     * Set an environment variable.  Change is visible to the shell after the
     * program callback returns.
     */
    setEnv(key: string, value: string): void;
    /**
     * Unset an environment variable.
     */
    unsetEnv(key: string): void;
    /**
     * Write bytes to the program's stderr.
     *
     * This is synchronous — `VecWriter` is always immediately ready.
     * You may still `await` the call from JavaScript (it resolves instantly).
     */
    writeStderr(data: Uint8Array): void;
    /**
     * Write bytes to the program's stdout.
     *
     * This is synchronous — `VecWriter` is always immediately ready.
     * You may still `await` the call from JavaScript (it resolves instantly).
     */
    writeStdout(data: Uint8Array): void;
    /**
     * Current working directory.
     */
    readonly cwd: string;
}

/**
 * A sandboxed shell instance exposed to JavaScript.
 *
 * ```js
 * import initWasmShell, { WasmShell } from 'wasm_shell';
 *
 * await initWasmShell();
 * const shell = new WasmShell();
 * shell.onStdout(chunk => process.stdout.write(chunk));
 * const result = await shell.exec('echo hello | tr a-z A-Z');
 * console.log(result.code); // 0
 * ```
 */
export class WasmShell {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Get the current working directory.
     */
    getCwd(): string;
    /**
     * Get an environment variable from the shell.
     */
    getEnv(key: string): string | undefined;
    /**
     * Create a new shell with a clean environment and empty VFS.
     */
    constructor();
    /**
     * Read a file from the virtual filesystem. Returns a `Uint8Array`.
     */
    readFile(path: string): Promise<Uint8Array>;
    /**
     * Set the current working directory (does not validate against VFS).
     */
    setCwd(path: string): void;
    /**
     * Set an environment variable on the shell.
     */
    setEnv(key: string, value: string): void;
    /**
     * Pre-load bytes to be provided as stdin for the next `exec` call.
     * The bytes are consumed on the next `exec`.
     */
    setStdin(data: Uint8Array): void;
    /**
     * Write bytes to a file in the virtual filesystem.
     */
    writeFile(path: string, data: Uint8Array): Promise<void>;
}
