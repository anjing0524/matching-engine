#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== core 5M soak =="
cargo test --release --test trading_long_soak -- --ignored --nocapture

echo "== timed soak (${TRADING_SOAK_SECS:-30}s) =="
TRADING_SOAK_SECS="${TRADING_SOAK_SECS:-30}" cargo test --release --test trading_timed_soak -- --ignored --nocapture

echo "== combo queue/fill soak =="
cargo test --release --test trading_combo_soak -- --nocapture

echo "== combo command-ring soak =="
cargo test --release --test trading_combo_e2e_soak -- --nocapture

echo "== multi-gateway TCP/UDP load =="
cargo test --release --test trading_tcp_load -- --ignored --nocapture
