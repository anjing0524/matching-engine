#!/usr/bin/env bash
# Denylist check for documentation that references removed interfaces.
set -euo pipefail
cd "$(dirname "$0")/.."

denied=(
  'repair_buffer_capacity'
  'GatewayRuntime::drain_gateway'
  'drain_gateway'
  'EventFanout::with_repair_buffer'
  'ComboRuntime::tick'
  'CoreRunner::drain_private'
  'CoreRunner::drain_public'
)
status=0
for symbol in "${denied[@]}"; do
  if grep -RIn --include='*.md' -- "$symbol" docs README.md 2>/dev/null; then
    echo "docs reference removed interface: $symbol" >&2
    status=1
  fi
done
if (( status == 0 )); then echo "docs denylist ok"; fi
exit "$status"
