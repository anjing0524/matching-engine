//! Command-line interface for the matching engine.
//!
//! Parses configuration and wires the matching runtime, TCP gateway, session
//! handler, signals, and observability server together. The data path is
//! blocking: no async runtime is involved.
use crate::application::services::{
    MatchingRuntime, PersistentMatchingService, RuntimeConfig, RuntimeError, ServiceError,
};
use crate::domain::orderbook::ContractSpec;
use crate::infrastructure::observability::ObservabilityServer;
use crate::infrastructure::persistence::{PersistenceConfig, PersistenceError};
use crate::interfaces::engine_ipc::{self, EngineIpcConfig};
use crate::interfaces::gateway::{Gateway, GatewayConfig, GatewayError};
use crate::interfaces::session::{self, SessionMetrics};
use clap::Parser;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// Command-line configuration for the matching engine.
#[derive(Parser, Debug, Clone)]
#[command(name = "matching-engine")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "高性能期货撮合引擎", long_about = None)]
pub struct CliConfig {
    /// Server bind address.
    #[arg(short = 'H', long, default_value = "127.0.0.1")]
    pub host: IpAddr,

    /// Server port.
    #[arg(short, long, default_value_t = 8080)]
    pub port: u16,

    /// Capacity of each price-level queue.
    #[arg(short = 'q', long, default_value_t = 2048)]
    pub queue_capacity: usize,

    /// Log level.
    #[arg(
        short = 'l',
        long,
        default_value = "info",
        value_parser = ["trace", "debug", "info", "warn", "error"]
    )]
    pub log_level: String,

    /// Print the configuration and exit without starting the server.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Port for the Prometheus/health HTTP server (0 disables it).
    #[arg(long, default_value_t = 9090)]
    pub metrics_port: u16,

    /// Maximum number of concurrently connected clients.
    #[arg(long, default_value_t = 1024)]
    pub max_connections: usize,

    /// Close connections that send nothing for this many seconds (0 disables).
    #[arg(long, default_value_t = 300)]
    pub idle_timeout_secs: u64,

    /// Maximum number of commands staged before one WAL flush/fsync (group commit).
    #[arg(long, default_value_t = 256)]
    pub commit_batch_size: usize,

    /// Bounded idempotency cache size (client_order_id duplicates).
    #[arg(long, default_value_t = 262_144)]
    pub dedup_cache_size: usize,

    /// Take a snapshot after this many WAL records (0 disables).
    ///
    /// Default is 0 so the matching thread never waits on a snapshot barrier;
    /// enable it only when accepted restart latency matters more.
    #[arg(long, default_value_t = 0)]
    pub snapshot_interval: u64,

    /// Maximum in-flight requests per client session; 1 preserves lowest latency.
    #[arg(long, default_value_t = 1)]
    pub max_in_flight: usize,

    /// fsync the WAL before acknowledging an order (disable only if the
    /// deployment can tolerate losing the OS page cache on power loss).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub wal_sync: bool,

    /// Engine IPC Unix socket path. When set, run the matching engine without
    /// accepting client TCP connections; a separate gateway process connects here.
    #[arg(long)]
    pub engine_uds: Option<PathBuf>,

    /// Data directory for WAL files.
    #[arg(short = 'd', long, default_value = "./data")]
    pub data_dir: PathBuf,

    /// Contract symbol.
    #[arg(short = 's', long, default_value = "BTCUSDT")]
    pub symbol: String,

    /// Minimum price.
    #[arg(long, default_value_t = 40000)]
    pub min_price: u64,

    /// Maximum price.
    #[arg(long, default_value_t = 70000)]
    pub max_price: u64,

    /// Tick size.
    #[arg(long, default_value_t = 1)]
    pub tick_size: u64,

    /// Pin the matching thread to this logical core (requires `--features cpu-affinity`).
    #[arg(long)]
    pub matching_core: Option<usize>,

    /// Spin on an empty matching queue instead of sleeping (dedicated core only).
    #[arg(long, default_value_t = false)]
    pub busy_poll: bool,

    /// Flush WAL on the matching thread but fsync on a dedicated thread.
    ///
    /// Snapshots use a durability barrier before writing. Requires `--wal-sync`.
    #[arg(long, default_value_t = false)]
    pub async_fsync: bool,
}

