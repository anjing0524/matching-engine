//! 基于Tick的Array索引订单簿 - 期货交易专用优化
//!
//! 核心设计理念（基于期货行业特性）：
//! 1. **价格离散化** - 期货价格按固定tick变化
//! 2. **Array索引** - O(1)查找替代BTreeMap的O(log n)
//! 3. **预分配数组** - 整个价格范围预分配
//! 4. 侵入式双向链表 - 每个价格层 O(1) 挂单/撤单
//!
//! 性能优势：
//! - `BTreeMap` 查找: O(log n) → Array索引: O(1)
//! - 连续内存遍历，极佳的缓存局部性
//! - 预分配消除运行时分配
//! - 预期比BTreeMap快50-80%

use crate::shared::collections::fast_bitmap::FastBitmap;
use crate::shared::protocol::{
    NewOrderRequest, OrderConfirmation, OrderStatus, OrderStatusType, OrderType, TradeNotification,
};
use crate::shared::symbol::SymbolId;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Identity hasher for dense, monotonically increasing order IDs.
///
/// `order_locations` is only keyed by order IDs assigned by this book, so the
/// identity function distributes them perfectly across hashbrown buckets and
/// avoids SipHash on every fill and cancellation.
#[derive(Default)]
struct OrderIdHasher(u64);

impl Hasher for OrderIdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

type OrderLocationMap = HashMap<u64, OrderLocation, BuildHasherDefault<OrderIdHasher>>;

/// Sentinel for "no node" in the intrusive linked list.
const NONE: u32 = u32::MAX;

/// Head/tail indices and length of one price level's FIFO queue.
///
/// Orders are nodes in a global slab and linked per level, so cancellation is
/// O(1) instead of rebuilding the queue.
#[derive(Clone, Copy, Debug)]
struct LevelQueue {
    head: u32,
    tail: u32,
    len: u32,
}

impl Default for LevelQueue {
    fn default() -> Self {
        Self {
            head: NONE,
            tail: NONE,
            len: 0,
        }
    }
}

/// 订单节点
#[derive(Clone, Debug)]
pub struct OrderNode {
    pub user_id: u64,
    pub order_id: u64,
    pub price: u64,
    pub quantity: u64,
    /// 原始订单数量（用于跟踪部分成交）
    pub original_quantity: u64,
    /// Previous node in this price level (intrusive list).
    prev: u32,
    /// Next node in this price level (intrusive list).
    next: u32,
}

/// One resting order in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestingOrder {
    pub user_id: u64,
    pub order_id: u64,
    pub side: OrderType,
    pub price: u64,
    pub quantity: u64,
    pub original_quantity: u64,
}

/// 合约配置
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractSpec {
    /// 合约代码
    pub symbol: String,
    /// 最小变动价位（tick size）
    pub tick_size: u64,
    /// 价格下限
    pub min_price: u64,
    /// 价格上限
    pub max_price: u64,
    /// 每个价格层的最大挂单数
    pub queue_capacity: usize,
}

impl ContractSpec {
    /// 创建标准合约配置
    ///
    /// # 示例
    /// ```rust,ignore
    /// // 螺纹钢期货: tick=1, 价格范围2000-8000
    /// let rb = ContractSpec::new("rb2501", 1, 2000, 8000);
    ///
    /// // 沪深300指数: tick=0.2, 价格范围2000-6000
    /// let if_spec = ContractSpec::new("IF2501", 1, 20000, 60000);
    /// ```
    #[must_use]
    pub fn new(symbol: &str, tick_size: u64, min_price: u64, max_price: u64) -> Self {
        assert!(tick_size > 0, "Tick size must be positive");
        assert!(max_price > min_price, "Max price must be greater than min");
        assert!(
            (max_price - min_price).is_multiple_of(tick_size),
            "Price range must be divisible by tick size"
        );

        Self {
            symbol: symbol.to_string(),
            tick_size,
            min_price,
            max_price,
            queue_capacity: 2048, // 增大容量以减少队列满的情况
        }
    }
}

/// Which side of the order book an order belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BookSide {
    Bid,
    Ask,
}

/// Location of a resting order, used for fast cancellation and status queries.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OrderLocation {
    /// Index into the price-level array.
    price_idx: usize,
    /// Side of the book the order rests on.
    side: BookSide,
    /// Index into the order slab.
    node_idx: u32,
}

/// Mutable state of an incoming order while it is being matched.
struct MatchOrder {
    /// Price index of the incoming order.
    request_idx: usize,
    /// Limit price of the incoming order.
    request_price: u64,
    /// User that submitted the order.
    user_id: u64,
    /// ID assigned to the incoming order.
    new_order_id: u64,
    /// Quantity originally requested.
    original_quantity: u64,
    /// Quantity still to be matched.
    remaining_quantity: u64,
    /// Interned symbol shared by all generated trades.
    symbol: SymbolId,
    /// Set when the remaining quantity could not rest because the price-level
    /// queue is full.
    rejected: bool,
}

/// Explicit outcome of one order submission.
///
/// The old tuple return could not distinguish a fully filled order from an
/// invalid price or a full price level; callers had to infer from `None`.
#[derive(Clone, Debug)]
pub enum MatchOutcome {
    /// The incoming order was fully filled; no remainder rests.
    Filled {
        trades: SmallVec<[TradeNotification; 8]>,
    },
    /// The incoming order traded and its remainder rested.
    Rested {
        trades: SmallVec<[TradeNotification; 8]>,
        confirmation: OrderConfirmation,
    },
    /// The incoming order was rejected; `trades` may be non-empty when a
    /// partially matched remainder could not rest.
    Rejected {
        reason: String,
        trades: SmallVec<[TradeNotification; 8]>,
    },
}

impl MatchOutcome {
    /// Returns trades and an optional resting confirmation.
    ///
    /// This preserves the convenience of the old tuple API for tests/benches;
    /// callers that care about rejection must match the enum directly.
    #[must_use]
    pub fn into_parts(self) -> (SmallVec<[TradeNotification; 8]>, Option<OrderConfirmation>) {
        match self {
            Self::Filled { trades } => (trades, None),
            Self::Rested {
                trades,
                confirmation,
            } => (trades, Some(confirmation)),
            Self::Rejected { trades, .. } => (trades, None),
        }
    }
}

/// 基于Tick的订单簿
pub struct TickBasedOrderBook {
    /// 合约规格
    spec: ContractSpec,

    /// 买单价格层（索引0 = min_price）；每个价位是一条侵入式 FIFO 链表。
    bid_levels: Vec<LevelQueue>,

    /// 卖单价格层（索引0 = min_price）。
    ask_levels: Vec<LevelQueue>,

    /// 买单价格层位图索引 - O(1)查找最优价
    /// bit=1表示该价格有订单，bit=0表示无订单
    /// 使用硬件指令 leading_zeros 实现O(n/64)查找
    bid_bitmap: FastBitmap,

    /// 卖单价格层位图索引 - O(1)查找最优价
    /// 使用硬件指令 trailing_zeros 实现O(n/64)查找
    ask_bitmap: FastBitmap,

