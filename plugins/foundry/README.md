# Foundry client workflows

Foundry provides six workflows for asking questions, building changes, fixing
defects, repairing CI, reviewing changes and inspecting session status. The shared
coordination skill uses five local MCP tools to create and inspect durable session
records, queue instructions and request cancellation. Queue acceptance does not
start execution or establish completion.

This source package requires preparation with an explicitly selected Foundry
executable, existing database and applicable third-party notices. A prepared
distribution includes the executable and local MCP connection files. Use the
client's supported local plugin mechanism with the prepared package directory;
loading a source checkout alone does not configure a working server.

Execution requires a separately configured local CLI and adapter. Preserve the
selected database and returned session IDs when inspecting or continuing work.
Client discovery and model availability depend on the installed client; static
package validation does not verify those capabilities.

See the [coordination skill](skills/foundry/SKILL.md) for operations and limits,
and [LICENSE](LICENSE) for this package's license. Keep the distribution's
third-party notices with its bundled executable.
