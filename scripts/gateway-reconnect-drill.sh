#!/usr/bin/env bash
# Verify the gateway survives an engine restart and reconnects to the same UDS.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --bin matching-engine --bin gateway --example network_client >/dev/null

TMP=$(mktemp -d "${TMPDIR:-/tmp}/matching-reconnect.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT

start_engine() {
  RUST_LOG=warn "$ROOT/target/release/matching-engine" \
    --engine-uds "$TMP/engine.sock" --data-dir "$TMP/data" \
    --snapshot-interval 0 --wal-sync false --metrics-port 0 --log-level error &
  PIDS+=("$!")
}

start_engine
sleep 1.2
RUST_LOG=info "$ROOT/target/release/gateway" \
  --listen 127.0.0.1:18260 --engine-uds "$TMP/engine.sock" --io-threads 2 \
  >"$TMP/gateway.log" 2>&1 &
GATEWAY=$!
PIDS+=("$GATEWAY")
sleep 1.0

SERVER_ADDR=127.0.0.1:18260 timeout 8 "$ROOT/target/release/examples/network_client" >/dev/null 2>&1
ENGINE=${PIDS[0]}
kill -9 "$ENGINE" 2>/dev/null || true
sleep 1.0
kill -0 "$GATEWAY" || { echo "gateway exited after engine loss" >&2; exit 1; }

start_engine
sleep 1.5
grep -q "engine IPC reconnected" "$TMP/gateway.log" || {
  echo "gateway did not reconnect" >&2
  cat "$TMP/gateway.log" >&2
  exit 1
}
SERVER_ADDR=127.0.0.1:18260 timeout 8 "$ROOT/target/release/examples/network_client" >/dev/null 2>&1
echo "gateway reconnect drill passed"
