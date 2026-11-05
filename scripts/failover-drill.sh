#!/usr/bin/env bash
# Full warm-standby failover:
#   primary + standby + gateway -> kill primary -> promote standby data dir on
#   the same engine UDS -> existing gateway reconnects -> new client trades.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --bin matching-engine --bin gateway --bin standby \
  --bin latency_probe --example network_client >/dev/null

TMP=$(mktemp -d "${TMPDIR:-/tmp}/matching-failover.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT

SOCK="$TMP/engine.sock"
REPL="$TMP/repl.sock"
PRIMARY_DATA="$TMP/primary"
STANDBY_DATA="$TMP/standby"
LISTEN=127.0.0.1:18261
METRICS=19235

RUST_LOG=warn "$ROOT/target/release/matching-engine" \
  --engine-uds "$SOCK" --data-dir "$PRIMARY_DATA" --replica-listen "$REPL" \
  --lease-file "$TMP/engine.lease" \
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

# Wait until the standby has acked all records and nothing was dropped.
for _ in $(seq 1 50); do
  HEALTH=$(curl -sf "http://127.0.0.1:$METRICS/health" || true)
  if echo "$HEALTH" | grep -q '"replication_lag_records":0' \
     && echo "$HEALTH" | grep -q '"replication_dropped_batches":0' \
     && echo "$HEALTH" | grep -q '"replication_connected":1'; then
    break
  fi
  sleep 0.1
done
if ! echo "${HEALTH:-}" | grep -q '"replication_lag_records":0'; then
  echo "standby was not safely caught up before failover" >&2
  exit 1
fi

# Primary failure. Gateway must stay alive and retry the same UDS path.
kill -9 "$PRIMARY" 2>/dev/null || true
sleep 1.0
kill -0 "$GATEWAY" || { echo "gateway exited on primary failure" >&2; exit 1; }
kill -TERM "$STANDBY" 2>/dev/null || true
sleep 0.5

# Promote the standby WAL on the same engine UDS path.
RUST_LOG=warn "$ROOT/target/release/matching-engine" \
  --engine-uds "$SOCK" --data-dir "$STANDBY_DATA" \
  --lease-file "$TMP/engine.lease" \
  --queue-capacity 100000 --snapshot-interval 0 --wal-sync false \
  --metrics-port 0 --log-level error >"$TMP/promoted.log" 2>&1 &
PROMOTED=$!
PIDS+=("$PROMOTED")

for _ in $(seq 1 50); do
  if grep -q "engine IPC reconnected" "$TMP/gateway.log"; then break; fi
  sleep 0.1
done
grep -q "engine IPC reconnected" "$TMP/gateway.log" || {
  echo "gateway did not reconnect to promoted engine" >&2
  cat "$TMP/gateway.log" >&2
  exit 1
}

# New user/client ids avoid the recovered idempotency keys.
"$ROOT/target/release/latency_probe" --server "$LISTEN" --clients 1 --seconds 2
echo "failover drill passed"
