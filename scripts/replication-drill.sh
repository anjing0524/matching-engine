#!/usr/bin/env bash
# Warm-standby replication + manual failover drill.
# Requires: built release binaries and a local Unix-socket filesystem.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --bin matching-engine --bin gateway --bin standby --example network_client >/dev/null

TMP=$(mktemp -d "${TMPDIR:-/tmp}/matching-repl-drill.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT

"$ROOT/target/release/matching-engine" \
  --engine-uds "$TMP/engine.sock" --data-dir "$TMP/primary" \
  --replica-listen "$TMP/repl.sock" --queue-capacity 100000 \
  --snapshot-interval 0 --wal-sync false --metrics-port 19234 --log-level error \
  >"$TMP/primary.log" 2>&1 &
PRIMARY=$!
PIDS+=("$PRIMARY")
sleep 1.2

"$ROOT/target/release/standby" \
  --primary-uds "$TMP/repl.sock" --data-dir "$TMP/standby" --wal-sync false \
  >"$TMP/standby.log" 2>&1 &
STANDBY=$!
PIDS+=("$STANDBY")
sleep 1.0

"$ROOT/target/release/gateway" \
  --listen 127.0.0.1:18232 --engine-uds "$TMP/engine.sock" \
  --io-threads 2 --max-in-flight 8 \
  >"$TMP/gateway.log" 2>&1 &
GATEWAY=$!
PIDS+=("$GATEWAY")
sleep 0.8

SERVER_ADDR=127.0.0.1:18232 timeout 10 "$ROOT/target/release/examples/network_client" >"$TMP/client.log" 2>&1
sleep 0.8

# Promotion guard: standby must have acked all shipped records and nothing may
# have been dropped because it was too slow.
HEALTH=$(curl -sf http://127.0.0.1:19234/health)
echo "$HEALTH" | grep -q '"replication_lag_records":0' || {
  echo "replication lag is non-zero; promotion unsafe: $HEALTH" >&2
  exit 1
}
echo "$HEALTH" | grep -q '"replication_dropped_batches":0' || {
  echo "replication batches were dropped; promotion unsafe: $HEALTH" >&2
  exit 1
}
echo "$HEALTH" | grep -q '"replication_connected":1' || {
  echo "standby is not connected: $HEALTH" >&2
  exit 1
}

# Simulate primary/gateway failure, then stop the standby and promote its data dir.
kill -9 "$PRIMARY" "$GATEWAY" 2>/dev/null || true
sleep 0.8
kill -TERM "$STANDBY" 2>/dev/null || true
sleep 0.5

grep -q "last_seq=3" "$TMP/standby.log" || {
  echo "standby did not receive expected WAL records" >&2
  cat "$TMP/standby.log" >&2
  exit 1
}

"$ROOT/target/release/matching-engine" \
  --engine-uds "$TMP/promoted.sock" --data-dir "$TMP/standby" \
  --snapshot-interval 0 --metrics-port 0 --log-level info \
  >"$TMP/promoted.log" 2>&1 &
PROMOTED=$!
PIDS+=("$PROMOTED")
sleep 1.2

grep -q "WAL replay complete records=4 last_seq=3" "$TMP/promoted.log" || {
  echo "promoted engine did not recover the shipped WAL" >&2
  cat "$TMP/promoted.log" >&2
  exit 1
}
echo "replication drill passed"
