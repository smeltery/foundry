# Maintenance patch

Owner: Foundry maintainers. Version: netwatch 0.19.3.

The published archive has SHA-256
`39da9cad5f23aa43401f09497d3b280e51b5feba115d9ffbf38ca35d6ff95e96`.
Its source revision is `0030e57fc70981895bb86212ff0f059cc41c8a4e` in
[n0-computer/net-tools](https://github.com/n0-computer/net-tools).

The normalized Cargo manifest backports the dependency updates from
[change 239](https://github.com/n0-computer/net-tools/pull/239): netlink core 0.9,
route 0.33, protocol 0.13, system 0.9 and netdev 0.46.3. The native netdev/Tokio
configuration is shared across Windows and other native platforms. This removes
the unmaintained macro dependency while retaining the existing netwatch API.
The original Cargo manifest remains in `Cargo.toml.orig`.

Rust executable code is unchanged. One UDP documentation sentence and a generated
manifest comment use neutral wording. The original README is retained as text;
the local README records the selected MIT distribution option. The MIT notice
was obtained unchanged from the same source revision and applies to this copy.
The Foundry root license does not replace this dependency license.

Remove this patch when a published compatible netwatch release includes these
dependency updates, or a tested Iroh release consumes the newer netwatch series.
Run dependency advisories, network tests and the required platform CI before
updating or removing it. No advisory exemption is configured.