    /// 最优买价索引（缓存）
    best_bid_idx: Option<usize>,
    /// Lowest populated bid index, used to bound downward searches.
    bid_min_idx: Option<usize>,
    /// Total resting bid quantity across all levels.
    bid_quantity: u64,

    /// 最优卖价索引（缓存）
    best_ask_idx: Option<usize>,
    /// Highest populated ask index, used to bound upward searches.
    ask_max_idx: Option<usize>,
    /// Total resting ask quantity across all levels.
    ask_quantity: u64,

    /// 下一个订单ID
    next_order_id: u64,

    /// 订单ID到位置的映射（用于 O(1) 取消/查询）
    order_locations: OrderLocationMap,

    /// Slab of order nodes. Free nodes are reused via `free_head`.
    nodes: Vec<OrderNode>,
    free_head: u32,

    /// Interned contract symbol; every trade in this book shares it.
    symbol: SymbolId,
}

impl TickBasedOrderBook {
    /// 创建新的订单簿
    #[must_use]
    pub fn new(spec: ContractSpec) -> Self {
        let num_levels = ((spec.max_price - spec.min_price) / spec.tick_size) as usize + 1;
        let symbol = SymbolId::intern(spec.symbol.as_str());

        Self {
            bid_levels: vec![LevelQueue::default(); num_levels],
            ask_levels: vec![LevelQueue::default(); num_levels],
            nodes: Vec::new(),
            free_head: NONE,
            bid_bitmap: FastBitmap::new(num_levels),
            ask_bitmap: FastBitmap::new(num_levels),
            best_bid_idx: None,
            bid_min_idx: None,
            bid_quantity: 0,
            best_ask_idx: None,
            ask_max_idx: None,
            ask_quantity: 0,
            next_order_id: 1,
            spec,
            order_locations: OrderLocationMap::default(),
            symbol,
        }
    }

    /// 价格转数组索引（O(1)算术运算）
    ///
    /// # 示例
    /// ```text
    /// base_price = 2000, tick_size = 10
    /// price = 2050 → index = (2050 - 2000) / 10 = 5
    /// ```
    #[inline]
    fn price_to_index(&self, price: u64) -> Option<usize> {
        if price < self.spec.min_price || price > self.spec.max_price {
            return None;
        }
        if !(price - self.spec.min_price).is_multiple_of(self.spec.tick_size) {
            return None; // 非法价格（不在tick上）
        }
        Some(((price - self.spec.min_price) / self.spec.tick_size) as usize)
    }

    /// 索引转价格
    #[inline]
    fn index_to_price(&self, index: usize) -> u64 {
        self.spec.min_price + (index as u64) * self.spec.tick_size
    }

    /// Matches a buy order against resting sell orders (best ask first).
    fn match_buy_order(
        &mut self,
        order: &mut MatchOrder,
        trades: &mut SmallVec<[TradeNotification; 8]>,
    ) {
        let mut current_idx = self.best_ask_idx;

        while let Some(idx) = current_idx {
            if order.remaining_quantity == 0 {
                break;
            }

            let price = self.index_to_price(idx);
            if price > order.request_price {
                break;
            }

            loop {
                let head = self.level(BookSide::Ask, idx).head;
                if head == NONE || order.remaining_quantity == 0 {
                    break;
                }
                let node_idx = head as usize;
                let counter_order = &self.nodes[node_idx];
                let counter_user_id = counter_order.user_id;
                let counter_order_id = counter_order.order_id;
                let counter_quantity = counter_order.quantity;

                let trade_qty = std::cmp::min(order.remaining_quantity, counter_quantity);
                trades.push(TradeNotification {
                    trade_id: 0,
                    symbol: order.symbol,
                    matched_price: price,
                    matched_quantity: trade_qty,
                    buyer_user_id: order.user_id,
                    buyer_order_id: order.new_order_id,
                    seller_user_id: counter_user_id,
                    seller_order_id: counter_order_id,
                    timestamp: 0,
                });

                order.remaining_quantity -= trade_qty;
                self.ask_quantity = self.ask_quantity.saturating_sub(trade_qty);
                self.nodes[node_idx].quantity = counter_quantity - trade_qty;
                if counter_quantity == trade_qty {
                    self.unlink_order(BookSide::Ask, idx, head);
                } else {
                    break;
                }
            }

            current_idx = self.find_next_ask(idx);
        }

        if order.remaining_quantity > 0 {
            let rested = self.add_order(
                BookSide::Bid,
                order.request_idx,
                order.user_id,
                order.remaining_quantity,
                order.original_quantity,
                order.new_order_id,
            );
            order.rejected = !rested;
        }
    }

    /// Matches a sell order against resting buy orders (best bid first).
    fn match_sell_order(
        &mut self,
        order: &mut MatchOrder,
        trades: &mut SmallVec<[TradeNotification; 8]>,
    ) {
        let mut current_idx = self.best_bid_idx;

        while let Some(idx) = current_idx {
            if order.remaining_quantity == 0 {
                break;
            }

            let price = self.index_to_price(idx);
            if price < order.request_price {
                break;
            }

            loop {
                let head = self.level(BookSide::Bid, idx).head;
                if head == NONE || order.remaining_quantity == 0 {
                    break;
                }
                let node_idx = head as usize;
                let counter_order = &self.nodes[node_idx];
                let counter_user_id = counter_order.user_id;
                let counter_order_id = counter_order.order_id;
                let counter_quantity = counter_order.quantity;

                let trade_qty = std::cmp::min(order.remaining_quantity, counter_quantity);
                trades.push(TradeNotification {
                    trade_id: 0,
                    symbol: order.symbol,
                    matched_price: price,
                    matched_quantity: trade_qty,
                    buyer_user_id: counter_user_id,
                    buyer_order_id: counter_order_id,
                    seller_user_id: order.user_id,
                    seller_order_id: order.new_order_id,
                    timestamp: 0,
                });

                order.remaining_quantity -= trade_qty;
                self.bid_quantity = self.bid_quantity.saturating_sub(trade_qty);
                self.nodes[node_idx].quantity = counter_quantity - trade_qty;
                if counter_quantity == trade_qty {
                    self.unlink_order(BookSide::Bid, idx, head);
                } else {
                    break;
                }
            }

            current_idx = self.find_next_bid(idx);
        }

        if order.remaining_quantity > 0 {
            let rested = self.add_order(
                BookSide::Ask,
                order.request_idx,
                order.user_id,
                order.remaining_quantity,
                order.original_quantity,
                order.new_order_id,
            );
            order.rejected = !rested;
        }
    }

    /// Matches `request` against the order book.
    ///
    /// Returns the generated trades and an optional confirmation for any
    /// quantity that rests in the book.
    pub fn match_order(&mut self, request: NewOrderRequest) -> MatchOutcome {
        self.match_order_impl(request)
    }

