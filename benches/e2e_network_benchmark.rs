/// End-to-End Network Performance Benchmark
/// 测试真实网络延迟，包括系统调用、内核处理等隐藏成本
///
/// 这个基准测试暴露当前内存中基准的缺陷

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use matching_engine::protocol::{NewOrderRequest, OrderType};

/// 启动简单的TCP回显服务器
fn start_echo_server(port: u16) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
            .expect("无法绑定服务器");

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let mut buffer = [0; 1024];
                    match stream.read(&mut buffer) {
                        Ok(n) if n > 0 => {
                            // 立即回显
                            let _ = stream.write_all(&buffer[..n]);
                        }
                        _ => {}
                    }
                }
                Err(_) => {}
            }
        }
    })
}

/// 启动带处理的应用服务器 (模拟匹配引擎)
fn start_matching_server(port: u16) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
            .expect("无法绑定服务器");

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let mut buffer = [0; 1024];
                    match stream.read(&mut buffer) {
                        Ok(n) if n > 0 => {
                            // 模拟JSON反序列化 + 匹配 + 序列化
                            let _data = String::from_utf8_lossy(&buffer[..n]);

                            // 模拟核心匹配逻辑 (~100 ns)
                            let mut sum = 0u64;
                            for i in 0..100 {
                                sum = sum.wrapping_add(i);
                            }

                            // 模拟响应序列化
                            let response = format!("{{\"result\":{}}}\n", sum);
                            let _ = stream.write_all(response.as_bytes());
                        }
                        _ => {}
                    }
                }
                Err(_) => {}
            }
        }
    })
}

/// 基准: 纯TCP往返时间 (echo)
fn bench_tcp_echo_rtt(c: &mut Criterion) {
    let mut group = c.benchmark_group("E2E - TCP Echo RTT");

    // 启动服务器
    let _server = start_echo_server(9001);
    thread::sleep(std::time::Duration::from_millis(100)); // 等待服务器启动

    group.bench_function("100_byte_echo", |b| {
        b.iter(|| {
            let mut client = TcpStream::connect("127.0.0.1:9001")
                .expect("无法连接");

            let request = "x".repeat(100);
            client.write_all(request.as_bytes()).expect("写入失败");

            let mut buffer = [0; 1024];
            let n = client.read(&mut buffer).expect("读取失败");
            black_box(n);
        });
    });

    group.bench_function("400_byte_echo", |b| {
        b.iter(|| {
            let mut client = TcpStream::connect("127.0.0.1:9001")
                .expect("无法连接");

            let request = "x".repeat(400);
            client.write_all(request.as_bytes()).expect("写入失败");

            let mut buffer = [0; 1024];
            let n = client.read(&mut buffer).expect("读取失败");
            black_box(n);
        });
    });

    group.finish();
}

/// 基准: 真实应用处理
fn bench_application_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("E2E - Application Processing");

    // 启动处理服务器
    let _server = start_matching_server(9002);
    thread::sleep(std::time::Duration::from_millis(100));

    group.bench_function("order_matching_e2e", |b| {
        b.iter(|| {
            let mut client = TcpStream::connect("127.0.0.1:9002")
                .expect("无法连接");

            // 构造订单请求
            let order = NewOrderRequest {
                user_id: 1,
                symbol: "BTC/USD".to_string(),
                order_type: OrderType::Buy,
                price: 50000,
                quantity: 100,
            };
            let request = serde_json::to_string(&order).unwrap();

            // 发送请求
            client.write_all(request.as_bytes()).expect("写入失败");

            // 接收响应
            let mut buffer = [0; 1024];
            let n = client.read(&mut buffer).expect("读取失败");
            black_box(n);
        });
    });

    group.finish();
}

/// 基准: 连接复用 vs 创建新连接
fn bench_connection_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("E2E - Connection Reuse");

    let _server = start_echo_server(9003);
    thread::sleep(std::time::Duration::from_millis(100));

    group.bench_function("new_connection_per_request", |b| {
        b.iter(|| {
            let mut client = TcpStream::connect("127.0.0.1:9003")
                .expect("无法连接");

            client.write_all(b"test").expect("写入失败");
            let mut buffer = [0; 1024];
            client.read(&mut buffer).expect("读取失败");
            // 连接在这里关闭 (RAII)
        });
    });

    group.bench_function("reuse_single_connection", |b| {
        let mut client = TcpStream::connect("127.0.0.1:9003")
            .expect("无法连接");

        b.iter(|| {
            client.write_all(b"test").expect("写入失败");
            let mut buffer = [0; 1024];
            let _ = client.read(&mut buffer);
        });
    });

    group.finish();
}

/// 基准: 同步网络 vs 异步网络估算
fn bench_syscall_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("E2E - System Call Overhead");

    let _server = start_echo_server(9004);
    thread::sleep(std::time::Duration::from_millis(100));

    for request_size in [100, 400, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}B", request_size)),
            request_size,
            |b, &request_size| {
                b.iter(|| {
                    let mut client = TcpStream::connect("127.0.0.1:9004")
                        .expect("无法连接");

                    let request = "x".repeat(request_size);

                    // 系统调用1: write
                    client.write_all(request.as_bytes()).expect("写入失败");

                    // 系统调用2: read (阻塞)
                    let mut buffer = [0; 2048];
                    let n = client.read(&mut buffer).expect("读取失败");
                    black_box(n);
                });
            },
        );
    }

    group.finish();
}

/// 单一持久连接的吞吐量测试
fn bench_persistent_connection_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("E2E - Persistent Connection");
    group.sample_size(30); // 减少样本，因为耗时更长

    let _server = start_echo_server(9005);
    thread::sleep(std::time::Duration::from_millis(100));

    group.bench_function("1000_sequential_messages", |b| {
        let mut client = TcpStream::connect("127.0.0.1:9005")
            .expect("无法连接");

        b.iter(|| {
            for _ in 0..1000 {
                client.write_all(b"x").expect("写入失败");
                let mut buffer = [0; 1024];
                let _ = client.read(&mut buffer);
            }
        });
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(10).measurement_time(std::time::Duration::from_secs(10));
    targets =
        bench_tcp_echo_rtt,
        bench_application_processing,
        bench_connection_reuse,
        bench_syscall_overhead,
        bench_persistent_connection_throughput
);

criterion_main!(benches);
