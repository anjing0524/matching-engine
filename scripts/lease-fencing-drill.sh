#!/usr/bin/env bash
# A second engine on the same lease must refuse to start; after the primary is
# killed the lease is released and promotion can bind it.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --bin matching-engine >/dev/null
TMP=$(mktemp -d "${TMPDIR:-/tmp}/matching-lease.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT
LEASE="$TMP/engine.lease"

"$ROOT/target/release/matching-engine" \
  --engine-uds "$TMP/primary.sock" --data-dir "$TMP/primary" \
  --lease-file "$LEASE" --snapshot-interval 0 --wal-sync false \
  --metrics-port 0 --log-level error >"$TMP/primary.log" 2>&1 &
PRIMARY=$!
PIDS+=("$PRIMARY")
sleep 1.2

if "$ROOT/target/release/matching-engine" \
  --engine-uds "$TMP/contender.sock" --data-dir "$TMP/contender" \
  --lease-file "$LEASE" --snapshot-interval 0 --wal-sync false \
  --metrics-port 0 --log-level error >"$TMP/contender.log" 2>&1; then
  echo "contender unexpectedly acquired a held lease" >&2
  exit 1
fi
grep -q "failed to acquire engine lease" "$TMP/contender.log" || {
  echo "contender failed for an unexpected reason" >&2
  cat "$TMP/contender.log" >&2
  exit 1
}

kill -9 "$PRIMARY" 2>/dev/null || true
sleep 0.5
"$ROOT/target/release/matching-engine" \
  --engine-uds "$TMP/promoted.sock" --data-dir "$TMP/promoted" \
  --lease-file "$LEASE" --snapshot-interval 0 --wal-sync false \
  --metrics-port 0 --log-level error >"$TMP/promoted.log" 2>&1 &
PROMOTED=$!
PIDS+=("$PROMOTED")
sleep 1.2
kill -0 "$PROMOTED" || { echo "promoted engine failed to acquire released lease" >&2; cat "$TMP/promoted.log" >&2; exit 1; }
echo "lease fencing drill passed"
