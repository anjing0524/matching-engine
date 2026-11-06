//! Split-process exchange core: consumes commands from a shared ring and
//! publishes public/private events to shared rings.

use clap::Parser;
use matching_engine::domain::orderbook::ContractSpec;
use matching_engine::exchange::core::ExchangeCore;
use matching_engine::exchange::plugins::NoPlugins;
use matching_engine::exchange::runner::CoreRunner;
use matching_engine::exchange::transport::{
    open_command_consumer, open_private_event_producer, open_public_event_producer,
};
use matching_engine::shared::SymbolId;
use std::path::Path;
use std::time::Duration;

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
    #[arg(long, default_value_t = 0)]
    iterations: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let consumer = open_command_consumer(Path::new(&args.command_ring))?;
    let public = open_public_event_producer(Path::new(&args.public_ring))?;
    let private = open_private_event_producer(Path::new(&args.private_ring))?;
    let spec = ContractSpec::new(&args.instrument, 1, 1, 1_000_000);
    let core = ExchangeCore::new(spec, SymbolId::intern(&args.instrument));
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
