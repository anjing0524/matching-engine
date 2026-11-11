#!/usr/bin/env bash
# Load one middleware/core shard with independent client processes.
#
# Usage:
#   scripts/run-load-clients.sh [clients] [orders_per_client] [in_flight]
# Example:
#   scripts/run-load-clients.sh 4 25000 32
set -euo pipefail
cd "$(dirname "$0")/.."
if ! command -v taskset >/dev/null 2>&1; then
  echo "taskset is required: all processes must be CPU-pinned" >&2
  exit 2
fi
root=$(pwd)
cargo build --release --bins >/dev/null

clients=${1:-4}
orders=${2:-25000}
in_flight=${3:-32}
middleware_cpus=${MIDDLEWARE_CPUS:-0}
core_cpu=${CORE_CPU:-1}
client_cpu_base=${CLIENT_CPU_BASE:-2}
client_cpu_span=${CLIENT_CPU_SPAN:-6}
if (( clients < 1 || clients > 10 )); then
  echo "clients must be 1..10" >&2
  exit 2
fi
dir=$(mktemp -d "${TMPDIR:-/tmp}/trading-clients.XXXXXX")
pids=()
cleanup() {
  for pid in "${pids[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  rm -rf "$dir"
}
trap cleanup EXIT

port=$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)
wait_port() {
  for _ in $(seq 1 100); do
    if python3 - "$port" <<'PY'
import socket, sys
try:
    socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=0.1).close()
except OSError:
    sys.exit(1)
PY
    then return 0; fi
    sleep 0.02
  done
  echo "port $port not ready" >&2
  return 1
}

taskset -c "$middleware_cpus" "$root/target/release/trading-middleware" \
  --listen "127.0.0.1:$port" \
  --command-ring "$dir/command.ring" \
  --public-ring "$dir/public.ring" \
  --private-ring "$dir/private.ring" \
  --iterations 0 --idle-timeout-secs 0 \
  >"$dir/middleware.log" 2>&1 &
pids+=($!)
wait_port
taskset -c "$core_cpu" "$root/target/release/trading-core" \
  --command-ring "$dir/command.ring" \
  --public-ring "$dir/public.ring" \
  --private-ring "$dir/private.ring" \
  --iterations 0 \
  >"$dir/core.log" 2>&1 &
pids+=($!)
sleep 0.2

client_pids=()
for ((i = 0; i < clients; i++)); do
  taskset -c $((client_cpu_base + i % client_cpu_span)) "$root/target/release/trading-load" \
    --addr "127.0.0.1:$port" \
    --members 1 \
    --orders-per-member "$orders" \
    --in-flight "$in_flight" \
    --buy-only \
    --member-offset $((i * 10)) \
    >"$dir/client-$i.log" 2>&1 &
  client_pids+=($!)
done
for pid in "${client_pids[@]}"; do wait "$pid"; done

total=0
for ((i = 0; i < clients; i++)); do
  line=$(tail -1 "$dir/client-$i.log")
  echo "client-$i: $line"
  rate=$(sed -n 's/.*rate=\([0-9]*\)\/s.*/\1/p' <<<"$line")
  total=$((total + rate))
done
echo "aggregate_clients: clients=$clients total_rate=${total}/s"
