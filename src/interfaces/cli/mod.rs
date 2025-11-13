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
    println!("========================================");

    // 如果是dry-run模式，仅显示配置
    if config.dry_run {
        println!("\nDry-run 模式 - 不启动服务器");
        return;
    }

    // TODO: 初始化应用服务
    // - 创建 MatchingService 或 PartitionedService
    // - 启动网络服务器
    // - 处理优雅关闭

    println!("\n服务器初始化中...");
    println!("(当前为演示实现，实际服务器功能待集成)");

    // 占位符：模拟服务器运行
    tracing::info!("服务器已准备就绪");

    // TODO: 实际实现应该启动异步服务器并等待
    // server.run().await.unwrap();
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
