import { describe, expect, test } from "bun:test";
import { EndpointError } from "../src/errors.ts";
import { LocalEndpoint } from "../src/endpoint.ts";
import { admitAddresses, classify } from "../src/network.ts";
import { MODEL, PROVIDER } from "./stub.ts";

function refusal(run: () => unknown): EndpointError {
  try {
    run();
  } catch (error) {
    if (error instanceof EndpointError) return error;
    throw error;
  }
  throw new Error("expected a refusal");
}

describe("network classes", () => {
  test("addresses are classified like the Rust client", () => {
    const cases: [string, string][] = [
      ["127.0.0.1", "loopback"],
      ["127.255.0.9", "loopback"],
      ["::1", "loopback"],
      ["::ffff:127.0.0.1", "loopback"],
      ["::ffff:7f00:1", "loopback"],
      ["10.1.2.3", "lan"],
      ["172.16.0.1", "lan"],
      ["172.31.255.255", "lan"],
      ["192.168.4.4", "lan"],
      ["fd12::1", "lan"],
      ["100.64.0.1", "tailnet"],
      ["100.127.255.254", "tailnet"],
      ["169.254.169.254", "link_local"],
      ["100.100.100.200", "link_local"],
      ["::ffff:100.100.100.200", "link_local"],
      ["fd00:ec2::254", "link_local"],
      ["fe80::1", "link_local"],
      ["0.0.0.0", "reserved"],
      ["224.0.0.1", "reserved"],
      ["255.255.255.255", "reserved"],
      ["::", "reserved"],
      ["::7f00:1", "reserved"],
      ["ff02::1", "reserved"],
      ["2002:7f00:1::", "reserved"],
      ["64:ff9b::7f00:1", "reserved"],
      ["172.32.0.1", "public"],
      ["203.0.113.10", "public"],
      ["2001:db8::1", "public"],
    ];
    for (const [address, expected] of cases) expect([address, classify(address)]).toEqual([address, expected]);
  });

  test("private classes need their own opt-in", () => {
    expect(admitAddresses(["127.0.0.1"], false, ["loopback"])).toEqual(["127.0.0.1"]);
    expect(admitAddresses(["10.0.0.2"], false, ["lan"])).toEqual(["10.0.0.2"]);
    expect(admitAddresses(["100.64.1.1"], false, ["tailnet"])).toEqual(["100.64.1.1"]);
    expect(refusal(() => admitAddresses(["127.0.0.1"], false, [])).errorClass).toBe("endpoint_not_permitted");
    expect(refusal(() => admitAddresses(["127.0.0.1"], false, ["lan", "tailnet"])).errorClass).toBe(
      "endpoint_not_permitted",
    );
  });

  test("link-local, reserved and metadata addresses are always refused", () => {
    for (const address of ["169.254.169.254", "100.100.100.200", "fe80::1", "0.0.0.0", "224.0.0.1"]) {
      expect(refusal(() => admitAddresses([address], true, ["loopback", "lan", "tailnet"])).errorClass).toBe(
        "endpoint_not_permitted",
      );
    }
  });

  test("answers that span classes are refused", () => {
    const error = refusal(() => admitAddresses(["127.0.0.1", "10.0.0.1"], false, ["loopback", "lan"]));
    expect(error.errorClass).toBe("endpoint_not_permitted");
    expect(refusal(() => admitAddresses(["203.0.113.10", "127.0.0.1"], true, ["loopback"])).errorClass).toBe(
      "endpoint_not_permitted",
    );
  });

  test("public addresses need HTTPS and no opt-in", () => {
    expect(admitAddresses(["203.0.113.10"], true, [])).toEqual(["203.0.113.10"]);
    expect(refusal(() => admitAddresses(["203.0.113.10"], false, [])).errorClass).toBe("insecure_transport");
  });

  test("literal addresses are judged when the endpoint is built", () => {
    const build = (baseUrl: string, optIn: ("loopback" | "lan")[] = []) => () =>
      new LocalEndpoint({ providerId: PROVIDER, baseUrl, models: [MODEL], optIn });
    expect(refusal(build("http://203.0.113.10/v1")).errorClass).toBe("insecure_transport");
    expect(refusal(build("http://127.0.0.1:9/v1")).errorClass).toBe("endpoint_not_permitted");
    expect(refusal(build("https://[fe80::1]/v1", ["lan"])).errorClass).toBe("endpoint_not_permitted");
    // Alternate IPv4 spellings are normalized before they are classified.
    expect(refusal(build("http://2130706433/v1", ["lan"])).errorClass).toBe("endpoint_not_permitted");
    expect(refusal(build("http://0x7f.1/v1", ["lan"])).errorClass).toBe("endpoint_not_permitted");
    expect(() => build("http://0x7f.1/v1", ["loopback"])()).not.toThrow();
    expect(() => build("http://[::1]:8/v1", ["loopback"])()).not.toThrow();
  });
});
