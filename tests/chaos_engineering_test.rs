#![allow(deprecated)]
/// 混沌工程测试
///
/// 测试系统在各种异常情况下的弹性和恢复能力
use matching_engine::*;
use rand::Rng;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// 测试配置
const _CHAOS_TEST_DURATION_SECS: u64 = 5;
const CONCURRENT_CLIENTS: usize = 10;
const ORDERS_PER_CLIENT: usize = 1000;

#[test]
#[ignore] // 默认跳过，使用 cargo test -- --ignored 运行
fn chaos_test_concurrent_order_submission() {
    // 创建引擎
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100_000);
    let orderbook = Arc::new(parking_lot::Mutex::new(
        domain::orderbook::TickBasedOrderBook::new(spec),
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

                let price = rng.gen_range(10_000..50000);
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
                let (_trades, confirmation) = book.match_order(order);
                if confirmation.is_some() {
                    successful_orders += 1;
                } else {
                    // 记录订单未被接受的情况
                    if i % 100 == 0 {
                        eprintln!(
                            "Client {}: Order not accepted at iteration {}",
                            client_id, i
                        );
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
    println!(
        "  Total successful orders: {}/{}",
        total_successful,
        CONCURRENT_CLIENTS * ORDERS_PER_CLIENT
    );

    // 验证：至少有一些订单成功
    assert!(total_successful > 0, "No orders succeeded");
}

#[test]
#[ignore]
fn chaos_test_random_cancellations() {
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100_000);
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
            price: 10_000 + (i * 10),
            quantity: 10,
        };

        let (_trades, confirmation) = orderbook.match_order(order);
        if let Some(conf) = confirmation {
            order_ids.push(conf.order_id);
        }
    }

    println!(
        "Submitted {} orders, {} partially filled",
        order_ids.len(),
        order_ids.len()
    );

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
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100_000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let test_cases = vec![
        // (price, quantity, description)
        (1000, 10, "最低价"),
        (100_000, 10, "最高价"),
        (50000, 10, "中间价格"),
        (1, 10, "极低价格"),
        (200000, 10, "极高价格"),
    ];

    for (price, quantity, desc) in test_cases {
        let order = shared::protocol::NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("CHAOS/TEST"),
            order_type: shared::protocol::OrderType::Buy,
            price,
            quantity,
        };

        let (_trades, confirmation) = orderbook.match_order(order);

        // 只记录结果，不做严格断言（混沌测试关注系统不崩溃）
        if confirmation.is_some() {
            println!("{}: Order accepted at price {}", desc, price);
        } else {
            println!("{}: Order not accepted at price {}", desc, price);
        }
    }
}

#[test]
#[ignore]
fn chaos_test_memory_pressure() {
    // 测试在大量订单下的内存使用
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100_000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let large_order_count = 10_000;
    let mut successful = 0;

    for i in 0..large_order_count {
        let order = shared::protocol::NewOrderRequest {
            user_id: (i % 100),
            symbol: Arc::from("CHAOS/TEST"),
            order_type: if i % 2 == 0 {
                shared::protocol::OrderType::Buy
            } else {
                shared::protocol::OrderType::Sell
            },
            price: 10_000 + ((i % 1000) * 10),
            quantity: (i % 100 + 1),
        };

        let (_trades, confirmation) = orderbook.match_order(order);
        if confirmation.is_some() {
            successful += 1;
        }

        // 每1000个订单打印进度
        if i % 1000 == 0 {
            println!("Processed {} orders, {} successful", i, successful);
        }
    }

    println!(
        "Memory pressure test: {}/{} orders successful",
        successful, large_order_count
    );

    assert!(
        successful > large_order_count / 2,
        "Should successfully process at least half the orders"
    );
}

#[test]
#[ignore]
fn chaos_test_rapid_order_churn() {
    // 测试快速提交和取消订单
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100_000);
    let mut orderbook = domain::orderbook::TickBasedOrderBook::new(spec);

    let mut order_ids = Vec::new();
    let iterations = 1000;

    for i in 0..iterations {
        // 提交新订单
        let order = shared::protocol::NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("CHAOS/TEST"),
            order_type: shared::protocol::OrderType::Buy,
            price: 10_000 + (i % 100) * 10,
            quantity: 10,
        };

        let (_trades, confirmation) = orderbook.match_order(order);
        if let Some(conf) = confirmation {
            order_ids.push(conf.order_id);
        }

        // 每隔几次，取消一个旧订单
        if i % 3 == 0 && !order_ids.is_empty() {
            let order_id = order_ids.remove(0);
            let _ = orderbook.cancel_order(order_id);
        }
    }

    println!(
        "Rapid churn test: {} orders remaining in book",
        order_ids.len()
    );
}

#[test]
#[ignore]
fn chaos_test_stress_single_price_level() {
    // 测试同一价格档位的大量订单
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100_000);
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

#[test]
#[ignore]
fn chaos_test_concurrent_lock_contention() {
    // 测试高并发下的锁竞争
    let spec = domain::orderbook::ContractSpec::new("CHAOS/TEST", 1, 1000, 100_000);
    let orderbook = Arc::new(parking_lot::Mutex::new(
        domain::orderbook::TickBasedOrderBook::new(spec),
    ));

    let thread_count = 20;
    let ops_per_thread = 100;
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let book = Arc::clone(&orderbook);
        let handle = thread::spawn(move || {
            let mut local_success = 0;
            for i in 0..ops_per_thread {
                let order = shared::protocol::NewOrderRequest {
                    user_id: thread_id,
                    symbol: Arc::from("CHAOS/TEST"),
                    order_type: if i % 2 == 0 {
                        shared::protocol::OrderType::Buy
                    } else {
                        shared::protocol::OrderType::Sell
                    },
                    price: 10_000 + (i % 50) * 100,
                    quantity: 10,
                };

                let mut book = book.lock();
                let (_trades, confirmation) = book.match_order(order);
                if confirmation.is_some() {
                    local_success += 1;
                }
            }
            local_success
        });
        handles.push(handle);
    }

    let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    println!(
        "Lock contention test: {}/{} orders successful",
        total,
        thread_count * ops_per_thread
    );

    assert!(total > 0, "Should process some orders successfully");
}
