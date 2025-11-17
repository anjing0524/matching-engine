/// CLI Interface Module
///
/// This module provides command-line interface functionality for the matching engine.
/// It serves as the primary entry point for the application when run as a standalone service.
///
/// ## Responsibilities
/// - Parse command-line arguments
/// - Initialize application services
/// - Configure and start the matching engine
/// - Handle graceful shutdown

use clap::Parser;
use std::net::IpAddr;
use std::path::PathBuf;

/// 撮合引擎命令行配置
#[derive(Parser, Debug, Clone)]
#[command(name = "matching-engine")]
#[command(author = "Matching Engine Team")]
#[command(version = "0.1.0")]
#[command(about = "高性能期货撮合引擎", long_about = None)]
pub struct CliConfig {
    /// 服务器监听地址
    #[arg(short = 'H', long, default_value = "127.0.0.1")]
    pub host: IpAddr,

    /// 服务器监听端口
    #[arg(short, long, default_value_t = 8080)]
    pub port: u16,

    /// 分区数量（0表示自动检测CPU核心数）
    #[arg(short = 'n', long, default_value_t = 0)]
    pub partitions: usize,

    /// 网络后端类型
    #[arg(short = 'b', long, default_value = "tokio", value_parser = ["tokio", "uring", "dpdk"])]
    pub network_backend: String,

    /// 每个价格层的队列容量
    #[arg(short = 'q', long, default_value_t = 1024)]
    pub queue_capacity: usize,

    /// 启用CPU亲和性绑定
    #[arg(long, default_value_t = false)]
    pub cpu_affinity: bool,

    /// 日志级别
    #[arg(short = 'l', long, default_value = "info", value_parser = ["trace", "debug", "info", "warn", "error"])]
    pub log_level: String,

    /// 仅显示配置不启动服务器（用于调试）
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// 数据目录（用于持久化）
    #[arg(short = 'd', long, default_value = "./data")]
    pub data_dir: PathBuf,

    /// 是否启用持久化
    #[arg(long, default_value_t = true)]
    pub persistence: bool,

    /// 合约代码
    #[arg(short = 's', long, default_value = "BTCUSDT")]
    pub symbol: String,

    /// 最小价格
    #[arg(long, default_value_t = 40000)]
    pub min_price: u64,

    /// 最大价格
    #[arg(long, default_value_t = 70000)]
    pub max_price: u64,

    /// Tick大小
    #[arg(long, default_value_t = 1)]
    pub tick_size: u64,
}

/// Runs the CLI application
///
/// This is the main entry point for the CLI interface.
/// Parses command-line arguments and configures the matching engine.
pub async fn run() {
    // 解析命令行参数
    let config = CliConfig::parse();

    // 初始化日志系统
    init_logging(&config.log_level);

    tracing::info!("撮合引擎启动");
    tracing::info!("配置: {:?}", config);

    // 自动检测分区数
    let partition_count = if config.partitions == 0 {
        let cpus = num_cpus::get();
        tracing::info!("自动检测到 {} 个CPU核心", cpus);
        cpus
    } else {
        config.partitions
    };

    // 显示配置信息
    println!("========================================");
    println!("  高性能期货撮合引擎 v0.1.0");
    println!("========================================");
    println!("监听地址:     {}:{}", config.host, config.port);
    println!("分区数量:     {}", partition_count);
    println!("网络后端:     {}", config.network_backend);
    println!("队列容量:     {}", config.queue_capacity);
    println!("CPU亲和性:    {}", if config.cpu_affinity { "启用" } else { "禁用" });
    println!("日志级别:     {}", config.log_level);
    println!("持久化:       {}", if config.persistence { "启用" } else { "禁用" });
    println!("数据目录:     {}", config.data_dir.display());
    println!("合约:         {} ({}-{})", config.symbol, config.min_price, config.max_price);
    println!("========================================");

    // 如果是dry-run模式，仅显示配置
    if config.dry_run {
        println!("\nDry-run 模式 - 不启动服务器");
        return;
    }

    // 初始化应用服务
    println!("\n服务器初始化中...");

    if config.persistence {
        // 使用持久化匹配服务
        if let Err(e) = run_persistent_server(config).await {
            tracing::error!("服务器错误: {}", e);
            std::process::exit(1);
        }
    } else {
        println!("⚠️ 非持久化模式尚未实现");
        println!("请使用 --persistence 启用持久化模式");
    }
}

