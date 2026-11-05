// Runs in a child process whose NODE_EXTRA_CA_CERTS trusts a throwaway test
// CA. For each certificate it serves one fixture reply over HTTPS on loopback
// and reports how the endpoint fared by name and by literal address.
import { readFileSync } from "node:fs";
import { createServer } from "node:https";
import { EndpointError } from "../src/errors.ts";
import { ask, chat, endpointFor } from "./stub.ts";

const [directory, ...names] = process.argv.slice(2);
const results: Record<string, string> = {};
for (const name of names) {
  const server = createServer(
    { key: readFileSync(`${directory}/${name}.key`), cert: readFileSync(`${directory}/${name}.pem`) },
    (request, response) => {
      request.resume();
      request.on("end", () => {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify(chat("trusted")));
      });
    },
  );
  server.on("tlsClientError", () => {});
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as { port: number };
  for (const host of ["stub.test", "127.0.0.1"]) {
    const endpoint = endpointFor(port, {
      baseUrl: `https://${host}:${port}/v1`,
      resolve: async () => ["127.0.0.1"],
    });
    results[`${name} ${host}`] = await endpoint.complete(ask).then(
      (completion) => completion.text,
      (error) => (error instanceof EndpointError ? `${error.category}` : "unexpected"),
    );
  }
  server.closeAllConnections();
  server.close();
}
console.log(JSON.stringify(results));