    /// Shared implementation used by [`TickBasedOrderBook::match_order`].
    fn match_order_impl(&mut self, request: NewOrderRequest) -> MatchOutcome {
        let symbol = self.symbol;
        let mut trades: SmallVec<[TradeNotification; 8]> = SmallVec::new();

        // Reject prices outside the configured tick grid.
        let request_idx = match self.price_to_index(request.price) {
            Some(idx) => idx,
            None => {
                tracing::warn!(
                    price = request.price,
                    "invalid order price: outside range or not on tick"
                );
                return MatchOutcome::Rejected {
                    reason: "price is outside the tick grid".to_string(),
                    trades,
                };
            }
        };

        // Allocate the order ID before matching so trades can reference it.
        let new_order_id = self.next_order_id;
        self.next_order_id += 1;

        let mut order = MatchOrder {
            request_idx,
            request_price: request.price,
            user_id: request.user_id,
            new_order_id,
            original_quantity: request.quantity,
            remaining_quantity: request.quantity,
            symbol,
            rejected: false,
        };

        match request.order_type {
            OrderType::Buy => self.match_buy_order(&mut order, &mut trades),
            OrderType::Sell => self.match_sell_order(&mut order, &mut trades),
        }

        // A full price-level queue means the remainder never became a resting
        // order, so the outcome is an explicit rejection, not a silent `None`.
        if order.rejected {
            return MatchOutcome::Rejected {
                reason: "price-level queue is full".to_string(),
                trades,
            };
        }

        if order.remaining_quantity > 0 {
            MatchOutcome::Rested {
                trades,
                confirmation: OrderConfirmation {
                    user_id: request.user_id,
                    order_id: new_order_id,
                },
            }
        } else {
            MatchOutcome::Filled { trades }
        }
    }

    /// Adds a resting order to `side` at price index `idx`.
    ///
    /// Returns `false` when the price-level queue is full; the order is dropped
    /// and no book state is updated.
    fn add_order(
        &mut self,
        side: BookSide,
        idx: usize,
        user_id: u64,
        quantity: u64,
        original_quantity: u64,
        order_id: u64,
    ) -> bool {
        if self.level(side, idx).len as usize >= self.spec.queue_capacity {
            tracing::warn!(?side, idx, "price-level queue is full; order not added");
            return false;
        }

        let node_idx = self.allocate_node(OrderNode {
            user_id,
            order_id,
            price: self.index_to_price(idx),
            quantity,
            original_quantity,
            prev: NONE,
            next: NONE,
        });

        let tail = self.level(side, idx).tail;
        self.nodes[node_idx as usize].prev = tail;
        if tail != NONE {
            self.nodes[tail as usize].next = node_idx;
        }

        {
            let level = self.level_mut(side, idx);
            if level.tail == NONE {
                level.head = node_idx;
            }
            level.tail = node_idx;
            level.len += 1;
        }

        self.order_locations.insert(
            order_id,
            OrderLocation {
                price_idx: idx,
                side,
                node_idx,
            },
        );
        match side {
            BookSide::Bid => self.bid_quantity = self.bid_quantity.saturating_add(quantity),
            BookSide::Ask => self.ask_quantity = self.ask_quantity.saturating_add(quantity),
        }
        self.mark_level(side, idx);
        true
    }

    /// Allocates a slab slot, reusing a free one when possible.
    fn allocate_node(&mut self, node: OrderNode) -> u32 {
        if self.free_head != NONE {
            let index = self.free_head;
            self.free_head = self.nodes[index as usize].next;
            self.nodes[index as usize] = node;
            index
        } else {
            let index = self.nodes.len() as u32;
            self.nodes.push(node);
            index
        }
    }

    /// Unlinks `node_idx` from `side`/`idx`, updates the level and recycles the
    /// slab slot. O(1).
    fn unlink_order(&mut self, side: BookSide, idx: usize, node_idx: u32) {
        let (prev, next, order_id) = {
            let node = &self.nodes[node_idx as usize];
            (node.prev, node.next, node.order_id)
        };

        if prev != NONE {
            self.nodes[prev as usize].next = next;
        } else {
            self.level_mut(side, idx).head = next;
        }
        if next != NONE {
            self.nodes[next as usize].prev = prev;
        } else {
            self.level_mut(side, idx).tail = prev;
        }
        self.level_mut(side, idx).len -= 1;
        let empty = self.level(side, idx).len == 0;

        self.order_locations.remove(&order_id);
        self.nodes[node_idx as usize].next = self.free_head;
        self.free_head = node_idx;

        if empty {
            self.clear_level(side, idx);
        }
    }

    /// Marks `idx` as non-empty on `side` and updates the cached best price.
    fn mark_level(&mut self, side: BookSide, idx: usize) {
        match side {
            BookSide::Bid => {
                self.bid_bitmap.set(idx, true);
                if self.bid_min_idx.is_none_or(|min| idx < min) {
                    self.bid_min_idx = Some(idx);
                }
                if self.best_bid_idx.is_none_or(|best| idx > best) {
                    self.best_bid_idx = Some(idx);
                }
            }
            BookSide::Ask => {
                self.ask_bitmap.set(idx, true);
                if self.ask_max_idx.is_none_or(|max| idx > max) {
                    self.ask_max_idx = Some(idx);
                }
                if self.best_ask_idx.is_none_or(|best| idx < best) {
                    self.best_ask_idx = Some(idx);
                }
            }
        }
    }

    /// Marks `idx` as empty on `side` and recomputes the cached best price if
    /// the removed level was the best one.
    fn clear_level(&mut self, side: BookSide, idx: usize) {
        match side {
            BookSide::Bid => {
                self.bid_bitmap.set(idx, false);
                if self.bid_min_idx == Some(idx) {
                    self.bid_min_idx = if idx == 0 {
                        None
                    } else {
                        self.bid_bitmap.find_prev_one(idx)
                    };
                }
                if self.best_bid_idx == Some(idx) {
                    self.best_bid_idx = self
                        .bid_min_idx
                        .filter(|min| *min < idx)
                        .map_or_else(|| self.bid_min_idx, |_| self.bid_bitmap.find_prev_one(idx));
                }
            }
            BookSide::Ask => {
                self.ask_bitmap.set(idx, false);
                if self.ask_max_idx == Some(idx) {
                    self.ask_max_idx = self.ask_bitmap.find_next_one(idx);
                }
                if self.best_ask_idx == Some(idx) {
                    self.best_ask_idx = self
                        .ask_max_idx
                        .filter(|max| *max > idx)
                        .map_or_else(|| self.ask_max_idx, |_| self.ask_bitmap.find_next_one(idx));
                }
            }
        }
    }

    /// Returns the price-level queue for `side` at `idx`.
    #[inline]
    fn level(&self, side: BookSide, idx: usize) -> &LevelQueue {
        match side {
            BookSide::Bid => &self.bid_levels[idx],
            BookSide::Ask => &self.ask_levels[idx],
        }
    }

    /// Returns the mutable price-level queue for `side` at `idx`.
    #[inline]
    fn level_mut(&mut self, side: BookSide, idx: usize) -> &mut LevelQueue {
        match side {
            BookSide::Bid => &mut self.bid_levels[idx],
            BookSide::Ask => &mut self.ask_levels[idx],
        }
    }

    /// 查找下一个买价（向下）- 使用位图硬件指令
    #[inline]
    fn find_next_bid(&self, current_idx: usize) -> Option<usize> {
        self.bid_bitmap.find_prev_one(current_idx)
    }

