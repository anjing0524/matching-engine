//! Standalone member gateway: relays frames to the middleware gateway listener.

use clap::Parser;
use trading_system_engine::trading::gateway::proxy;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:8000")]
    listen: String,
    #[arg(long, default_value = "127.0.0.1:7000")]
    middleware: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let proxy = proxy::start(&args.listen, args.middleware.clone())?;
    println!(
        "gateway listening on {} -> middleware {}",
        proxy.addr(),
        args.middleware
    );
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
