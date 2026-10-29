#!/usr/bin/env bash
# Execute checks and preparation with the same locked development environment.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
flox_command=${FOUNDRY_FLOX:-flox}
export FLOX_DISABLE_METRICS=true
if [[ $("$flox_command" --version) != "1.19.1" ]]; then
  echo "Foundry checks require Flox 1.19.1" >&2
  exit 1
fi
if [[ ! -s .flox/env/manifest.lock ]]; then
  echo "The populated development environment lock is missing" >&2
  exit 1
fi
if [[ $# -eq 0 ]]; then
  echo "Usage: bash scripts/ci/flox.sh COMMAND [ARGUMENT...]" >&2
  exit 1
fi

snapshot=$(mktemp)
trap 'rm -f "$snapshot"' EXIT
cp .flox/env/manifest.lock "$snapshot"
status=0
# Expand the command body only inside the activated child shell.
# shellcheck disable=SC2016
"$flox_command" activate --dir "$root" -- bash -euo pipefail -c '
  python3 scripts/ci/flox_verify.py
  if [[ -n ${RUSTFLAGS+x} || -n ${CARGO_ENCODED_RUSTFLAGS+x} ]]; then
    echo "Canonical checks require the selected target compiler flags" >&2
    exit 1
  fi
  for override in RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER; do
    if [[ -n ${!override:-} ]]; then
      echo "Canonical checks reject compiler overrides: $override" >&2
      exit 1
    fi
  done
  source "$FLOX_ENV/share/foundry/toolchain.sh"
  "$@"
' foundry-check "$@" || status=$?
if ! cmp -s "$snapshot" .flox/env/manifest.lock; then
  echo "Development environment activation changed the lock" >&2
  exit 1
fi
exit "$status"
