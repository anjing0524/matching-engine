/// 网络中间件性能基准测试
///
/// 测试场景:
/// 1. 零拷贝缓冲区性能
/// 2. 编解码器性能
/// 3. 性能指标更新开销
/// 4. 缓冲区池性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use matching_engine::network_middleware::buffer::{BufferPool, SharedBuffer};
use matching_engine::network_middleware::codec::{BincodeCodec, Codec, LengthDelimitedCodec};
use matching_engine::network_middleware::metrics::NetworkMetrics;
use matching_engine::network_middleware::traits::ZeroCopyBuffer;
use matching_engine::protocol::{ClientMessage, NewOrderRequest, OrderType};
use std::sync::Arc;
use std::time::Duration;

/// 基准测试: SharedBuffer 零拷贝克隆
fn bench_shared_buffer_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("shared_buffer");

    for size in [64, 256, 1024, 4096, 16384].iter() {
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::new("clone", size), size, |b, &size| {
            let data = vec![0u8; size];
            let buf = SharedBuffer::from_vec(data);

            b.iter(|| {
                let cloned = buf.clone_ref();
                black_box(cloned);
            });
        });

        group.bench_with_input(BenchmarkId::new("slice", size), size, |b, &size| {
            let data = vec![0u8; size];
            let buf = SharedBuffer::from_vec(data);

            b.iter(|| {
                let slice = buf.slice(0, size / 2);
                black_box(slice);
            });
        });

        group.bench_with_input(BenchmarkId::new("as_slice", size), size, |b, &size| {
            let data = vec![0u8; size];
            let buf = SharedBuffer::from_vec(data);

            b.iter(|| {
                let slice = buf.as_slice();
                black_box(slice);
            });
        });
    }

    group.finish();
}

