# Trading Matching Engine - System Architecture Diagrams

## 1. High-Level System Architecture

```
┌───────────────────────────────────────────────────────────────────────────┐
│                           TRADING SYSTEM                                   │
├───────────────────────────────────────────────────────────────────────────┤
│                                                                            │
│  ┌─────────────────────────────────────────────────────────────────────┐  │
│  │              TOKIO ASYNC RUNTIME (Main Thread)                      │  │
│  │                                                                     │  │
│  │  ┌──────────────────────────────────────────────────────────────┐  │  │
│  │  │ TCP Server (127.0.0.1:8080)                                │  │  │
│  │  │ - Listens for client connections                          │  │  │
│  │  │ - Spawns handler task per client                          │  │  │
│  │  └──────────────────────────────────────────────────────────────┘  │  │
│  │                         ↓                                           │  │
│  │  ┌──────────────────────────────────────────────────────────────┐  │  │
│  │  │ Handler Task (per connection)                              │  │  │
│  │  │ - LengthDelimitedCodec for message framing                │  │  │
│  │  │ - Recv: NewOrderRequest, CancelOrderRequest               │  │  │
│  │  │ - Send: TradeNotification, OrderConfirmation              │  │  │
│  │  │ - Broadcast channel subscription                          │  │  │
│  │  └──────────────────────────────────────────────────────────────┘  │  │
│  │                         ↓                                           │  │
│  │  ┌──────────────────────────────────────────────────────────────┐  │  │
│  │  │ Broadcast Channel                                          │  │  │
│  │  │ - All market updates (trades)                             │  │  │
│  │  │ - Capacity: 1024                                          │  │  │
│  │  │ - Distributes to all clients                              │  │  │
│  │  └──────────────────────────────────────────────────────────────┘  │  │
│  │                                                                     │  │
│  └─────────────────────────────────────────────────────────────────────┘  │
│                                                                            │
│                              IPC Channels                                  │
│                                                                            │
│                 UnboundedSender<EngineCommand>                           │
│                 UnboundedSender<EngineOutput>                            │
│                                                                            │
│  ┌─────────────────────────────────────────────────────────────────────┐  │
│  │         MATCHING ENGINE THREAD (System Thread)                      │  │
│  │                                                                     │  │
│  │  ┌──────────────────────────────────────────────────────────────┐  │  │
│  │  │ MatchingEngine::run() - Blocking Main Loop                 │  │  │
│  │  │                                                             │  │  │
│  │  │  while let Some(command) = recv() {                       │  │  │
│  │  │      match command {                                       │  │  │
│  │  │          NewOrder(req) => {                                │  │  │
│  │  │              let (trades, conf) = orderbook.match(req);    │  │  │
│  │  │              send(trades);                                 │  │  │
│  │  │              send(confirmation);                           │  │  │
│  │  │          }                                                 │  │  │
│  │  │          CancelOrder(req) => { /* TODO */ }               │  │  │
│  │  │      }                                                      │  │  │
│  │  │  }                                                          │  │  │
│  │  └──────────────────────────────────────────────────────────────┘  │  │
│  │                         ↓                                           │  │
│  │  ┌──────────────────────────────────────────────────────────────┐  │  │
│  │  │ OrderBook Data Structure                                   │  │  │
│  │  │                                                             │  │  │
│  │  │  bids: BTreeMap<Price, PriceLevel>                        │  │  │
│  │  │  asks: BTreeMap<Price, PriceLevel>                        │  │  │
│  │  │  orders: Vec<OrderNode> [Object Pool]                     │  │  │
│  │  │  free_list_head: [Memory Reuse]                           │  │  │
│  │  └──────────────────────────────────────────────────────────────┘  │  │
│  │                                                                     │  │
│  └─────────────────────────────────────────────────────────────────────┘  │
│                                                                            │
└───────────────────────────────────────────────────────────────────────────┘
```

## 2. Client Request Flow

```
┌────────────────┐
│  TCP Client    │
│   User A       │
└────────┬────────┘
         │
         │ JSON: NewOrderRequest
         │ Buy 10 BTC @ 50000
         │
         ↓
┌────────────────────────────────────────┐
│  Network Handler (Tokio Task)          │
│  - Decode JSON                         │
│  - Create EngineCommand::NewOrder      │
└────────┬───────────────────────────────┘
         │
         │ UnboundedSender<EngineCommand>
         │
         ↓
┌────────────────────────────────────────┐
│  Matching Engine Thread                │
│  - Pop command from receiver           │
│  - Call orderbook.match_order()        │
└────────┬───────────────────────────────┘
         │
         ├─→ Iterate asks (lowest price first)
         ├─→ Find matching sellers
         ├─→ Generate TradeNotification
         └─→ Return Vec<Trade>, Option<Confirmation>
         │
         ↓
┌────────────────────────────────────────┐
│  Broadcast Channel                     │
│  - TradeNotification (JSON)            │
│  - OrderConfirmation (JSON)            │
└────────┬───────────────────────────────┘
         │
         ├─→ User A (sender)
         ├─→ User B (counterparty)
         ├─→ User C (other clients)
         └─→ User N (all subscribed)
         │
         ↓
┌────────────────────────────────────────┐
│  Multiple TCP Clients                  │
│  All receive market updates            │
└────────────────────────────────────────┘
```

