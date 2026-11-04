#!/usr/bin/env bash
# Run a command with the pinned compiler settings inside the activated Foundry environment.
set -euo pipefail

if [[ -z ${FLOX_ENV:-} || ! -f $FLOX_ENV/share/foundry/toolchain.sh ]]; then
  echo "Run this inside the activated Foundry Flox environment" >&2
  exit 1
fi
[[ -z ${RUSTFLAGS+x} && -z ${CARGO_ENCODED_RUSTFLAGS+x} ]] || { echo "Canonical checks require the selected target compiler flags" >&2; exit 1; }
for override in RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER; do
  if [[ -n ${!override:-} ]]; then
    echo "Canonical checks reject compiler overrides: $override" >&2
    exit 1
  fi
done
# shellcheck source=/dev/null
source "$FLOX_ENV/share/foundry/toolchain.sh"
exec "$@"