/// 运行持久化服务器
async fn run_persistent_server(config: CliConfig) -> Result<(), Box<dyn std::error::Error>> {
    use crate::application::services::PersistentMatchingService;
    use crate::domain::orderbook::ContractSpec;
    use crate::infrastructure::persistence::PersistenceConfig;
    use crate::infrastructure::network::backends::TokioTransport;
    use crate::infrastructure::network::traits::{NetworkTransport, Connection};
    use crate::infrastructure::network::codec::{Codec, BincodeCodec, LengthDelimitedCodec};
    use crate::shared::protocol::ClientMessage;
    use futures::StreamExt;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // 创建合约规格
    let spec = ContractSpec::new(
        &config.symbol,
        config.tick_size,
        config.min_price,
        config.max_price,
    );

    // 创建持久化配置
    let persistence_config = PersistenceConfig {
        data_dir: config.data_dir.clone(),
        wal_sync: true,
        ..Default::default()
    };

    // 尝试恢复或创建新服务
    let service = if persistence_config.data_dir.exists() {
        tracing::info!("检测到数据目录，尝试恢复状态...");
        match PersistentMatchingService::recover(spec.clone(), persistence_config.clone()) {
            Ok(svc) => {
                tracing::info!("✅ 状态恢复成功");
                println!("✅ 从持久化状态恢复成功");
                svc
            }
            Err(e) => {
                tracing::warn!("恢复失败: {}, 创建新服务", e);
                println!("⚠️ 恢复失败，创建新服务");
                PersistentMatchingService::new(spec, persistence_config)?
            }
        }
    } else {
        tracing::info!("创建新的持久化服务");
        println!("创建新的持久化服务");
        PersistentMatchingService::new(spec, persistence_config)?
    };

    let service = Arc::new(RwLock::new(service));

    // 创建网络传输层
    let mut transport = TokioTransport::new()?;
    let addr = format!("{}:{}", config.host, config.port).parse()?;
    transport.bind(addr).await?;

    tracing::info!("服务器已启动，监听 {}", addr);
    println!("✅ 服务器已启动");
    println!("📡 监听地址: {}", addr);
    println!("\n等待客户端连接...\n");

    // 接受连接循环
    loop {
        match transport.accept().await {
            Ok(conn) => {
                let peer_addr = conn.peer_addr().ok();
                tracing::info!("接受新连接: {:?}", peer_addr);
                println!("🔗 新连接: {:?}", peer_addr);

                let service_clone = Arc::clone(&service);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(conn, service_clone).await {
                        tracing::error!("连接处理错误: {}", e);
                    }
                });
            }
            Err(e) => {
                tracing::error!("接受连接失败: {}", e);
            }
        }
    }
}

/// 处理单个客户端连接
async fn handle_connection(
    mut conn: Box<dyn crate::infrastructure::network::traits::Connection>,
    service: std::sync::Arc<tokio::sync::RwLock<crate::application::services::PersistentMatchingService>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::infrastructure::network::codec::{Codec, BincodeCodec, LengthDelimitedCodec};
    use crate::shared::protocol::ClientMessage;

    let peer_addr = conn.peer_addr().ok();
    tracing::info!("开始处理连接: {:?}", peer_addr);

    let mut codec = LengthDelimitedCodec::new(BincodeCodec::<ClientMessage>::new());

    loop {
        // 接收数据（零拷贝）
        let buf = conn.recv().await?;
        let data = buf.as_slice();

        // 解码消息
        match codec.decode(data)? {
            Some(ClientMessage::NewOrder(order)) => {
                tracing::debug!(
                    "收到订单: symbol={}, type={:?}, price={}, qty={}",
                    order.symbol,
                    order.order_type,
                    order.price,
                    order.quantity
                );

                // 提交到持久化服务
                let mut svc = service.write().await;
                match svc.match_order(order.clone()) {
                    Ok((trades, _confirmation)) => {
                        if !trades.is_empty() {
                            tracing::info!("订单撮合成功，产生 {} 笔成交", trades.len());
                            println!("  ✅ 订单撮合成功，产生 {} 笔成交", trades.len());
                        } else {
                            tracing::info!("订单已挂单");
                            println!("  📋 订单已挂单");
                        }
                    }
                    Err(e) => {
                        tracing::error!("订单处理失败: {}", e);
                        println!("  ❌ 订单处理失败: {}", e);
                    }
                }
            }
            Some(ClientMessage::CancelOrder(_cancel)) => {
                tracing::warn!("取消订单功能当前不支持");
                println!("  ⚠️ 取消订单功能当前不支持");
            }
            None => {
                // 不完整的消息，继续接收
                continue;
            }
        }
    }
}

/// 初始化日志系统
fn init_logging(level: &str) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_config_default() {
        // 测试默认配置
        let config = CliConfig::parse_from(&["matching-engine"]);
        assert_eq!(config.port, 8080);
        assert_eq!(config.partitions, 0);
        assert_eq!(config.network_backend, "tokio");
        assert_eq!(config.queue_capacity, 1024);
        assert!(!config.cpu_affinity);
        assert_eq!(config.log_level, "info");
        assert!(!config.dry_run);
    }

    #[test]
    fn test_cli_config_custom() {
        // 测试自定义配置
        let config = CliConfig::parse_from(&[
            "matching-engine",
            "--host", "0.0.0.0",
            "--port", "9000",
            "--partitions", "8",
            "--network-backend", "uring",
            "--queue-capacity", "2048",
            "--cpu-affinity",
            "--log-level", "debug",
            "--dry-run",
        ]);

        assert_eq!(config.host.to_string(), "0.0.0.0");
        assert_eq!(config.port, 9000);
        assert_eq!(config.partitions, 8);
        assert_eq!(config.network_backend, "uring");
        assert_eq!(config.queue_capacity, 2048);
        assert!(config.cpu_affinity);
        assert_eq!(config.log_level, "debug");
        assert!(config.dry_run);
    }

    #[test]
    fn test_cli_config_short_flags() {
        // 测试短参数
        let config = CliConfig::parse_from(&[
            "matching-engine",
            "-H", "192.168.1.1",
            "-p", "7000",
            "-n", "4",
            "-b", "tokio",
            "-q", "512",
            "-l", "warn",
        ]);

        assert_eq!(config.host.to_string(), "192.168.1.1");
        assert_eq!(config.port, 7000);
        assert_eq!(config.partitions, 4);
        assert_eq!(config.queue_capacity, 512);
        assert_eq!(config.log_level, "warn");
    }
}
