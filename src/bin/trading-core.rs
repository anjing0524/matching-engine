//! Split-process trading core: consumes commands from a shared ring and
//! publishes public/private events to shared rings.

use clap::Parser;
use std::path::Path;
use std::time::Duration;
use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::shared::SymbolId;
use trading_system_engine::trading::core::TradingCore;
use trading_system_engine::trading::plugins::NoPlugins;
use trading_system_engine::trading::runner::CoreRunner;
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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let consumer = open_command_consumer(Path::new(&args.command_ring))?;
    let public = open_public_event_producer(Path::new(&args.public_ring))?;
    let private = open_private_event_producer(Path::new(&args.private_ring))?;
    let core = if let Some(snapshot_path) = &args.snapshot {
        let snapshot = TradingCore::load_snapshot(Path::new(snapshot_path))?;
        let mut core = TradingCore::new(snapshot.spec.clone(), snapshot.symbol);
        core.restore_snapshot(snapshot)?;
        core
    } else {
        let spec = ContractSpec::new(&args.instrument, 1, 1, 1_000_000);
        TradingCore::new(spec, SymbolId::intern(&args.instrument))
    };
    let mut runner =
        CoreRunner::new(core, consumer, NoPlugins).with_event_transports(public, private);

    let mut ticks = 0u64;
    loop {
        runner.poll();
        ticks += 1;
        if args.iterations > 0 && ticks >= args.iterations {
            break;
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    Ok(())
}
