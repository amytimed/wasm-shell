# wasm-shell

A sandboxed, embeddable WASM shell

## Intro
This is a virtual shell, with no real disk or processes, except for ones you explicitly define yourself. Use in Bun, Node, or the browser.

## Install
```sh
bun i wasm-shell
```

## Quickstart
```ts
import { WasmShell } from 'wasm-shell';

const shell = new WasmShell();
const dec = new TextDecoder();

const result = await shell.exec('echo hello world | tr a-z A-Z');
console.log(dec.decode(result.stdout)); // "HELLO WORLD\n"
console.log(result.code); // 0
```

---

## API

### `new WasmShell()`

Create a new isolated shell.  Each instance has its own environment
variables, working directory, virtual filesystem, and program registry.

### `shell.exec(src): Promise<ExecResult>`

Parse and execute a shell script string.  Returns a plain object:

```ts
interface ExecResult {
  code:   number;     // exit code
  stdout: Uint8Array; // captured stdout
  stderr: Uint8Array; // captured stderr
}
```

Throws a `string` error if the script cannot be parsed.

### `shell.addProgram(name, callback)`

Register a virtual program.  The callback receives a `ProgramContext`:

```ts
const enc = new TextEncoder();
shell.addProgram('my-tool', async (ctx) => {
  const name = ctx.args[1] ?? 'world';
  await ctx.writeStdout(enc.encode(`Hello, ${name}!\n`));
  return 0; // exit code
});

await shell.exec('my-tool Alice | grep Alice');
```

`ProgramContext` fields: `args`, `cwd`, `env`, `getEnv`, `setEnv`,
`unsetEnv`, `writeStdout`, `writeStderr`, `readStdin`.

### `shell.mount(virtualPath, options)`

Overlay a host filesystem onto a virtual path:

```ts
import { readFile, writeFile, readdir, rm, stat } from 'fs/promises';
import { join } from 'path';

const ROOT = '/tmp/my-workspace';
shell.mount('/home', {
  read:   (p) => readFile(join(ROOT, p)),
  write:  async (p, d) => { await writeFile(join(ROOT, p), d); },
  list:   async (p) => (await readdir(join(ROOT, p))).map(e => e.name ?? e),
  stat:   async (p) => { const s = await stat(join(ROOT, p)); return { isFile: s.isFile(), isDir: s.isDirectory(), isDevice: false, size: s.size }; },
  remove: (p) => rm(join(ROOT, p), { recursive: true, force: true }),
});
```

Callback paths are relative to `virtualPath` (e.g. mounting at `/home` and reading `/home/foo.txt` passes `"/foo.txt"`).

### `shell.onStdout(cb)` / `shell.onStderr(cb)`

Register streaming output callbacks.  Called once per `exec` with the
full output buffer:

```ts
shell.onStdout(chunk => process.stdout.write(dec.decode(chunk)));
shell.onStderr(chunk => process.stderr.write(dec.decode(chunk)));
```

### `shell.setStdin(data)` / `shell.readFile(path)` / `shell.writeFile(path, data)`

Pre-load stdin bytes or directly access the virtual filesystem.

### `shell.setEnv(key, value)` / `shell.getEnv(key)` / `shell.getCwd()`

Inspect and modify the shell's environment.

---

## Built-in commands

All standard POSIX utilities are available without PATH resolution:

`echo`, `cat`, `ls`, `cd`, `pwd`, `mkdir`, `rm`, `mv`, `cp`, `touch`,
`find`, `grep`, `sed`, `head`, `tail`, `wc`, `sort`, `uniq`, `cut`,
`tr`, `xargs`, `env`, `export`, `unset`, `test`, `true`, `false`,
`printf`, `read`, `exit`, `sleep`, `which`, `type`, `source`/`.`,
`alias`/`unalias`.

---

## Supported shell syntax

Pipelines (`|`), redirections (`>`, `>>`, `<`, `2>`, `2>&1`), logical
operators (`&&`, `||`), sequences (`;`), variable and command
substitution (`$VAR`, `$(cmd)`), heredocs, subshells `( )`, groups
`{ }`.

**Not supported:** `if`/`for`/`while`/`case` (use `&&`/`||` instead),
shell functions, process substitution.

---

## Limitations

- Pipeline stages currently run sequentially (not concurrently) to keep the VFS model simple. True concurrent pipelines are planned.

---

## Building from source

Requires [wasm-pack](https://rustwasm.github.io/wasm-pack/) and Rust with
the `wasm32-unknown-unknown` target:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# from the repo root:
npm run build --prefix npm/wasm_shell
```
