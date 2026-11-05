#!/usr/bin/env bash
# A standby joins after the primary has already accepted orders. It must
# receive a bootstrap snapshot/WAL prefix plus live records, then be promotable.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --bin matching-engine --bin gateway --bin standby \
  --bin latency_probe --example network_client >/dev/null

TMP=$(mktemp -d "${TMPDIR:-/tmp}/matching-catchup.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT
SOCK="$TMP/engine.sock"
REPL="$TMP/repl.sock"
LISTEN=127.0.0.1:18264
METRICS=19237

RUST_LOG=warn "$ROOT/target/release/matching-engine" \
  --engine-uds "$SOCK" --data-dir "$TMP/primary" --replica-listen "$REPL" \
  --lease-file "$TMP/engine.lease" --queue-capacity 100000 \
  --snapshot-interval 2 --wal-sync false --metrics-port "$METRICS" \
  --log-level error >"$TMP/primary.log" 2>&1 &
PRIMARY=$!
PIDS+=("$PRIMARY")
sleep 1.2
RUST_LOG=info "$ROOT/target/release/gateway" \
  --listen "$LISTEN" --engine-uds "$SOCK" --io-threads 2 \
  >"$TMP/gateway.log" 2>&1 &
GATEWAY=$!
PIDS+=("$GATEWAY")
sleep 1.0
# Traffic happens BEFORE the standby joins.
SERVER_ADDR="$LISTEN" timeout 10 "$ROOT/target/release/examples/network_client" >/dev/null 2>&1

RUST_LOG=warn "$ROOT/target/release/standby" \
  --primary-uds "$REPL" --data-dir "$TMP/standby" --wal-sync true \
  >"$TMP/standby.log" 2>&1 &
STANDBY=$!
PIDS+=("$STANDBY")

HEALTH=""
for _ in $(seq 1 100); do
  HEALTH=$(curl -sf "http://127.0.0.1:$METRICS/health" || true)
  echo "$HEALTH" | grep -q '"replication_lag_records":0' \
    && echo "$HEALTH" | grep -q '"replication_connected":1' && break
  sleep 0.1
done
echo "$HEALTH" | grep -q '"replication_lag_records":0' || {
  echo "late standby never caught up" >&2
  cat "$TMP/standby.log" >&2
  exit 1
}

kill -9 "$PRIMARY" 2>/dev/null || true
sleep 0.8
kill -TERM "$STANDBY" 2>/dev/null || true
sleep 0.4
RUST_LOG=warn "$ROOT/target/release/matching-engine" \
  --engine-uds "$SOCK" --data-dir "$TMP/standby" \
  --lease-file "$TMP/engine.lease" --queue-capacity 100000 \
  --snapshot-interval 0 --wal-sync false --metrics-port 0 \
  --log-level error >"$TMP/promoted.log" 2>&1 &
PROMOTED=$!
PIDS+=("$PROMOTED")
for _ in $(seq 1 50); do
  grep -q "engine IPC reconnected" "$TMP/gateway.log" && break
  sleep 0.1
done
grep -q "engine IPC reconnected" "$TMP/gateway.log" || {
  echo "gateway did not reconnect to promoted late standby" >&2
  exit 1
}
"$ROOT/target/release/latency_probe" --server "$LISTEN" --clients 1 --seconds 2
echo "standby catch-up drill passed"
