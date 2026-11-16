#!/usr/bin/env bash
# mvn with a retry on a Maven Central rate limit or transfer failure only.
# A test failure is never retried: only an output that shows the build could
# not download something (HTTP 429, "Could not transfer", "Unresolveable build
# extension") gets another attempt, with a growing pause.
# Usage: scripts/mvn-retry.sh <mvn args...>
set -uo pipefail
log=$(mktemp)
for attempt in 1 2 3; do
  mvn "$@" 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  [ "$rc" -eq 0 ] && exit 0
  if ! grep -qE "HTTP Status: 429|Could not transfer|Unresolveable build extension" "$log"; then
    exit "$rc"
  fi
  echo "mvn-retry: a download failed (attempt $attempt of 3); pausing" >&2
  sleep $((attempt * 30))
done
exit "$rc"
