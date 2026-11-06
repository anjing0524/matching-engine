#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== core 5M soak =="
cargo test --release --test exchange_long_soak -- --ignored --nocapture

echo "== combo queue/fill soak =="
cargo test --release --test exchange_combo_soak -- --nocapture

echo "== combo command-ring soak =="
cargo test --release --test exchange_combo_e2e_soak -- --nocapture

echo "== multi-gateway TCP/UDP load =="
cargo test --release --test exchange_tcp_load -- --ignored --nocapture
