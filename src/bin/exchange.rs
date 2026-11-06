//! Single-process exchange runtime entry point.
//!
//! This is the executable form of gateway + middleware + core for a local
//! deployment or smoke test. Split-process deployment is described in
//! `deploy/README.md`.

use clap::{Parser, Subcommand};
use matching_engine::domain::orderbook::ContractSpec;
use matching_engine::exchange::core::ExchangeCore;
use matching_engine::exchange::exchange_runtime::ExchangeRuntime;
use matching_engine::exchange::gateway_runtime::GatewayRuntime;
use matching_engine::exchange::gateway_server::start as start_gateway;
use matching_engine::exchange::market_data::MarketDataPublisher;
use matching_engine::exchange::metrics::ExchangeMetrics;
use matching_engine::exchange::metrics_server;
use matching_engine::exchange::middleware::MiddlewareSequencer;
use matching_engine::exchange::plugins::NoPlugins;
use matching_engine::exchange::runner::CoreRunner;
use matching_engine::exchange::transport::create;
use matching_engine::shared::SymbolId;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "exchange", version, about = "Exchange runtime")]
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
    /// Append accepted commands to this replay log segment.
    #[arg(long)]
    command_log: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run(args),
    }
}

fn run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    let pid = std::process::id();
    let core_path = std::env::temp_dir().join(format!("exchange-command-{pid}.ring"));
    let combo_path = std::env::temp_dir().join(format!("exchange-combo-{pid}.ring"));
    for path in [&core_path, &combo_path] {
        let _ = std::fs::remove_file(path);
    }
    let (cmd_producer, cmd_consumer) = create(&core_path, 1 << 16)?;
    let (combo_producer, _combo_consumer) = create(&combo_path, 1 << 12)?;

    let middleware = MiddlewareSequencer::new(cmd_producer).with_combo_transport(combo_producer);
    let (gateway, gateway_events) = start_gateway(&args.listen)?;
    let gateway_addr = gateway.addr();
    let mut gateway_runtime =
        GatewayRuntime::with_replay_capacity(middleware, gateway_events, 4096);
    if let Some(token) = args.auth_token {
        gateway_runtime = gateway_runtime.with_auth_token(token);
    }

    let spec = ContractSpec::new("BTCUSDT", 1, 1, 1_000_000);
    let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
    let runner = CoreRunner::new(core, cmd_consumer, NoPlugins);

    let metrics = Arc::new(Mutex::new(ExchangeMetrics::new()));
    let mut runtime = ExchangeRuntime::new(gateway_runtime, runner, gateway)
        .with_metrics_sink(Arc::clone(&metrics));
    if args.idle_timeout_secs > 0 {
        runtime = runtime.with_idle_timeout(Duration::from_secs(args.idle_timeout_secs));
    }
    if let (Some(group), Some(port)) = (&args.multicast_group, args.multicast_port) {
        let publisher = MarketDataPublisher::bind_multicast_on(
            "0.0.0.0:0",
            group,
            port,
            args.multicast_interface.parse()?,
        )?;
        runtime = runtime.with_market_data(publisher);
    }

    let mut metrics_server = metrics_server::start(&args.metrics_listen, metrics)?;
    println!(
        "gateway={} metrics={} multicast={}",
        gateway_addr,
        metrics_server.addr(),
        args.multicast_group.is_some()
    );

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
    for path in [&core_path, &combo_path] {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
