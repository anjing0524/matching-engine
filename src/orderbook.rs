use crate::protocol::{NewOrderRequest, OrderConfirmation, OrderType, TradeNotification};
use std::collections::BTreeMap;

// 订单簿中的一个节点，代表一个具体的订单
#[derive(Clone)]
pub struct OrderNode {
    pub user_id: u64,
    pub order_id: u64,
    pub price: u64,
    pub quantity: u64,
    pub order_type: OrderType,
    // 指向同一个价格队列中的下一个订单
    pub next: Option<usize>,
    // 指向同一个价格队列中的上一个订单
    pub prev: Option<usize>,
}

// 代表一个价格层级的所有订单，以双向链表形式存在
#[derive(Clone)]
struct PriceLevel {
    // 链表头
    head: Option<usize>,
    // 链表尾
    tail: Option<usize>,
}

// 订单簿核心结构
#[derive(Clone)]
pub struct OrderBook {
    // 买单侧，按价格从高到低排序
    bids: BTreeMap<u64, PriceLevel>,
    // 卖单侧，按价格从低到高排序
    asks: BTreeMap<u64, PriceLevel>,
    // 订单节点池，所有订单实体都存放在这里
    orders: Vec<OrderNode>,
    // 从 order_id 到 Vec 索引的映射，用于快速查找
    order_id_to_index: BTreeMap<u64, usize>,
    // 空闲节点链表的头指针，用于复用已删除的订单节点空间
    free_list_head: Option<usize>,
    // 用于生成唯一订单 ID
    next_order_id: u64,
}

impl OrderBook {
    pub fn new() -> Self {
        OrderBook {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            orders: Vec::with_capacity(1_000_000), // 预分配一百万个订单的空间
            order_id_to_index: BTreeMap::new(),
            free_list_head: None,
            next_order_id: 1,
        }
    }

    // 撮合一个新订单
    // 返回值是一个元组，包含 (成交列表, 新挂单的确认信息)
    pub fn match_order(&mut self, mut request: NewOrderRequest) -> (Vec<TradeNotification>, Option<OrderConfirmation>) {
        let mut trades = Vec::new();
        let mut remaining_quantity = request.quantity;

        // 移除已完全成交的对手订单ID列表
        let mut orders_to_remove = Vec::new();
        // 需要从价格map中移除的key列表
        let mut prices_to_remove = Vec::new();

        match request.order_type {
            OrderType::Buy => {
                // 对手盘是卖单(asks)，从价格最低的开始匹配
                for (&price, level) in self.asks.iter() {
                    if remaining_quantity == 0 || request.price < price {
                        break; // 新订单已完全成交，或对手价格已高于买价
                    }

                    let mut current_node_idx = level.head;
                    while let Some(node_idx) = current_node_idx {
                        let counter_order = &mut self.orders[node_idx];
                        let trade_quantity = std::cmp::min(remaining_quantity, counter_order.quantity);

                        trades.push(TradeNotification {
                            trade_id: 0, 
                            symbol: request.symbol.clone(),
                            matched_price: counter_order.price,
                            matched_quantity: trade_quantity,
                            buyer_user_id: request.user_id,
                            buyer_order_id: self.next_order_id, // 假设新订单ID
                            seller_user_id: counter_order.user_id,
                            seller_order_id: counter_order.order_id,
                            timestamp: 0,
                        });

                        remaining_quantity -= trade_quantity;
                        counter_order.quantity -= trade_quantity;

                        if counter_order.quantity == 0 {
                            orders_to_remove.push(counter_order.order_id);
                        }
                        current_node_idx = counter_order.next;

                        if remaining_quantity == 0 {
                            break;
                        }
                    }
                    if level.head.is_some() && self.orders[level.head.unwrap()].quantity == 0 {
                        prices_to_remove.push(price);
                    }
                }
            }
            OrderType::Sell => {
                // 对手盘是买单(bids)，从价格最高的开始匹配
                for (&price, level) in self.bids.iter().rev() { // 使用 .rev() 来反向遍历
                    if remaining_quantity == 0 || request.price > price {
                        break; // 新订单已完全成交，或对手价格已低于卖价
                    }

                    let mut current_node_idx = level.head;
                    while let Some(node_idx) = current_node_idx {
                        let counter_order = &mut self.orders[node_idx];
                        let trade_quantity = std::cmp::min(remaining_quantity, counter_order.quantity);

                        trades.push(TradeNotification {
                            trade_id: 0,
                            symbol: request.symbol.clone(),
                            matched_price: counter_order.price,
                            matched_quantity: trade_quantity,
                            buyer_user_id: counter_order.user_id,
                            buyer_order_id: counter_order.order_id,
                            seller_user_id: request.user_id,
                            seller_order_id: self.next_order_id, // 假设新订单ID
                            timestamp: 0,
                        });

                        remaining_quantity -= trade_quantity;
                        counter_order.quantity -= trade_quantity;

                        if counter_order.quantity == 0 {
                            orders_to_remove.push(counter_order.order_id);
                        }
                        current_node_idx = counter_order.next;

                        if remaining_quantity == 0 {
                            break;
                        }
                    }
                    if level.head.is_some() && self.orders[level.head.unwrap()].quantity == 0 {
                        prices_to_remove.push(price);
                    }
                }
            }
        }

        // 移除已成交的订单和价格层级
        for order_id in orders_to_remove {
            self.remove_order(order_id);
        }
        for price in prices_to_remove {
            match request.order_type {
                OrderType::Buy => self.asks.remove(&price),
                OrderType::Sell => self.bids.remove(&price),
            };
        }

        // 如果新订单还有剩余数量，则将其添加到订单簿中
        if remaining_quantity > 0 {
            request.quantity = remaining_quantity;
            let (new_order_id, user_id) = self.add_order(request);
            let confirmation = OrderConfirmation { order_id: new_order_id, user_id };
            (trades, Some(confirmation))
        } else {
            (trades, None) // 完全成交，没有新挂单
        }
    }

