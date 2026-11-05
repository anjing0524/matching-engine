#!/usr/bin/env bash
# Reproducible Mac (Apple Silicon) benchmark aligned to the local core count.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --bin matching-engine --bin gateway --bin latency_probe \
  --bin load_generator >/dev/null

TMP=$(mktemp -d "${TMPDIR:-/tmp}/matching-bench.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT

NCpu=$(sysctl -n hw.ncpu)
P=$(sysctl -n hw.perflevel0.logicalcpu 2>/dev/null || echo "?")
E=$(sysctl -n hw.perflevel1.logicalcpu 2>/dev/null || echo "?")
echo "# Mac benchmark"
echo
echo "- CPU: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)"
echo "- logical cores: $NCpu (P=$P, E=$E)"
echo "- rustc: $(rustc --version)"
echo

start_engine_uds() {
  local tag=$1 data=$2 sync=$3 metrics=${4:-0}
  "$ROOT/target/release/matching-engine" --engine-uds "$TMP/$tag.sock" \
    --data-dir "$data" --queue-capacity 100000 --snapshot-interval 0 \
    --wal-sync "$sync" --metrics-port "$metrics" --log-level error \
    >"$TMP/$tag.engine.log" 2>&1 &
  ENGINE=$!
  PIDS+=("$ENGINE")
  sleep 1.0
}
start_engine_ring() {
  local metrics=${1:-0}
  "$ROOT/target/release/matching-engine" --ipc-ring-dir "$TMP/ring-ipc" \
    --ipc-ring-threads 4 --data-dir "$TMP/ring-data" --queue-capacity 100000 \
    --snapshot-interval 0 --wal-sync false --metrics-port "$metrics" --log-level error \
    >"$TMP/ring.engine.log" 2>&1 &
  ENGINE=$!
  PIDS+=("$ENGINE")
  sleep 1.0
}
start_gateway() {
  local tag=$1 port=$2 transport=$3 inflight=$4
  if [[ $transport == uds ]]; then
    "$ROOT/target/release/gateway" --listen "127.0.0.1:$port" \
      --engine-uds "$TMP/$tag.sock" --io-threads 4 --max-in-flight "$inflight" \
      >"$TMP/$tag.gateway.log" 2>&1 &
  else
    "$ROOT/target/release/gateway" --listen "127.0.0.1:$port" \
      --ipc-ring-dir "$TMP/ring-ipc" --io-threads 4 --max-in-flight "$inflight" \
      >"$TMP/$tag.gateway.log" 2>&1 &
  fi
  GATEWAY=$!
  PIDS+=("$GATEWAY")
  sleep 0.8
}
stop_pair() {
  kill -9 "$GATEWAY" "$ENGINE" 2>/dev/null || true
  wait "$GATEWAY" 2>/dev/null || true
  wait "$ENGINE" 2>/dev/null || true
  sleep 0.2
}

echo "## Closed-loop latency (4 clients, 8s)"
rm -rf "$TMP/uds-lat"; mkdir -p "$TMP/uds-lat"
start_engine_uds uds "$TMP/uds-lat" false
start_gateway uds 18270 uds 64
"$ROOT/target/release/latency_probe" --server 127.0.0.1:18270 --clients 4 --seconds 8
stop_pair

rm -rf "$TMP/ring-data" "$TMP/ring-ipc"; mkdir -p "$TMP/ring-data"
start_engine_ring
start_gateway ring 18271 ring 64
"$ROOT/target/release/latency_probe" --server 127.0.0.1:18271 --clients 4 --seconds 8
stop_pair

echo
echo "## High-concurrency throughput (64 clients)"
run_load() {
  local port=$1
  CLIENTS=64 SERVER_ADDR="127.0.0.1:$port" METRICS_ADDR="127.0.0.1:19270" \
    timeout 15 "$ROOT/target/release/load_generator" 2>&1 | \
    grep -E "engine rate|engine\)|total trades|rejected|引擎订单速率|请求\(engine\)|总撮合|被拒绝" || true
}
rm -rf "$TMP/uds-tp"; mkdir -p "$TMP/uds-tp"
start_engine_uds uds "$TMP/uds-tp" false 19270
"$ROOT/target/release/gateway" --listen 127.0.0.1:18272 \
  --engine-uds "$TMP/uds.sock" --io-threads 4 --max-in-flight 1024 \
  >"$TMP/tp-uds.log" 2>&1 &
GATEWAY=$!; PIDS+=("$GATEWAY"); sleep 0.8
run_load 18272
kill -9 "$GATEWAY" "$ENGINE" 2>/dev/null || true
wait "$GATEWAY" 2>/dev/null || true; wait "$ENGINE" 2>/dev/null || true

rm -rf "$TMP/ring-data" "$TMP/ring-ipc"; mkdir -p "$TMP/ring-data"
start_engine_ring 19270
"$ROOT/target/release/gateway" --listen 127.0.0.1:18273 \
  --ipc-ring-dir "$TMP/ring-ipc" --io-threads 4 --max-in-flight 1024 \
  >"$TMP/tp-ring.log" 2>&1 &
GATEWAY=$!; PIDS+=("$GATEWAY"); sleep 0.8
run_load 18273
kill -9 "$GATEWAY" "$ENGINE" 2>/dev/null || true
wait "$GATEWAY" 2>/dev/null || true; wait "$ENGINE" 2>/dev/null || true

echo
echo "## Durable group commit (UDS engine, 16 clients, 8s)"
rm -rf "$TMP/durable"; mkdir -p "$TMP/durable"
start_engine_uds durable "$TMP/durable" true
start_gateway durable 18274 uds 64
"$ROOT/target/release/latency_probe" --server 127.0.0.1:18274 --clients 16 --seconds 8
stop_pair
