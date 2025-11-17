/// 崩溃恢复场景测试
///
/// 这些测试演示了服务器突然崩溃时的各种场景

use matching_engine::application::services::PersistentMatchingService;
use matching_engine::domain::orderbook::ContractSpec;
use matching_engine::infrastructure::persistence::PersistenceConfig;
use matching_engine::protocol::{NewOrderRequest, OrderType};
use std::sync::Arc;
use tempfile::TempDir;

/// 测试场景：未刷盘数据的丢失
///
/// 这个测试演示了当批量刷盘大小为1000时，
/// 如果在提交了100个订单后服务器崩溃（未刷盘），
/// 重启后这些订单会丢失
#[test]
fn test_unflushed_data_loss() {
    let temp_dir = TempDir::new().unwrap();

    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let config = PersistenceConfig {
        data_dir: temp_dir.path().to_path_buf(),
        wal_sync: true,
        wal_batch_size: 1000, // 每1000条才刷盘
        ..Default::default()
    };

    // 第一阶段：提交100个订单，但不显式刷盘（模拟崩溃）
    {
        let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();

        for i in 0..100 {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                price: 50000 + (i % 10),
                quantity: 10,
            };

            service.match_order(order).unwrap();
        }

        // 💥 模拟崩溃：Drop service但不调用flush
        // 由于batch_size=1000，这100条订单可能还在内存缓冲区中
        std::mem::drop(service);
    }

    // 第二阶段：从持久化状态恢复
    {
        let recovered = PersistentMatchingService::recover(spec.clone(), config.clone()).unwrap();

        // 检查订单簿状态
        let orderbook = recovered.orderbook();

        // ⚠️ 关键点：如果数据未刷盘，订单簿应该是空的
        // 这就是批量刷盘的风险！

        println!("📊 恢复后的订单簿统计:");
        println!("  - 最优买价: {:?}", orderbook.best_bid());
        println!("  - 最优卖价: {:?}", orderbook.best_ask());

        // 注意：这个测试可能会通过（如果Drop时成功flush了）
        // 或者失败（如果数据丢失了）
        // 在真实的SIGKILL场景下，数据一定会丢失

        // ⚠️ 关键观察点：
        // 如果best_bid和best_ask都是None，说明数据丢失了
        // 这就是批量刷盘的风险！
    }
}

/// 测试场景：显式刷盘后的崩溃恢复
///
/// 这个测试演示了如果在提交订单后显式调用flush，
/// 即使服务器崩溃，数据也能完整恢复
#[test]
fn test_flush_before_crash() {
    let temp_dir = TempDir::new().unwrap();

    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let config = PersistenceConfig {
        data_dir: temp_dir.path().to_path_buf(),
        wal_sync: true,
        wal_batch_size: 1000,
        ..Default::default()
    };

    // 第一阶段：提交订单并显式刷盘
    {
        let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();

        for i in 0..100 {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                price: 50000 + (i % 10),
                quantity: 10,
            };

            service.match_order(order).unwrap();
        }

        // ✅ 显式刷盘，确保数据持久化
        service.flush().unwrap();

        // 💥 模拟崩溃
        std::mem::drop(service);
    }

    // 第二阶段：恢复并验证
    {
        let recovered = PersistentMatchingService::recover(spec.clone(), config.clone()).unwrap();
        let orderbook = recovered.orderbook();

        println!("📊 恢复后的订单簿统计（已刷盘）:");
        println!("  - 最优买价: {:?}", orderbook.best_bid());
        println!("  - 最优卖价: {:?}", orderbook.best_ask());

        // ✅ 数据应该完整恢复
        // 由于订单可能撮合，这里只检查有数据
        let has_orders = orderbook.best_bid().is_some() || orderbook.best_ask().is_some();

        assert!(
            has_orders,
            "显式刷盘后崩溃，数据应该能完整恢复"
        );
    }
}

/// 测试场景：小批量刷盘（batch_size=10）
///
/// 演示使用小批量刷盘可以减少数据丢失窗口
#[test]
fn test_small_batch_size() {
    let temp_dir = TempDir::new().unwrap();

    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let config = PersistenceConfig {
        data_dir: temp_dir.path().to_path_buf(),
        wal_sync: true,
        wal_batch_size: 10, // 每10条刷盘
        ..Default::default()
    };

    // 提交15个订单
    {
        let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();

        for i in 0..15 {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: OrderType::Buy,
                price: 50000,
                quantity: 10,
            };

            service.match_order(order).unwrap();
        }

        // 不显式刷盘，模拟崩溃
        // 前10条应该已经自动刷盘，后5条可能丢失
        std::mem::drop(service);
    }

    // 恢复并验证
    {
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        let orderbook = recovered.orderbook();

        println!("📊 小批量刷盘恢复结果:");
        println!("  - 最优买价: {:?}", orderbook.best_bid());

        // 应该至少恢复了10条订单
        // 注意：由于所有订单价格相同，在同一个价格层上
        assert!(orderbook.best_bid().is_some(), "至少前10条订单应该恢复");
    }
}