    // 添加一个新订单到订单簿，返回 (order_id, user_id)
    fn add_order(&mut self, request: NewOrderRequest) -> (u64, u64) {
        let order_id = self.next_order_id;
        self.next_order_id += 1;

        let user_id = request.user_id;

        let node = OrderNode {
            user_id,
            order_id,
            price: request.price,
            quantity: request.quantity,
            order_type: request.order_type,
            next: None,
            prev: None,
        };

        // 分配节点索引，优先从 free list 中获取
        let node_index = if let Some(free_index) = self.free_list_head {
            // 更新 free list 头指针
            self.free_list_head = self.orders[free_index].next;
            self.orders[free_index] = node;
            free_index
        } else {
            self.orders.push(node);
            self.orders.len() - 1
        };

        // 存储 order_id 到索引的映射
        self.order_id_to_index.insert(order_id, node_index);

        let price_map = match request.order_type {
            OrderType::Buy => &mut self.bids,
            OrderType::Sell => &mut self.asks,
        };

        let level = price_map.entry(request.price).or_insert(PriceLevel { head: None, tail: None });

        // 将新节点添加到价格队列的尾部
        if let Some(tail_index) = level.tail {
            self.orders[tail_index].next = Some(node_index);
            self.orders[node_index].prev = Some(tail_index);
            level.tail = Some(node_index);
        } else {
            // 队列为空
            level.head = Some(node_index);
            level.tail = Some(node_index);
        }

        (order_id, user_id)
    }

    // 从订单簿中移除一个订单
    fn remove_order(&mut self, order_id: u64) {
        // 1. 通过 order_id 找到节点索引
        let node_index = if let Some(index) = self.order_id_to_index.remove(&order_id) {
            index
        } else {
            // 订单不存在，直接返回
            return;
        };

        let (prev, next, price, order_type) = {
            let node = &self.orders[node_index];
            (node.prev, node.next, node.price, node.order_type)
        };

        // 2. 从价格队列的双向链表中移除节点
        if let Some(prev_index) = prev {
            self.orders[prev_index].next = next;
        } else {
            // 节点是头节点
            let price_map = match order_type {
                OrderType::Buy => &mut self.bids,
                OrderType::Sell => &mut self.asks,
            };
            if let Some(level) = price_map.get_mut(&price) {
                level.head = next;
            }
        }

        if let Some(next_index) = next {
            self.orders[next_index].prev = prev;
        } else {
            // 节点是尾节点
            let price_map = match order_type {
                OrderType::Buy => &mut self.bids,
                OrderType::Sell => &mut self.asks,
            };
            if let Some(level) = price_map.get_mut(&price) {
                level.tail = prev;
            }
        }

        // 3. 如果价格队列为空，则从 BTreeMap 中移除该价格层级
        let price_map = match order_type {
            OrderType::Buy => &mut self.bids,
            OrderType::Sell => &mut self.asks,
        };
        if let Some(level) = price_map.get(&price) {
            if level.head.is_none() {
                price_map.remove(&price);
            }
        }

        // 4. 将移除的节点索引添加到 free list 头部
        self.orders[node_index].next = self.free_list_head;
        self.free_list_head = Some(node_index);
    }
}