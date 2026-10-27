# Official MCP SDK interoperability check

This independently authored test launches the built CLI through the official
Python MCP SDK. It uses temporary SQLite state and no provider, model, desktop
client, personal configuration, or network service. Package installation needs
network access; the test itself communicates only over local subprocess stdio.

Use CPython 3.13 with an isolated environment outside the checkout:

```sh
python3.13 -m venv /tmp/foundry-mcp-sdk
/tmp/foundry-mcp-sdk/bin/python -m pip install --only-binary=:all: --require-hashes -r tests/mcp-sdk/requirements.txt
cargo build --locked -p f5y
/tmp/foundry-mcp-sdk/bin/python tests/mcp-sdk/verify.py --binary target/debug/f5y --report /tmp/foundry-mcp-sdk-result.json
```

`requirements.txt` pins the SDK, type package and all transitive versions. Wheel
hashes cover the following CPython 3.13 targets. Source builds are excluded.

| Flox system | Wheel availability | Validation |
| --- | --- | --- |
| `aarch64-darwin` | All 28 packages | Fresh hashed install and SDK checks passed |
| `x86_64-linux` | All 28 packages | Wheel-only resolution and hashes checked |
| `aarch64-linux` | All 28 packages | Wheel-only resolution and hashes checked |
| `x86_64-darwin` | 27 of 28 packages | Blocked by missing cryptography wheel |

The pinned [cryptography 50.0.2 release](https://pypi.org/project/cryptography/50.0.2/#files)
has no macOS x86_64 or universal2 wheel. Hashes for the other macOS x86_64 packages
are present, but the full install must fail until a reviewed compatible pin is
selected. Do not silently skip the test or permit source builds. Download checks
for cross-platform wheels do not establish native execution on those platforms.

Other Python versions require reviewed additional wheel hashes. These are CI-only
dependencies, not shipped CLI runtime dependencies. Preserve installed distribution
licenses. The shared check runner owns environment provisioning and CI scheduling;
dependency updates require the usual notice and compatibility review.

Checks cover initialization at protocol `2025-11-25`, ping, all five session tools,
input schemas, idempotent creation/message retries, conflicting message content,
cancellation, no automatic restart, and persistence across server processes.
They also cover nonempty request metadata, opaque metadata values, and the SDK's
optional progress callback. No progress notifications are promised or fabricated.
Negative requests must retain their invalid-params errors. Evidence includes
binary SHA-256, SDK version, runtime/platform and individual checks.

This is subprocess SDK interoperability evidence, not full MCP conformance,
hosted OAuth, Streamable HTTP, GUI-client installation or marketplace approval.

Primary references:

- [Official Python SDK, pinned release](https://github.com/modelcontextprotocol/python-sdk/tree/v2.3.0)
- [SDK API documentation](https://py.sdk.modelcontextprotocol.io/)
- [Protocol metadata](https://modelcontextprotocol.io/specification/2025-11-25/basic/index)
- [Optional progress](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/progress)
- [Stdio transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
