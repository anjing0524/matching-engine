/// 端到端匹配引擎性能基准测试
///
/// 测试完整的订单处理流程：
/// 网络接收 → 解码 → 订单簿撮合 → 编码 → 网络发送
///
/// 测试场景：
/// 1. 单连接订单吞吐量
/// 2. 订单处理延迟分布
/// 3. 不同订单类型性能
/// 4. 并发连接处理能力
/// 5. 内存使用效率

use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
use matching_engine::network_middleware::buffer::SharedBuffer;
use matching_engine::network_middleware::codec::{BincodeCodec, Codec, LengthDelimitedCodec};
use matching_engine::network_middleware::traits::ZeroCopyBuffer;
use matching_engine::orderbook_tick::{ContractSpec, TickBasedOrderBook};
use matching_engine::protocol::{ClientMessage, NewOrderRequest, OrderType};
use std::sync::Arc;
use std::time::Instant;

/// 创建测试订单簿
fn create_test_orderbook() -> TickBasedOrderBook {
    let spec = ContractSpec {
        symbol: "BTCUSDT".to_string(),
        tick_size: 1,
        min_price: 40000,
        max_price: 70000,
        queue_capacity: 10000,
    };
    TickBasedOrderBook::new(spec)
}

/// 创建测试订单
fn create_test_order(user_id: u64, order_type: OrderType, price: u64, quantity: u64) -> NewOrderRequest {
    NewOrderRequest {
        user_id,
        symbol: Arc::from("BTCUSDT"),
        order_type,
        price,
        quantity,
    }
}

/// 编码订单消息
fn encode_order(order: &NewOrderRequest) -> Vec<u8> {
    let msg = ClientMessage::NewOrder(order.clone());
    let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());
    let mut buf = vec![0u8; 4096];
    let size = codec.encode(&msg, &mut buf).unwrap();
    buf.truncate(size);
    buf
}

/// 基准测试：订单编解码性能
fn bench_order_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("order_codec");

    let order = create_test_order(1001, OrderType::Buy, 50000, 100);

    group.bench_function("encode_order", |b| {
        b.iter(|| {
            let encoded = encode_order(&order);
            black_box(encoded);
        });
    });

    group.bench_function("decode_order", |b| {
        let encoded = encode_order(&order);
        let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());

        b.iter(|| {
            let decoded = codec.decode(&encoded).unwrap();
            black_box(decoded);
        });
    });

    group.bench_function("encode_decode_roundtrip", |b| {
        let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());
        let mut buf = vec![0u8; 4096];

        b.iter(|| {
            let msg = ClientMessage::NewOrder(order.clone());
            let size = codec.encode(&msg, &mut buf).unwrap();
            let decoded = codec.decode(&buf[..size]).unwrap();
            black_box(decoded);
        });
    });

    group.finish();
}

/// 基准测试：订单簿撮合性能
fn bench_orderbook_matching(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_matching");

    group.bench_function("single_buy_order", |b| {
        let mut ob = create_test_orderbook();
        let order = create_test_order(1001, OrderType::Buy, 50000, 100);

        b.iter(|| {
            let result = ob.match_order(order.clone());
            black_box(result);
        });
    });

    group.bench_function("single_sell_order", |b| {
        let mut ob = create_test_orderbook();
        let order = create_test_order(2001, OrderType::Sell, 50000, 100);

        b.iter(|| {
            let result = ob.match_order(order.clone());
            black_box(result);
        });
    });

    group.bench_function("matching_trade", |b| {
        let mut ob = create_test_orderbook();

        b.iter(|| {
            // 先挂一个买单
            let buy_order = create_test_order(1001, OrderType::Buy, 50000, 100);
            ob.match_order(buy_order);

            // 再来一个卖单撮合
            let sell_order = create_test_order(2001, OrderType::Sell, 50000, 50);
            let result = ob.match_order(sell_order);
            black_box(result);
        });
    });

    group.finish();
}

/// 基准测试：端到端订单处理
fn bench_e2e_order_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_order_processing");

    group.bench_function("full_pipeline_no_match", |b| {
        let mut ob = create_test_orderbook();
        let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());
        let order = create_test_order(1001, OrderType::Buy, 50000, 100);

        b.iter(|| {
            // 1. 编码
            let encoded = encode_order(&order);

            // 2. 创建零拷贝缓冲区
            let buf = SharedBuffer::from_vec(encoded);

            // 3. 解码
            let decoded = codec.decode(buf.as_slice()).unwrap();

            // 4. 提取订单
            if let Some(ClientMessage::NewOrder(order)) = decoded {
                // 5. 订单簿处理
                let result = ob.match_order(order);
                black_box(result);
            }
        });
    });

    group.bench_function("full_pipeline_with_match", |b| {
        let mut ob = create_test_orderbook();
        let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());

        // 预先放入买单
        let buy_order = create_test_order(1001, OrderType::Buy, 50000, 1000);
        ob.match_order(buy_order);

        let sell_order = create_test_order(2001, OrderType::Sell, 50000, 100);

        b.iter(|| {
            // 完整流程
            let encoded = encode_order(&sell_order);
            let buf = SharedBuffer::from_vec(encoded);
            let decoded = codec.decode(buf.as_slice()).unwrap();

            if let Some(ClientMessage::NewOrder(order)) = decoded {
                let result = ob.match_order(order);
                black_box(result);
            }
        });
    });

    group.finish();
}

