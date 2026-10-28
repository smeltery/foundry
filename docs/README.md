# Documentation

Foundry brings agent sessions, execution and tools into one workspace. Development
is underway; each page distinguishes tested functionality from planned features.

## Using Foundry

- [CLI commands and current availability](user/cli.md)
- [Sessions, attempts and cancellation](user/sessions.md)
- [Network command interface](user/network.md)
- [Plugin command surface](user/plugins.md)

## Architecture

- [Session and backend contracts](architecture/contracts.md)
- [Complete CLI target contract](architecture/cli-contract.md)
- [CLI configuration boundary](architecture/cli-configuration.md)
- [Durable storage and recovery](architecture/storage.md)
- [Checkpoint restoration and continuation ownership](architecture/restoration.md)
- [Local adapter supervision](architecture/runtime.md)
- [Runtime architecture decision](architecture/runtime-decision.md)
- [Application service and receipt delivery](architecture/service.md)
- [MCP transport and tool contracts](architecture/mcp.md)
- [Network invitation ticket format](../crates/network/docs/tickets.md)
- [Configured network identity proofs](../crates/network/docs/proofs.md)
- [Durable team state and removal](architecture/teams.md)

## Development

- [Flox environment and pinned tools](development/environment.md)
- [Shared CI checks and local hooks](development/checks.md)
- [Code budgets](development/code-budgets.md)
