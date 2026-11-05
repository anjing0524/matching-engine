#!/usr/bin/env bash
# Automated promotion drill: a watchdog detects primary exit, verifies the last
# known replication state, stops the standby, promotes its WAL on the same UDS,
# and waits for the existing gateway to reconnect.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --bin matching-engine --bin gateway --bin standby \
  --bin latency_probe --example network_client >/dev/null

TMP=$(mktemp -d "${TMPDIR:-/tmp}/matching-auto-failover.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT

SOCK="$TMP/engine.sock"
REPL="$TMP/repl.sock"
STANDBY_DATA="$TMP/standby"
LISTEN=127.0.0.1:18263
METRICS=19236

RUST_LOG=warn "$ROOT/target/release/matching-engine" \
  --engine-uds "$SOCK" --data-dir "$TMP/primary" --replica-listen "$REPL" \
  --queue-capacity 100000 --snapshot-interval 0 --wal-sync false \
  --metrics-port "$METRICS" --log-level error >"$TMP/primary.log" 2>&1 &
PRIMARY=$!
PIDS+=("$PRIMARY")
sleep 1.2
RUST_LOG=warn "$ROOT/target/release/standby" \
  --primary-uds "$REPL" --data-dir "$STANDBY_DATA" --wal-sync true \
  >"$TMP/standby.log" 2>&1 &
STANDBY=$!
PIDS+=("$STANDBY")
sleep 1.0
RUST_LOG=info "$ROOT/target/release/gateway" \
  --listen "$LISTEN" --engine-uds "$SOCK" --io-threads 2 \
  >"$TMP/gateway.log" 2>&1 &
GATEWAY=$!
PIDS+=("$GATEWAY")
sleep 1.0
SERVER_ADDR="$LISTEN" timeout 10 "$ROOT/target/release/examples/network_client" >/dev/null 2>&1

# Capture the last known safe replication state before simulating failure.
HEALTH=""
for _ in $(seq 1 50); do
  HEALTH=$(curl -sf "http://127.0.0.1:$METRICS/health" || true)
  echo "$HEALTH" | grep -q '"replication_lag_records":0' \
    && echo "$HEALTH" | grep -q '"replication_dropped_batches":0' \
    && echo "$HEALTH" | grep -q '"replication_connected":1' && break
  sleep 0.1
done
echo "$HEALTH" | grep -q '"replication_lag_records":0' || {
  echo "standby not caught up; refusing automatic promotion" >&2
  exit 1
}

# Watchdog: automatic promotion after primary exit.
(
  while kill -0 "$PRIMARY" 2>/dev/null; do sleep 0.05; done
  kill -TERM "$STANDBY" 2>/dev/null || true
  sleep 0.3
  RUST_LOG=warn "$ROOT/target/release/matching-engine" \
    --engine-uds "$SOCK" --data-dir "$STANDBY_DATA" \
    --queue-capacity 100000 --snapshot-interval 0 --wal-sync false \
    --metrics-port 0 --log-level error >"$TMP/promoted.log" 2>&1 &
  echo $! >"$TMP/promoted.pid"
) &
WATCHDOG=$!

kill -9 "$PRIMARY" 2>/dev/null || true
wait "$WATCHDOG"

for _ in $(seq 1 50); do
  grep -q "engine IPC reconnected" "$TMP/gateway.log" && break
  sleep 0.1
done
grep -q "engine IPC reconnected" "$TMP/gateway.log" || {
  echo "gateway did not reconnect after automatic promotion" >&2
  cat "$TMP/gateway.log" >&2
  exit 1
}
PROMOTED=$(cat "$TMP/promoted.pid")
PIDS+=("$PROMOTED")
"$ROOT/target/release/latency_probe" --server "$LISTEN" --clients 1 --seconds 2
echo "automatic failover drill passed"
