/// 基于Tick的Array索引订单簿 - 期货交易专用优化
///
/// 核心设计理念（基于期货行业特性）：
/// 1. **价格离散化** - 期货价格按固定tick变化
/// 2. **Array索引** - O(1)查找替代BTreeMap的O(log n)
/// 3. **预分配数组** - 整个价格范围预分配
/// 4. **RingBuffer** - 每个价格层使用零分配队列
///
/// 性能优势：
/// - BTreeMap查找: O(log n) → Array索引: O(1)
/// - 连续内存遍历，极佳的缓存局部性
/// - 预分配消除运行时分配
/// - 预期比BTreeMap快50-80%

use crate::shared::protocol::{NewOrderRequest, OrderConfirmation, OrderType, TradeNotification};
use crate::shared::collections::ringbuffer::RingBuffer;
use crate::shared::symbol_pool::SymbolPool;
use crate::shared::collections::fast_bitmap::FastBitmap;
use smallvec::SmallVec;
use std::sync::Arc;
use std::collections::HashMap;

/// 订单节点
#[derive(Clone, Debug)]
pub struct OrderNode {
    pub user_id: u64,
    pub order_id: u64,
    pub price: u64,
    pub quantity: u64,
    /// 是否已取消（标记删除法）
    pub cancelled: bool,
}

/// 合约配置
#[derive(Clone, Debug)]
pub struct ContractSpec {
    /// 合约代码
    pub symbol: String,
    /// 最小变动价位（tick size）
    pub tick_size: u64,
    /// 价格下限
    pub min_price: u64,
    /// 价格上限
    pub max_price: u64,
    /// 每个价格层RingBuffer容量
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
    pub fn new(symbol: &str, tick_size: u64, min_price: u64, max_price: u64) -> Self {
        assert!(tick_size > 0, "Tick size must be positive");
        assert!(max_price > min_price, "Max price must be greater than min");
        assert!(
            (max_price - min_price) % tick_size == 0,
            "Price range must be divisible by tick size"
        );

        Self {
            symbol: symbol.to_string(),
            tick_size,
            min_price,
            max_price,
            queue_capacity: 2048,  // 增大容量以减少队列满的情况
        }
    }
}

/// 订单位置信息（用于快速取消）
#[derive(Clone, Debug)]
struct OrderLocation {
    /// 价格索引
    price_idx: usize,
    /// 订单类型（买/卖）
    is_bid: bool,
}

/// 基于Tick的订单簿
pub struct TickBasedOrderBook {
    /// 合约规格
    spec: ContractSpec,

    /// 买单价格层数组（索引0 = min_price）
    /// Option<RingBuffer>: Some表示有订单，None表示该价位无订单
    bid_levels: Vec<Option<RingBuffer<OrderNode>>>,

    /// 卖单价格层数组（索引0 = min_price）
    ask_levels: Vec<Option<RingBuffer<OrderNode>>>,

    /// 买单价格层位图索引 - O(1)查找最优价
    /// bit=1表示该价格有订单，bit=0表示无订单
    /// 使用硬件指令 leading_zeros 实现O(n/64)查找
    bid_bitmap: FastBitmap,

    /// 卖单价格层位图索引 - O(1)查找最优价
    /// 使用硬件指令 trailing_zeros 实现O(n/64)查找
    ask_bitmap: FastBitmap,

    /// 最优买价索引（缓存）
    best_bid_idx: Option<usize>,

    /// 最优卖价索引（缓存）
    best_ask_idx: Option<usize>,

    /// 下一个订单ID
    next_order_id: u64,

    /// 符号池
    symbol_pool: Arc<SymbolPool>,

    /// 订单ID到位置的映射（用于快速取消）
    pub(crate) order_locations: HashMap<u64, OrderLocation>,
}

impl TickBasedOrderBook {
    /// 创建新的订单簿
    pub fn new(spec: ContractSpec) -> Self {
        Self::with_symbol_pool(spec, Arc::clone(crate::symbol_pool::global_symbol_pool()))
    }

