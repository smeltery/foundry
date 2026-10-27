import { runAdapter } from "./controller.ts";

// No provider binding ships here. Strip credentials before any future binding
// could be constructed; a reviewed explicit handoff is required to add one.
const allowed = new Set(["PATH", "LANG", "LC_ALL", "TZ"]);
for (const name of Object.keys(process.env)) {
  if (!allowed.has(name)) delete process.env[name];
}

const result = await runAdapter({
  input: process.stdin,
  write: (frame) =>
    new Promise<void>((resolve, reject) => {
      process.stdout.write(frame, (error) => (error ? reject(error) : resolve()));
    }),
});
process.stdin.destroy();
process.exit(result.status === "failed" ? 1 : 0);
