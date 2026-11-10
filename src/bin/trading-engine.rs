//! Single-process trading runtime entry point.
//!
//! This is the executable form of gateway + middleware + core for a local
//! deployment or smoke test. Split-process deployment is described in
//! `deploy/README.md`.

use clap::{Parser, Subcommand};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::TradingCore;
use trading_system_engine::trading::gateway_directory::{GatewayDirectory, HealthMonitor};
use trading_system_engine::trading::gateway_runtime::GatewayRuntime;
use trading_system_engine::trading::gateway_server::start as start_gateway;
use trading_system_engine::trading::market_data::MarketDataPublisher;
use trading_system_engine::trading::metrics::TradingMetrics;
use trading_system_engine::trading::metrics_server;
use trading_system_engine::trading::middleware::MiddlewareSequencer;
use trading_system_engine::trading::plugins::NoPlugins;
use trading_system_engine::trading::runner::CoreRunner;
use trading_system_engine::trading::runtime::{RuntimeOptions, TradingRuntime};
use trading_system_engine::trading::transport::create;

#[derive(Parser)]
#[command(name = "trading", version, about = "Trading runtime")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run gateway + middleware + core in one process.
    Run(RunArgs),
}

#[derive(Parser)]
struct RunArgs {
    /// Gateway TCP listen address.
    #[arg(long, default_value = "127.0.0.1:8000")]
    listen: String,
    /// Metrics HTTP listen address.
    #[arg(long, default_value = "127.0.0.1:9100")]
    metrics_listen: String,
    /// Public multicast group (requires --multicast-port).
    #[arg(long)]
    multicast_group: Option<String>,
    /// Public multicast port.
    #[arg(long)]
    multicast_port: Option<u16>,
    /// Local multicast interface.
    #[arg(long, default_value = "127.0.0.1")]
    multicast_interface: String,
    /// Run this many ticks then exit; 0 means forever.
    #[arg(long, default_value_t = 0)]
    iterations: u64,
    /// Gateway idle timeout in seconds; 0 disables expiry.
    #[arg(long, default_value_t = 30)]
    idle_timeout_secs: u64,
    /// Optional shared login token.
    #[arg(long)]
    auth_token: Option<u64>,
    /// Address clients should reconnect to after session takeover.
    #[arg(long)]
    redirect_target: Option<String>,
    /// Comma-separated gateway endpoints; the first healthy one is used when
    /// --redirect-target is absent.
    #[arg(long)]
    redirect_endpoints: Option<String>,
    /// Append accepted commands to this replay log segment.
    #[arg(long)]
    command_log: Option<String>,
    /// Persist public events to this result log.
    #[arg(long)]
    public_log: Option<String>,
    /// Persist private events to this result log.
    #[arg(long)]
    private_log: Option<String>,
    /// Optional full runtime snapshot to restore before serving.
    #[arg(long)]
    snapshot: Option<String>,
    /// Rotate accepted commands into this directory.
    #[arg(long)]
    command_log_dir: Option<String>,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    command_log_max_bytes: u64,
    #[arg(long, default_value_t = 4)]
    command_log_retain: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run(args),
    }
}

fn run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    let pid = std::process::id();
    let core_path = std::env::temp_dir().join(format!("trading-command-{pid}.ring"));
    let combo_path = std::env::temp_dir().join(format!("trading-combo-{pid}.ring"));
    for path in [&core_path, &combo_path] {
        let _ = std::fs::remove_file(path);
    }
    let (cmd_producer, cmd_consumer) = create(&core_path, 1 << 16)?;
    let (combo_producer, _combo_consumer) = create(&combo_path, 1 << 12)?;

    let mut middleware =
        MiddlewareSequencer::new(cmd_producer).with_combo_transport(combo_producer);
    if let Some(path) = &args.command_log {
        middleware = middleware.with_command_log(std::path::Path::new(path))?;
    }
    if let Some(directory) = &args.command_log_dir {
        middleware = middleware.with_rotating_command_log(
            std::path::Path::new(directory),
            "commands",
            args.command_log_max_bytes,
            args.command_log_retain,
        )?;
    }
    let (gateway, gateway_events) = start_gateway(&args.listen)?;
    let gateway_addr = gateway.addr();
    let mut gateway_runtime =
        GatewayRuntime::with_replay_capacity(middleware, gateway_events, 4096);
    if let Some(token) = args.auth_token {
        gateway_runtime = gateway_runtime.with_auth_token(token);
    }
    let health_directory = args.redirect_endpoints.as_deref().and_then(|endpoints| {
        GatewayDirectory::new(endpoints.split(',').map(str::to_string))
            .ok()
            .map(Arc::new)
    });
    let mut health_monitor = health_directory.as_ref().map(|directory| {
        HealthMonitor::start(
            Arc::clone(directory),
            Duration::from_secs(1),
            Duration::from_millis(200),
        )
    });
    let redirect_target = args.redirect_target.clone().or_else(|| {
        health_directory.as_ref().and_then(|directory| {
            directory
                .first_healthy_cached(Duration::from_millis(200), Duration::from_secs(5))
                .map(|address| address.to_string())
        })
    });
    if let Some(target) = redirect_target {
        gateway_runtime = gateway_runtime.with_redirect_target(target);
    }

    let spec = ContractSpec::new("BTCUSDT", 1, 1, 1_000_000);
    let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
    let runner = CoreRunner::new(core, cmd_consumer, NoPlugins);

    let metrics = Arc::new(Mutex::new(TradingMetrics::new()));
    let mut options = RuntimeOptions {
        metrics_sink: Some(Arc::clone(&metrics)),
        idle_timeout: (args.idle_timeout_secs > 0)
            .then(|| Duration::from_secs(args.idle_timeout_secs)),
        ..RuntimeOptions::default()
    };
    options.public_log = args
        .public_log
        .as_ref()
        .map(|path| {
            trading_system_engine::trading::result_log::PublicResultLog::create(
                std::path::Path::new(path),
            )
        })
        .transpose()?;
    options.private_log = args
        .private_log
        .as_ref()
        .map(|path| {
            trading_system_engine::trading::result_log::PrivateResultLog::create(
                std::path::Path::new(path),
            )
        })
        .transpose()?;
    if let (Some(group), Some(port)) = (&args.multicast_group, args.multicast_port) {
        options.market_data = Some(MarketDataPublisher::bind_multicast_on(
            "0.0.0.0:0",
            group,
            port,
            args.multicast_interface.parse()?,
        )?);
    }
    let mut runtime = TradingRuntime::new(gateway_runtime, runner, gateway).with_options(options);
    if let Some(path) = &args.snapshot {
        let snapshot = trading_system_engine::trading::runtime::TradingSnapshot::load(
            std::path::Path::new(path),
        )?;
        runtime.restore_snapshot(snapshot)?;
    }

    let mut metrics_server = metrics_server::start(&args.metrics_listen, metrics)?;
    {
        use std::io::Write;
        println!(
            "gateway={} metrics={} multicast={}",
            gateway_addr,
            metrics_server.addr(),
            args.multicast_group.is_some()
        );
        let _ = std::io::stdout().flush();
    }

    let mut ticks = 0u64;
    loop {
        runtime.tick();
        ticks += 1;
        if args.iterations > 0 && ticks >= args.iterations {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    metrics_server.stop();
    if let Some(monitor) = &mut health_monitor {
        monitor.stop();
    }
    for path in [&core_path, &combo_path] {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
