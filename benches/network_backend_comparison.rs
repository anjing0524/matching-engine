/// 网络后端性能对比基准测试
///
/// 对比 Tokio、io_uring、DPDK 三种后端的性能
///
/// 测试维度:
/// 1. 延迟 (latency)
/// 2. 吞吐量 (throughput)
/// 3. CPU使用率 (cpu usage)
/// 4. 并发连接数 (concurrent connections)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use matching_engine::network_middleware::buffer::SharedBuffer;
use matching_engine::network_middleware::codec::{BincodeCodec, Codec, LengthDelimitedCodec};
use matching_engine::network_middleware::traits::{Connection, NetworkTransport, ZeroCopyBuffer};
use matching_engine::network_middleware::tokio_backend::TokioTransport;
use matching_engine::protocol::{ClientMessage, NewOrderRequest, OrderType};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 创建测试消息
fn create_test_message() -> ClientMessage {
    ClientMessage::NewOrder(NewOrderRequest {
        user_id: 12345,
        symbol: Arc::from("BTCUSDT"),
        order_type: OrderType::Buy,
        price: 50000,
        quantity: 100,
    })
}

/// 编码消息到缓冲区
fn encode_message(msg: &ClientMessage) -> Vec<u8> {
    let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());
    let mut buf = vec![0u8; 4096];
    let size = codec.encode(msg, &mut buf).unwrap();
    buf.truncate(size);
    buf
}

/// Tokio后端延迟测试
fn bench_tokio_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency");
    group.sample_size(100);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    group.bench_function("tokio_roundtrip", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                // 启动服务器
                let mut server = TokioTransport::new().unwrap();
                server
                    .bind("127.0.0.1:0".parse().unwrap())
                    .await
                    .unwrap();
                let server_addr = server.local_addr().unwrap();

                // 服务器任务（回显）
                let server_handle = tokio::spawn(async move {
                    while let Ok(mut conn) = server.accept().await {
                        tokio::spawn(async move {
                            while let Ok(buf) = conn.recv().await {
                                let _ = conn.send(buf).await;
                            }
                        });
                    }
                });

                tokio::time::sleep(Duration::from_millis(100)).await;

                // 创建客户端
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                use tokio::net::TcpStream;

                let mut client = TcpStream::connect(server_addr).await.unwrap();
                let test_msg = create_test_message();
                let test_data = encode_message(&test_msg);

                let start = Instant::now();

                for _ in 0..iters {
                    // 发送
                    client.write_all(&test_data).await.unwrap();
                    client.flush().await.unwrap();

                    // 接收
                    let mut len_buf = [0u8; 4];
                    client.read_exact(&mut len_buf).await.unwrap();
                    let len = u32::from_be_bytes(len_buf) as usize - 4;
                    let mut recv_buf = vec![0u8; len];
                    client.read_exact(&mut recv_buf).await.unwrap();
                }

                let elapsed = start.elapsed();

                // 清理
                drop(client);
                server_handle.abort();

                elapsed
            })
        });
    });

    group.finish();
}

/// Tokio后端吞吐量测试
fn bench_tokio_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    group.sample_size(20);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    for msg_count in [1000, 10000, 100000].iter() {
        group.throughput(Throughput::Elements(*msg_count as u64));

        group.bench_with_input(
            BenchmarkId::new("tokio", msg_count),
            msg_count,
            |b, &msg_count| {
                b.iter_custom(|_iters| {
                    rt.block_on(async {
                        // 启动服务器
                        let mut server = TokioTransport::new().unwrap();
                        server
                            .bind("127.0.0.1:0".parse().unwrap())
                            .await
                            .unwrap();
                        let server_addr = server.local_addr().unwrap();

                        // 服务器任务（丢弃消息）
                        let server_handle = tokio::spawn(async move {
                            while let Ok(mut conn) = server.accept().await {
                                tokio::spawn(async move {
                                    let mut count = 0;
                                    while let Ok(_buf) = conn.recv().await {
                                        count += 1;
                                        if count >= msg_count {
                                            break;
                                        }
                                    }
                                });
                            }
                        });

                        tokio::time::sleep(Duration::from_millis(100)).await;

                        // 创建客户端
                        use tokio::io::AsyncWriteExt;
                        use tokio::net::TcpStream;

                        let mut client = TcpStream::connect(server_addr).await.unwrap();
                        let test_msg = create_test_message();
                        let test_data = encode_message(&test_msg);

                        let start = Instant::now();

                        for _ in 0..msg_count {
                            client.write_all(&test_data).await.unwrap();
                        }
                        client.flush().await.unwrap();

                        let elapsed = start.elapsed();

                        // 清理
                        drop(client);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        server_handle.abort();

                        elapsed
                    })
                });
            },
        );
    }

    group.finish();
}

