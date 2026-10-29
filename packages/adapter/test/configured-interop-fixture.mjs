import { createHash } from "node:crypto";
import { runConfiguredAdapter } from "../dist/index.js";

// Actual cross-language transport fixture; no provider or native-state claim.
const result = await runConfiguredAdapter({
  input: process.stdin,
  write: (frame) => new Promise((resolve, reject) => {
    process.stdout.write(frame, (error) => error ? reject(error) : resolve());
  }),
  async createFacade(body) {
    if (body.length !== 65_536 || body.some((byte, index) => byte !== index % 256))
      throw new Error("fixture_body_mismatch");
    return {
      selectionDigest: createHash("sha256").update("public-fixture-selection").digest("hex"),
      facade: {
        async run(_session, context) {
          await context.text("configuration accepted");
          return { outcome: "action_required", result: "transport verified" };
        },
      },
    };
  },
});
process.stdin.destroy();
if (result.status === "failed") process.exitCode = 1;
