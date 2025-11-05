# Trading Matching Engine - Codebase Structure & Architecture Overview

## 1. High-Level Project Architecture

### 1.1 System Design Overview

This is a **high-performance futures trading matching engine** written in 100% Safe Rust, designed to process millions of order matches per second. The system follows a **decoupled actor-based architecture** with three main components:

```
┌─────────────────────────────────────────────────────────────────┐
│                     Async Network Layer                         │
│         (Tokio Runtime - Multiple Concurrent Connections)       │
├─────────────────────────────────────────────────────────────────┤
│  TCP Server → Length-Delimited Codec → Client Handler Tasks     │
│                         ↓                                        │
│                 Unbounded MPSC Channels                         │
│                         ↓                                        │
├─────────────────────────────────────────────────────────────────┤
│          Single-Threaded Matching Engine (Actor Model)          │
│              (Synchronous Core - Blocking Recv)                 │
│                                                                 │
│  • OrderBook (BTreeMap + Vec-based Index Pool)                │
│  • Order Matching & Execution Logic                           │
│  • Trade Notification Generation                              │
├─────────────────────────────────────────────────────────────────┤
│                   Broadcast Output Channel                      │
│         (Distributes Trade Notifications to All Clients)        │
└─────────────────────────────────────────────────────────────────┘
```

### 1.2 Key Architectural Decisions

1. **Decoupled Processing**: Network I/O runs on Tokio async runtime, core matching engine runs on dedicated system thread
2. **Single-threaded Matching Engine**: Eliminates contention, simplifies logic, maximizes throughput
3. **Broadcast Pattern**: All clients receive all market updates (trades) in real-time
4. **Memory Efficiency**: Object pool pattern with free list for order reuse

## 2. Directory Structure & Component Purposes

```
/Users/liushuo/code/tradeing/matching-engine/
├── src/                          # Main Rust source code
│   ├── lib.rs                   # Library module declarations (exported to tests/benches)
│   ├── main.rs                  # Application entry point & orchestration
│   ├── protocol.rs              # Data types for client-server communication
│   ├── orderbook.rs             # Core order book implementation (282 lines)
│   ├── engine.rs                # Matching engine main loop (75 lines)
│   ├── network.rs               # TCP server & client connection handling (93 lines)
│   └── bin/
│       └── load_generator.rs    # Performance testing tool with concurrent clients
├── tests/
│   └── basic_trade.rs           # Integration test: buy/sell order matching
├── benches/
│   └── orderbook_benchmark.rs   # Criterion benchmarks for order matching performance
├── Cargo.toml                   # Project manifest & dependencies
├── Cargo.lock                   # Locked dependency versions
├── PROGRESS.md                  # Implementation progress tracking
├── BENCHMARK_REPORT.md          # Performance analysis & results
└── target/                      # Build artifacts (Rust standard)
```

## 3. Core Modules Description

### 3.1 `protocol.rs` (50 lines)
**Purpose**: Defines all data structures for client-server communication

**Key Types**:
- `OrderType`: Enum for Buy/Sell orders
- `NewOrderRequest`: Client submits new order
- `CancelOrderRequest`: Client cancels existing order
- `OrderConfirmation`: Server confirms pending order
- `TradeNotification`: Server broadcasts executed trade details

**Design Pattern**: All types use `serde` for JSON serialization/deserialization

### 3.2 `orderbook.rs` (282 lines)
**Purpose**: Core matching engine data structure and matching algorithm

**Key Components**:
```rust
OrderBook {
    bids: BTreeMap<u64, PriceLevel>,        // Buy orders, price descending
    asks: BTreeMap<u64, PriceLevel>,        // Sell orders, price ascending
    orders: Vec<OrderNode>,                 // Dense object pool for all orders
    order_id_to_index: BTreeMap<u64, usize>, // Fast O(log n) lookup
    free_list_head: Option<usize>,          // Free list for memory reuse
    next_order_id: u64,                     // Order ID generator
}

PriceLevel {
    head: Option<usize>,                    // Doubly-linked list of orders at price
    tail: Option<usize>,                    // FIFO ordering (price-time priority)
}

OrderNode {
    user_id, order_id, price, quantity,
    order_type, next, prev                  // Doubly-linked list pointers
}
```

**Matching Algorithm**:
1. For **Buy orders**: Iterate asks (price ascending), match against cheapest sellers
2. For **Sell orders**: Iterate bids (price descending), match against highest buyers
3. **Partial Fill**: Reduce quantity, keep remainder in order book
4. **Full Fill**: Recycle node via free list
5. **No Match**: Add order to appropriate price level

**Performance Characteristics**:
- Add order: O(log n) for price level insertion + O(1) node allocation
- Remove order: O(log n) for price lookup
- Match order: O(n) worst case (iterate all price levels)

### 3.3 `engine.rs` (75 lines)
**Purpose**: Matching engine main loop - receives commands, processes orders

