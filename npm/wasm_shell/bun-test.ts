/**
 * wasm_shell — Bun integration test
 *
 * Run:  bun run bun-test
 *       (or directly: bun bun-test.ts)
 *
 * Exercises WasmShell through the compiled WASM binary.
 * Each test is a plain async function; failures throw.
 */

import { WasmShell } from "../../crates/wasm_shell/pkg/wasm_shell.js";

// ── Helpers ───────────────────────────────────────────────────────────────────

const enc = new TextEncoder();
const dec = new TextDecoder();

function str(u: Uint8Array): string {
  return dec.decode(u);
}

let passed = 0;
let failed = 0;
const failures: string[] = [];

async function test(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn();
    console.log(`  ✓  ${name}`);
    passed++;
  } catch (err: any) {
    const msg = err?.message ?? String(err);
    console.error(`  ✗  ${name}\n       ${msg}`);
    failed++;
    failures.push(`${name}: ${msg}`);
  }
}

function expect(actual: unknown, expected: unknown, label = "") {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${label ? label + ": " : ""}expected ${e}, got ${a}`);
}

function expectContains(haystack: string, needle: string) {
  if (!haystack.includes(needle))
    throw new Error(`expected output to contain ${JSON.stringify(needle)}, got ${JSON.stringify(haystack)}`);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

console.log("\nwasm_shell — Bun integration tests\n");
console.log("── Builtins ────────────────────────────────");

// echo
await test("echo basic", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo hello world");
  expect(str(r.stdout), "hello world\n");
  expect(r.code, 0);
});

await test("echo -n (no newline)", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo -n hi");
  expect(str(r.stdout), "hi");
});

await test("echo -e (escape sequences)", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo -e 'a\\tb'");
  expect(str(r.stdout), "a\tb\n");
});

await test("pwd (default /)", async () => {
  const s = new WasmShell();
  const r = await s.exec("pwd");
  expect(str(r.stdout), "/\n");
});

await test("cd + pwd", async () => {
  const s = new WasmShell();
  await s.exec("cd /tmp");
  expect(s.getCwd(), "/tmp");
  const r = await s.exec("pwd");
  expect(str(r.stdout), "/tmp\n");
});

await test("mkdir + ls", async () => {
  const s = new WasmShell();
  await s.exec("mkdir /tmp/mydir");
  const r = await s.exec("ls /tmp");
  expectContains(str(r.stdout), "mydir");
});

await test("touch + stat via readFile", async () => {
  const s = new WasmShell();
  await s.exec("touch /tmp/f.txt");
  const data = await s.readFile("/tmp/f.txt");
  expect(data.length, 0);
});

await test("cat file", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/cat.txt", enc.encode("hello\n"));
  const r = await s.exec("cat /tmp/cat.txt");
  expect(str(r.stdout), "hello\n");
});

await test("rm file", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/del.txt", enc.encode("x"));
  await s.exec("rm /tmp/del.txt");
  let threw = false;
  try { await s.readFile("/tmp/del.txt"); } catch { threw = true; }
  if (!threw) throw new Error("file should have been removed");
});

await test("rm -r recursive", async () => {
  const s = new WasmShell();
  await s.exec("mkdir -p /tmp/d/sub");
  await s.exec("touch /tmp/d/sub/f.txt");
  await s.exec("rm -r /tmp/d");
  let threw = false;
  try { await s.readFile("/tmp/d"); } catch { threw = true; }
  if (!threw) throw new Error("directory should have been removed");
});

await test("mv rename", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/a.txt", enc.encode("data"));
  await s.exec("mv /tmp/a.txt /tmp/b.txt");
  const data = await s.readFile("/tmp/b.txt");
  expect(str(data), "data");
  let threw = false;
  try { await s.readFile("/tmp/a.txt"); } catch { threw = true; }
  if (!threw) throw new Error("source should no longer exist");
});

await test("cp file", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/src.txt", enc.encode("copy me"));
  await s.exec("cp /tmp/src.txt /tmp/dst.txt");
  const data = await s.readFile("/tmp/dst.txt");
  expect(str(data), "copy me");
});

await test("head -n 3", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/nums.txt", enc.encode("1\n2\n3\n4\n5\n"));
  const r = await s.exec("head -n 3 /tmp/nums.txt");
  expect(str(r.stdout), "1\n2\n3\n");
});

await test("tail -n 2", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/t.txt", enc.encode("1\n2\n3\n4\n5\n"));
  const r = await s.exec("tail -n 2 /tmp/t.txt");
  expect(str(r.stdout), "4\n5\n");
});

await test("wc -l", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/wc.txt", enc.encode("a\nb\nc\n"));
  const r = await s.exec("wc -l /tmp/wc.txt");
  expectContains(str(r.stdout).trim(), "3");
});

await test("grep basic", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/g.txt", enc.encode("foo\nbar\nfoo2\n"));
  const r = await s.exec("grep foo /tmp/g.txt");
  expect(str(r.stdout), "foo\nfoo2\n");
});

await test("grep -v (invert)", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/g2.txt", enc.encode("foo\nbar\n"));
  const r = await s.exec("grep -v foo /tmp/g2.txt");
  expect(str(r.stdout), "bar\n");
});

await test("sed substitute", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/s.txt", enc.encode("hello world\n"));
  const r = await s.exec("sed 's/world/wasm/' /tmp/s.txt");
  expect(str(r.stdout), "hello wasm\n");
});

await test("export persists", async () => {
  const s = new WasmShell();
  await s.exec("export FOO=bar");
  expect(s.getEnv("FOO"), "bar");
});

await test("unset removes var", async () => {
  const s = new WasmShell();
  s.setEnv("X", "1");
  await s.exec("unset X");
  expect(s.getEnv("X"), undefined);
});

await test("env prints variables", async () => {
  const s = new WasmShell();
  s.setEnv("MY_VAR", "hello");
  const r = await s.exec("env");
  expectContains(str(r.stdout), "MY_VAR=hello");
});

await test("test -f (file exists)", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/x.txt", enc.encode(""));
  const r = await s.exec("test -f /tmp/x.txt && echo yes");
  expect(str(r.stdout), "yes\n");
});

await test("test string equality", async () => {
  const s = new WasmShell();
  const r = await s.exec("test hello = hello && echo yes");
  expect(str(r.stdout), "yes\n");
});

await test("test numeric comparison", async () => {
  const s = new WasmShell();
  const r = await s.exec("test 3 -gt 2 && echo yes");
  expect(str(r.stdout), "yes\n");
});

await test("true / false exit codes", async () => {
  const s = new WasmShell();
  const t = await s.exec("true");
  expect(t.code, 0);
  const f = await s.exec("false");
  expect(f.code, 1);
});

await test("printf formatting", async () => {
  const s = new WasmShell();
  const r = await s.exec('printf "%s=%d\\n" answer 42');
  expect(str(r.stdout), "answer=42\n");
});

await test("exit code returned", async () => {
  const s = new WasmShell();
  const r = await s.exec("exit 7");
  expect(r.code, 7);
});

await test("source script", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/script.sh", enc.encode("export SOURCED=yes\n"));
  await s.exec("source /tmp/script.sh");
  expect(s.getEnv("SOURCED"), "yes");
});

await test("which echo", async () => {
  const s = new WasmShell();
  const r = await s.exec("which echo");
  expect(str(r.stdout), "/usr/bin/echo\n");
});

await test("cd with no args falls back to $HOME", async () => {
  const s = new WasmShell();
  s.setEnv("HOME", "/home");
  await s.exec("cd /tmp");
  await s.exec("cd");
  expect(s.getCwd(), "/home");
});

console.log("\n── Pipelines & redirections ────────────────");

await test("pipeline two stages", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo 'hello world' | tr a-z A-Z");
  expect(str(r.stdout), "HELLO WORLD\n");
});

await test("pipeline three stages", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/p.txt", enc.encode("banana\napple\ncherry\n"));
  const r = await s.exec("cat /tmp/p.txt | sort | head -n 2");
  expect(str(r.stdout), "apple\nbanana\n");
});

await test("redirect stdout to file", async () => {
  const s = new WasmShell();
  await s.exec("echo written > /tmp/out.txt");
  const data = await s.readFile("/tmp/out.txt");
  expect(str(data), "written\n");
});

await test("redirect append >>", async () => {
  const s = new WasmShell();
  await s.exec("echo line1 > /tmp/app.txt");
  await s.exec("echo line2 >> /tmp/app.txt");
  const data = await s.readFile("/tmp/app.txt");
  expect(str(data), "line1\nline2\n");
});

await test("redirect stdin from file", async () => {
  const s = new WasmShell();
  await s.writeFile("/tmp/stdin.txt", enc.encode("piped\n"));
  const r = await s.exec("cat < /tmp/stdin.txt");
  expect(str(r.stdout), "piped\n");
});

await test("sort pipeline", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo -e 'c\\na\\nb' | sort");
  expect(str(r.stdout), "a\nb\nc\n");
});

await test("cut fields", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo 'a:b:c' | cut -d: -f2");
  expect(str(r.stdout), "b\n");
});

await test("sed global replace in pipeline", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo aaa | sed 's/a/b/g'");
  expect(str(r.stdout), "bbb\n");
});

await test("&& short-circuit: left fails, right skipped", async () => {
  const s = new WasmShell();
  const r = await s.exec("false && echo SHOULD_NOT_PRINT");
  expect(str(r.stdout), "");
  expect(r.code, 1);
});

await test("|| short-circuit: left fails, right runs", async () => {
  const s = new WasmShell();
  const r = await s.exec("false || echo fallback");
  expect(str(r.stdout), "fallback\n");
});

await test("sequence ; runs both", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo first; echo second");
  expect(str(r.stdout), "first\nsecond\n");
});

await test("stderr redirect 2>&1", async () => {
  const s = new WasmShell();
  const r = await s.exec("cat /nonexistent 2>&1");
  // Error message should land on stdout (merged)
  expect(r.code, 1);
  expect(r.stderr.length, 0, "stderr should be empty after 2>&1");
  expect(r.stdout.length > 0, true, "stdout should have the error text");
});

console.log("\n── Custom programs ──────────────────────────");

await test("addProgram basic", async () => {
  const s = new WasmShell();
  s.addProgram("greet", async (ctx: any) => {
    const name = ctx.args[1] ?? "world";
    await ctx.writeStdout(enc.encode(`Hello, ${name}!\n`));
    return 0;
  });
  const r = await s.exec("greet Alice");
  expect(str(r.stdout), "Hello, Alice!\n");
});

await test("addProgram reads stdin via pipeline", async () => {
  const s = new WasmShell();
  s.addProgram("shout", async (ctx: any) => {
    const data = await ctx.readStdin();
    await ctx.writeStdout(enc.encode(str(data).toUpperCase()));
    return 0;
  });
  const r = await s.exec("echo 'hello' | shout");
  expect(str(r.stdout), "HELLO\n");
});

await test("addProgram setEnv propagates to shell", async () => {
  const s = new WasmShell();
  s.addProgram("setter", async (ctx: any) => {
    ctx.setEnv("SET_BY_PROG", "yes");
    return 0;
  });
  await s.exec("setter");
  expect(s.getEnv("SET_BY_PROG"), "yes");
});

await test("addProgram non-zero exit code", async () => {
  const s = new WasmShell();
  s.addProgram("fail42", async (_ctx: any) => 42);
  const r = await s.exec("fail42");
  expect(r.code, 42);
});

await test("addProgram in pipeline with grep", async () => {
  const s = new WasmShell();
  s.addProgram("gen", async (ctx: any) => {
    await ctx.writeStdout(enc.encode("foo\nbar\nfoo2\n"));
    return 0;
  });
  const r = await s.exec("gen | grep foo");
  expect(str(r.stdout), "foo\nfoo2\n");
});

console.log("\n── VFS & I/O API ────────────────────────────");

await test("writeFile + readFile roundtrip", async () => {
  const s = new WasmShell();
  const payload = enc.encode("binary\x00data");
  await s.writeFile("/tmp/bin.dat", payload);
  const back = await s.readFile("/tmp/bin.dat");
  expect(back.join(","), payload.join(","));
});

await test("onStdout callback", async () => {
  const s = new WasmShell();
  let received = "";
  s.onStdout((chunk: Uint8Array) => { received += str(chunk); });
  await s.exec("echo callback");
  expect(received, "callback\n");
});

await test("onStderr callback", async () => {
  const s = new WasmShell();
  let received = "";
  s.onStderr((chunk: Uint8Array) => { received += str(chunk); });
  await s.exec("cat /does_not_exist");
  expect(received.length > 0, true);
});

await test("setStdin consumed by exec", async () => {
  const s = new WasmShell();
  s.setStdin(enc.encode("from stdin\n"));
  const r = await s.exec("cat");
  expect(str(r.stdout), "from stdin\n");
});

await test("setEnv / getEnv", async () => {
  const s = new WasmShell();
  s.setEnv("CUSTOM", "42");
  expect(s.getEnv("CUSTOM"), "42");
  expect(s.getEnv("NOPE"), undefined);
});

await test("getCwd default is /", async () => {
  const s = new WasmShell();
  expect(s.getCwd(), "/");
});

await test("getCwd updates after cd", async () => {
  const s = new WasmShell();
  await s.exec("cd /tmp");
  expect(s.getCwd(), "/tmp");
});

await test("variable expansion in script", async () => {
  const s = new WasmShell();
  s.setEnv("GREETING", "hi");
  const r = await s.exec("echo $GREETING");
  expect(str(r.stdout), "hi\n");
});

await test("command substitution $(cmd)", async () => {
  const s = new WasmShell();
  const r = await s.exec("echo $(echo nested)");
  expect(str(r.stdout), "nested\n");
});

// ── Summary ───────────────────────────────────────────────────────────────────

const total = passed + failed;
console.log(`\n${"─".repeat(48)}`);
console.log(`Tests: ${total}  |  Passed: ${passed}  |  Failed: ${failed}`);
if (failures.length) {
  console.error("\nFailed tests:");
  for (const f of failures) console.error(`  • ${f}`);
  process.exit(1);
} else {
  console.log("\nAll tests passed ✓");
}
