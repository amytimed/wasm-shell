/**
 * wasm_shell - interactive REPL
 * Run:  bun run interactive   (or: bun bun-interactive.ts)
 */

import { WasmShell } from "../../crates/wasm_shell/pkg/wasm_shell.js";
import { mkdir, readdir, readFile, writeFile, rm, stat as fsStat } from "fs/promises";
import { join } from "path";

const dec = new TextDecoder();

const shell = new WasmShell();

// ── workspace mount ───────────────────────────────────────────────────────────
// Mount a real on-disk directory at ~/workspace/ so the user has a persistent
// writable home directory.  The directory is created on first run.

const WORKSPACE_DIR = new URL("./workspace", import.meta.url).pathname;
await mkdir(WORKSPACE_DIR, { recursive: true });

// Paths passed by the VFS are relative to the mount root:
//   ""          → the root of the mount  (= WORKSPACE_DIR)
//   "/foo.txt"  → WORKSPACE_DIR + "/foo.txt"
const toReal = (rel: string) => join(WORKSPACE_DIR, rel);

shell.mount("/home/", {
  async read(path) {
    return readFile(toReal(path));
  },
  async write(path, data) {
    const full = toReal(path);
    await mkdir(full.substring(0, full.lastIndexOf("/")), { recursive: true });
    await writeFile(full, data);
  },
  async list(path) {
    const entries = await readdir(toReal(path), { withFileTypes: true });
    return entries.map(e => e.name);
  },
  async stat(path) {
    const s = await fsStat(toReal(path));
    return { isFile: s.isFile(), isDir: s.isDirectory(), isDevice: false, size: s.size };
  },
  async remove(path) {
    await rm(toReal(path), { recursive: true, force: true });
  },
});

shell.setEnv("HOME", "/home");
shell.setCwd("/home");

// colour helpers (ANSI, stripped when stdout is not a tty)
const isTTY = process.stdout.isTTY ?? false;
const c = (code: string, s: string) => (isTTY ? `\x1b[${code}m${s}\x1b[0m` : s);
const bold   = (s: string) => c("1",  s);
const dim    = (s: string) => c("2",  s);
const cyan   = (s: string) => c("36", s);
const green  = (s: string) => c("32", s);
const red    = (s: string) => c("31", s);
const yellow = (s: string) => c("33", s);

function prompt(): string {
  const home = shell.getEnv("HOME") ?? "/home";
  const cwd  = shell.getCwd();
  const display = cwd === home
    ? "~"
    : cwd.startsWith(home + "/")
      ? "~" + cwd.slice(home.length)
      : cwd;
  return `${bold(cyan("wasm_shell"))} ${dim(display)} ${green(">")} `;
}

shell.onStdout((data: Uint8Array) => { if (data.length) process.stdout.write(data); });
shell.onStderr((data: Uint8Array) => { if (data.length) process.stderr.write(data); });

const REPL_HELP = `
${bold("Shell built-ins:")}
  echo, cat, ls, cd, pwd, mkdir, rm, mv, cp, touch
  head, tail, wc, grep, sed, sort, uniq, cut, tr
  xargs, env, export, unset, test, true, false
  printf, read, exit, sleep, which, type, source, alias
  uname, neofetch

${bold("Pipelines & redirections:")}
  cmd1 | cmd2        pipe
  cmd > file         redirect stdout
  cmd >> file        append stdout
  cmd < file         stdin from file
  cmd1 && cmd2       run cmd2 only on success
  cmd1 || cmd2       run cmd2 only on failure
  cmd1; cmd2         sequence

${bold("REPL meta-commands:")}
  help               show this help
  clear              clear the screen
  .exit              quit (same as exit / Ctrl-D)
`;

async function* readLines(): AsyncGenerator<string> {
  let buf = "";
  for await (const chunk of Bun.stdin.stream()) {
    buf += dec.decode(chunk);
    const lines = buf.split("\n");
    buf = lines.pop()!;
    for (const line of lines) yield line;
  }
  if (buf) yield buf;
}

// Returns true when the loop should stop.
async function handleLine(line: string): Promise<boolean> {
  const cmd = line.trim();
  if (!cmd || cmd.startsWith("#")) return false;
  if (cmd === "help")  { process.stdout.write(REPL_HELP); return false; }
  if (cmd === "clear") { process.stdout.write("\x1b[2J\x1b[H"); return false; }
  if (cmd === ".exit") return true;

  try {
    const result = await shell.exec(cmd);
    if (result.code !== 0) {
      process.stderr.write(dim(`[exit ${result.code}]\n`));
    }
    if (cmd === "exit" || cmd.startsWith("exit ")) return true;
  } catch (err: any) {
    process.stderr.write(red(`error: ${err?.message ?? String(err)}\n`));
  }
  return false;
}

async function ttyLoop() {
  const { createInterface } = await import("readline");
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const ask = () => new Promise<string | null>((resolve) => {
    const onClose = () => resolve(null);
    rl.question(prompt(), (answer) => {
      rl.removeListener("close", onClose);
      resolve(answer);
    });
    rl.once("close", onClose);
  });
  while (true) {
    const line = await ask();
    if (line === null) break;
    const done = await handleLine(line);
    if (done) break;
  }
  rl.close();
}

async function pipedLoop() {
  // Buffer all of stdin first so that when Bun delivers everything in one
  // chunk the lines are still processed one-at-a-time with each exec()
  // completing before the next prompt is shown.
  let buf = "";
  for await (const chunk of Bun.stdin.stream()) {
    buf += dec.decode(chunk);
  }
  for (const line of buf.split("\n")) {
    process.stdout.write(prompt() + line + "\n");
    const done = await handleLine(line);
    if (done) break;
  }
}

process.stdout.write(`
${bold("wasm_shell")} interactive REPL  ${dim("(wasm + Bun)")}
Type ${yellow("help")} to list built-ins.  ${dim("Ctrl-D or \`exit\` to quit.")}
Persistent workspace mounted at ${cyan("~")}

`);

if (isTTY) {
  await ttyLoop();
  process.stdout.write("\nBye!\n");
} else {
  await pipedLoop();
}

process.exit(0);