/// 基准测试：不同订单量级性能
fn bench_order_volumes(c: &mut Criterion) {
    let mut group = c.benchmark_group("order_volumes");

    for qty in [10, 100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*qty as u64));

        group.bench_with_input(
            BenchmarkId::new("batch_orders", qty),
            qty,
            |b, &qty| {
                b.iter(|| {
                    let mut ob = create_test_orderbook();

                    // 批量处理订单
                    for i in 0..qty {
                        let order = create_test_order(
                            1000 + i as u64,
                            if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                            50000 + (i % 10) as u64,
                            100,
                        );
                        let result = ob.match_order(order);
                        black_box(result);
                    }
                });
            },
        );
    }

    group.finish();
}

/// 基准测试：内存效率
fn bench_memory_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_efficiency");

    group.bench_function("zerocopy_buffer_reuse", |b| {
        let order = create_test_order(1001, OrderType::Buy, 50000, 100);
        let encoded = encode_order(&order);

        b.iter(|| {
            // 测试零拷贝缓冲区复用
            let buf1 = SharedBuffer::from_vec(encoded.clone());
            let buf2 = buf1.clone_ref();
            let buf3 = buf2.clone_ref();

            black_box((buf1, buf2, buf3));
        });
    });

    group.bench_function("orderbook_memory_reuse", |b| {
        b.iter(|| {
            let mut ob = create_test_orderbook();

            // 添加和撮合多个订单
            for i in 0..100 {
                let buy = create_test_order(1000 + i, OrderType::Buy, 50000 - i, 100);
                ob.match_order(buy);

                let sell = create_test_order(2000 + i, OrderType::Sell, 50000 + i, 100);
                ob.match_order(sell);
            }

            black_box(ob);
        });
    });

    group.finish();
}

/// 基准测试：价格层深度性能
fn bench_price_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("price_depth");

    for depth in [10, 50, 100, 500].iter() {
        group.bench_with_input(
            BenchmarkId::new("build_depth", depth),
            depth,
            |b, &depth| {
                b.iter(|| {
                    let mut ob = create_test_orderbook();

                    // 构建买卖盘深度
                    for i in 0..depth {
                        let buy = create_test_order(1000 + i as u64, OrderType::Buy, 50000 - i as u64, 100);
                        ob.match_order(buy);

                        let sell = create_test_order(2000 + i as u64, OrderType::Sell, 50100 + i as u64, 100);
                        ob.match_order(sell);
                    }

                    black_box(ob);
                });
            },
        );
    }

    group.finish();
}

/// 基准测试：撮合延迟分布
fn bench_matching_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("matching_latency");
    group.sample_size(1000); // 增加样本数以获得更准确的分布

    group.bench_function("instant_match", |b| {
        b.iter_custom(|iters| {
            let mut ob = create_test_orderbook();

            // 预先放入买单
            let buy = create_test_order(1001, OrderType::Buy, 50000, 1000000);
            ob.match_order(buy);

            let sell = create_test_order(2001, OrderType::Sell, 50000, 100);

            let start = Instant::now();

            for _ in 0..iters {
                let result = ob.match_order(sell.clone());
                black_box(result);
            }

            start.elapsed()
        });
    });

    group.bench_function("no_match", |b| {
        b.iter_custom(|iters| {
            let mut ob = create_test_orderbook();
            let order = create_test_order(1001, OrderType::Buy, 50000, 100);

            let start = Instant::now();

            for i in 0..iters {
                let mut order_clone = order.clone();
                order_clone.user_id = 1000 + i;
                let result = ob.match_order(order_clone);
                black_box(result);
            }

            start.elapsed()
        });
    });

    group.finish();
}

/// 基准测试：并发场景模拟
fn bench_concurrent_simulation(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_simulation");

    group.bench_function("alternating_orders", |b| {
        b.iter(|| {
            let mut ob = create_test_orderbook();

            // 模拟交替的买卖单
            for i in 0..1000 {
                if i % 2 == 0 {
                    let buy = create_test_order(1000 + i as u64, OrderType::Buy, 49900 + (i % 100) as u64, 100);
                    ob.match_order(buy);
                } else {
                    let sell = create_test_order(2000 + i as u64, OrderType::Sell, 50100 - (i % 100) as u64, 100);
                    ob.match_order(sell);
                }
            }

            black_box(ob);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_order_codec,
    bench_orderbook_matching,
    bench_e2e_order_processing,
    bench_order_volumes,
    bench_memory_efficiency,
    bench_price_depth,
    bench_matching_latency,
    bench_concurrent_simulation,
);

criterion_main!(benches);
