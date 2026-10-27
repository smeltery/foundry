import { expect, test } from "bun:test";
import { chmod, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Parent builds the exact Rust binary first. This suite never mutates Cargo files
// or silently falls back to a stale/installed executable from PATH.
const binary = process.env.FOUNDRY_TEST_BINARY;
if (binary && !isAbsolute(binary)) throw new Error("FOUNDRY_TEST_BINARY must be absolute");
const interop = test.skipIf(!binary || process.platform === "win32");

async function fixture(configured: boolean) {
  const directory = await realpath(await mkdtemp(join(tmpdir(), "f5y-interop-")));
  const bun = await realpath(process.execPath);
  if (/[\r\n]/.test(bun)) throw new Error("invalid Bun executable path");
  const shim = join(directory, "test-adapter.ts");
  const module = fileURLToPath(
    new URL(configured ? "./interop-fixture.ts" : "../src/main.ts", import.meta.url),
  );
  const source = configured
    ? `import { runTestFacade } from ${JSON.stringify(pathToFileURL(module).href)}; await runTestFacade("tool_denial");`
    : `await import(${JSON.stringify(pathToFileURL(module).href)});`;
  await writeFile(shim, `#!${bun}\n${source}\n`);
  await chmod(shim, 0o700);
  const child = Bun.spawn(
    [
      binary!,
      "--database",
      join(directory, "sessions.db"),
      "--json",
      "run",
      "--task",
      "offline protocol test",
    ],
    {
      cwd: directory,
      env: {
        FOUNDRY_ADAPTER: shim,
        PATH: "/nonexistent-fixture-path",
        EXAMPLE_API_TOKEN: "offline-sentinel-not-a-credential",
      },
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
    },
  );
  const deadline = setTimeout(() => child.kill(), 5000);
  try {
    const [stdout, stderr, code] = await Promise.all([
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
      child.exited,
    ]);
    return { stdout, stderr, code };
  } finally {
    clearTimeout(deadline);
    child.kill();
    await child.exited;
    await rm(directory, { recursive: true, force: true });
  }
}

interop(
  "Rust service supervises the TS facade and denies its tool request",
  async () => {
    const result = await fixture(true);
    expect(result.stderr).toBe("");
    expect(result.code).toBe(3);
    const response = JSON.parse(result.stdout);
    expect(response.session.lifecycle).toBe("exit");
    expect(response.session.outcome).toBe("action_required");
    expect(response.session.result).toBe("offline_fixture:tool_denied");
    expect(result.stdout).not.toContain("offline-sentinel-not-a-credential");
  },
  10_000,
);

interop(
  "Rust supervisor rejects the unconfigured TS executable without success",
  async () => {
    const result = await fixture(false);
    expect(result.code).not.toBe(0);
    expect(result.stdout).not.toContain('"outcome":"success"');
    expect(result.stderr).not.toContain("offline-sentinel-not-a-credential");
    expect(JSON.parse(result.stderr).error.code).toBe("execution");
  },
  10_000,
);
