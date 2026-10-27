#!/usr/bin/env bash
set -euo pipefail

# Ubuntu restricts user namespaces for downloaded browsers. Allow only this binary.
mapfile -d '' -t browsers < <(find "$PWD/.flox/cache/puppeteer/chrome-headless-shell" \
  -type f -name chrome-headless-shell -print0)
test "${#browsers[@]}" -eq 1
browser="${browsers[0]}"
[[ "$browser" =~ ^/[a-zA-Z0-9_./-]+$ ]]
test -x "$browser"
printf 'abi <abi/4.0>,\ninclude <tunables/global>\nprofile foundry-docs "%s" flags=(unconfined) {\n  userns,\n}\n' \
  "$browser" | sudo tee /etc/apparmor.d/foundry-docs >/dev/null
sudo apparmor_parser -r /etc/apparmor.d/foundry-docs
