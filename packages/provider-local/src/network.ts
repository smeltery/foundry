import { isIPv4, isIPv6 } from "node:net";
import { EndpointError } from "./errors.ts";

/** Most DNS answers a single endpoint admission accepts. */
const MAX_ADDRESSES = 64;

/** The address class an endpoint resolves to. Names are provisional. */
export type NetworkClass =
  | "loopback"
  | "lan"
  | "tailnet"
  /** Always refused, including cloud metadata addresses. */
  | "link_local"
  /** Unspecified, multicast, broadcast, reserved and IPv4-embedding IPv6. Always refused. */
  | "reserved"
  | "public";

/** A private class an endpoint explicitly opts into. Public needs no opt-in. */
export type OptIn = "loopback" | "lan" | "tailnet";
export const OPT_INS: readonly OptIn[] = ["loopback", "lan", "tailnet"];

/** Resolves a host name to address strings. Tests inject a fixture resolver. */
export type Resolve = (host: string) => Promise<string[]>;

/**
 * Cloud metadata addresses outside 169.254/16 and fe80::/10. They sit inside
 * the tailnet and LAN ranges, so they are classed as link-local explicitly.
 */
const METADATA_V4 = "100.100.100.200";
const METADATA_V6 = [0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254];

function v4Octets(text: string): number[] {
  return text.split(".").map(Number);
}

/** Expands IPv6 text, including an embedded IPv4 tail, into eight segments. */
function v6Segments(text: string): number[] {
  const tail = text.match(/^(.*:)(\d+\.\d+\.\d+\.\d+)$/);
  const [a, b, c, d] = tail ? v4Octets(tail[2]!) : [];
  const hex = (high: number, low: number) => ((high << 8) | low).toString(16);
  const source = tail ? `${tail[1]}${hex(a!, b!)}:${hex(c!, d!)}` : text;
  const [head, rest] = source.split("::") as [string, string | undefined];
  const parse = (part: string) => (part ? part.split(":").map((group) => parseInt(group, 16)) : []);
  const left = parse(head);
  const right = rest === undefined ? [] : parse(rest);
  return [...left, ...Array(8 - left.length - right.length).fill(0), ...right];
}

function classifyV4([a, b]: number[]): NetworkClass {
  if (a === 127) return "loopback";
  if (a === 10 || (a === 172 && b! >= 16 && b! <= 31) || (a === 192 && b === 168)) return "lan";
  if (a === 100 && b! >= 64 && b! <= 127) return "tailnet";
  if (a === 169 && b === 254) return "link_local";
  if (a === 0 || a! >= 224) return "reserved";
  return "public";
}

function classifyV6(segments: number[]): NetworkClass {
  const [first, second] = segments as [number, number];
  if (zeroPrefix(segments, 7) && segments[7] === 1) return "loopback";
  if ((first & 0xfe00) === 0xfc00) return "lan";
  if ((first & 0xffc0) === 0xfe80) return "link_local";
  // Unspecified, multicast, and IPv4-compatible, 6to4 and NAT64 addresses
  // that embed an IPv4 destination.
  const embedsV4 = zeroPrefix(segments, 6) || first === 0x2002 || (first === 0x64 && second === 0xff9b);
  return embedsV4 || (first & 0xff00) === 0xff00 ? "reserved" : "public";
}

/** Classifies one address. An IPv4-mapped IPv6 address takes its IPv4 class. */
export function classify(address: string): NetworkClass {
  if (isIPv4(address)) {
    return address === METADATA_V4 ? "link_local" : classifyV4(v4Octets(address));
  }
  if (!isIPv6(address)) return "reserved";
  const segments = v6Segments(address);
  if (segments.every((segment, index) => segment === METADATA_V6[index])) return "link_local";
  if (zeroPrefix(segments, 5) && segments[5] === 0xffff) {
    const mapped = [segments[6]! >> 8, segments[6]! & 0xff, segments[7]! >> 8, segments[7]! & 0xff];
    return mapped.join(".") === METADATA_V4 ? "link_local" : classifyV4(mapped);
  }
  return classifyV6(segments);
}

function zeroPrefix(segments: number[], count: number): boolean {
  return segments.slice(0, count).every((segment) => segment === 0);
}

/**
 * Decides the class from every answer before any connection is made. Answers
 * that span classes or a class without its opt-in are refused, and a public
 * endpoint must use HTTPS.
 */
export function admitAddresses(addresses: string[], https: boolean, optIn: readonly OptIn[]): string[] {
  const unique = [...new Set(addresses)];
  if (unique.length === 0 || unique.length > MAX_ADDRESSES) throw EndpointError.notPermitted();
  const classes = new Set(unique.map(classify));
  const [networkClass] = classes;
  if (classes.size !== 1 || networkClass === "link_local" || networkClass === "reserved") {
    throw EndpointError.notPermitted();
  }
  if (networkClass === "public") {
    if (!https) throw EndpointError.insecureTransport();
    return unique;
  }
  if (!optIn.includes(networkClass as OptIn)) throw EndpointError.notPermitted();
  return unique;
}
