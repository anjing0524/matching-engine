//! Split-process middleware + gateway: creates shared rings for the core,
//! sequences gateway commands, and routes core events back to gateways/public.

use clap::Parser;
use matching_engine::exchange::gateway_runtime::GatewayRuntime;
use matching_engine::exchange::gateway_server::start as start_gateway;
use matching_engine::exchange::market_data::MarketDataPublisher;
use matching_engine::exchange::middleware::MiddlewareSequencer;
use matching_engine::exchange::transport::{
    create, create_private_event_transport, create_public_event_transport,
    open_private_event_consumer, open_public_event_consumer,
};
use std::path::Path;
use std::time::Duration;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8000")]
    listen: String,
    #[arg(long)]
    command_ring: String,
    #[arg(long)]
    public_ring: String,
    #[arg(long)]
    private_ring: String,
    #[arg(long)]
    multicast_group: Option<String>,
    #[arg(long)]
    multicast_port: Option<u16>,
    #[arg(long, default_value = "127.0.0.1")]
    multicast_interface: String,
    #[arg(long, default_value_t = 0)]
    iterations: u64,
    #[arg(long, default_value_t = 30)]
    idle_timeout_secs: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let (command_producer, _core_command_consumer) =
        create(Path::new(&args.command_ring), 1 << 16)?;
    let (_core_public_producer, _unused_public_consumer) =
        create_public_event_transport(Path::new(&args.public_ring), 1 << 16)?;
    let (_core_private_producer, _unused_private_consumer) =
        create_private_event_transport(Path::new(&args.private_ring), 1 << 16)?;
    // The core process opens the producer ends; middleware opens consumers.
    let public_consumer = open_public_event_consumer(Path::new(&args.public_ring))?;
    let private_consumer = open_private_event_consumer(Path::new(&args.private_ring))?;

    let middleware = MiddlewareSequencer::new(command_producer);
    let (server, events) = start_gateway(&args.listen)?;
    let mut gateway = GatewayRuntime::with_replay_capacity(middleware, events, 4096);
    let publisher = match (&args.multicast_group, args.multicast_port) {
        (Some(group), Some(port)) => Some(MarketDataPublisher::bind_multicast_on(
            "0.0.0.0:0",
            group,
            port,
            args.multicast_interface.parse()?,
        )?),
        _ => None,
    };

    let mut ticks = 0u64;
    loop {
        gateway.poll();
        for (connection_id, replay) in gateway.take_all_replays() {
            for event in replay {
                let _ = server.send_private(connection_id, &event);
            }
        }
        let private: Vec<_> = std::iter::from_fn(|| private_consumer.recv())
            .filter_map(Result::ok)
            .collect();
        let _ = gateway.dispatch_private(&server, private);
        if let Some(publisher) = &publisher {
            while let Some(Ok(event)) = public_consumer.recv() {
                let _ = publisher.send(&event);
            }
        }
        if args.idle_timeout_secs > 0 {
            for connection_id in gateway.expire_idle(Duration::from_secs(args.idle_timeout_secs)) {
                server.close(connection_id);
            }
        }
        ticks += 1;
        if args.iterations > 0 && ticks >= args.iterations {
            break;
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    Ok(())
}
