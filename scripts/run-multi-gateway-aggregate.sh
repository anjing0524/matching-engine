#!/usr/bin/env bash
# Multi-gateway aggregate profile.
#
# One middleware + one core, N gateway proxies on separate CPUs, and one
# external pipelined client process per gateway.
#
# Usage:
#   scripts/run-multi-gateway-aggregate.sh [gateways] [orders_per_gateway] [in_flight]
# Example:
#   scripts/run-multi-gateway-aggregate.sh 2 250000 32
set -euo pipefail
cd "$(dirname "$0")/.."

gateways=${1:-2}
orders=${2:-250000}
in_flight=${3:-32}
if (( gateways < 1 || gateways > 4 )); then
  echo "gateways must be 1..4" >&2
  exit 2
fi
root=$(pwd)
dir=$(mktemp -d "${TMPDIR:-/tmp}/trading-multi-gateway.XXXXXX")
pids=()
cleanup() {
  for pid in "${pids[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  rm -rf "$dir"
}
trap cleanup EXIT

pick_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

wait_port() {
  local port=$1
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
  echo "port $port did not become ready" >&2
  return 1
}

command_ring="$dir/command.ring"
public_ring="$dir/public.ring"
private_ring="$dir/private.ring"
middleware_port=$(pick_port)

taskset -c 0 "$root/target/release/trading-middleware" \
  --listen "127.0.0.1:$middleware_port" \
  --command-ring "$command_ring" \
  --public-ring "$public_ring" \
  --private-ring "$private_ring" \
  --iterations 0 --idle-timeout-secs 0 \
  >"$dir/middleware.log" 2>&1 &
pids+=($!)
wait_port "$middleware_port"

taskset -c 1 "$root/target/release/trading-core" \
  --command-ring "$command_ring" \
  --public-ring "$public_ring" \
  --private-ring "$private_ring" \
  --iterations 0 \
  >"$dir/core.log" 2>&1 &
pids+=($!)

gateway_ports=()
for ((i = 0; i < gateways; i++)); do
  gateway_port=$(pick_port)
  gateway_ports+=("$gateway_port")
  cpu=$((2 + i))
  taskset -c "$cpu" "$root/target/release/trading-gateway" \
    --listen "127.0.0.1:$gateway_port" \
    --middleware "127.0.0.1:$middleware_port" \
    >"$dir/gateway-$i.log" 2>&1 &
  pids+=($!)
  wait_port "$gateway_port"
done

client_cpus=(4 5 6 7)
client_pids=()
for ((i = 0; i < gateways; i++)); do
  taskset -c "${client_cpus[$i]}" "$root/target/release/trading-load" \
    --addr "127.0.0.1:${gateway_ports[$i]}" \
    --members 1 \
    --orders-per-member "$orders" \
    --in-flight "$in_flight" \
    --member-offset "$((i * 10))" \
    >"$dir/client-$i.log" 2>&1 &
  client_pids+=($!)
done

for pid in "${client_pids[@]}"; do
  wait "$pid"
done

total=0
for ((i = 0; i < gateways; i++)); do
  line=$(tail -1 "$dir/client-$i.log")
  echo "gateway-$i: $line"
  rate=$(sed -n 's/.*rate=\([0-9]*\)\/s.*/\1/p' <<<"$line")
  total=$((total + rate))
done
echo "aggregate_gateways: gateways=$gateways total_rate=${total}/s"