**Key Logic**:
```rust
MatchingEngine {
    orderbook: OrderBook,
    command_receiver: UnboundedReceiver<EngineCommand>,  // From network
    output_sender: UnboundedSender<EngineOutput>,        // To broadcast
    next_trade_id: u64,
}
```

**Main Loop**:
1. Block on `command_receiver.blocking_recv()`
2. Process `NewOrderRequest` → call `orderbook.match_order()`
3. For each trade generated:
   - Assign trade ID and timestamp
   - Send via `output_sender`
4. If order partially fills, send `OrderConfirmation`
5. Loop until channel closes

**Trade Flow**:
```
NewOrderRequest → orderbook.match_order() → (Vec<TradeNotification>, Option<OrderConfirmation>)
                                             ↓
                                         Send to output_sender
```

### 3.4 `network.rs` (93 lines)
**Purpose**: Async TCP server handling multiple concurrent client connections

**Architecture**:
```
TCP Server (127.0.0.1:8080)
    ↓
For each client connection:
    ├─ LengthDelimitedCodec (message framing)
    ├─ Tokio select! loop:
    │  ├─ Receive client commands (NewOrderRequest/CancelOrderRequest)
    │  │  └─ Forward to command_sender (unbounded channel)
    │  └─ Receive broadcast messages (trade results)
    │     └─ Send to client via TCP
    └─ Broadcast channel subscription (receive all market updates)
```

**Key Features**:
- `tokio::select!` for concurrent read/write handling
- `LengthDelimitedCodec` from `tokio-util` for framing (prevents message fragmentation)
- Broadcast channel for efficient multi-client distribution
- Connection auto-closes on client disconnect

### 3.5 `main.rs` (51 lines)
**Purpose**: Application orchestration and bootstrapping

**Startup Sequence**:
1. Initialize `tracing` logging
2. Create MPSC channels for command/output
3. Spawn system thread for matching engine
4. Parse server address (127.0.0.1:8080)
5. Spawn Tokio task for network server
6. Wait for Ctrl+C or server shutdown

**Threading Model**:
- **System Thread**: Blocking matching engine loop
- **Tokio Runtime**: Async network I/O in main thread

## 4. Technology Stack & Key Dependencies

### 4.1 Core Dependencies

| Dependency | Version | Purpose | Notes |
|-----------|---------|---------|-------|
| `tokio` | 1.x | Async runtime | All features enabled for flexibility |
| `bytes` | 1.x | Efficient byte handling | Required by tokio-util |
| `tokio-util` | 0.7 | Codec utilities | LengthDelimitedCodec for framing |
| `parking_lot` | 0.12 | Mutex replacement | Faster, no poisoning |
| `bumpalo` | 3.16 | Arena allocator | Fast allocation, potential future use |
| `serde` + `serde_json` | 1.0 | Serialization | All JSON protocol communication |
| `tracing` | 0.1 | Structured logging | With env-filter subscriber |
| `futures` | 0.3 | Async utilities | StreamExt, SinkExt traits |
| `rand` | 0.8 | Random number generation | Used in load generator |

### 4.2 Dev & Optional Dependencies

| Dependency | Purpose |
|-----------|---------|
| `criterion` | Benchmarking with statistical rigor |
| `tikv-jemallocator` (optional) | High-performance memory allocator for release builds |

### 4.3 Language & Toolchain

- **Language**: Rust 2021 Edition
- **Minimum Rust**: 1.56+ (due to dependency requirements)
- **Safety**: 100% Safe Rust (no `unsafe` blocks)
- **Threading**: Native Tokio + standard threads

## 5. Build & Development Setup Requirements

### 5.1 Prerequisites

```bash
# Install Rust (if not present)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Verify installation
rustc --version
cargo --version
```

### 5.2 Project Structure Setup

```bash
cd /Users/liushuo/code/tradeing/matching-engine

# Install dependencies (automatic via Cargo)
cargo build

# Build release (with optimizations)
cargo build --release

# Run tests
cargo test

# Run benchmarks
cargo bench

# Run load generator binary
cargo run --release --bin load_generator
```

### 5.3 Build Output Locations

- **Debug binary**: `target/debug/matching-engine`
- **Release binary**: `target/release/matching-engine`
- **Load generator**: `target/release/load_generator`
- **Test results**: Console output + `.criterion/` directory
- **Artifacts**: `Cargo.lock` for dependency pinning

### 5.4 Development Workflow

**Running the server**:
```bash
cargo run --release
# Server listens on 127.0.0.1:8080
```

**Running integration tests** (requires server running):
```bash
# In another terminal
cargo test --test basic_trade -- --nocapture
```

**Running benchmarks**:
```bash
cargo bench
```

**Running load generator** (requires server running):
```bash
cargo run --release --bin load_generator
# Spawns 8 concurrent clients, 10-second test
```