## 3. Order Matching Logic

```
NEW ORDER ARRIVES
     │
     ↓
┌──────────────────────────────────────────────┐
│  Is it a BUY order?                          │
└──────────────────────────────────────────────┘
     │
     ├─ YES: Iterate asks from lowest price ─┐
     │                                        │
     │  ┌─────────────────────────────────────┤
     │  │ For each price level in asks:       │
     │  │                                     │
     │  │  Is seller price <= buyer price?   │
     │  │  AND remaining qty > 0?             │
     │  │                                     │
     │  │  YES: Match this order              │
     │  │       qty_to_match = min(buyer_qty,│
     │  │                          seller_qty)│
     │  │       Create TradeNotification      │
     │  │       Update quantities             │
     │  │       Remove if qty = 0             │
     │  │       Continue to next order        │
     │  │                                     │
     │  │  NO: Stop matching (no more cross) │
     │  │                                     │
     │  └─────────────────────────────────────┘
     │                                        │
     └────────────────────────────────────────┘
     │
     ├─ NO: Iterate bids from highest price ┐
     │                                       │
     │  ┌────────────────────────────────────┤
     │  │ For each price level in bids:      │
     │  │                                    │
     │  │  Is buyer price >= seller price?  │
     │  │  AND remaining qty > 0?            │
     │  │                                    │
     │  │  YES: Match this order             │
     │  │       qty_to_match = min(seller_qty,
     │  │                          buyer_qty)│
     │  │       Create TradeNotification     │
     │  │       Update quantities            │
     │  │       Remove if qty = 0            │
     │  │       Continue to next order       │
     │  │                                    │
     │  │  NO: Stop matching (no more cross)│
     │  │                                    │
     │  └────────────────────────────────────┘
     │                                       │
     └───────────────────────────────────────┘
     │
     ↓
┌──────────────────────────────────────────────┐
│  Is remaining quantity > 0?                  │
└──────────────────────────────────────────────┘
     │
     ├─ YES: Add order to order book
     │       Send OrderConfirmation
     │
     └─ NO: No confirmation needed
```

## 4. Data Structure Layout

```
┌──────────────────────────────────────────────────────────────┐
│  OrderBook                                                   │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│  bids (BTreeMap<u64, PriceLevel>)                           │
│  │                                                           │
│  ├─ 50500 → PriceLevel {                                    │
│  │           head: Some(5) ─┬─ OrderNode[5] ─┐            │
│  │           tail: Some(8)  │  user_id: 102   │            │
│  │         }                │  qty: 5          │            │
│  │                          │  next: Some(8) ──┬─ Linked List
│  │                          └─────────────────┘│            │
│  │                                             │            │
│  ├─ 50000 → PriceLevel {                       │            │
│  │           head: Some(2)                     │            │
│  │           tail: Some(2)                     │            │
│  │         }                                   │            │
│  │                                             │            │
│  └─ 49800 → ...                                │            │
│                                                 │            │
│  asks (BTreeMap<u64, PriceLevel>)              │            │
│  │                                              │            │
│  ├─ 50500 → PriceLevel { ... }                 │            │
│  ├─ 50800 → PriceLevel { ... }                 │            │
│  └─ 51000 → PriceLevel { ... }                 │            │
│                                                 │            │
│  orders (Vec<OrderNode>) - Object Pool         │            │
│  │                                              │            │
│  [0] OrderNode (FREE)                          │            │
│  [1] OrderNode (FREE)                          │            │
│  [2] OrderNode { id: 1001, qty: 10, ... }      │            │
│  [3] OrderNode (FREE)                          │            │
│  [4] OrderNode (FREE)                          │            │
│  [5] OrderNode { id: 1002, qty: 5, ... } ─────┘            │
│  ...                                                        │
│  [N] OrderNode { id: 1003, qty: 7, ... }                   │
│                                                              │
│  order_id_to_index (BTreeMap<u64, usize>)                  │
│  │                                                           │
│  ├─ 1001 → 2                                                │
│  ├─ 1002 → 5                                                │
│  ├─ 1003 → N                                                │
│  └─ ...                                                     │
│                                                              │
│  free_list_head: Some(0)                                    │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

## 5. Network Protocol

```
╔════════════════════════════════════════════════════╗
║         TCP Stream (Length-Delimited)              ║
╚════════════════════════════════════════════════════╝

