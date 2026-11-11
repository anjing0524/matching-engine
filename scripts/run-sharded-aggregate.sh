#!/usr/bin/env bash
# Independent middleware/core shards with one gateway and one client each.
#
# Usage:
#   scripts/run-sharded-aggregate.sh [shards] [orders_per_shard] [in_flight]
# Example:
#   scripts/run-sharded-aggregate.sh 2 50000 32
set -euo pipefail
cd "$(dirname "$0")/.."

shards=${1:-2}
orders=${2:-50000}
in_flight=${3:-32}
use_proxy=${USE_PROXY:-1}
if (( shards < 1 || shards > 2 )); then
  echo "shards must be 1..2 on this 4C/8T host" >&2
  exit 2
fi
if [[ "${USE_PROXY:-1}" == "1" && $shards -gt 1 ]]; then
  echo "2 shards with proxy need more than 8 hardware threads; use USE_PROXY=0" >&2
  exit 2
fi
root=$(pwd)
dir=$(mktemp -d "${TMPDIR:-/tmp}/trading-shards.XXXXXX")
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
  echo "port $port not ready" >&2
  return 1
}

declare -a middleware_ports
declare -a gateway_ports
for ((i = 0; i < shards; i++)); do
  command_ring="$dir/shard-$i-command.ring"
  public_ring="$dir/shard-$i-public.ring"
  private_ring="$dir/shard-$i-private.ring"
  middleware_port=$(pick_port)
  gateway_port=$(pick_port)
  middleware_ports+=("$middleware_port")
  gateway_ports+=("$gateway_port")
  if [[ "$use_proxy" == "1" ]]; then
    middleware_cpu=$((i * 3))
    core_cpu=$((i * 3 + 1))
    gateway_cpu=$((i * 3 + 2))
  else
    middleware_cpu=$((i * 2))
    core_cpu=$((i * 2 + 1))
    gateway_cpu=0
  fi
  taskset -c "$middleware_cpu" "$root/target/release/trading-middleware" \
    --listen "127.0.0.1:$middleware_port" \
    --command-ring "$command_ring" \
    --public-ring "$public_ring" \
    --private-ring "$private_ring" \
    --iterations 0 --idle-timeout-secs 0 \
    >"$dir/shard-$i-middleware.log" 2>&1 &
  pids+=($!)
  wait_port "$middleware_port"
  taskset -c "$core_cpu" "$root/target/release/trading-core" \
    --command-ring "$command_ring" \
    --public-ring "$public_ring" \
    --private-ring "$private_ring" \
    --iterations 0 \
    >"$dir/shard-$i-core.log" 2>&1 &
  pids+=($!)
  if [[ "$use_proxy" == "1" ]]; then
    taskset -c "$gateway_cpu" "$root/target/release/trading-gateway" \
      --listen "127.0.0.1:$gateway_port" \
      --middleware "127.0.0.1:$middleware_port" \
      >"$dir/shard-$i-gateway.log" 2>&1 &
    pids+=($!)
    wait_port "$gateway_port"
  else
    gateway_ports[$i]="$middleware_port"
  fi
done
sleep 0.1

client_pids=()
for ((i = 0; i < shards; i++)); do
  if [[ "$use_proxy" == "1" ]]; then
    client_cpus="3,4"
  else
    client_cpus="$((4 + i * 2)),$((5 + i * 2))"
  fi
  taskset -c "$client_cpus" "$root/target/release/trading-load" \
    --addr "127.0.0.1:${gateway_ports[$i]}" \
    --members 1 \
    --orders-per-member "$orders" \
    --in-flight "$in_flight" \
    --buy-only \
    --member-offset $((i * 10)) \
    >"$dir/shard-$i-client.log" 2>&1 &
  client_pids+=($!)
done
for pid in "${client_pids[@]}"; do wait "$pid"; done

total=0
for ((i = 0; i < shards; i++)); do
  line=$(tail -1 "$dir/shard-$i-client.log")
  echo "shard-$i: $line"
  rate=$(sed -n 's/.*rate=\([0-9]*\)\/s.*/\1/p' <<<"$line")
  total=$((total + rate))
done
echo "aggregate_shards: shards=$shards total_rate=${total}/s"
