#!/usr/bin/env bash
# Preflight for CPU-pinned trading benchmarks.
#
# Usage:
#   scripts/check-test-host.sh [--strict]
#
# --strict exits non-zero when stale trading processes or missing taskset are
# found. Aggregate scripts pass --strict; an interactive run may omit it.
set -euo pipefail
cd "$(dirname "$0")/.."

strict=0
if [[ "${1:-}" == "--strict" ]]; then strict=1; fi
failed=0

if ! command -v taskset >/dev/null 2>&1; then
  echo "test-host: taskset is required for performance runs" >&2
  failed=1
fi

if command -v lscpu >/dev/null 2>&1; then
  echo "test-host: CPU topology"
  lscpu -e=CPU,CORE,ONLINE 2>/dev/null | head -12 || true
  echo "test-host: available_parallelism=$(nproc)"
fi

if [[ -r /proc/loadavg ]]; then
  echo "test-host: load=$(cat /proc/loadavg)"
fi

stale=$(ps -eo pid=,args= | awk '{
  n = split($2, parts, "/");
  if (parts[n] ~ /^trading-(middleware|core|gateway|load)$/) print
}' || true)
if [[ -n "$stale" ]]; then
  echo "test-host: stale trading processes found:" >&2
  echo "$stale" >&2
  echo "test-host: kill them before measuring" >&2
  failed=1
fi

for bin in trading-middleware trading-core trading-gateway trading-load; do
  if [[ ! -x "target/release/$bin" ]]; then
    echo "test-host: missing target/release/$bin (scripts rebuild first)"
    continue
  fi
  newer=$(find src crates -name '*.rs' -newer "target/release/$bin" -print -quit 2>/dev/null || true)
  if [[ -n "$newer" ]]; then
    echo "test-host: target/release/$bin is older than $newer; rebuild before measuring"
  fi
done

if (( failed && strict )); then
  echo "test-host: preflight failed" >&2
  exit 1
fi
echo "test-host: preflight ok"
