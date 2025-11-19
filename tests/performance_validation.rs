/// 性能验证测试 - 验证百万级订单处理能力
///
/// 这个测试直接测量单核吞吐量，验证是否达到百万订单/秒的处理速度
use matching_engine::domain::orderbook::traits::OrderBook;
use matching_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use matching_engine::shared::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;
use std::time::Instant;

#[test]
#[ignore] // 使用 cargo test --ignored 运行
fn test_million_orders_per_second_throughput() {
    println!("\n=====================================");
    println!("  百万订单每秒吞吐量验证测试");
    println!("=====================================\n");

    let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
    let mut orderbook = TickBasedOrderBook::new(spec);

    // 测试不同规模的订单量
    for &order_count in &[10_000, 100_000, 500_000, 1_000_000] {
        println!("📊 测试 {} 订单处理...", order_count);

        let start = Instant::now();

        for i in 0..order_count {
            let order_type = if i % 2 == 0 {
                OrderType::Buy
            } else {
                OrderType::Sell
            };

            let order = NewOrderRequest {
                user_id: i as u64,
                symbol: Arc::from("BTC2501"),
                order_type,
                price: 50000 + (i % 100) as u64,
                quantity: 10,
            };

            orderbook.match_order(order);
        }

        let elapsed = start.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();
        let ops_per_sec = order_count as f64 / elapsed_secs;

        println!("  ⏱️  总用时: {:.2}秒", elapsed_secs);
        println!("  📈 吞吐量: {:.2}M ops/s", ops_per_sec / 1_000_000.0);
        println!("  ⚡ 平均延迟: {:.2}μs\n", elapsed.as_micros() as f64 / order_count as f64);

        // 清空订单簿准备下一轮测试
        let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
        orderbook = TickBasedOrderBook::new(spec);
    }

    println!("=====================================");
    println!("  性能验证完成 ✅");
    println!("=====================================\n");
}

#[test]
fn test_single_core_peak_performance() {
    println!("\n=====================================");
    println!("  单核峰值性能测试");
    println!("=====================================\n");

    let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
    let mut orderbook = TickBasedOrderBook::new(spec);

    // 预热
    for i in 0..1000 {
        let order = NewOrderRequest {
            user_id: i,
            symbol: Arc::from("BTC2501"),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            price: 50000 + (i % 100),
            quantity: 10,
        };
        orderbook.match_order(order);
    }

    // 重置订单簿
    let spec = ContractSpec::new("BTC2501", 1, 40000, 70000);
    let mut orderbook = TickBasedOrderBook::new(spec);

    // 测量峰值性能 - 100万订单
    const TOTAL_ORDERS: usize = 1_000_000;
    println!("🚀 处理 {} 订单...", TOTAL_ORDERS);

    let start = Instant::now();

    for i in 0..TOTAL_ORDERS {
        let order_type = if i % 2 == 0 {
            OrderType::Buy
        } else {
            OrderType::Sell
        };

        let order = NewOrderRequest {
            user_id: i as u64,
            symbol: Arc::from("BTC2501"),
            order_type,
            price: 50000 + (i % 100) as u64,
            quantity: 10,
        };

        orderbook.match_order(order);
    }

    let elapsed = start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let ops_per_sec = TOTAL_ORDERS as f64 / elapsed_secs;

    println!("\n📊 性能结果:");
    println!("  总订单数: {}", TOTAL_ORDERS);
    println!("  总用时: {:.3}秒", elapsed_secs);
    println!("  吞吐量: {:.2}M ops/s", ops_per_sec / 1_000_000.0);
    println!("  平均延迟: {:.2}μs", elapsed.as_micros() as f64 / TOTAL_ORDERS as f64);

    println!("\n✅ 性能验证:");
    if ops_per_sec >= 1_000_000.0 {
        println!("  ✅ 达到百万级吞吐量目标！({:.2}M ops/s)", ops_per_sec / 1_000_000.0);
    } else {
        println!("  ⚠️  未达到百万级目标 (当前: {:.2}M ops/s, 目标: ≥1.0M ops/s)",
                 ops_per_sec / 1_000_000.0);
    }

    println!("\n=====================================\n");

    // 断言：必须达到百万级吞吐量
    assert!(
        ops_per_sec >= 1_000_000.0,
        "吞吐量 {:.2}M ops/s 未达到 1.0M ops/s 目标",
        ops_per_sec / 1_000_000.0
    );
}