/// Maximum number of pre-allocated price levels accepted by the CLI.
const MAX_PRICE_LEVELS: u64 = 1_000_000;

fn validate_config(config: &CliConfig) -> Result<(), CliError> {
    if config.tick_size == 0 {
        return Err(CliError::Config("tick size must be greater than 0".into()));
    }
    if config.max_price <= config.min_price {
        return Err(CliError::Config(
            "max price must be greater than min price".into(),
        ));
    }
    if config.queue_capacity == 0 {
        return Err(CliError::Config(
            "queue capacity must be greater than 0".into(),
        ));
    }
    if config.max_connections == 0 {
        return Err(CliError::Config(
            "max connections must be greater than 0".into(),
        ));
    }
    if config.commit_batch_size == 0 {
        return Err(CliError::Config(
            "commit batch size must be greater than 0".into(),
        ));
    }
    if config.max_in_flight == 0 {
        return Err(CliError::Config(
            "max in-flight requests must be greater than 0".into(),
        ));
    }
    let ticks = config.max_price - config.min_price;
    if !ticks.is_multiple_of(config.tick_size) {
        return Err(CliError::Config(
            "price range must be divisible by tick size".into(),
        ));
    }
    let levels = ticks / config.tick_size + 1;
    if levels > MAX_PRICE_LEVELS {
        return Err(CliError::Config(format!(
            "price range covers {levels} levels, exceeding the limit of {MAX_PRICE_LEVELS}"
        )));
    }
    if config.async_fsync && !config.wal_sync {
        return Err(CliError::Config(
            "async fsync requires WAL fsync to be enabled".into(),
        ));
    }
    Ok(())
}

/// Errors that can terminate the server.
#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Address(#[from] std::net::AddrParseError),
    #[error("matching worker stopped unexpectedly")]
    WorkerStopped,
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("fatal persistence failure: {0}")]
    Fatal(String),
}

/// Parses command-line arguments and runs the matching engine.
pub fn run() {
    let config = CliConfig::parse();
    init_logging(&config.log_level);

    tracing::info!(?config, "matching engine starting");

    println!("========================================");
    println!("  高性能期货撮合引擎 v{}", env!("CARGO_PKG_VERSION"));
    println!("========================================");
    println!("监听地址:     {}:{}", config.host, config.port);
    println!("队列容量:     {}", config.queue_capacity);
    println!("日志级别:     {}", config.log_level);
    println!("数据目录:     {}", config.data_dir.display());
    println!(
        "合约:         {} ({}-{})",
        config.symbol, config.min_price, config.max_price
    );
    println!("WAL fsync:    {}", config.wal_sync);
    println!("========================================");

    if config.dry_run {
        println!("\nDry-run 模式 - 不启动服务器");
        return;
    }

    if let Err(error) = run_server(config) {
        tracing::error!(%error, "server error");
        eprintln!("服务器错误: {error}");
        std::process::exit(1);
    }
}

