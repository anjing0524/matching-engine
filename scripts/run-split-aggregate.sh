#!/usr/bin/env bash
# Split-process aggregate load profile.
#
# gateway -> middleware -> core, with an external pipelined load generator.
#
# Usage:
#   scripts/run-split-aggregate.sh [members] [orders_per_member] [in_flight]
# Example:
#   scripts/run-split-aggregate.sh 4 250000 32
set -euo pipefail
cd "$(dirname "$0")/.."

members=${1:-2}
orders=${2:-250000}
in_flight=${3:-32}
middleware_cpus=${MIDDLEWARE_CPUS:-0}
core_cpus=${CORE_CPUS:-1}
gateway_cpus=${GATEWAY_CPUS:-2}
client_cpus=${CLIENT_CPUS:-4,5,6,7}
if ! command -v taskset >/dev/null 2>&1; then
  echo "taskset is required: all processes must be CPU-pinned" >&2
  exit 2
fi
root=$(pwd)
cargo build --release --bins >/dev/null
dir=$(mktemp -d "${TMPDIR:-/tmp}/trading-split-aggregate.XXXXXX")
middleware_pid=
core_pid=
gateway_pid=
cleanup() {
  for pid in "$gateway_pid" "$core_pid" "$middleware_pid"; do
    if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; fi
  done
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
gateway_port=$(pick_port)

taskset -c "$middleware_cpus" "$root/target/release/trading-middleware" \
  --listen "127.0.0.1:$middleware_port" \
  --command-ring "$command_ring" \
  --public-ring "$public_ring" \
  --private-ring "$private_ring" \
  --iterations 0 --idle-timeout-secs 0 \
  >"$dir/middleware.log" 2>&1 &
middleware_pid=$!
wait_port "$middleware_port"

taskset -c "$core_cpus" "$root/target/release/trading-core" \
  --command-ring "$command_ring" \
  --public-ring "$public_ring" \
  --private-ring "$private_ring" \
  --iterations 0 \
  >"$dir/core.log" 2>&1 &
core_pid=$!

taskset -c "$gateway_cpus" "$root/target/release/trading-gateway" \
  --listen "127.0.0.1:$gateway_port" \
  --middleware "127.0.0.1:$middleware_port" \
  >"$dir/gateway.log" 2>&1 &
gateway_pid=$!
wait_port "$gateway_port"
sleep 0.1

taskset -c "$client_cpus" "$root/target/release/trading-load" \
  --addr "127.0.0.1:$gateway_port" \
  --members "$members" \
  --orders-per-member "$orders" \
  --in-flight "$in_flight"
