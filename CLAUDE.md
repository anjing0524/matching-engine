# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a **high-performance futures trading matching engine** written in 100% Safe Rust. The project implements an order matching system with:
- Low-latency order matching (microseconds per operation)
- Thousands of concurrent TCP clients
- ~4.4M order processing ops/sec throughput
- Memory-efficient object pooling
- Real-time trade broadcast to all clients

## Project Structure

```
matching-engine/
├── src/
│   ├── main.rs           # Orchestrates engine thread + network server
│   ├── lib.rs            # Module exports
│   ├── protocol.rs       # Client-server message types (NewOrderRequest, TradeNotification)
│   ├── orderbook.rs      # Core matching logic (282 lines) - BTreeMap + free list
│   ├── engine.rs         # Actor main loop (75 lines) - blocking_recv from MPSC channel
│   ├── network.rs        # TCP server (93 lines) - Tokio async runtime
│   └── bin/load_generator.rs # Load testing tool
├── tests/basic_trade.rs  # Integration test
├── benches/orderbook_benchmark.rs # Criterion benchmarks
└── Cargo.toml
```

## Architecture

### Three-Layer Design

1. **Async Network Layer (Tokio)**
   - TCP server on 127.0.0.1:8080
   - Multiple concurrent connections via `tokio::spawn`
   - JSON protocol using `serde_json` + `tokio_util::codec::LengthDelimitedCodec`
   - Unbounded MPSC channels send commands to engine, receive outputs via broadcast

2. **Single-Threaded Matching Engine (Actor)**
   - Dedicated system thread running `blocking_recv()` on command channel
   - Synchronous state machine - no contention, maximizes throughput
   - Processes `EngineCommand::NewOrder` and `EngineCommand::CancelOrder`
   - Generates `EngineOutput::Trade` and `EngineOutput::Confirmation`

3. **OrderBook Data Structure**
   - Bids: `BTreeMap<price, PriceLevel>` sorted descending (highest price first)
   - Asks: `BTreeMap<price, PriceLevel>` sorted ascending (lowest price first)
   - Orders: Dense `Vec<OrderNode>` with object pool via free list
   - PriceLevel contains doubly-linked list (FIFO ordering at each price)

### Order Matching Flow

```
Client TCP → JSON Frame → EngineCommand::NewOrder
                              ↓
                         OrderBook::match_order()
                              ↓
                         [Buy: iterate asks lowest→highest]
                         [Sell: iterate bids highest→lowest]
                              ↓
                         Generate TradeNotification + OrderConfirmation
                              ↓
                         Broadcast to all TCP clients
```

## Common Development Tasks

### Build & Run

```bash
cd /Users/liushuo/code/tradeing/matching-engine

# Development
cargo build
cargo run

# Release (optimized)
cargo build --release
cargo run --release
```

Server listens on `127.0.0.1:8080`

### Testing

```bash
# Integration test (buy/sell matching)
cargo test --test basic_trade -- --nocapture

# All tests
cargo test

# With logging
RUST_LOG=debug cargo test --lib
```

### Benchmarking

```bash
# Run Criterion benchmarks
cargo bench

# Results saved to: target/criterion/
```

### Load Testing

```bash
# In terminal 1: start server
cargo run --release

# In terminal 2: run load generator (8 clients, 10s duration)
cargo run --release --bin load_generator

# Measures TPS (transactions per second) and latency
```

### Code Quality

```bash
cargo check              # Fast syntax check
cargo clippy             # Lint warnings
cargo fmt                # Format code
cargo clippy --fix       # Auto-fix clippy warnings
```

## Key Design Patterns

### 1. Actor Model for Thread Safety
- Single-threaded matching engine eliminates lock contention
- MPSC channels decouple network I/O from order processing
- No `Arc<Mutex<>>` or atomic variables needed

### 2. Memory Pooling with Free List
See `orderbook.rs`:
- Reuse allocated `OrderNode` structures instead of allocating per order
- Free list head tracks recycled nodes
- `order_id_to_index` enables O(log n) lookups by order ID

### 3. BTreeMap Price Levels
- Bids sorted price descending (maximizes sell matches)
- Asks sorted price ascending (minimizes buy prices)
- O(log n) insertion/removal at any price

### 4. Broadcast Output Channel
- All TCP clients subscribe to engine output
- Trade notifications broadcast immediately to all clients
- Real-time market view consistency

## Important Implementation Notes

### OrderBook Matching Algorithm (orderbook.rs)

- **Buy Order**: Match against `asks` from lowest price upward, execute as many as possible
- **Sell Order**: Match against `bids` from highest price downward, execute as many as possible
- Partial fills: Remaining quantity added to book as new order
- Full fills: Order node recycled via free list

### Protocol (protocol.rs)

- `NewOrderRequest`: `{user_id, order_id, price, quantity, order_type}`
- `TradeNotification`: `{trade_id, buyer_id, seller_id, price, quantity, timestamp}`
- `OrderConfirmation`: `{order_id, remaining_quantity}`

### Engine Loop (engine.rs)

- Blocking receive on command channel: `self.command_receiver.blocking_recv()`
- Process one command at a time (synchronous)
- Send outputs to broadcast channel immediately
- No async/await in engine itself

## Performance Characteristics

| Operation | Time | Throughput |
|-----------|------|-----------|
| Add order (no match) | ~227 ns | ~4.4M ops/sec |
| Memory per order | 100-150 bytes | ~1M max orders |
| Concurrent clients | Thousands (TCP limited) | |
| Typical latency | <1ms | (network + matching) |

## Current Implementation Status

**Completed:**
- Order matching (Buy/Sell)
- TCP network server
- FIFO order queues at price levels
- Trade generation & broadcast
- Integration tests & benchmarks

**TODO:**
- Order cancellation (skeleton in `engine.rs` line 67-70)
- Multi-symbol support (currently hardcoded to one symbol)
- Margin/leverage features
- Data persistence
- Authentication

## Recommended Practices

1. **No Unsafe Code**: This is a safety-first project. Keep it 100% safe Rust.
2. **Protocol Changes**: Update both `protocol.rs` AND client-server message flow documentation
3. **OrderBook Changes**: Test with `cargo bench` before/after to catch performance regressions
4. **Adding Features**: Prefer actor message types (EngineCommand/EngineOutput) over shared state
5. **Logging**: Use `tracing` crate for structured logging, not println!

## Dependencies

Core dependencies in `Cargo.toml`:
- `tokio` - Async runtime for TCP server
- `tokio-util` - Length-delimited codec
- `serde`/`serde_json` - JSON serialization
- `parking_lot` - Optimized locks (imported but not actively used)
- `bumpalo` - Arena allocator (optional feature)
- `criterion` - Benchmarking framework

Optional: `tikv-jemallocator` for memory profiling