/// Runs the persistent matching server until SIGINT/SIGTERM.
fn run_server(config: CliConfig) -> Result<(), CliError> {
    validate_config(&config)?;

    let mut spec = ContractSpec::new(
        &config.symbol,
        config.tick_size,
        config.min_price,
        config.max_price,
    );
    spec.queue_capacity = config.queue_capacity;

    let persistence_config = PersistenceConfig {
        data_dir: config.data_dir.clone(),
        wal_sync: config.wal_sync,
        dedup_cache_size: config.dedup_cache_size,
        snapshot_every_records: config.snapshot_interval,
        ..Default::default()
    };

    // A corrupt or unreadable WAL is fatal: starting with an empty book would
    // silently discard acknowledged orders.
    let service = if persistence_config.data_dir.exists() {
        tracing::info!("data directory found, recovering from WAL");
        PersistentMatchingService::recover(spec, persistence_config)?
    } else {
        PersistentMatchingService::new(spec, persistence_config)?
    };

    let runtime = MatchingRuntime::start(
        service,
        RuntimeConfig {
            queue_capacity: 10_000,
            batch_size: config.commit_batch_size,
            matching_core: config.matching_core,
            busy_poll: config.busy_poll,
            async_fsync: config.async_fsync && config.engine_uds.is_none(),
            decouple_wal: config.engine_uds.is_some(),
        },
    )?;
    let matching = runtime.handle();

    if let Some(engine_uds) = config.engine_uds.clone() {
        if config.metrics_port > 0 {
            let server = ObservabilityServer::new(config.metrics_port);
            thread::Builder::new()
                .name("metrics-http".to_string())
                .stack_size(256 * 1024)
                .spawn(move || {
                    if let Err(error) = server.run() {
                        tracing::error!(%error, "observability server failed");
                    }
                })?;
            println!(
                "📊 Metrics/健康检查: http://127.0.0.1:{}",
                config.metrics_port
            );
        }

        let shutdown = Arc::new(AtomicBool::new(false));
        let ipc = engine_ipc::start(
            EngineIpcConfig {
                path: engine_uds.clone(),
                max_connections: config.max_connections,
                symbol: config.symbol.clone(),
            },
            matching,
            Arc::clone(&shutdown),
        )?;
        println!("🔌 Engine IPC: {}", engine_uds.display());

        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let (signal_tx, signal_rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name("signals".to_string())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    let _ = signal_tx.send(());
                }
            })?;
        let _ = signal_rx.recv();
        shutdown.store(true, Ordering::Release);
        let _ = ipc.join();
        engine_ipc::cleanup(&engine_uds);
        match runtime.shutdown() {
            Ok(()) => return Ok(()),
            Err(RuntimeError::Fatal(message)) => return Err(CliError::Fatal(message)),
            Err(RuntimeError::WorkerPanicked | RuntimeError::Closed) => {
                return Err(CliError::WorkerStopped);
            }
            Err(RuntimeError::Service(error)) => {
                return Err(CliError::Persistence(match error {
                    ServiceError::Persistence(error) => error,
                    other => PersistenceError::Io(std::io::Error::other(other.to_string())),
                }));
            }
            Err(RuntimeError::Persistence(error)) => {
                return Err(CliError::Persistence(error));
            }
        }
    }

    let addr = format!("{}:{}", config.host, config.port).parse()?;
    let gateway = Gateway::bind(GatewayConfig {
        addr,
        max_connections: config.max_connections,
        session_stack_size: 256 * 1024,
    })?;
    let control = gateway.control();
    let local_addr = gateway.local_addr();

    tracing::info!(%local_addr, "server started");
    println!("✅ 服务器已启动，监听 {local_addr}");

    if config.metrics_port > 0 {
        let server = ObservabilityServer::new(config.metrics_port);
        thread::Builder::new()
            .name("metrics-http".to_string())
            .stack_size(256 * 1024)
            .spawn(move || {
                if let Err(error) = server.run() {
                    tracing::error!(%error, "observability server failed");
                }
            })?;
        println!(
            "📊 Metrics/健康检查: http://127.0.0.1:{}",
            config.metrics_port
        );
    }

    // SIGINT/SIGTERM: the gateway wakes blocked reads and drains sessions.
    let signal_control = control.clone();
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    thread::Builder::new()
        .name("signals".to_string())
        .spawn(move || {
            if signals.forever().next().is_some() {
                tracing::info!("received shutdown signal");
                signal_control.request_shutdown();
            }
        })?;

    let session_metrics = SessionMetrics::new(&config.symbol);
    let idle_timeout =
        (config.idle_timeout_secs > 0).then(|| Duration::from_secs(config.idle_timeout_secs));
    let outcome = gateway.run(move |conn| {
        if let Err(error) = session::serve(
            conn,
            matching.clone(),
            session_metrics.clone(),
            control.clone(),
            idle_timeout,
            config.max_in_flight,
        ) {
            tracing::error!(%error, "connection handler failed");
        }
    })?;

    // Flush and stop the matching worker only after sessions have drained.
    let shutdown_result = runtime.shutdown();
    if let Some(message) = outcome.fatal {
        return Err(CliError::Fatal(message));
    }
    match shutdown_result {
        Ok(()) => {}
        Err(RuntimeError::Fatal(message)) => return Err(CliError::Fatal(message)),
        Err(RuntimeError::WorkerPanicked | RuntimeError::Closed) => {
            return Err(CliError::WorkerStopped);
        }
        Err(RuntimeError::Persistence(error)) => return Err(error.into()),
        Err(RuntimeError::Service(error)) => return Err(service_error(error)),
    }

    tracing::info!("server stopped");
    println!("👋 服务器已停止");
    Ok(())
}

