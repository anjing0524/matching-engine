# Code audit: public API and dependencies

## Method

- scanned every tracked Rust file
- listed `pub` definitions and counted name occurrences
- tightened internal module visibility, then let `-D dead_code` reveal
  test-only or unused methods
- checked each manifest dependency against real `use`/path usage

`cargo udeps` was not run: it requires nightly and the installation attempt was
aborted. The dependency result below is a manual audit, not a udeps result.

## Removed: public functions with no call sites

```text
TickBasedOrderBook::order_owner
TickBasedOrderBook::can_accept_order
SymbolId::with_name
refill_interval
MarketDataPublisher::join_multicast
read_private_log_dir
```

## Removed or hidden after visibility tightening

```text
EventFanout::switch_interface        removed (no call)
SpscRing::slot_size                  removed (no call)
SpscRing::try_pop_into               kept private to tests
EventFanout::repair                  kept test-only (used by runtime test)
```

## Module visibility tightened to pub(crate)

```text
trading::transport::spsc_ring
trading::runtime::event_fanout
trading::combo::exec
domain::orderbook::tick_based
```

Public types are still re-exported where needed.

## Dependencies

Manual check found every declared dependency used:

```text
serde              snapshot serialization
tracing            order-book warnings
tikv-jemallocator  global allocator
memmap2            mmap SPSC ring
socket2            multicast socket
smallvec           core trade/event buffers
clap               binaries
crc32fast          snapshot checksum
serde_json         snapshot envelope
criterion          benches (dev)
tempfile           tests (dev)
```

`cargo udeps` remains an open check for CI on a nightly toolchain.

## Verification

```text
cargo fmt --check ✅
cargo clippy --workspace --all-targets --all-features -D warnings ✅
cargo test --workspace --all-features
  trading-domain:         30 passed
  trading-system-engine:  181 passed, 4 ignored
```

## Remaining public API candidates

Items whose only non-definition uses are tests or internal helpers still exist.
They were not removed because they are exercised behavior; tightening them
further should be done with a nightly `cargo udeps`/public-api check and an
explicit compatibility decision.