/// 测试场景：每条订单都刷盘（batch_size=1）
///
/// 演示最安全的配置（但性能最差）
#[test]
fn test_immediate_flush() {
    let temp_dir = TempDir::new().unwrap();

    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let config = PersistenceConfig {
        data_dir: temp_dir.path().to_path_buf(),
        wal_sync: true,
        wal_batch_size: 1, // 每条都刷盘
        ..Default::default()
    };

    // 提交订单
    {
        let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();

        for i in 0..10 {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: OrderType::Buy,
                price: 50000 + i,
                quantity: 10,
            };

            service.match_order(order).unwrap();
            // 每条订单都会立即刷盘
        }

        // 不显式刷盘，模拟崩溃
        std::mem::drop(service);
    }

    // 恢复并验证
    {
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        let orderbook = recovered.orderbook();

        println!("📊 立即刷盘恢复结果:");
        println!("  - 最优买价: {:?}", orderbook.best_bid());
        println!("  - 价差: {:?}", orderbook.spread_ticks());

        // ✅ 所有订单都应该恢复
        assert!(
            orderbook.best_bid().is_some(),
            "立即刷盘模式下，所有订单都应该恢复"
        );
    }
}

/// 测试场景：快照恢复
///
/// 演示从快照恢复的场景
#[test]
fn test_snapshot_recovery() {
    let temp_dir = TempDir::new().unwrap();

    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
    let config = PersistenceConfig {
        data_dir: temp_dir.path().to_path_buf(),
        wal_sync: true,
        wal_batch_size: 1,
        snapshot_interval_secs: 1, // 1秒创建快照
        ..Default::default()
    };

    // 第一阶段：提交订单，触发快照
    {
        let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();

        // 提交足够多的订单以触发快照
        for i in 0..100 {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: OrderType::Buy,
                price: 50000 + (i % 10),
                quantity: 10,
            };

            service.match_order(order).unwrap();
        }

        service.flush().unwrap();

        // 等待快照创建
        std::thread::sleep(std::time::Duration::from_secs(2));

        // 继续提交订单
        for i in 100..120 {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: OrderType::Buy,
                price: 50000 + (i % 10),
                quantity: 10,
            };

            service.match_order(order).unwrap();
        }

        service.flush().unwrap();
        std::mem::drop(service);
    }

    // 第二阶段：从快照恢复
    {
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        let orderbook = recovered.orderbook();

        println!("📊 从快照恢复:");
        println!("  - 最优买价: {:?}", orderbook.best_bid());
        println!("  - 订单簿已恢复");

        // 快照+WAL重放应该恢复所有数据
        assert!(orderbook.best_bid().is_some(), "从快照恢复后应有数据");
    }
}

/// 基准测试：不同刷盘策略的性能对比
#[test]
#[ignore] // 性能测试，默认不运行
fn bench_flush_strategies() {
    use std::time::Instant;

    let temp_dir = TempDir::new().unwrap();
    let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);

    let strategies = vec![
        ("batch=1000", 1000),
        ("batch=100", 100),
        ("batch=10", 10),
        ("batch=1", 1),
    ];

    for (name, batch_size) in strategies {
        let config = PersistenceConfig {
            data_dir: temp_dir.path().join(name).to_path_buf(),
            wal_sync: true,
            wal_batch_size: batch_size,
            ..Default::default()
        };

        std::fs::create_dir_all(&config.data_dir).unwrap();

        let mut service = PersistentMatchingService::new(spec.clone(), config).unwrap();

        let start = Instant::now();
        for i in 0..1000 {
            let order = NewOrderRequest {
                user_id: i,
                symbol: Arc::from("BTCUSDT"),
                order_type: OrderType::Buy,
                price: 50000,
                quantity: 10,
            };

            service.match_order(order).unwrap();
        }
        let elapsed = start.elapsed();

        println!("{:12} | {:8.2}ms | {:.0} ops/s",
            name,
            elapsed.as_secs_f64() * 1000.0,
            1000.0 / elapsed.as_secs_f64()
        );

        service.flush().unwrap();
    }
}
