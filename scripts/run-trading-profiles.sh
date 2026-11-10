#!/usr/bin/env bash
# Category-based trading-system profiles.
#
# Usage:
#   scripts/run-trading-profiles.sh <category>
#
# Categories:
#   correctness  fmt + clippy + workspace tests
#   core         single- and multi-contract core latency
#   e2e          in-process end-to-end single-member latency
#   remote       out-of-process client latency
#   split        gateway -> middleware -> core latency
#   load         two-member load/latency
#   all          all categories
set -euo pipefail
cd "$(dirname "$0")/.."

category=${1:-all}
iters=${PROFILE_ITERS:-100000}
core_iters=${PROFILE_CORE_ITERS:-5000000}
contracts=${PROFILE_CONTRACTS:-3000}
members=${PROFILE_MEMBERS:-2}
orders=${PROFILE_ORDERS:-50000}

run_correctness() {
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-features
}

run_core() {
  CORE_LATENCY_ITERS="$core_iters" cargo test --release --lib \
    trading::core::kernel::tests::core_latency_profile -- --ignored --nocapture
  CORE_MULTI_CONTRACTS="$contracts" CORE_LATENCY_ITERS="${PROFILE_MULTI_ITERS:-1000000}" \
    cargo test --release --lib trading::core::kernel::tests::multi_contract_latency_profile \
    -- --ignored --nocapture
}

run_e2e() {
  LATENCY_ITERS="$iters" cargo test --release --test trading_latency_e2e \
    -- --ignored --nocapture
}

run_remote() {
  LATENCY_ITERS="$iters" cargo test --release --test trading_remote_latency \
    -- --ignored --nocapture
}

run_split() {
  LATENCY_ITERS="$iters" cargo test --release --test trading_split_latency \
    -- --ignored --nocapture
}

run_load() {
  LATENCY_MEMBERS="$members" LATENCY_ORDERS="$orders" \
    cargo test --release --test trading_load_latency -- --ignored --nocapture
  LATENCY_MEMBERS="$members" LATENCY_ORDERS="$orders" LATENCY_RATE=100000 \
    cargo test --release --test trading_load_latency -- --ignored --nocapture
}

case "$category" in
  correctness) run_correctness ;;
  core) run_core ;;
  e2e) run_e2e ;;
  remote) run_remote ;;
  split) run_split ;;
  load) run_load ;;
  all)
    run_correctness
    run_core
    run_e2e
    run_remote
    run_split
    run_load
    ;;
  *)
    echo "unknown category: $category" >&2
    exit 2
    ;;
esac
