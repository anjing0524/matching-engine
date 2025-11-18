/// 混沌工程测试
///
/// 测试系统在各种异常情况下的弹性和恢复能力
use matching_engine::*;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use rand::Rng;

/// 测试配置
const CHAOS_TEST_DURATION_SECS: u64 = 5;
const CONCURRENT_CLIENTS: usize = 10;
const ORDERS_PER_CLIENT: usize = 1000;

#[test]
#[ignore] // 默认跳过，使用 cargo test -- --ignored 运行
fn chaos_test_concurrent_order_submission() {
    // 创建引擎
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100000);
    let orderbook = Arc::new(parking_lot::Mutex::new(
        domain::orderbook::TickBasedOrderBook::new(spec)
    ));

    let mut handles = vec![];

    // 启动多个并发客户端
    for client_id in 0..CONCURRENT_CLIENTS {
        let book = Arc::clone(&orderbook);
        let handle = thread::spawn(move || {
            let mut rng = rand::thread_rng();
            let mut successful_orders = 0;

            for i in 0..ORDERS_PER_CLIENT {
                // 随机订单参数
                let order_type = if rng.gen_bool(0.5) {
                    shared::protocol::OrderType::Buy
                } else {
                    shared::protocol::OrderType::Sell
                };

                let price = rng.gen_range(10000..50000);
                let quantity = rng.gen_range(1..100);

                // 构造订单
                let order = shared::protocol::NewOrderRequest {
                    user_id: client_id as u64,
                    symbol: Arc::from("CHAOS/TEST"),
                    order_type,
                    price,
                    quantity,
                };

                // 提交订单（可能失败）
                let mut book = book.lock();
                match book.match_order(order) {
                    Ok(_) => successful_orders += 1,
                    Err(e) => {
                        // 记录错误但继续
                        if i % 100 == 0 {
                            eprintln!("Client {}: Order failed: {:?}", client_id, e);
                        }
                    }
                }

                // 偶尔短暂休眠，模拟真实负载
                if i % 100 == 0 {
                    thread::sleep(Duration::from_micros(100));
                }
            }

            successful_orders
        });

        handles.push(handle);
    }

    // 等待所有客户端完成
    let mut total_successful = 0;
    for handle in handles {
        let successful = handle.join().expect("Thread panicked");
        total_successful += successful;
    }

    println!("Chaos test completed:");
    println!("  Total clients: {}", CONCURRENT_CLIENTS);
    println!("  Orders per client: {}", ORDERS_PER_CLIENT);
    println!("  Total successful orders: {}/{}",
             total_successful,
             CONCURRENT_CLIENTS * ORDERS_PER_CLIENT);

    // 验证：至少有一些订单成功
    assert!(total_successful > 0, "No orders succeeded");
}

#[test]
#[ignore]
fn chaos_test_random_cancellations() {
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let mut rng = rand::thread_rng();
    let mut order_ids = Vec::new();

    // 提交一批订单
    for i in 0..100 {
        let order = shared::protocol::NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("CHAOS/TEST"),
            order_type: if i % 2 == 0 {
                shared::protocol::OrderType::Buy
            } else {
                shared::protocol::OrderType::Sell
            },
            price: 10000 + (i * 10),
            quantity: 10,
        };

        match orderbook.match_order(order) {
            Ok(result) => {
                if let Some(conf) = result.confirmation {
                    order_ids.push(conf.order_id);
                }
            }
            Err(_) => {}
        }
    }

    println!("Submitted {} orders, {} partially filled",
             order_ids.len(), order_ids.len());

    // 随机取消订单
    let mut cancelled = 0;
    for _ in 0..50 {
        if !order_ids.is_empty() {
            let idx = rng.gen_range(0..order_ids.len());
            let order_id = order_ids.remove(idx);

            match orderbook.cancel_order(order_id) {
                Ok(_) => cancelled += 1,
                Err(_) => {
                    // 订单可能已经完全成交
                }
            }
        }
    }

    println!("Successfully cancelled {} orders", cancelled);
    assert!(cancelled > 0, "Should cancel at least some orders");
}