    /// 查找下一个卖价（向上）- 使用位图硬件指令
    #[inline]
    fn find_next_ask(&self, current_idx: usize) -> Option<usize> {
        self.ask_bitmap.find_next_one(current_idx)
    }

    /// 获取买卖价差（最优买卖价之间的tick数）
    #[must_use]
    pub fn spread_ticks(&self) -> Option<usize> {
        match (self.best_bid_idx, self.best_ask_idx) {
            (Some(bid), Some(ask)) => {
                if ask > bid {
                    Some(ask - bid)
                } else {
                    Some(0)
                }
            }
            _ => None,
        }
    }

    /// 获取最优买价
    #[must_use]
    pub fn best_bid(&self) -> Option<u64> {
        self.best_bid_idx.map(|idx| self.index_to_price(idx))
    }

    /// 获取最优卖价
    #[must_use]
    pub fn best_ask(&self) -> Option<u64> {
        self.best_ask_idx.map(|idx| self.index_to_price(idx))
    }

    /// Cancels the resting order `order_id` on behalf of `user_id`.
    ///
    /// Returns an error if the order does not exist or belongs to another user.
    pub fn cancel_order(&mut self, order_id: u64, user_id: u64) -> Result<(), String> {
        let location = *self
            .order_locations
            .get(&order_id)
            .ok_or_else(|| format!("Order {order_id} not found"))?;

        let node = &self.nodes[location.node_idx as usize];
        if node.user_id != user_id {
            return Err(format!(
                "User {user_id} not authorized to cancel order {order_id}"
            ));
        }

        let quantity = self.nodes[location.node_idx as usize].quantity;
        match location.side {
            BookSide::Bid => self.bid_quantity = self.bid_quantity.saturating_sub(quantity),
            BookSide::Ask => self.ask_quantity = self.ask_quantity.saturating_sub(quantity),
        }
        self.unlink_order(location.side, location.price_idx, location.node_idx);
        Ok(())
    }

    /// Returns the next order id that will be assigned.
    #[must_use]
    pub fn next_order_id(&self) -> u64 {
        self.next_order_id
    }

    /// Reserves and returns the next exchange order id.
    pub fn reserve_order_id(&mut self) -> u64 {
        let id = self.next_order_id;
        self.next_order_id += 1;
        id
    }

    /// Inserts a pre-existing resting order without matching it.
    ///
    /// Used to move call-auction remainder into the continuous book while
    /// preserving the auction-assigned order id.
    ///
    /// # Errors
    /// Returns a reason for duplicate ids, off-grid prices, or a full level.
    pub fn insert_resting_order(
        &mut self,
        user_id: u64,
        side: OrderType,
        price: u64,
        quantity: u64,
        original_quantity: u64,
        order_id: u64,
    ) -> Result<(), String> {
        if quantity == 0 || original_quantity == 0 {
            return Err(format!("order {order_id} has zero quantity"));
        }
        if self.order_locations.contains_key(&order_id) {
            return Err(format!("duplicate order id {order_id}"));
        }
        let idx = self
            .price_to_index(price)
            .ok_or_else(|| format!("order {order_id} price is off the tick grid"))?;
        let book_side = match side {
            OrderType::Buy => BookSide::Bid,
            OrderType::Sell => BookSide::Ask,
        };
        if !self.add_order(
            book_side,
            idx,
            user_id,
            quantity,
            original_quantity,
            order_id,
        ) {
            return Err(format!("price level for order {order_id} is full"));
        }
        self.next_order_id = self.next_order_id.max(order_id + 1);
        Ok(())
    }

    /// Returns the contract specification.
    #[must_use]
    pub fn spec(&self) -> &ContractSpec {
        &self.spec
    }

    /// Total resting quantity on one side.
    #[must_use]
    pub fn side_quantity(&self, side: OrderType) -> u64 {
        match side {
            OrderType::Buy => self.bid_quantity,
            OrderType::Sell => self.ask_quantity,
        }
    }

    /// Exports all resting orders in level/FIFO order for a snapshot.
    #[must_use]
    pub fn snapshot_orders(&self) -> Vec<RestingOrder> {
        let mut orders = Vec::with_capacity(self.order_locations.len());

        if let (Some(mut idx), Some(max_idx)) = (self.bid_min_idx, self.best_bid_idx) {
            loop {
                let mut node = self.bid_levels[idx].head;
                while node != NONE {
                    let order = &self.nodes[node as usize];
                    orders.push(RestingOrder {
                        user_id: order.user_id,
                        order_id: order.order_id,
                        side: OrderType::Buy,
                        price: order.price,
                        quantity: order.quantity,
                        original_quantity: order.original_quantity,
                    });
                    node = order.next;
                }
                if idx >= max_idx {
                    break;
                }
                let Some(next) = self.bid_bitmap.find_next_one(idx) else {
                    break;
                };
                idx = next;
            }
        }

        if let (Some(mut idx), Some(max_idx)) = (self.best_ask_idx, self.ask_max_idx) {
            loop {
                let mut node = self.ask_levels[idx].head;
                while node != NONE {
                    let order = &self.nodes[node as usize];
                    orders.push(RestingOrder {
                        user_id: order.user_id,
                        order_id: order.order_id,
                        side: OrderType::Sell,
                        price: order.price,
                        quantity: order.quantity,
                        original_quantity: order.original_quantity,
                    });
                    node = order.next;
                }
                if idx >= max_idx {
                    break;
                }
                let Some(next) = self.ask_bitmap.find_next_one(idx) else {
                    break;
                };
                idx = next;
            }
        }

        orders
    }

    /// Rebuilds a book directly from snapshot orders without matching them.
    ///
    /// # Errors
    /// Returns a reason if an order is invalid or the snapshot exceeds the
    /// configured level capacity.
    pub fn restore(
        spec: ContractSpec,
        orders: &[RestingOrder],
        next_order_id: u64,
    ) -> Result<Self, String> {
        let mut book = Self::new(spec);
        for order in orders {
            if order.quantity == 0 || order.original_quantity == 0 {
                return Err(format!("order {} has zero quantity", order.order_id));
            }
            if book.order_locations.contains_key(&order.order_id) {
                return Err(format!("duplicate order id {}", order.order_id));
            }
            let idx = book
                .price_to_index(order.price)
                .ok_or_else(|| format!("order {} price is off the tick grid", order.order_id))?;
            let side = match order.side {
                OrderType::Buy => BookSide::Bid,
                OrderType::Sell => BookSide::Ask,
            };
            if book.level(side, idx).len as usize >= book.spec.queue_capacity {
                return Err(format!("price level for order {} is full", order.order_id));
            }
            let node_idx = book.allocate_node(OrderNode {
                user_id: order.user_id,
                order_id: order.order_id,
                price: order.price,
                quantity: order.quantity,
                original_quantity: order.original_quantity,
                prev: NONE,
                next: NONE,
            });
            let tail = book.level(side, idx).tail;
            book.nodes[node_idx as usize].prev = tail;
            if tail != NONE {
                book.nodes[tail as usize].next = node_idx;
            }
            {
                let level = book.level_mut(side, idx);
                if level.tail == NONE {
                    level.head = node_idx;
                }
                level.tail = node_idx;
                level.len += 1;
            }
            match side {
                BookSide::Bid => {
                    book.bid_quantity = book.bid_quantity.saturating_add(order.quantity);
                }
                BookSide::Ask => {
                    book.ask_quantity = book.ask_quantity.saturating_add(order.quantity);
                }
            }
            book.order_locations.insert(
                order.order_id,
                OrderLocation {
                    price_idx: idx,
                    side,
                    node_idx,
                },
            );
            book.mark_level(side, idx);
            book.next_order_id = book.next_order_id.max(order.order_id + 1);
        }
        book.next_order_id = book.next_order_id.max(next_order_id);
        Ok(book)
    }

