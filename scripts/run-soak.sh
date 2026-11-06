#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== core 5M soak =="
cargo test --release --test exchange_long_soak -- --ignored --nocapture

echo "== timed soak (${EXCHANGE_SOAK_SECS:-30}s) =="
EXCHANGE_SOAK_SECS="${EXCHANGE_SOAK_SECS:-30}" cargo test --release --test exchange_timed_soak -- --ignored --nocapture

echo "== combo queue/fill soak =="
cargo test --release --test exchange_combo_soak -- --nocapture

echo "== combo command-ring soak =="
cargo test --release --test exchange_combo_e2e_soak -- --nocapture

echo "== multi-gateway TCP/UDP load =="
cargo test --release --test exchange_tcp_load -- --ignored --nocapture