#[test]
#[ignore]
fn chaos_test_extreme_price_ranges() {
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let test_cases = vec![
        // (price, should_succeed)
        (1000, true),      // 最低价
        (999, false),      // 低于最低价
        (100000, true),    // 最高价
        (100001, false),   // 高于最高价
        (50000, true),     // 中间价格
        (0, false),        // 零价格
    ];

    for (price, should_succeed) in test_cases {
        let order = shared::protocol::NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("CHAOS/TEST"),
            order_type: shared::protocol::OrderType::Buy,
            price,
            quantity: 10,
        };

        let result = orderbook.match_order(order);

        if should_succeed {
            assert!(result.is_ok() || matches!(result, Ok(_)),
                    "Order at price {} should succeed", price);
        } else {
            assert!(result.is_err(),
                    "Order at price {} should fail", price);
        }
    }
}

#[test]
#[ignore]
fn chaos_test_memory_pressure() {
    // 测试在大量订单下的内存使用
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let large_order_count = 10_000;
    let mut successful = 0;

    for i in 0..large_order_count {
        let order = shared::protocol::NewOrderRequest {
            user_id: (i % 100) as u64,
            symbol: Arc::from("CHAOS/TEST"),
            order_type: if i % 2 == 0 {
                shared::protocol::OrderType::Buy
            } else {
                shared::protocol::OrderType::Sell
            },
            price: 10000 + ((i % 1000) * 10),
            quantity: (i % 100 + 1) as u64,
        };

        if orderbook.match_order(order).is_ok() {
            successful += 1;
        }

        // 每1000个订单打印进度
        if i % 1000 == 0 {
            println!("Processed {} orders, {} successful", i, successful);
        }
    }

    println!("Memory pressure test: {}/{} orders successful",
             successful, large_order_count);

    assert!(successful > large_order_count / 2,
            "Should successfully process at least half the orders");
}

#[test]
#[ignore]
fn chaos_test_rapid_order_churn() {
    // 测试快速提交和取消订单
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let mut order_ids = Vec::new();
    let iterations = 1000;

    for i in 0..iterations {
        // 提交新订单
        let order = shared::protocol::NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("CHAOS/TEST"),
            order_type: shared::protocol::OrderType::Buy,
            price: 10000 + (i % 100) * 10,
            quantity: 10,
        };

        if let Ok(result) = orderbook.match_order(order) {
            if let Some(conf) = result.confirmation {
                order_ids.push(conf.order_id);
            }
        }

        // 每隔几次，取消一个旧订单
        if i % 3 == 0 && !order_ids.is_empty() {
            let order_id = order_ids.remove(0);
            let _ = orderbook.cancel_order(order_id);
        }
    }

    println!("Rapid churn test: {} orders remaining in book", order_ids.len());
}

#[test]
#[ignore]
fn chaos_test_stress_single_price_level() {
    // 测试同一价格档位的大量订单
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let same_price = 50000;
    let order_count = 1000;
    let mut buy_orders = 0;
    let mut sell_orders = 0;

    // 提交大量相同价格的订单
    for i in 0..order_count {
        let order_type = if i < order_count / 2 {
            buy_orders += 1;
            shared::protocol::OrderType::Buy
        } else {
            sell_orders += 1;
            shared::protocol::OrderType::Sell
        };

        let order = shared::protocol::NewOrderRequest {
            user_id: i as u64,
            symbol: Arc::from("CHAOS/TEST"),
            order_type,
            price: same_price,
            quantity: 1,
        };

        let _ = orderbook.match_order(order);
    }

    println!("Single price level stress test:");
    println!("  Buy orders: {}", buy_orders);
    println!("  Sell orders: {}", sell_orders);
    println!("  Price level: {}", same_price);

    // 大部分订单应该成交（买卖对冲）
}