    /// 使用指定符号池创建
    pub fn with_symbol_pool(spec: ContractSpec, symbol_pool: Arc<SymbolPool>) -> Self {
        let num_levels = ((spec.max_price - spec.min_price) / spec.tick_size) as usize + 1;

        Self {
            bid_levels: (0..num_levels).map(|_| None).collect(),
            ask_levels: (0..num_levels).map(|_| None).collect(),
            bid_bitmap: FastBitmap::new(num_levels),
            ask_bitmap: FastBitmap::new(num_levels),
            best_bid_idx: None,
            best_ask_idx: None,
            next_order_id: 1,
            spec,
            symbol_pool,
            order_locations: HashMap::new(),
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
        if (price - self.spec.min_price) % self.spec.tick_size != 0 {
            return None; // 非法价格（不在tick上）
        }
        Some(((price - self.spec.min_price) / self.spec.tick_size) as usize)
    }

    /// 索引转价格
    #[inline]
    fn index_to_price(&self, index: usize) -> u64 {
        self.spec.min_price + (index as u64) * self.spec.tick_size
    }

    /// 撮合订单
    pub fn match_order(
        &mut self,
        request: NewOrderRequest,
    ) -> (SmallVec<[TradeNotification; 8]>, Option<OrderConfirmation>) {
        let symbol = self.symbol_pool.intern(&request.symbol);
        let mut trades: SmallVec<[TradeNotification; 8]> = SmallVec::new();
        let mut remaining_quantity = request.quantity;

        // 检查价格合法性
        let request_idx = match self.price_to_index(request.price) {
            Some(idx) => idx,
            None => {
                eprintln!(
                    "Invalid price {}: outside range or not on tick",
                    request.price
                );
                return (trades, None);
            }
        };

        // 预先为新订单分配order_id（如果订单可能挂单）
        // 这样可以确保TradeNotification和OrderConfirmation使用同一个ID
        let new_order_id = self.next_order_id;
        self.next_order_id += 1;

        match request.order_type {
            OrderType::Buy => {
                // 匹配卖单：从最优卖价开始
                let mut current_idx = self.best_ask_idx;

                while let Some(idx) = current_idx {
                    if remaining_quantity == 0 {
                        break;
                    }

                    let price = self.index_to_price(idx);
                    if price > request.price {
                        break; // 价格太高，停止匹配
                    }

                    // 处理该价格层
                    if let Some(queue) = &mut self.ask_levels[idx] {
                        while let Some(counter_order) = queue.front_mut() {
                            if remaining_quantity == 0 {
                                break;
                            }

                            // 跳过已取消的订单
                            if counter_order.cancelled {
                                let order_id_to_remove = counter_order.order_id;
                                queue.pop();
                                // 清理order_locations防止内存泄漏
                                self.order_locations.remove(&order_id_to_remove);
                                continue;
                            }

                            let trade_qty = std::cmp::min(remaining_quantity, counter_order.quantity);

                            trades.push(TradeNotification {
                                trade_id: 0,
                                symbol: symbol.clone(),
                                matched_price: price,
                                matched_quantity: trade_qty,
                                buyer_user_id: request.user_id,
                                buyer_order_id: new_order_id,  // 使用预先分配的order_id
                                seller_user_id: counter_order.user_id,
                                seller_order_id: counter_order.order_id,
                                timestamp: 0,
                            });

                            remaining_quantity -= trade_qty;
                            counter_order.quantity -= trade_qty;

                            if counter_order.quantity == 0 {
                                // 保存order_id后再pop
                                let order_id_to_remove = counter_order.order_id;
                                queue.pop();
                                // 从位置映射中删除已成交订单
                                self.order_locations.remove(&order_id_to_remove);
                            } else {
                                break;
                            }
                        }

                        // 如果队列空了，清理
                        if queue.is_empty() {
                            self.ask_levels[idx] = None;
                            self.ask_bitmap.set(idx, false); // 清除位图标记
                        }
                    }

                    // 移动到下一个价格（向上）
                    current_idx = self.find_next_ask(idx);
                }

                // 更新最优卖价
                self.best_ask_idx = self.find_best_ask();

                // 添加剩余订单到买单
                if remaining_quantity > 0 {
                    self.add_bid_order(request_idx, request.user_id, remaining_quantity, new_order_id);
                }
            }
            OrderType::Sell => {
                // 匹配买单：从最优买价开始
                let mut current_idx = self.best_bid_idx;

                while let Some(idx) = current_idx {
                    if remaining_quantity == 0 {
                        break;
                    }

                    let price = self.index_to_price(idx);
                    if price < request.price {
                        break; // 价格太低，停止匹配
                    }

                    // 处理该价格层
                    if let Some(queue) = &mut self.bid_levels[idx] {
                        while let Some(counter_order) = queue.front_mut() {
                            if remaining_quantity == 0 {
                                break;
                            }

                            // 跳过已取消的订单
                            if counter_order.cancelled {
                                let order_id_to_remove = counter_order.order_id;
                                queue.pop();
                                // 清理order_locations防止内存泄漏
                                self.order_locations.remove(&order_id_to_remove);
                                continue;
                            }

                            let trade_qty = std::cmp::min(remaining_quantity, counter_order.quantity);

                            trades.push(TradeNotification {
                                trade_id: 0,
                                symbol: symbol.clone(),
                                matched_price: price,
                                matched_quantity: trade_qty,
                                buyer_user_id: counter_order.user_id,
                                buyer_order_id: counter_order.order_id,
                                seller_user_id: request.user_id,
                                seller_order_id: new_order_id,  // 使用预先分配的order_id
                                timestamp: 0,
                            });

                            remaining_quantity -= trade_qty;
                            counter_order.quantity -= trade_qty;

                            if counter_order.quantity == 0 {
                                // 保存order_id后再pop
                                let order_id_to_remove = counter_order.order_id;
                                queue.pop();
                                // 从位置映射中删除已成交订单
                                self.order_locations.remove(&order_id_to_remove);
                            } else {
                                break;
                            }
                        }

                        // 如果队列空了，清理
                        if queue.is_empty() {
                            self.bid_levels[idx] = None;
                            self.bid_bitmap.set(idx, false); // 清除位图标记
                        }
                    }

                    // 移动到下一个价格（向下）
                    current_idx = self.find_next_bid(idx);
                }

                // 更新最优买价
                self.best_bid_idx = self.find_best_bid();

                // 添加剩余订单到卖单
                if remaining_quantity > 0 {
                    self.add_ask_order(request_idx, request.user_id, remaining_quantity, new_order_id);
                }
            }
        }

        // 返回订单确认（如果有剩余数量挂单）
        let confirmation = if remaining_quantity > 0 {
            Some(OrderConfirmation {
                user_id: request.user_id,
                order_id: new_order_id,  // 使用预先分配的order_id
            })
        } else {
            None
        };

        (trades, confirmation)
    }

    /// 添加买单
    ///
    /// # 参数
    /// - `idx`: 价格索引
    /// - `user_id`: 用户ID
    /// - `quantity`: 数量
    /// - `order_id`: 订单ID（由调用方预先生成）
    fn add_bid_order(&mut self, idx: usize, user_id: u64, quantity: u64, order_id: u64) {
        let order = OrderNode {
            user_id,
            order_id,
            price: self.index_to_price(idx),
            quantity,
            cancelled: false,
        };

        let queue = self.bid_levels[idx]
            .get_or_insert_with(|| RingBuffer::with_capacity(self.spec.queue_capacity));

        if queue.push(order).is_err() {
            eprintln!("Warning: Bid queue full at index {}", idx);
        } else {
            // 记录订单位置
            self.order_locations.insert(order_id, OrderLocation {
                price_idx: idx,
                is_bid: true,
            });
        }

        // 设置位图标记
        self.bid_bitmap.set(idx, true);

        // 更新最优买价
        if self.best_bid_idx.is_none() || idx > self.best_bid_idx.unwrap() {
            self.best_bid_idx = Some(idx);
        }
    }

    /// 添加卖单
    ///
    /// # 参数
    /// - `idx`: 价格索引
    /// - `user_id`: 用户ID
    /// - `quantity`: 数量
    /// - `order_id`: 订单ID（由调用方预先生成）
    fn add_ask_order(&mut self, idx: usize, user_id: u64, quantity: u64, order_id: u64) {
        let order = OrderNode {
            user_id,
            order_id,
            price: self.index_to_price(idx),
            quantity,
            cancelled: false,
        };

        let queue = self.ask_levels[idx]
            .get_or_insert_with(|| RingBuffer::with_capacity(self.spec.queue_capacity));

        if queue.push(order).is_err() {
            eprintln!("Warning: Ask queue full at index {}", idx);
        } else {
            // 记录订单位置
            self.order_locations.insert(order_id, OrderLocation {
                price_idx: idx,
                is_bid: false,
            });
        }

        // 设置位图标记
        self.ask_bitmap.set(idx, true);

        // 更新最优卖价
        if self.best_ask_idx.is_none() || idx < self.best_ask_idx.unwrap() {
            self.best_ask_idx = Some(idx);
        }
    }

    /// 查找最优买价 - O(n/64)位图索引 + 硬件指令
    ///
    /// 使用硬件指令快速查找最高有效位
    /// - 买单: 从高到低，需要找到最后一个设置的bit
    /// - 时间复杂度: O(n/64) + 硬件指令 leading_zeros
    /// - 对于6000个价格层: 最多94个u64块比较
    #[inline]
    fn find_best_bid(&self) -> Option<usize> {
        self.bid_bitmap.find_last_one()
    }

    /// 查找最优卖价 - O(n/64)位图索引 + 硬件指令
    ///
    /// 使用硬件指令快速查找最低有效位
    /// - 卖单: 从低到高，需要找到第一个设置的bit
    /// - 时间复杂度: O(n/64) + 硬件指令 trailing_zeros
    /// - 对于6000个价格层: 最多94个u64块比较
    #[inline]
    fn find_best_ask(&self) -> Option<usize> {
        self.ask_bitmap.find_first_one()
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
    pub fn best_bid(&self) -> Option<u64> {
        self.best_bid_idx.map(|idx| self.index_to_price(idx))
    }

    /// 获取最优卖价
    pub fn best_ask(&self) -> Option<u64> {
        self.best_ask_idx.map(|idx| self.index_to_price(idx))
    }
}

// ============================================================================
// OrderBook Trait Implementation
// ============================================================================

impl crate::domain::orderbook::traits::OrderBook for TickBasedOrderBook {
    fn match_order(
        &mut self,
        request: NewOrderRequest,
    ) -> (SmallVec<[TradeNotification; 8]>, Option<OrderConfirmation>) {
        // Delegate to the existing implementation
        self.match_order(request)
    }

    fn cancel_order(&mut self, order_id: u64) -> Result<(), String> {
        // Step 1: 查找订单位置
        let location = self.order_locations.get(&order_id).cloned()
            .ok_or_else(|| format!("Order {} not found", order_id))?;

        // Step 2: 获取对应的价格层队列
        let levels = if location.is_bid {
            &mut self.bid_levels
        } else {
            &mut self.ask_levels
        };

        let queue = levels[location.price_idx]
            .as_mut()
            .ok_or_else(|| format!("Price level not found for order {}", order_id))?;

        // Step 3: 从队列中移除订单（物理删除）
        // 注意: 由于RingBuffer无iter_mut，需要重建队列
        let mut found = false;
        let capacity = queue.capacity();
        let mut temp_orders = Vec::with_capacity(capacity);

        // 将队列中的订单取出，过滤掉目标订单
        while let Some(order) = queue.pop() {
            if order.order_id == order_id {
                // 找到目标订单，标记但不放回（物理删除）
                found = true;
                // 不push到temp_orders，实现删除
            } else {
                // 保留其他订单
                temp_orders.push(order);
            }
        }

        // 将剩余订单放回队列
        for order in temp_orders {
            let _ = queue.push(order);
        }

        if !found {
            return Err(format!("Order {} not found in queue", order_id));
        }

        // Step 4: 如果价格层变空，更新位图
        if queue.is_empty() {
            if location.is_bid {
                self.bid_bitmap.set(location.price_idx, false);
                // 如果这是最优买价，需要重新查找
                if self.best_bid_idx == Some(location.price_idx) {
                    self.best_bid_idx = self.find_best_bid();
                }
            } else {
                self.ask_bitmap.set(location.price_idx, false);
                // 如果这是最优卖价，需要重新查找
                if self.best_ask_idx == Some(location.price_idx) {
                    self.best_ask_idx = self.find_best_ask();
                }
            }
        }

        // Step 5: 从位置映射中删除
        self.order_locations.remove(&order_id);

        Ok(())
    }

    fn get_best_bid(&self) -> Option<u64> {
        self.best_bid()
    }

    fn get_best_ask(&self) -> Option<u64> {
        self.best_ask()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::traits::OrderBook;

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
    fn test_basic_matching() {
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        // 添加卖单
        let sell = NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("TEST"),
            order_type: OrderType::Sell,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(sell);
        assert!(trades.is_empty());
        assert!(conf.is_some());
        assert_eq!(book.best_ask(), Some(1500));

        // 添加买单（完全匹配）
        let buy = NewOrderRequest {
            user_id: 2,
            symbol: Arc::from("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(buy);
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
            symbol: Arc::from("TEST"),
            order_type: OrderType::Buy,
            price: 1490,
            quantity: 100,
        });

        // 添加卖单
        book.match_order(NewOrderRequest {
            user_id: 2,
            symbol: Arc::from("TEST"),
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
            symbol: Arc::from("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,
        };

        let (trades, conf) = book.match_order(buy);
        assert!(trades.is_empty(), "Should have no trades");
        assert!(conf.is_some(), "Should have confirmation for pending order");

        let order_id = conf.unwrap().order_id;

        // 验证order_id在locations中（只有挂单才会被记录）
        assert!(book.order_locations.contains_key(&order_id),
                "Pending order should be in order_locations");

        // 取消订单
        book.cancel_order(order_id).unwrap();

        // 验证order_locations已清理
        assert!(!book.order_locations.contains_key(&order_id),
                "order_locations should be cleaned after cancel");
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
                symbol: Arc::from("TEST"),
                order_type: OrderType::Sell,
                price: 1500,
                quantity: 50,
            };
            let (trades, conf) = book.match_order(sell);
            assert!(trades.is_empty(), "Should have no trades initially");
            assert!(conf.is_some(), "Should have confirmation for pending order");
            order_ids.push(conf.unwrap().order_id);
        }

        // 验证3个订单都在locations中
        assert_eq!(book.order_locations.len(), 3);
        for &order_id in &order_ids {
            assert!(book.order_locations.contains_key(&order_id));
        }

        // 取消中间的订单
        book.cancel_order(order_ids[1]).unwrap();

        // 验证第二个订单已从locations清理
        assert!(!book.order_locations.contains_key(&order_ids[1]),
                "Cancelled order should be removed from locations");
        assert_eq!(book.order_locations.len(), 2, "Should have 2 orders left");

        // 添加买单匹配剩余订单
        let buy = NewOrderRequest {
            user_id: 100,
            symbol: Arc::from("TEST"),
            order_type: OrderType::Buy,
            price: 1500,
            quantity: 100,  // 正好匹配所有剩余订单（2个卖单 x 50 = 100）
        };

        let (trades, conf) = book.match_order(buy);

        // 买单应该完全成交，无剩余挂单
        assert!(conf.is_none(), "Buy order should be fully matched with no remainder");

        // 应该只有2笔成交（第1和第3个订单，第2个已取消）
        assert_eq!(trades.len(), 2, "Should have 2 trades (orders 1 and 3)");
        assert_eq!(trades[0].matched_quantity, 50);
        assert_eq!(trades[1].matched_quantity, 50);

        // 验证所有订单的locations都被清理
        assert!(!book.order_locations.contains_key(&order_ids[0]),
                "Matched order 1 should be removed");
        assert!(!book.order_locations.contains_key(&order_ids[1]),
                "Cancelled order 2 should still be removed");
        assert!(!book.order_locations.contains_key(&order_ids[2]),
                "Matched order 3 should be removed");

        // 验证order_locations为空（无内存泄漏）
        assert!(book.order_locations.is_empty(),
                "order_locations should be empty after all orders matched/cancelled");
    }

    #[test]
    fn test_cancel_nonexistent_order() {
        // 测试取消不存在的订单返回错误
        let spec = ContractSpec::new("TEST", 10, 1000, 2000);
        let mut book = TickBasedOrderBook::new(spec);

        let result = book.cancel_order(999);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }
}