fn service_error(error: ServiceError) -> CliError {
    match error {
        ServiceError::Persistence(error) => CliError::Persistence(error),
        ServiceError::Rejected(reason) => {
            CliError::Fatal(format!("unexpected rejection outside session: {reason}"))
        }
    }
}

/// Initializes the tracing subscriber.
fn init_logging(level: &str) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

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
        let config = CliConfig::parse_from(["matching-engine"]);
        assert_eq!(config.port, 8080);
        assert_eq!(config.queue_capacity, 2048);
        assert_eq!(config.metrics_port, 9090);
        assert_eq!(config.max_connections, 1024);
        assert_eq!(config.idle_timeout_secs, 300);
        assert_eq!(config.commit_batch_size, 256);
        assert_eq!(config.dedup_cache_size, 262_144);
        assert_eq!(config.max_in_flight, 1);
        assert_eq!(config.snapshot_interval, 0);
        assert!(!config.async_fsync);
        assert!(config.wal_sync);
        assert_eq!(config.log_level, "info");
        assert!(!config.dry_run);
        assert_eq!(config.matching_core, None);
        assert!(!config.busy_poll);
    }

    #[test]
    fn test_cli_config_custom() {
        let config = CliConfig::parse_from([
            "matching-engine",
            "--host",
            "0.0.0.0",
            "--port",
            "9000",
            "--queue-capacity",
            "4096",
            "--log-level",
            "debug",
            "--dry-run",
            "--symbol",
            "ETHUSDT",
            "--wal-sync",
            "false",
        ]);

        assert_eq!(config.host.to_string(), "0.0.0.0");
        assert_eq!(config.port, 9000);
        assert_eq!(config.queue_capacity, 4096);
        assert_eq!(config.log_level, "debug");
        assert!(config.dry_run);
        assert_eq!(config.symbol, "ETHUSDT");
        assert!(!config.wal_sync);
    }

    #[test]
    fn test_cli_config_short_flags() {
        let config = CliConfig::parse_from([
            "matching-engine",
            "-H",
            "192.168.1.1",
            "-p",
            "7000",
            "-q",
            "512",
            "-l",
            "warn",
        ]);

        assert_eq!(config.host.to_string(), "192.168.1.1");
        assert_eq!(config.port, 7000);
        assert_eq!(config.queue_capacity, 512);
        assert_eq!(config.log_level, "warn");
    }

    #[test]
    fn test_invalid_config_is_rejected() {
        let cases = [
            vec!["matching-engine", "--tick-size", "0"],
            vec![
                "matching-engine",
                "--min-price",
                "70000",
                "--max-price",
                "40000",
            ],
            vec!["matching-engine", "--queue-capacity", "0"],
            vec!["matching-engine", "--max-connections", "0"],
            vec!["matching-engine", "--max-in-flight", "0"],
            vec![
                "matching-engine",
                "--min-price",
                "40000",
                "--max-price",
                "40002",
                "--tick-size",
                "5",
            ],
            vec![
                "matching-engine",
                "--min-price",
                "0",
                "--max-price",
                "1000000",
            ],
            vec![
                "matching-engine",
                "--async-fsync",
                "--snapshot-interval",
                "0",
                "--wal-sync",
                "false",
            ],
        ];
        for args in cases {
            let config = CliConfig::parse_from(args.clone());
            assert!(
                validate_config(&config).is_err(),
                "expected invalid config for {args:?}"
            );
        }

        let config = CliConfig::parse_from(["matching-engine"]);
        assert!(validate_config(&config).is_ok());

        let async_config = CliConfig::parse_from([
            "matching-engine",
            "--async-fsync",
            "--snapshot-interval",
            "0",
        ]);
        assert!(validate_config(&async_config).is_ok());
    }
}