Client → Server:

┌─ Frame 1 ────────────────────────────────┐
│ Length: N bytes                          │
│ Payload: {                               │
│   "user_id": 101,                        │
│   "symbol": "BTC/USD",                   │
│   "order_type": "Buy",                   │
│   "price": 50000,                        │
│   "quantity": 10                         │
│ }                                        │
│ Type: NewOrderRequest                    │
└─────────────────────────────────────────┘

Server → Client:

┌─ Frame 1 ────────────────────────────────┐
│ Length: M bytes                          │
│ Payload: {                               │
│   "order_id": 1001,                      │
│   "user_id": 101                         │
│ }                                        │
│ Type: OrderConfirmation                  │
└─────────────────────────────────────────┘

┌─ Frame 2 ────────────────────────────────┐
│ Length: K bytes                          │
│ Payload: {                               │
│   "trade_id": 1,                         │
│   "symbol": "BTC/USD",                   │
│   "matched_price": 50000,                │
│   "matched_quantity": 10,                │
│   "buyer_user_id": 101,                  │
│   "buyer_order_id": 1001,                │
│   "seller_user_id": 102,                 │
│   "seller_order_id": 1002,               │
│   "timestamp": 1730688000000000000       │
│ }                                        │
│ Type: TradeNotification                  │
└─────────────────────────────────────────┘
```

## 6. Performance Hierarchy

```
                    Execution Speed (ns per operation)
                    ↓
        ┌───────────────────────────────────────┐
        │                                       │
   1 ns │ L1 Cache hit                          │
        │                                       │
  10 ns │ L2 Cache hit                          │
        │                                       │
 100 ns │ L3 Cache hit                          │
        │ Main memory access                    │
        │ OrderBook::add_order (~227 ns)  ─────→
        │                                       │
  1 µs  │ System call (syscall)                 │
        │ Disk I/O starts                       │
        │                                       │
 10 µs  │ Disk seek / SSD latency              │
        │                                       │
100 µs  │ Disk transfer                        │
        │ Network latency (LAN)                │
        │                                       │
  1 ms  │ OrderBook::match_order (~4.3ms) ─→  │
        │ Network latency (WAN)                │
        │                                       │
 10 ms  │ Human perception threshold           │
        │ Disk I/O complete                    │
        │                                       │
100 ms  │ Context switch                       │
        │                                       │
  1 s   │ Human interaction time               │
        │                                       │
        └───────────────────────────────────────┘
```

## 7. Deployment Architecture

```
┌─────────────────────────────────────────────┐
│         Production Environment              │
├─────────────────────────────────────────────┤
│                                             │
│  ┌───────────────────────────────────────┐ │
│  │   Load Balancer / Reverse Proxy       │ │
│  │   (Optional: HAProxy, Nginx)          │ │
│  └───────────────┬───────────────────────┘ │
│                  │                         │
│                  ├──────┬────────┬────┐    │
│                  ↓      ↓        ↓    ↓    │
│  ┌──────┐  ┌──────┐  ┌──────┐  ┌──────┐  │
│  │      │  │      │  │      │  │      │  │
│  │ API  │  │ API  │  │ API  │  │ API  │  │
│  │ Node │  │ Node │  │ Node │  │ Node │  │
│  │      │  │      │  │      │  │      │  │
│  └───┬──┘  └───┬──┘  └───┬──┘  └───┬──┘  │
│      │         │         │         │     │
│      └─────────┼─────────┼─────────┘     │
│                │         │               │
│                ↓         ↓               │
│          ┌──────────────────────┐       │
│          │  Matching Engine     │       │
│          │  (Single instance)   │       │
│          │  Actor-based Core    │       │
│          └──────────┬───────────┘       │
│                     │                   │
│                     ↓                   │
│          ┌──────────────────────┐       │
│          │  Persistent Store    │       │
│          │  (Database)          │       │
│          │  - Trade history     │       │
│          │  - User balances     │       │
│          └──────────────────────┘       │
│                                         │
└─────────────────────────────────────────┘
```

---

This architecture ensures:
- **Horizontal scalability** via API nodes
- **Single point of truth** for matching (no splits)
- **Consistent market view** for all clients
- **High throughput** via single-threaded matching
- **Low latency** via memory-based order book
