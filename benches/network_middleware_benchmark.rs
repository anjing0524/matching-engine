//! Wire codec benchmarks.
//!
//! Measures the cost of the engine's own framing/encoding path. The concrete
//! The blocking connection performs no `dyn` dispatch, so what remains is the codec
//! and one payload allocation per message.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use matching_engine::shared::protocol::{ClientMessage, NewOrderRequest, OrderType};
use matching_engine::shared::wire::{decode, decode_frame, encode_frame, encode_to_vec};
use std::hint::black_box;

fn test_message() -> ClientMessage {
    ClientMessage::NewOrder(NewOrderRequest {
        user_id: 12_345,
        client_order_id: 0,
        symbol: matching_engine::shared::SymbolId::intern("BTCUSDT"),
        order_type: OrderType::Buy,
        price: 50_000,
        quantity: 100,
    })
}

fn bench_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_codec");
    let message = test_message();
    let payload = encode_to_vec(&message).unwrap();
    let frame = encode_frame(&message).unwrap();
    group.throughput(Throughput::Bytes(payload.len() as u64));

    group.bench_function("encode", |b| {
        b.iter(|| black_box(encode_to_vec(&message).unwrap()));
    });

    group.bench_function("decode", |b| {
        b.iter(|| black_box(decode::<ClientMessage>(&payload).unwrap()));
    });

    group.bench_function("decode_frame", |b| {
        b.iter(|| black_box(decode_frame::<ClientMessage>(&frame).unwrap()));
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let payload = encode_to_vec(&message).unwrap();
            black_box(decode::<ClientMessage>(&payload).unwrap());
        });
    });

    group.finish();
}

criterion_group!(benches, bench_codec);
criterion_main!(benches);
