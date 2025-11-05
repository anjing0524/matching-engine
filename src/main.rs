use std::net::SocketAddr;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use matching_engine::{engine, network};

#[tokio::main]
async fn main() {
    println!("程序启动 - main() 函数入口");
    // 强制刷新，确保即使立即崩溃也能看到输出
    use std::io::{self, Write};
    io::stdout().flush().unwrap();

    // 初始化日志
    tracing_subscriber::fmt::init();

    println!("日志系统已初始化");

    // 创建用于网络层和引擎层通信的通道
    let (command_sender, command_receiver) = mpsc::unbounded_channel::<engine::EngineCommand>();
    let (output_sender, output_receiver) = mpsc::unbounded_channel::<engine::EngineOutput>();

    println!("通道已创建");

    // 在一个独立的系统线程中运行撮合引擎
    let engine_thread = thread::spawn(move || {
        let mut engine = engine::MatchingEngine::new(command_receiver, output_sender);
        engine.run();
    });

    println!("撮合引擎线程已启动");

    // 在 Tokio 运行时中启动网络服务器
    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let server_handle = tokio::spawn(network::run_server(addr, command_sender, output_receiver));

    println!("网络服务器任务已启动");

    // 引入延迟，以便观察进程状态和日志文件
    println!("进入2秒休眠...");
    thread::sleep(Duration::from_secs(2));
    println!("休眠结束");

    // 等待服务器任务结束
    if let Err(e) = server_handle.await {
        eprintln!("网络服务器任务出现严重错误: {:?}", e);
    }

    // 等待引擎线程结束（虽然在当前设计中它是一个无限循环）
    engine_thread.join().expect("撮合引擎线程崩溃");
}
