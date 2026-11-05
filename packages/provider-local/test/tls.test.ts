import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Certificates are generated per run with the openssl CLI and never committed.
let directory = "";

const CONFIG = `[req]
distinguished_name = dn
prompt = no
[dn]
CN = fixture-ca
[ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
[self]
basicConstraints = critical,CA:FALSE
subjectAltName = DNS:stub.test,IP:127.0.0.1
`;

function openssl(...args: string[]) {
  const result = spawnSync("openssl", args, { cwd: directory, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`openssl ${args[0]} failed: ${result.error ?? result.stderr}`);
}

/** A leaf certificate for `altNames`, issued by the throwaway CA. */
function issue(name: string, altNames: string) {
  writeFileSync(join(directory, `${name}.ext`), `subjectAltName = ${altNames}\nextendedKeyUsage = serverAuth\n`);
  openssl("req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", `${name}.key`, "-out", `${name}.csr`,
    "-config", "openssl.cnf", "-subj", `/CN=${name}`);
  openssl("x509", "-req", "-in", `${name}.csr`, "-CA", "ca.pem", "-CAkey", "ca.key", "-set_serial", "2",
    "-days", "1", "-sha256", "-extfile", `${name}.ext`, "-out", `${name}.pem`);
}

beforeAll(() => {
  directory = mkdtempSync(join(tmpdir(), "provider-local-tls-"));
  writeFileSync(join(directory, "openssl.cnf"), CONFIG);
  openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "1",
    "-sha256", "-config", "openssl.cnf", "-extensions", "ca");
  issue("matching", "DNS:stub.test,IP:127.0.0.1");
  issue("mismatched", "DNS:other.test,IP:127.0.0.2");
  // Valid for both names, but signed by nobody the child trusts.
  openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "self.key", "-out", "self.pem", "-days", "1",
    "-sha256", "-config", "openssl.cnf", "-extensions", "self", "-subj", "/CN=stub.test");
});

afterAll(() => {
  // Only the directory this file created.
  if (directory) rmSync(directory, { recursive: true });
});

describe("HTTPS certificate checks", () => {
  test("the chain and the host identity are both required, by name and by literal address", () => {
    const child = spawnSync(
      process.execPath,
      [join(import.meta.dir, "tls_child.ts"), directory, "matching", "mismatched", "self"],
      { encoding: "utf8", timeout: 60_000, env: { PATH: process.env.PATH, NODE_EXTRA_CA_CERTS: join(directory, "ca.pem") } },
    );
    expect(child.stderr).toBe("");
    expect(JSON.parse(child.stdout)).toEqual({
      "matching stub.test": "trusted",
      "matching 127.0.0.1": "trusted",
      "mismatched stub.test": "tls",
      "mismatched 127.0.0.1": "tls",
      "self stub.test": "tls",
      "self 127.0.0.1": "tls",
    });
  }, 60_000);
});
