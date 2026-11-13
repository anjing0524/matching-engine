//! Split-process trading core: consumes commands from a shared ring and
//! publishes public/private events to shared rings.

use clap::Parser;
use std::path::Path;
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::TradingCore;
use trading_system_engine::trading::core::plugins::NoPlugins;
use trading_system_engine::trading::transport::runner::{CoreRunner, VenueRunner};
use trading_system_engine::trading::transport::{
    open_command_consumer, open_private_event_producer, open_public_event_producer,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    command_ring: String,
    #[arg(long)]
    public_ring: String,
    #[arg(long)]
    private_ring: String,
    #[arg(long, default_value = "BTCUSDT")]
    instrument: String,
    /// Optional versioned core snapshot to restore before consuming commands.
    #[arg(long)]
    snapshot: Option<String>,
    #[arg(long, default_value_t = 0)]
    iterations: u64,
    /// Print loop counters to stderr every N milliseconds; 0 disables.
    #[arg(long, default_value_t = 0)]
    stats_interval_ms: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let consumer = open_command_consumer(Path::new(&args.command_ring))?;
    let public = open_public_event_producer(Path::new(&args.public_ring))?;
    let private = open_private_event_producer(Path::new(&args.private_ring))?;
    let core = if let Some(snapshot_path) = &args.snapshot {
        let snapshot: trading_system_engine::trading::core::kernel::CoreSnapshot =
            trading_system_engine::trading::snapshot::snapshot_file::load(
                Path::new(snapshot_path),
                trading_system_engine::trading::snapshot::CORE_SNAPSHOT_VERSION,
            )?;
        let mut core = TradingCore::new(snapshot.spec.clone(), snapshot.symbol);
        core.restore_snapshot(snapshot)?;
        core
    } else {
        let spec = ContractSpec::new(&args.instrument, 1, 1, 1_000_000);
        TradingCore::new(spec, SymbolId::intern(&args.instrument))
    };
    let mut runner = VenueRunner::core(
        CoreRunner::new(core, consumer, NoPlugins).with_event_transports(public, private),
    );

    let mut ticks = 0u64;
    let mut commands = 0u64;
    let mut stats_start = std::time::Instant::now();
    loop {
        commands += runner.poll_events().commands as u64;
        ticks += 1;
        if args.stats_interval_ms > 0
            && stats_start.elapsed() >= std::time::Duration::from_millis(args.stats_interval_ms)
        {
            eprintln!("core_stats ticks={ticks} commands={commands}");
            stats_start = std::time::Instant::now();
        }
        if args.iterations > 0 && ticks >= args.iterations {
            break;
        }
        std::hint::spin_loop();
    }
    Ok(())
}