    /// Returns the contract symbol this book matches.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.spec.symbol
    }

    /// Returns the owner of resting order `order_id`, if present.
    #[must_use]
    pub fn order_owner(&self, order_id: u64) -> Option<u64> {
        self.get_order_status(order_id).map(|status| status.user_id)
    }

    /// Validates an incoming order against the contract specification.
    ///
    /// # Errors
    /// Returns a human-readable reason when the order cannot be accepted.
    pub fn validate_order(&self, order: &NewOrderRequest) -> Result<(), String> {
        if order.symbol != self.symbol {
            return Err(format!(
                "unknown symbol {}: this book matches {}",
                order.symbol, self.spec.symbol
            ));
        }
        if order.quantity == 0 {
            return Err("quantity must be greater than zero".to_string());
        }
        if self.price_to_index(order.price).is_none() {
            return Err(format!(
                "price {} is outside [{}, {}] or not on a {} tick",
                order.price, self.spec.min_price, self.spec.max_price, self.spec.tick_size
            ));
        }
        Ok(())
    }

    /// Validates an order and checks that its own price level can accept a
    /// resting order.
    ///
    /// The service calls this before writing to the WAL so a full queue is
    /// rejected up front rather than acknowledged and silently dropped.
    ///
    /// # Errors
    /// Returns a human-readable rejection reason.
    pub fn can_accept_order(&self, order: &NewOrderRequest) -> Result<(), String> {
        self.validate_order(order)?;
        let idx = self
            .price_to_index(order.price)
            .ok_or_else(|| "price is outside the tick grid".to_string())?;
        let side = match order.order_type {
            OrderType::Buy => BookSide::Bid,
            OrderType::Sell => BookSide::Ask,
        };
        if self.level(side, idx).len as usize >= self.spec.queue_capacity {
            return Err("price-level queue is full".to_string());
        }
        Ok(())
    }

    /// Returns the full status of resting order `order_id`, if present.
    #[must_use]
    pub fn get_order_status(&self, order_id: u64) -> Option<OrderStatus> {
        let location = *self.order_locations.get(&order_id)?;
        let order = &self.nodes[location.node_idx as usize];
        let side = match location.side {
            BookSide::Bid => OrderType::Buy,
            BookSide::Ask => OrderType::Sell,
        };

        // A remaining quantity smaller than the original means the order was
        // partially filled.
        let status = if order.quantity < order.original_quantity {
            OrderStatusType::PartiallyFilled
        } else {
            OrderStatusType::Resting
        };

        Some(OrderStatus {
            order_id: order.order_id,
            user_id: order.user_id,
            side,
            price: order.price,
            original_quantity: order.original_quantity,
            remaining_quantity: order.quantity,
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_to_index() {
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let book = TickBasedOrderBook::new(spec);

        assert_eq!(book.price_to_index(1000), Some(0));
        assert_eq!(book.price_to_index(1010), Some(1));
        assert_eq!(book.price_to_index(2000), Some(100));
        assert_eq!(book.price_to_index(999), None); // 超出范围
        assert_eq!(book.price_to_index(1005), None); // 不在tick上
    }

    #[test]
    fn test_match_outcome_is_explicit() {
        let spec = ContractSpec::new("TEST", 1, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        let invalid = book.match_order(NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 999,
            quantity: 1,
        });
        assert!(matches!(invalid, MatchOutcome::Rejected { .. }));

        let resting = book.match_order(NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 5,
        });
        assert!(matches!(resting, MatchOutcome::Rested { .. }));

        let filled = book.match_order(NewOrderRequest {
            user_id: 2,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1500,
            quantity: 5,
        });
        assert!(matches!(filled, MatchOutcome::Filled { .. }));
    }

    #[test]
    fn test_queue_full_is_rejected_not_silent() {
        let mut spec = ContractSpec::new("TEST", 1, 1000, 2000);
        spec.queue_capacity = 1;
        let mut book = TickBasedOrderBook::new(spec);

        let first = book.match_order(NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1500,
            quantity: 1,
        });
        assert!(matches!(first, MatchOutcome::Rested { .. }));

        let second = book.match_order(NewOrderRequest {
            user_id: 2,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1500,
            quantity: 1,
        });
        assert!(matches!(second, MatchOutcome::Rejected { .. }));
    }

    #[test]
    fn test_basic_matching() {
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加卖单
        let sell = NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(sell).into_parts();
        assert!(trades.is_empty());
        assert!(conf.is_some());
        assert_eq!(book.best_ask(), Some(1500));

        // 添加买单（完全匹配）
        let buy = NewOrderRequest {
            user_id: 2,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(buy).into_parts();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].matched_quantity, 100);
        assert_eq!(trades[0].matched_price, 1500);
        assert!(conf.is_none());
        assert_eq!(book.best_ask(), None); // 卖单已清空
    }

    #[test]
    fn test_spread() {
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加买单
        book.match_order(NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1490,
            quantity: 100,
        });

        // 添加卖单
        book.match_order(NewOrderRequest {
            user_id: 2,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1510,
            quantity: 100,
        });

        assert_eq!(book.best_bid(), Some(1490));
        assert_eq!(book.best_ask(), Some(1510));
        assert_eq!(book.spread_ticks(), Some(2)); // (1510 - 1490) / 10 = 2 ticks
    }

    #[test]
    fn test_cancel_order_cleans_locations() {
        // 测试订单取消后order_locations被正确清理（修复内存泄漏）
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加买单（没有对手方，会挂单）
        let buy = NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(buy).into_parts();
        assert!(trades.is_empty(), "Should have no trades");
        assert!(conf.is_some(), "Should have confirmation for pending order");

        let order_id = conf.unwrap().order_id;

        // 验证order_id在locations中（只有挂单才会被记录）
        assert!(
            book.order_locations.contains_key(&order_id),
            "Pending order should be in order_locations"
        );

        // 取消订单（使用正确的 user_id）
        book.cancel_order(order_id, 1).unwrap();

        // 验证order_locations已清理
        assert!(
            !book.order_locations.contains_key(&order_id),
            "order_locations should be cleaned after cancel"
        );
    }

    #[test]
    fn test_cancel_then_match_cleanup() {
        // 测试取消订单后匹配时正确清理order_locations
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加3个卖单在同一价格（没有对手方，都会挂单）
        let mut order_ids = Vec::new();
        for i in 1..=3 {
            let sell = NewOrderRequest {
                user_id: i,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("TEST"),
                order_type: OrderType::Sell,
                price: 1500,
                quantity: 50,
            };
            let (trades, conf) = book.match_order(sell).into_parts();
            assert!(trades.is_empty(), "Should have no trades initially");
            assert!(conf.is_some(), "Should have confirmation for pending order");
            order_ids.push(conf.unwrap().order_id);
        }

        // 验证3个订单都在locations中
        assert_eq!(book.order_locations.len(), 3);
        for &order_id in &order_ids {
            assert!(book.order_locations.contains_key(&order_id));
        }

        // 取消中间的订单（order_ids[1] 对应 user_id = 2）
        book.cancel_order(order_ids[1], 2).unwrap();

        // 验证第二个订单已从locations清理
        assert!(
            !book.order_locations.contains_key(&order_ids[1]),
            "Cancelled order should be removed from locations"
        );
        assert_eq!(book.order_locations.len(), 2, "Should have 2 orders left");

        // 添加买单匹配剩余订单
        let buy = NewOrderRequest {
            user_id: 100,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100, // 正好匹配所有剩余订单（2个卖单 x 50 = 100）
        };

        let (trades, conf) = book.match_order(buy).into_parts();

        // 买单应该完全成交，无剩余挂单
        assert!(
            conf.is_none(),
            "Buy order should be fully matched with no remainder"
        );

        // 应该只有2笔成交（第1和第3个订单，第2个已取消）
        assert_eq!(trades.len(), 2, "Should have 2 trades (orders 1 and 3)");
        assert_eq!(trades[0].matched_quantity, 50);
        assert_eq!(trades[1].matched_quantity, 50);

        // 验证所有订单的locations都被清理
        assert!(
            !book.order_locations.contains_key(&order_ids[0]),
            "Matched order 1 should be removed"
        );
        assert!(
            !book.order_locations.contains_key(&order_ids[1]),
            "Cancelled order 2 should still be removed"
        );
        assert!(
            !book.order_locations.contains_key(&order_ids[2]),
            "Matched order 3 should be removed"
        );

        // 验证order_locations为空（无内存泄漏）
        assert!(
            book.order_locations.is_empty(),
            "order_locations should be empty after all orders matched/cancelled"
        );
    }

    #[test]
    fn test_cancel_nonexistent_order() {
        // 测试取消不存在的订单返回错误
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        let result = book.cancel_order(999, 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_cancel_order_authorization() {
        // 测试订单授权验证 - 用户只能取消自己的订单
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 用户1创建订单
        let order_user1 = NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(order_user1).into_parts();
        assert!(trades.is_empty(), "Should have no trades");
        assert!(conf.is_some(), "Should have confirmation for pending order");

        let order_id = conf.unwrap().order_id;

        // 用户2尝试取消用户1的订单 - 应该失败
        let result = book.cancel_order(order_id, 2);
        assert!(
            result.is_err(),
            "Should fail when unauthorized user tries to cancel"
        );
        assert!(
            result.unwrap_err().contains("not authorized"),
            "Error should indicate authorization failure"
        );

        // 验证订单仍然存在
        assert!(
            book.order_locations.contains_key(&order_id),
            "Order should still exist after failed cancel"
        );

        // 用户1取消自己的订单 - 应该成功
        let result = book.cancel_order(order_id, 1);
        assert!(
            result.is_ok(),
            "Should succeed when authorized user cancels"
        );

        // 验证订单已被删除
        assert!(
            !book.order_locations.contains_key(&order_id),
            "Order should be removed after successful cancel"
        );
    }

    #[test]
    fn test_cancel_order_multiple_users() {
        // 测试多用户场景下的授权验证
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 三个不同用户创建订单
        let mut order_ids = Vec::new();
        for user_id in 1..=3 {
            let order = NewOrderRequest {
                user_id,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("TEST"),
                order_type: OrderType::Sell,
                price: 1500,
                quantity: 50,
            };
            let (_, conf) = book.match_order(order).into_parts();
            if let Some(c) = conf {
                order_ids.push((c.order_id, user_id));
            }
        }

        assert_eq!(order_ids.len(), 3, "Should have 3 pending orders");

        // 每个用户尝试取消其他用户的订单 - 都应该失败
        for &(order_id, owner_id) in &order_ids {
            for attacker_id in 1..=3 {
                if attacker_id != owner_id {
                    let result = book.cancel_order(order_id, attacker_id);
                    assert!(
                        result.is_err(),
                        "User {} should not be able to cancel user {}'s order",
                        attacker_id,
                        owner_id
                    );
                }
            }
        }

        // 验证所有订单仍然存在
        for &(order_id, _) in &order_ids {
            assert!(
                book.order_locations.contains_key(&order_id),
                "All orders should still exist"
            );
        }

        // 每个用户取消自己的订单 - 都应该成功
        for &(order_id, user_id) in &order_ids {
            let result = book.cancel_order(order_id, user_id);
            assert!(
                result.is_ok(),
                "User {} should be able to cancel their own order",
                user_id
            );
        }

        // 验证所有订单已被删除
        assert_eq!(
            book.order_locations.len(),
            0,
            "All orders should be removed"
        );
    }

    #[test]
    fn test_multi_level_matching() {
        // P1: 测试跨多个价格层的订单匹配
        // 验证买单能够匹配多个价格层的卖单
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 在3个不同价格层添加卖单
        // 价格层1: 1500, 数量50
        book.match_order(NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1500,
            quantity: 50,
        });

        // 价格层2: 1510, 数量30
        book.match_order(NewOrderRequest {
            user_id: 2,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1510,
            quantity: 30,
        });

        // 价格层3: 1520, 数量40
        book.match_order(NewOrderRequest {
            user_id: 3,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1520,
            quantity: 40,
        });

        // 验证订单簿状态
        assert_eq!(book.best_ask(), Some(1500), "Best ask should be 1500");

        // 提交大额买单，价格足够高以匹配所有3个价格层
        let buy = NewOrderRequest {
            user_id: 100,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1530,   // 高于所有卖单价格
            quantity: 120, // 正好匹配所有卖单 (50 + 30 + 40 = 120)
        };

        let (trades, conf) = book.match_order(buy).into_parts();

        // 验证成交结果
        assert_eq!(
            trades.len(),
            3,
            "Should have 3 trades across 3 price levels"
        );

        // 验证第一笔成交 - 最优价格 1500
        assert_eq!(
            trades[0].matched_price, 1500,
            "First trade should be at price 1500"
        );
        assert_eq!(
            trades[0].matched_quantity, 50,
            "First trade should match 50 units"
        );
        assert_eq!(
            trades[0].seller_user_id, 1,
            "First trade should match user 1's order"
        );

        // 验证第二笔成交 - 次优价格 1510
        assert_eq!(
            trades[1].matched_price, 1510,
            "Second trade should be at price 1510"
        );
        assert_eq!(
            trades[1].matched_quantity, 30,
            "Second trade should match 30 units"
        );
        assert_eq!(
            trades[1].seller_user_id, 2,
            "Second trade should match user 2's order"
        );

        // 验证第三笔成交 - 第三价格层 1520
        assert_eq!(
            trades[2].matched_price, 1520,
            "Third trade should be at price 1520"
        );
        assert_eq!(
            trades[2].matched_quantity, 40,
            "Third trade should match 40 units"
        );
        assert_eq!(
            trades[2].seller_user_id, 3,
            "Third trade should match user 3's order"
        );

        // 验证买单完全成交，无剩余挂单
        assert!(
            conf.is_none(),
            "Buy order should be fully matched with no remainder"
        );

        // 验证订单簿已清空
        assert_eq!(book.best_ask(), None, "All sell orders should be matched");
        assert_eq!(book.best_bid(), None, "No buy orders should remain");
    }

    #[test]
    fn test_fifo_queue_discipline() {
        // P1: 测试FIFO（先进先出）队列纪律
        // 验证同一价格层的订单严格按提交顺序匹配
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 在相同价格1500添加5个卖单，按顺序从用户1-5
        let price = 1500;
        let quantity_per_order = 20;

        for user_id in 1..=5 {
            let sell = NewOrderRequest {
                user_id,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("TEST"),
                order_type: OrderType::Sell,
                price,
                quantity: quantity_per_order,
            };
            let (trades, conf) = book.match_order(sell).into_parts();
            assert!(
                trades.is_empty(),
                "No trades should occur when adding sell orders"
            );
            assert!(conf.is_some(), "Each sell order should be pending");
        }

        // 验证订单簿状态
        assert_eq!(
            book.best_ask(),
            Some(price),
            "Best ask should be at the specified price"
        );

        // 提交第一个买单，只匹配第一个订单（用户1）
        let buy1 = NewOrderRequest {
            user_id: 100,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price,
            quantity: quantity_per_order,
        };

        let (trades1, _) = book.match_order(buy1).into_parts();
        assert_eq!(trades1.len(), 1, "Should have exactly 1 trade");
        assert_eq!(
            trades1[0].seller_user_id, 1,
            "First trade should match user 1 (FIFO)"
        );
        assert_eq!(trades1[0].matched_quantity, quantity_per_order);

        // 提交第二个买单，应该匹配第二个订单（用户2）
        let buy2 = NewOrderRequest {
            user_id: 101,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price,
            quantity: quantity_per_order,
        };

        let (trades2, _) = book.match_order(buy2).into_parts();
        assert_eq!(trades2.len(), 1, "Should have exactly 1 trade");
        assert_eq!(
            trades2[0].seller_user_id, 2,
            "Second trade should match user 2 (FIFO)"
        );

        // 提交大额买单匹配剩余所有订单（用户3、4、5）
        let buy3 = NewOrderRequest {
            user_id: 102,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price,
            quantity: quantity_per_order * 3, // 匹配剩余3个订单
        };

        let (trades3, _) = book.match_order(buy3).into_parts();
        assert_eq!(
            trades3.len(),
            3,
            "Should have 3 trades for remaining orders"
        );

        // 验证FIFO顺序：用户3、4、5
        assert_eq!(
            trades3[0].seller_user_id, 3,
            "Third trade should match user 3 (FIFO)"
        );
        assert_eq!(
            trades3[1].seller_user_id, 4,
            "Fourth trade should match user 4 (FIFO)"
        );
        assert_eq!(
            trades3[2].seller_user_id, 5,
            "Fifth trade should match user 5 (FIFO)"
        );

        // 验证订单簿已清空
        assert_eq!(
            book.best_ask(),
            None,
            "All sell orders should be matched in FIFO order"
        );
    }

    #[test]
    fn test_queue_capacity_limit() {
        // 测试价位队列容量限制
        // 验证当价格层队列达到容量上限时的行为

        // 创建小容量配置以便测试
        let mut spec = ContractSpec::new("TEST", 10, 1000, 2000);
        spec.queue_capacity = 10; // 设置较小容量便于测试

        let mut book = TickBasedOrderBook::new(spec);

        let price = 1500;
        let capacity = 10;

        // 填满队列 - 添加10个订单
        for user_id in 1..=capacity {
            let sell = NewOrderRequest {
                user_id: user_id as u64,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("TEST"),
                order_type: OrderType::Sell,
                price,
                quantity: 10,
            };
            let (trades, conf) = book.match_order(sell).into_parts();
            assert!(
                trades.is_empty(),
                "No trades should occur when adding sell orders"
            );
            assert!(conf.is_some(), "Order {} should be confirmed", user_id);
        }

        // 验证队列已满 - 尝试添加第11个订单
        // 注意：当价位队列满时，订单不会被添加
        let overflow_order = NewOrderRequest {
            user_id: 999,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price,
            quantity: 10,
        };

        let (trades_overflow, conf_overflow) = book.match_order(overflow_order).into_parts();
        assert!(trades_overflow.is_empty(), "No trades should occur");
        assert!(
            conf_overflow.is_none(),
            "a dropped order must not be confirmed as resting"
        );

        // Verify the first 10 orders are still present and matchable.
        let buy_all = NewOrderRequest {
            user_id: 100,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price,
            quantity: 100, // 足以匹配所有10个订单
        };

        let (trades_match, _) = book.match_order(buy_all).into_parts();

        // 应该只有10笔成交（容量限制）
        assert_eq!(
            trades_match.len(),
            10,
            "Should match exactly 10 orders (capacity limit)"
        );

        // 验证成交的是最初的10个订单（用户1-10）
        for (i, trade) in trades_match.iter().enumerate() {
            assert_eq!(
                trade.seller_user_id,
                (i + 1) as u64,
                "Trade {} should match user {} in FIFO order",
                i,
                i + 1
            );
        }

        // 验证订单簿已清空
        assert_eq!(book.best_ask(), None, "All orders should be matched");
        assert!(
            book.order_locations.is_empty(),
            "No pending orders should remain"
        );
    }

    #[test]
    fn test_cancel_middle_preserves_fifo_and_cleanup() {
        let spec = ContractSpec::new("TEST", 1, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);
        let price = 1500;
        let mut order_ids = Vec::new();

        for user_id in 1..=100u64 {
            let order = NewOrderRequest {
                user_id,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("TEST"),
                order_type: OrderType::Sell,
                price,
                quantity: 1,
            };
            let (_, confirmation) = book.match_order(order).into_parts();
            order_ids.push(confirmation.expect("order must rest").order_id);
        }

        // Cancel every other order from the middle of the queue.
        for (index, order_id) in order_ids.iter().enumerate() {
            if index % 2 == 0 {
                continue;
            }
            book.cancel_order(*order_id, (index + 1) as u64)
                .expect("cancel must succeed");
        }
        assert_eq!(book.order_locations.len(), 50);

        let buy = NewOrderRequest {
            user_id: 999,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price,
            quantity: 50,
        };
        let (trades, _) = book.match_order(buy).into_parts();
        assert_eq!(trades.len(), 50);
        let sellers: Vec<u64> = trades.iter().map(|trade| trade.seller_user_id).collect();
        let expected: Vec<u64> = (1..=100u64).filter(|user_id| user_id % 2 == 1).collect();
        assert_eq!(sellers, expected, "cancellation must preserve FIFO order");
        assert!(book.order_locations.is_empty());
    }

    #[test]
    fn test_get_order_status_resting() {
        use crate::shared::protocol::OrderStatusType;

        // 测试查询静止订单状态
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加一个买单（没有对手方，会挂单）
        let buy = NewOrderRequest {
            user_id: 123,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(buy).into_parts();
        assert!(trades.is_empty(), "Should have no trades");
        assert!(conf.is_some(), "Should have confirmation");

        let order_id = conf.unwrap().order_id;

        // 查询订单状态
        let status = book.get_order_status(order_id);
        assert!(status.is_some(), "Should find order status");

        let status = status.unwrap();
        assert_eq!(status.order_id, order_id);
        assert_eq!(status.user_id, 123);
        assert_eq!(status.side, OrderType::Buy);
        assert_eq!(status.price, 1500);
        assert_eq!(status.original_quantity, 100);
        assert_eq!(status.remaining_quantity, 100);
        assert_eq!(status.status, OrderStatusType::Resting);
    }

    #[test]
    fn test_get_order_status_partially_filled() {
        use crate::shared::protocol::OrderStatusType;

        // 测试查询部分成交订单状态
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 先添加一个小的卖单
        let sell_small = NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Sell,
            price: 1500,
            quantity: 30,
        };
        book.match_order(sell_small);

        // 添加一个大的买单（会部分成交）
        let buy_large = NewOrderRequest {
            user_id: 123,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(buy_large).into_parts();
        assert_eq!(trades.len(), 1, "Should have 1 trade");
        assert_eq!(trades[0].matched_quantity, 30);
        assert!(conf.is_some(), "Should have confirmation for remaining");

        let order_id = conf.unwrap().order_id;

        // 查询订单状态
        let status = book.get_order_status(order_id);
        assert!(status.is_some(), "Should find order status");

        let status = status.unwrap();
        assert_eq!(status.order_id, order_id);
        assert_eq!(status.user_id, 123);
        assert_eq!(status.side, OrderType::Buy);
        assert_eq!(status.price, 1500);
        assert_eq!(status.original_quantity, 100);
        assert_eq!(status.remaining_quantity, 70); // 100 - 30 = 70
        assert_eq!(status.status, OrderStatusType::PartiallyFilled);
    }

    #[test]
    fn test_get_order_status_nonexistent() {
        // 测试查询不存在的订单
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let book = TickBasedOrderBook::new(spec);

        let status = book.get_order_status(999);
        assert!(status.is_none(), "Should not find nonexistent order");
    }

    #[test]
    fn test_get_order_status_after_cancel() {
        // 测试取消后查询订单状态
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加订单
        let buy = NewOrderRequest {
            user_id: 123,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (_, conf) = book.match_order(buy).into_parts();
        let order_id = conf.unwrap().order_id;

        // 验证订单存在
        assert!(book.get_order_status(order_id).is_some());

        // 取消订单
        book.cancel_order(order_id, 123).unwrap();

        // 取消后应该查询不到
        let status = book.get_order_status(order_id);
        assert!(status.is_none(), "Should not find cancelled order");
    }

    #[test]
    fn test_get_order_status_multiple_orders() {
        use crate::shared::protocol::OrderStatusType;

        // 测试多个订单的状态查询
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加多个不同价格的订单
        let orders = vec![
            (1, OrderType::Buy, 1490, 100),
            (2, OrderType::Buy, 1500, 200),
            (3, OrderType::Sell, 1510, 150),
            (4, OrderType::Sell, 1520, 250),
        ];

        let mut order_ids = Vec::new();
        for (user_id, order_type, price, quantity) in orders {
            let request = NewOrderRequest {
                user_id,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("TEST"),
                order_type,
                price,
                quantity,
            };
            let (_, conf) = book.match_order(request).into_parts();
            if let Some(c) = conf {
                order_ids.push((c.order_id, user_id, order_type, price, quantity));
            }
        }

        // 验证所有订单状态
        for (order_id, user_id, side, price, quantity) in order_ids {
            let status = book.get_order_status(order_id);
            assert!(status.is_some(), "Should find order {}", order_id);

            let status = status.unwrap();
            assert_eq!(status.order_id, order_id);
            assert_eq!(status.user_id, user_id);
            assert_eq!(status.side, side);
            assert_eq!(status.price, price);
            assert_eq!(status.original_quantity, quantity);
            assert_eq!(status.remaining_quantity, quantity);
            assert_eq!(status.status, OrderStatusType::Resting);
        }
    }
    #[test]
    fn best_prices_survive_removing_extreme_levels() {
        let spec = ContractSpec::new("BTCUSDT", 1, 1_000, 10_000);
        let mut book = TickBasedOrderBook::new(spec);
        book.insert_resting_order(1, OrderType::Buy, 2_000, 1, 1, 1)
            .unwrap();
        book.insert_resting_order(1, OrderType::Buy, 2_100, 1, 1, 2)
            .unwrap();
        book.insert_resting_order(1, OrderType::Buy, 2_200, 1, 1, 3)
            .unwrap();
        assert_eq!(book.best_bid(), Some(2_200));
        book.cancel_order(3, 1).unwrap();
        assert_eq!(book.best_bid(), Some(2_100));
        book.cancel_order(1, 1).unwrap();
        assert_eq!(book.best_bid(), Some(2_100));
        book.cancel_order(2, 1).unwrap();
        assert_eq!(book.best_bid(), None);

        book.insert_resting_order(1, OrderType::Sell, 2_500, 1, 1, 4)
            .unwrap();
        book.insert_resting_order(1, OrderType::Sell, 2_600, 1, 1, 5)
            .unwrap();
        book.insert_resting_order(1, OrderType::Sell, 2_700, 1, 1, 6)
            .unwrap();
        assert_eq!(book.best_ask(), Some(2_500));
        book.cancel_order(4, 1).unwrap();
        assert_eq!(book.best_ask(), Some(2_600));
        book.cancel_order(6, 1).unwrap();
        assert_eq!(book.best_ask(), Some(2_600));
        book.cancel_order(5, 1).unwrap();
        assert_eq!(book.best_ask(), None);
    }
    #[test]
    fn side_quantity_tracks_resting_and_fills() {
        let spec = ContractSpec::new("BTCUSDT", 1, 1_000, 10_000);
        let mut book = TickBasedOrderBook::new(spec);
        book.insert_resting_order(1, OrderType::Buy, 2_000, 5, 5, 1)
            .unwrap();
        book.insert_resting_order(2, OrderType::Sell, 2_500, 7, 7, 2)
            .unwrap();
        assert_eq!(book.side_quantity(OrderType::Buy), 5);
        assert_eq!(book.side_quantity(OrderType::Sell), 7);

        book.cancel_order(2, 2).unwrap();
        assert_eq!(book.side_quantity(OrderType::Sell), 0);

        let request = NewOrderRequest {
            user_id: 3,
            client_order_id: 9,
            symbol: SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Sell,
            price: 2_000,
            quantity: 3,
        };
        let _ = book.match_order(request);
        assert_eq!(book.side_quantity(OrderType::Buy), 2);
    }
}
