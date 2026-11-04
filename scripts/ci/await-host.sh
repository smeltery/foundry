#!/usr/bin/env bash
# Wait out a short DNS or connection outage before a step that downloads from URL.
# Any HTTP response proves the host resolves and accepts connections. Only network
# failures are retried, with exponential backoff, until the next delay would
# exceed the window; every other curl failure stops at once.
set -euo pipefail

if [[ $# -ne 1 || $1 != https://* ]]; then
  echo "Usage: bash scripts/ci/await-host.sh https://HOST/PATH" >&2
  exit 2
fi
url=$1
host=${url#https://}
host=${host%%/*}
window_seconds=240
max_delay_seconds=60
delay=2
waited=0
attempt=1
while true; do
  status=0
  curl --head --silent --show-error --output /dev/null \
    --connect-timeout 15 --max-time 30 "$url" || status=$?
  if [[ $status -eq 0 ]]; then
    echo "Reached $host on attempt $attempt"
    exit 0
  fi
  # Proxy or host resolution, connection, timeout, TLS handshake, empty or broken reply.
  case $status in
    5 | 6 | 7 | 28 | 35 | 52 | 55 | 56) ;;
    *)
      echo "Cannot reach $host: curl exited $status, which is not a transient network failure" >&2
      exit 1
      ;;
  esac
  if ((waited + delay > window_seconds)); then
    echo "Could not reach $host after $attempt attempts over ${waited}s of backoff (last curl exit $status)" >&2
    exit 1
  fi
  echo "Cannot reach $host yet (curl exit $status); retrying in ${delay}s" >&2
  sleep "$delay"
  waited=$((waited + delay))
  delay=$((delay * 2 > max_delay_seconds ? max_delay_seconds : delay * 2))
  attempt=$((attempt + 1))
done
