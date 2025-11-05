/// Network Layer Performance Benchmarks
/// Tests the zero-copy networking stack impact on total latency

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use matching_engine::protocol::{NewOrderRequest, OrderType, TradeNotification};
use serde_json;
use bytes::{BytesMut, BufMut};
use std::sync::Arc;

/// ============================================================================
/// 1. JSON SERIALIZATION COST
/// ============================================================================

fn bench_json_encode_order_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - JSON Encode");
    group.throughput(Throughput::Bytes(300));

    group.bench_function("new_order_request", |b| {
        let order = NewOrderRequest {
            user_id: 12345,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 100,
        };

        b.iter(|| {
            serde_json::to_string(&black_box(&order)).unwrap()
        });
    });

    group.finish();
}

fn bench_json_decode_order_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - JSON Decode");
    group.throughput(Throughput::Bytes(300));

    let json = r#"{"user_id":12345,"symbol":"BTC/USD","order_type":"Buy","price":50000,"quantity":100}"#;

    group.bench_function("new_order_request", |b| {
        b.iter(|| {
            serde_json::from_str::<NewOrderRequest>(black_box(json)).unwrap()
        });
    });

    group.finish();
}

fn bench_json_encode_trade_notification(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - JSON Encode Trade");
    group.throughput(Throughput::Bytes(400));

    group.bench_function("trade_notification", |b| {
        let trade = TradeNotification {
            trade_id: 1,
            symbol: Arc::from("BTC/USD"),
            matched_price: 50000,
            matched_quantity: 100,
            buyer_user_id: 1,
            buyer_order_id: 101,
            seller_user_id: 2,
            seller_order_id: 102,
            timestamp: 1234567890123,
        };

        b.iter(|| {
            serde_json::to_string(&black_box(&trade)).unwrap()
        });
    });

    group.finish();
}

/// ============================================================================
/// 2. BYTESMUT BUFFER OPERATIONS
/// ============================================================================

fn bench_bytesmut_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - BytesMut Push");
    group.throughput(Throughput::Bytes(100));

    group.bench_function("push_100_bytes", |b| {
        let data = vec![0u8; 100];
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(1024);
            buf.extend_from_slice(&black_box(&data));
            black_box(buf);
        });
    });

    group.finish();
}

fn bench_bytesmut_framing(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - Length Framing");
    group.throughput(Throughput::Bytes(104)); // 4 bytes length + 100 data

    group.bench_function("encode_with_length_prefix", |b| {
        let payload = "payload".repeat(14);  // ~100 bytes
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(128);
            let payload_bytes = black_box(payload.as_bytes());

            // Simulate length-delimited codec
            let len = payload_bytes.len() as u32;
            buf.put_u32(len);
            buf.put_slice(payload_bytes);

            black_box(buf);
        });
    });

    group.finish();
}

/// ============================================================================
/// 3. COMBINED ENCODE/DECODE PIPELINE
/// ============================================================================

fn bench_full_request_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - Full Request Pipeline");

    group.bench_function("order_to_json_to_bytes", |b| {
        let order = NewOrderRequest {
            user_id: 12345,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 100,
        };

        b.iter(|| {
            // Encode to JSON
            let json = serde_json::to_string(&black_box(&order)).unwrap();

            // Frame with length prefix
            let mut buf = BytesMut::with_capacity(json.len() + 4);
            let len = json.len() as u32;
            buf.put_u32(len);
            buf.put_slice(json.as_bytes());

            black_box(buf);
        });
    });

    group.finish();
}

fn bench_full_response_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - Full Response Pipeline");

    group.bench_function("trade_to_json_to_bytes", |b| {
        let trade = TradeNotification {
            trade_id: 1,
            symbol: Arc::from("BTC/USD"),
            matched_price: 50000,
            matched_quantity: 100,
            buyer_user_id: 1,
            buyer_order_id: 101,
            seller_user_id: 2,
            seller_order_id: 102,
            timestamp: 1234567890123,
        };

        b.iter(|| {
            // Encode to JSON
            let json = serde_json::to_string(&black_box(&trade)).unwrap();

            // Frame with length prefix
            let mut buf = BytesMut::with_capacity(json.len() + 4);
            let len = json.len() as u32;
            buf.put_u32(len);
            buf.put_slice(json.as_bytes());

            black_box(buf);
        });
    });

    group.finish();
}

/// ============================================================================
/// 4. BROADCAST CHANNEL SIMULATION
/// ============================================================================

fn bench_broadcast_string_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("Network - Broadcast Clone");
    group.throughput(Throughput::Bytes(300));

    group.bench_function("clone_300byte_string", |b| {
        let msg = "message".repeat(40); // ~280 bytes
        b.iter(|| {
            let cloned = black_box(&msg).clone();
            black_box(cloned);
        });
    });

    group.finish();
}

// Criterion Setup

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(100).measurement_time(std::time::Duration::from_secs(5));
    targets =
        bench_json_encode_order_request,
        bench_json_decode_order_request,
        bench_json_encode_trade_notification,
        bench_bytesmut_push,
        bench_bytesmut_framing,
        bench_full_request_pipeline,
        bench_full_response_pipeline,
        bench_broadcast_string_clone
);

criterion_main!(benches);
