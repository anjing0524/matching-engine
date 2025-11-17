/// 综合性能基准测试
///
/// 测试各个组件的性能，找出瓶颈

use matching_engine::application::services::{HighPerformanceService, HighPerformanceConfig};
use matching_engine::domain::orderbook::{TickBasedOrderBook, ContractSpec};
use matching_engine::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

/// 测试纯订单簿性能（无持久化）
#[test]
fn bench_pure_orderbook() {
    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let mut orderbook = TickBasedOrderBook::new(spec);

    let count = 1_000_000;
    let start = Instant::now();

    for i in 0..count {
        let order = NewOrderRequest {
            user_id: i,
            symbol: Arc::from("BTCUSDT"),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            price: 50000 + (i % 100),
            quantity: 10,
        };

        orderbook.match_order(order);
    }

    let elapsed = start.elapsed();
    let ops_per_sec = count as f64 / elapsed.as_secs_f64();

    println!("\n====== 纯订单簿性能 ======");
    println!("吞吐量: {:.0} ops/s", ops_per_sec);
    println!("平均延迟: {:.2}μs", elapsed.as_micros() as f64 / count as f64);
    println!("总耗时: {:.2}s", elapsed.as_secs_f64());
}

/// 测试高性能服务（异步WAL + 订单簿）
#[tokio::test]
async fn bench_high_performance_service() {
    let temp_dir = TempDir::new().unwrap();

    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let config = HighPerformanceConfig {
        data_dir: temp_dir.path().to_path_buf(),
        wal_sync: false, // 禁用fsync以测试纯内存性能
        flush_interval_ms: 10,
        buffer_threshold: 100_000, // 提高缓冲区阈值
        ..Default::default()
    };

    let mut service = HighPerformanceService::new(spec, config).unwrap();

    let count = 1_000_000;
    let start = Instant::now();

    for i in 0..count {
        let order = NewOrderRequest {
            user_id: i,
            symbol: Arc::from("BTCUSDT"),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            price: 50000 + (i % 100),
            quantity: 10,
        };

        service.match_order(order).unwrap();
    }

    let elapsed = start.elapsed();
    let ops_per_sec = count as f64 / elapsed.as_secs_f64();

    println!("\n====== 高性能服务 (异步WAL) ======");
    println!("吞吐量: {:.0} ops/s", ops_per_sec);
    println!("平均延迟: {:.2}μs", elapsed.as_micros() as f64 / count as f64);
    println!("总耗时: {:.2}s", elapsed.as_secs_f64());

    // 等待刷盘
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    service.close().await.unwrap();

    // 验证性能目标
    if ops_per_sec >= 1_000_000.0 {
        println!("✅ 达到百万级吞吐量目标！");
    } else {
        println!("⚠️ 未达到百万级吞吐量，当前: {:.0} ops/s (目标: 1,000,000 ops/s)", ops_per_sec);
        println!("   差距: {:.1}%", (1_000_000.0 - ops_per_sec) / 1_000_000.0 * 100.0);
    }
}

/// 测试不同配置的性能对比
#[tokio::test]
#[ignore] // 默认不运行，耗时较长
async fn bench_configuration_comparison() {
    let configs = vec![
        ("10ms刷盘, 1万缓冲区", 10, 10_000),
        ("10ms刷盘, 10万缓冲区", 10, 100_000),
        ("50ms刷盘, 10万缓冲区", 50, 100_000),
        ("100ms刷盘, 10万缓冲区", 100, 100_000),
    ];

    println!("\n====== 配置性能对比 ======");
    println!("{:30} | {:12} | {:10}", "配置", "吞吐量", "延迟");
    println!("{:-<30}-+-{:-<12}-+-{:-<10}", "", "", "");

    for (name, flush_interval_ms, buffer_threshold) in configs {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
        let config = HighPerformanceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            flush_interval_ms,
            buffer_threshold,
            ..Default::default()
        };

        let mut service = HighPerformanceService::new(spec, config).unwrap();

        let count = 100_000;
        let start = Instant::now();

        for i in 0..count {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                price: 50000 + (i % 100),
                quantity: 10,
            };

            service.match_order(order).unwrap();
        }

        let elapsed = start.elapsed();
        let ops_per_sec = count as f64 / elapsed.as_secs_f64();
        let avg_latency_us = elapsed.as_micros() as f64 / count as f64;

        println!("{:30} | {:10.0} /s | {:8.2}μs", name, ops_per_sec, avg_latency_us);

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        service.close().await.unwrap();
    }
}

/// 测试在fsync启用时的性能
#[tokio::test]
#[ignore] // fsync会显著降低性能，默认不运行
async fn bench_with_fsync() {
    let temp_dir = TempDir::new().unwrap();

    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let config = HighPerformanceConfig {
        data_dir: temp_dir.path().to_path_buf(),
        wal_sync: true, // 启用fsync
        flush_interval_ms: 10,
        buffer_threshold: 100_000,
        ..Default::default()
    };

    let mut service = HighPerformanceService::new(spec, config).unwrap();

    let count = 10_000; // 减少测试量，因为fsync很慢
    let start = Instant::now();

    for i in 0..count {
        let order = NewOrderRequest {
            user_id: i,
            symbol: Arc::from("BTCUSDT"),
            order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
            price: 50000 + (i % 100),
            quantity: 10,
        };

        service.match_order(order).unwrap();
    }

    let elapsed = start.elapsed();
    let ops_per_sec = count as f64 / elapsed.as_secs_f64();

    println!("\n====== 启用fsync的性能 ======");
    println!("吞吐量: {:.0} ops/s", ops_per_sec);
    println!("平均延迟: {:.2}μs", elapsed.as_micros() as f64 / count as f64);
    println!("\n注意: fsync会显著降低性能，但提供更强的数据安全性");

    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    service.close().await.unwrap();
}