/// 基准测试: BufferPool 分配性能
fn bench_buffer_pool(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool");

    for pool_size in [16, 64, 256, 1024].iter() {
        group.bench_with_input(
            BenchmarkId::new("alloc_free", pool_size),
            pool_size,
            |b, &pool_size| {
                let pool = Arc::new(BufferPool::new(1024, pool_size));

                b.iter(|| {
                    if let Some(buf) = pool.alloc() {
                        black_box(&buf);
                        pool.free(buf);
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("alloc_only", pool_size),
            pool_size,
            |b, &pool_size| {
                let pool = Arc::new(BufferPool::new(1024, pool_size));

                b.iter(|| {
                    let buf = pool.alloc();
                    black_box(buf);
                });
            },
        );
    }

    group.finish();
}

/// 基准测试: Codec 编解码性能
fn bench_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec");

    // 测试消息
    let test_message = ClientMessage::NewOrder(NewOrderRequest {
        user_id: 12345,
        symbol: Arc::from("BTCUSDT"),
        order_type: OrderType::Buy,
        price: 50000,
        quantity: 100,
    });

    group.bench_function("bincode_encode", |b| {
        let mut codec = BincodeCodec::<ClientMessage>::new();
        let mut buf = vec![0u8; 1024];

        b.iter(|| {
            let size = codec.encode(&test_message, &mut buf).unwrap();
            black_box(size);
        });
    });

    group.bench_function("bincode_decode", |b| {
        let mut codec = BincodeCodec::<ClientMessage>::new();
        let mut encode_buf = vec![0u8; 1024];
        let size = codec.encode(&test_message, &mut encode_buf).unwrap();
        let decode_buf = &encode_buf[..size];

        b.iter(|| {
            let msg = codec.decode(decode_buf).unwrap();
            black_box(msg);
        });
    });

    group.bench_function("length_delimited_encode", |b| {
        let inner = BincodeCodec::<ClientMessage>::new();
        let mut codec = LengthDelimitedCodec::new(inner);
        let mut buf = vec![0u8; 1024];

        b.iter(|| {
            let size = codec.encode(&test_message, &mut buf).unwrap();
            black_box(size);
        });
    });

    group.bench_function("length_delimited_decode", |b| {
        let inner = BincodeCodec::<ClientMessage>::new();
        let mut codec = LengthDelimitedCodec::new(inner);
        let mut encode_buf = vec![0u8; 1024];
        let size = codec.encode(&test_message, &mut encode_buf).unwrap();
        let decode_buf = &encode_buf[..size];

        b.iter(|| {
            let msg = codec.decode(decode_buf).unwrap();
            black_box(msg);
        });
    });

    group.finish();
}

/// 基准测试: 网络指标更新性能
fn bench_metrics(c: &mut Criterion) {
    let mut group = c.benchmark_group("metrics");

    group.bench_function("record_rx_packet", |b| {
        let metrics = Arc::new(NetworkMetrics::new());

        b.iter(|| {
            metrics.record_rx_packet(64);
            black_box(&metrics);
        });
    });

    group.bench_function("record_tx_packet", |b| {
        let metrics = Arc::new(NetworkMetrics::new());

        b.iter(|| {
            metrics.record_tx_packet(64);
            black_box(&metrics);
        });
    });

    group.bench_function("record_latency", |b| {
        let metrics = Arc::new(NetworkMetrics::new());

        b.iter(|| {
            metrics.record_latency(Duration::from_micros(10));
            black_box(&metrics);
        });
    });

    group.bench_function("snapshot", |b| {
        let metrics = Arc::new(NetworkMetrics::new());
        // 预热数据
        for _ in 0..1000 {
            metrics.record_rx_packet(64);
            metrics.record_tx_packet(64);
        }

        b.iter(|| {
            let snapshot = metrics.snapshot();
            black_box(snapshot);
        });
    });

    group.bench_function("concurrent_updates", |b| {
        let metrics = Arc::new(NetworkMetrics::new());

        b.iter(|| {
            // 模拟并发更新
            metrics.record_rx_packet(64);
            metrics.record_tx_packet(64);
            metrics.record_latency(Duration::from_micros(10));
            black_box(&metrics);
        });
    });

    group.finish();
}

/// 基准测试: 完整编解码流程
fn bench_full_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_pipeline");
    group.throughput(Throughput::Elements(1));

    let test_message = ClientMessage::NewOrder(NewOrderRequest {
        user_id: 12345,
        symbol: Arc::from("BTCUSDT"),
        order_type: OrderType::Buy,
        price: 50000,
        quantity: 100,
    });

    group.bench_function("encode_decode_roundtrip", |b| {
        let inner = BincodeCodec::<ClientMessage>::new();
        let mut codec = LengthDelimitedCodec::new(inner);
        let mut buf = vec![0u8; 1024];

        b.iter(|| {
            // 编码
            let size = codec.encode(&test_message, &mut buf).unwrap();

            // 解码
            let decoded = codec.decode(&buf[..size]).unwrap();
            black_box(decoded);
        });
    });

    group.bench_function("with_zero_copy_buffer", |b| {
        let inner = BincodeCodec::<ClientMessage>::new();
        let mut codec = LengthDelimitedCodec::new(inner);
        let mut encode_buf = vec![0u8; 1024];

        b.iter(|| {
            // 编码到缓冲区
            let size = codec.encode(&test_message, &mut encode_buf).unwrap();

            // 创建零拷贝视图
            let buf = SharedBuffer::from_vec(encode_buf[..size].to_vec());

            // 解码
            let decoded = codec.decode(buf.as_slice()).unwrap();
            black_box(decoded);
        });
    });

    group.finish();
}

/// 基准测试: 缓冲区池 + 编解码组合
fn bench_pool_with_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_with_codec");

    let pool = Arc::new(BufferPool::new(1024, 128));
    let test_message = ClientMessage::NewOrder(NewOrderRequest {
        user_id: 12345,
        symbol: Arc::from("BTCUSDT"),
        order_type: OrderType::Buy,
        price: 50000,
        quantity: 100,
    });

    group.bench_function("pool_alloc_encode_free", |b| {
        let inner = BincodeCodec::<ClientMessage>::new();
        let mut codec = LengthDelimitedCodec::new(inner);

        b.iter(|| {
            // 从池中分配
            if let Some(mut buf) = pool.alloc() {
                // 编码
                let size = codec.encode(&test_message, buf.as_mut_slice()).unwrap();
                black_box(size);

                // 归还到池
                pool.free(buf);
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_shared_buffer_clone,
    bench_buffer_pool,
    bench_codec,
    bench_metrics,
    bench_full_pipeline,
    bench_pool_with_codec,
);

criterion_main!(benches);
