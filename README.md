# Foundry

[![CI](https://github.com/smeltery/foundry/actions/workflows/ci.yml/badge.svg)](https://github.com/smeltery/foundry/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust)
![Bun](https://img.shields.io/badge/Bun-1.3.11-f9f1e1?logo=bun)
[![License](https://img.shields.io/badge/license-source--available-blue)](LICENSE)

Foundry is a unified platform for coding agents, with the `f5y` command-line interface.

The project brings task execution, durable sessions, peer messaging and connected tools into one workspace. The current development build provides local session storage, explicit subprocess adapters, event watching and an MCP stdio interface. Running compatible adapters can receive queued instructions with validated receipts; delivery does not prove task completion. Model-provider integration, networking and release installation remain under development.

Start with [the documentation](docs/README.md). Contributors should read [the check workflow](docs/development/checks.md) and [code budgets](docs/development/code-budgets.md). See [the license](LICENSE) for usage terms.