## 6. Key Design Patterns & Architectural Decisions

### 6.1 Patterns Used

| Pattern | Location | Benefit |
|---------|----------|---------|
| **Actor Model** | `engine.rs` | Single-threaded core, message passing |
| **Object Pool** | `orderbook.rs` | Memory efficiency, cache locality |
| **Command Pattern** | `engine.rs` | Decouples client commands from execution |
| **Observer/Broadcast** | `network.rs` | All clients receive market updates |
| **FIFO Queue** | `orderbook.rs` | Price-time priority in doubly-linked lists |

### 6.2 Design Rationale

1. **Why single-threaded engine?**
   - Eliminates lock contention
   - Predictable performance
   - Easier reasoning about order of execution
   - Perfect for order matching (inherently sequential)

2. **Why async network layer?**
   - Thousands of concurrent connections with minimal threads
   - Efficient CPU utilization
   - Non-blocking I/O

3. **Why BTreeMap + Vec pool?**
   - O(log n) price level lookup
   - O(1) order insertion at price
   - Cache-friendly dense allocation
   - Fast reuse via free list

4. **Why broadcast channel?**
   - Each client sees consistent market view
   - Scales to many connections
   - No per-client filtering overhead

## 7. Current Status & Known Issues

### 7.1 Implementation Status

✅ **Completed**:
- Full order matching algorithm (Buy/Sell)
- Network protocol (TCP + JSON)
- FIFO order queue at each price level
- Trade generation and distribution
- Integration tests
- Benchmark framework

❌ **Not Yet Implemented**:
- Order cancellation logic
- Multiple trading symbols
- Margin/leverage features
- PnL calculation
- Persistent order book snapshots

### 7.2 Performance Benchmark Results

| Operation | Median Time | Throughput |
|-----------|------------|-----------|
| Add order (no match) | ~227 ns | ~4,400,000 ops/sec |
| Full match | ~4.287 ms | ~233 ops/sec* |

*Note: Matching performance is unreliable due to benchmark test design issues (includes setup overhead). See `BENCHMARK_REPORT.md` for detailed analysis.

### 7.3 Known Limitations

1. **No cancellation**: `EngineCommand::CancelOrder` not yet implemented
2. **Single symbol**: All orders for "BTC/USD" hardcoded
3. **No persistence**: All data lost on shutdown
4. **No authentication**: Any client can trade
5. **Test data only**: Not production-ready

## 8. Communication Paths

### 8.1 Inter-Process Communication

```
Client (TCP) ↔ Network Handler (Tokio)
                      ↓
                 UnboundedSender<EngineCommand>
                      ↓
              Matching Engine Thread
              (blocking_recv loop)
                      ↓
                 UnboundedSender<EngineOutput>
                      ↓
              Broadcast Channel
                      ↓
         All Connected Clients (TCP)
```

### 8.2 Channel Characteristics

| Channel | Type | Direction | Behavior |
|---------|------|-----------|----------|
| Commands | Unbounded MPSC | Network → Engine | Blocking recv in engine |
| Output | Unbounded MPSC | Engine → Broadcast | Non-blocking send |
| Broadcast | Tokio Broadcast | Broadcast → Clients | ~1024 capacity |

## 9. Testing Strategy

### 9.1 Test Coverage

**Integration Test** (`tests/basic_trade.rs`):
- Creates TCP connection to server
- Sends buy order (pending)
- Sends sell order (matches)
- Verifies trade notification received
- Verifies correctness of trade details

**Benchmark** (`benches/orderbook_benchmark.rs`):
- Pre-populates 1000 price levels
- Measures single matching operation
- Uses Criterion for statistical analysis

**Load Generator** (`src/bin/load_generator.rs`):
- 8 concurrent TCP clients
- Randomized buy/sell orders
- 10-second duration test
- Measures throughput (TPS) and latency percentiles

### 9.2 Running Tests

```bash
# Unit & integration tests
cargo test

# Benchmarks (with statistical analysis)
cargo bench

# Load testing
cargo run --release --bin load_generator
```

## 10. Performance Characteristics

### 10.1 Expected Behavior

- **Order arrival to fill**: <1ms typical (network + matching)
- **Orders per second**: Millions (orders)/second theoretical with optimized matching
- **Concurrent clients**: Thousands (limited by TCP stack)
- **Memory per order**: ~100-150 bytes (OrderNode + overhead)
- **Max capacity**: ~1M orders in memory (with 1-1.5 GB RAM)

### 10.2 Optimization Opportunities

1. **Hardware affinity**: Pin engine thread to specific CPU core
2. **Allocator**: Enable jemalloc in release mode
3. **Message batching**: Process multiple orders per iteration
4. **Better benchmarks**: Fix matching performance measurement
5. **Lock-free structures**: Replace BTreeMap with skip list (advanced)

---

**Project Status**: Functional prototype with solid architecture foundation. Ready for further optimization and feature development.
