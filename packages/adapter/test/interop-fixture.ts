import { runAdapter } from "../src/controller.ts";

/** Explicitly test-only binding, imported solely by temporary interop shims. */
export async function runTestFacade(mode: "tool_denial"): Promise<void> {
  if (mode !== "tool_denial") throw new Error("unsupported_fixture");
  const result = await runAdapter({
    input: process.stdin,
    write: (frame) =>
      new Promise<void>((resolve, reject) => {
        process.stdout.write(frame, (error) => (error ? reject(error) : resolve()));
      }),
    facade: {
      async run(session, context) {
        if (process.env.EXAMPLE_API_TOKEN || process.env.HOME)
          throw new Error("environment_leaked");
        if (process.cwd() !== session.directory) throw new Error("directory_mismatch");
        await context.text("Offline protocol fixture; no model executed.");
        const reply = await context.tool("fixture-call", "read", { path: "/never-opened" });
        if (
          !reply ||
          typeof reply !== "object" ||
          Array.isArray(reply) ||
          reply.status !== "denied"
        ) {
          throw new Error("tool_denial_missing");
        }
        return { outcome: "action_required", result: "offline_fixture:tool_denied" };
      },
    },
  });
  process.stdin.destroy();
  process.exit(result.status === "failed" ? 1 : 0);
}