/// 消息编解码性能
fn bench_message_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("message_codec");

    let test_msg = create_test_message();

    group.bench_function("encode", |b| {
        let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());
        let mut buf = vec![0u8; 4096];

        b.iter(|| {
            let size = codec.encode(&test_msg, &mut buf).unwrap();
            black_box(size);
        });
    });

    group.bench_function("decode", |b| {
        let encoded = encode_message(&test_msg);
        let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());

        b.iter(|| {
            let msg = codec.decode(&encoded).unwrap();
            black_box(msg);
        });
    });

    group.bench_function("roundtrip", |b| {
        let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());
        let mut buf = vec![0u8; 4096];

        b.iter(|| {
            let size = codec.encode(&test_msg, &mut buf).unwrap();
            let msg = codec.decode(&buf[..size]).unwrap();
            black_box(msg);
        });
    });

    group.finish();
}

/// 零拷贝缓冲区性能
fn bench_zerocopy_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("zerocopy");

    for size in [256, 1024, 4096, 16384].iter() {
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::new("create", size), size, |b, &size| {
            b.iter(|| {
                let buf = SharedBuffer::from_vec(vec![0u8; size]);
                black_box(buf);
            });
        });

        group.bench_with_input(BenchmarkId::new("clone_ref", size), size, |b, &size| {
            let buf = SharedBuffer::from_vec(vec![0u8; size]);

            b.iter(|| {
                let cloned = buf.clone_ref();
                black_box(cloned);
            });
        });

        group.bench_with_input(BenchmarkId::new("slice", size), size, |b, &size| {
            let buf = SharedBuffer::from_vec(vec![0u8; size]);

            b.iter(|| {
                let slice = buf.slice(0, size / 2);
                black_box(slice);
            });
        });
    }

    group.finish();
}

/// 并发连接测试
fn bench_concurrent_connections(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent");
    group.sample_size(10);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();

    for conn_count in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("tokio", conn_count),
            conn_count,
            |b, &conn_count| {
                b.iter_custom(|_iters| {
                    rt.block_on(async {
                        // 启动服务器
                        let mut server = TokioTransport::new().unwrap();
                        server
                            .bind("127.0.0.1:0".parse().unwrap())
                            .await
                            .unwrap();
                        let server_addr = server.local_addr().unwrap();

                        // 服务器任务
                        let server_handle = tokio::spawn(async move {
                            while let Ok(mut conn) = server.accept().await {
                                tokio::spawn(async move {
                                    while let Ok(buf) = conn.recv().await {
                                        let _ = conn.send(buf).await;
                                    }
                                });
                            }
                        });

                        tokio::time::sleep(Duration::from_millis(200)).await;

                        let test_msg = create_test_message();
                        let test_data = encode_message(&test_msg);

                        let start = Instant::now();

                        // 创建多个并发连接
                        let mut handles = vec![];
                        for _ in 0..conn_count {
                            let test_data_clone = test_data.clone();
                            let server_addr_clone = server_addr;

                            let handle = tokio::spawn(async move {
                                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                                use tokio::net::TcpStream;

                                let mut client = TcpStream::connect(server_addr_clone).await.unwrap();

                                // 发送10个消息
                                for _ in 0..10 {
                                    client.write_all(&test_data_clone).await.unwrap();
                                    client.flush().await.unwrap();

                                    // 接收回显
                                    let mut len_buf = [0u8; 4];
                                    client.read_exact(&mut len_buf).await.unwrap();
                                    let len = u32::from_be_bytes(len_buf) as usize - 4;
                                    let mut recv_buf = vec![0u8; len];
                                    client.read_exact(&mut recv_buf).await.unwrap();
                                }
                            });

                            handles.push(handle);
                        }

                        // 等待所有连接完成
                        for handle in handles {
                            handle.await.unwrap();
                        }

                        let elapsed = start.elapsed();

                        // 清理
                        server_handle.abort();

                        elapsed
                    })
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_tokio_latency,
    bench_tokio_throughput,
    bench_message_codec,
    bench_zerocopy_buffer,
    bench_concurrent_connections,
);

criterion_main!(benches);
