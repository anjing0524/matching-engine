# matching-engine

A simple matching engine.

# Safe Rust Futures Trading Matching Engine

A high-performance, low-latency order matching engine written in 100% Safe Rust, designed to process millions of trades per second.

## Quick Start

### Prerequisites
- Rust 1.56+ ([Install](https://www.rust-lang.org/tools/install))

### Build & Run

```bash
# Clone/navigate to project
cd /Users/liushuo/code/tradeing/matching-engine

# Build (development)
cargo build

# Build (optimized release)
cargo build --release

# Run the server
cargo run --release
# Server listens on 127.0.0.1:8080

# In another terminal, run integration test
cargo test --test basic_trade -- --nocapture

# Or run load generator
cargo run --release --bin load_generator
```

## Project Structure

```
src/
├── main.rs              # Application entry point (51 lines)
├── lib.rs               # Module exports
├── protocol.rs          # Client-server data types (50 lines)
├── engine.rs            # Matching engine main loop (75 lines)
├── network.rs           # TCP server implementation (93 lines)
├── orderbook.rs         # Order matching logic (282 lines)
└── bin/
    └── load_generator.rs # Performance test tool (140 lines)

tests/
└── basic_trade.rs       # Integration test

benches/
└── orderbook_benchmark.rs # Criterion benchmarks

Documentation:
├── ARCHITECTURE.md      # Complete system design (431 lines)
├── SYSTEM_DIAGRAM.md    # Visual architecture diagrams (374 lines)
├── PROGRESS.md          # Implementation status
└── BENCHMARK_REPORT.md  # Performance analysis
```

## Architecture Overview

### Three-Layer Design

```
┌─────────────────────────────────┐
│  Async Network Layer (Tokio)    │
│  - TCP Server on :8080          │
│  - Multiple concurrent clients  │
│  - JSON protocol                │
└──────────────┬──────────────────┘
               │ MPSC Channels
┌──────────────↓──────────────────┐
│  Matching Engine Thread         │
│  - Single-threaded actor        │
│  - Blocking message loop        │
│  - Order matching logic         │
└──────────────┬──────────────────┘
               │ OrderBook
┌──────────────↓──────────────────┐
│  Order Book Data Structure      │
│  - BTreeMap for prices          │
│  - Vec object pool              │
│  - Free list for memory reuse   │
└─────────────────────────────────┘
```

### Key Design Decisions

1. **Single-threaded matching engine** - Eliminates contention, maximizes throughput
2. **Async network layer** - Handles thousands of concurrent clients efficiently
3. **Memory-pooled order book** - O(1) allocation via free list
4. **BTreeMap price levels** - O(log n) price lookup with sorted iteration
5. **Broadcast output** - All clients see consistent market updates

## Core Concepts

### OrderBook Data Structure

- **Bids**: BTreeMap sorted by price descending (highest first)
- **Asks**: BTreeMap sorted by price ascending (lowest first)
- **Orders**: Dense Vec with object pool for memory efficiency
- **Price Levels**: Doubly-linked lists for FIFO ordering (price-time priority)

### Order Matching Algorithm

1. For Buy orders: Iterate asks from lowest price, match sellers
2. For Sell orders: Iterate bids from highest price, match buyers
3. For each match: Create TradeNotification, update quantities
4. If quantity remains: Add order to book, send OrderConfirmation
5. If fully filled: Recycle OrderNode via free list

### Communication Flow

```
Client TCP Connection
        ↓
    JSON Frame
        ↓
NewOrderRequest → Network Handler → EngineCommand → Matching Engine
                                                          ↓
                                              [Process via OrderBook]
                                                          ↓
                                    TradeNotification + OrderConfirmation
                                                          ↓
                                              Broadcast Channel
                                                          ↓
                                         All TCP Clients Receive Update
```

## Technology Stack

| Component | Technology | Version |
|-----------|-----------|---------|
| **Runtime** | Tokio | 1.x |
| **Language** | Rust | 2021 Edition |
| **Serialization** | serde + serde_json | 1.0 |
| **Network** | tokio-util | 0.7 |
| **Logging** | tracing | 0.1 |
| **Benchmarking** | Criterion | 0.5 |
| **Allocator** | (optional) jemalloc | 0.5 |

## Performance Metrics

### Current Benchmarks

| Operation | Time | Throughput |
|-----------|------|-----------|
| Add order (no match) | ~227 ns | ~4.4M ops/sec |
| Full match | ~4.287 ms | ~233 ops/sec* |

*Note: Matching benchmark includes setup overhead - see BENCHMARK_REPORT.md for details*

### Capacity Estimates

- **Memory per order**: ~100-150 bytes
- **Max orders**: ~1M in memory (1-1.5 GB RAM)
- **Concurrent clients**: Thousands (TCP limited)
- **Latency**: <1ms typical (network + matching)

## Testing

### Integration Tests
```bash
cargo test --test basic_trade -- --nocapture
```
Tests: Buy order → Sell order matching → Trade verification

### Benchmarks
```bash
cargo bench
```
Statistical analysis using Criterion framework

### Load Generator
```bash
cargo run --release --bin load_generator
```
- 8 concurrent TCP clients
- 10-second duration
- Measures throughput (TPS) and latency

## Current Status

### Completed ✓
- Order matching algorithm (Buy/Sell)
- TCP network server with JSON protocol
- FIFO order queue at each price level
- Trade generation and broadcast
- Integration tests & benchmarks
- Comprehensive documentation

### Not Yet Implemented ✗
- Order cancellation logic (skeleton exists)
- Multiple trading symbols
- Margin/leverage features
- Data persistence
- Authentication/authorization

## Documentation

- **[ARCHITECTURE.md](ARCHITECTURE.md)** - Complete system design, modules, patterns
- **[SYSTEM_DIAGRAM.md](SYSTEM_DIAGRAM.md)** - Visual diagrams of architecture, flows, data structures
- **[PROGRESS.md](PROGRESS.md)** - Implementation checklist and status
- **[BENCHMARK_REPORT.md](BENCHMARK_REPORT.md)** - Performance analysis and optimization opportunities

## Recommended Next Steps

1. **Fix matching benchmark** - Separate setup from measurement
2. **Implement cancellation** - Complete unfinished feature
3. **Multi-symbol support** - Parameterize symbol handling
4. **Persistence layer** - Add database for trade history
5. **Hardware optimization** - CPU affinity, jemalloc, batching
6. **Stress testing** - Comprehensive load scenarios

## Build Artifacts

- **Debug binary**: `target/debug/matching-engine`
- **Release binary**: `target/release/matching-engine`
- **Load generator**: `target/release/load_generator`
- **Benchmark results**: `target/criterion/`

## Useful Commands

```bash
# Development
cargo build                    # Debug build
cargo build --release         # Release build
cargo run --release          # Run server
cargo test                   # Run all tests
cargo bench                  # Run benchmarks
cargo clean                  # Clean artifacts

# Code quality
cargo check                  # Fast syntax check
cargo clippy                 # Lint warnings
cargo fmt                    # Format code
cargo doc --open            # Generate & view docs

# Dependencies
cargo tree                   # Show dependency graph
cargo update                 # Update dependencies
cargo outdated              # Check for updates
```

## Project Statistics

- **Total lines of code**: ~556 (excluding tests/benches)
- **Safe Rust**: 100% (no `unsafe` blocks)
- **Dependencies**: 9 core + 1 optional
- **Documentation**: 1100+ lines across 4 files

## License

[Add your license here]

## Author

Safe Rust Futures Matching Engine - Prototype Implementation

---

**For detailed architecture information, see [ARCHITECTURE.md](ARCHITECTURE.md)**

**For visual diagrams, see [SYSTEM_DIAGRAM.md](SYSTEM_DIAGRAM.md)**
