//! Split-process middleware + gateway: creates shared rings for the core,
//! sequences gateway commands, and routes core events back to gateways/public.

use clap::Parser;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use trading_system_engine::trading::gateway::runtime::GatewayRuntime;
use trading_system_engine::trading::gateway::server::start as start_gateway;
use trading_system_engine::trading::market_data::MarketDataPublisher;
use trading_system_engine::trading::transport::middleware::MiddlewareSequencer;
use trading_system_engine::trading::transport::{
    create, create_private_event_transport, create_public_event_transport,
    open_private_event_consumer, open_public_event_consumer,
};

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
    /// Print loop counters to stderr every N milliseconds; 0 disables.
    #[arg(long, default_value_t = 0)]
    stats_interval_ms: u64,
    /// Persist public events to this result log.
    #[arg(long)]
    public_log: Option<String>,
    /// Persist private events to this result log.
    #[arg(long)]
    private_log: Option<String>,
    /// Rotate public events into this directory.
    #[arg(long)]
    public_log_dir: Option<String>,
    /// Rotate private events into this directory.
    #[arg(long)]
    private_log_dir: Option<String>,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    result_log_max_bytes: u64,
    #[arg(long, default_value_t = 4)]
    result_log_retain: usize,
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
    /// Rotate accepted commands into this directory.
    #[arg(long)]
    command_log_dir: Option<String>,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    command_log_max_bytes: u64,
    #[arg(long, default_value_t = 4)]
    command_log_retain: usize,
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
    let mut public_log = args
        .public_log
        .as_deref()
        .map(|path| {
            trading_system_engine::trading::transport::result_log::PublicResultLog::create(
                Path::new(path),
            )
        })
        .transpose()?;
    let mut private_log = args
        .private_log
        .as_deref()
        .map(|path| {
            trading_system_engine::trading::transport::result_log::PrivateResultLog::create(
                Path::new(path),
            )
        })
        .transpose()?;
    let mut rotating_public_log = args
        .public_log_dir
        .as_deref()
        .map(|dir| {
            trading_system_engine::trading::transport::result_log::RotatingPublicResultLog::open(
                Path::new(dir),
                "public",
                args.result_log_max_bytes,
                args.result_log_retain,
            )
        })
        .transpose()?;
    let mut rotating_private_log = args
        .private_log_dir
        .as_deref()
        .map(|dir| {
            trading_system_engine::trading::transport::result_log::RotatingPrivateResultLog::open(
                Path::new(dir),
                "private",
                args.result_log_max_bytes,
                args.result_log_retain,
            )
        })
        .transpose()?;

    let mut middleware = MiddlewareSequencer::new(command_producer);
    if let Some(path) = &args.command_log {
        middleware = middleware.with_command_log(Path::new(path))?;
    }
    if let Some(directory) = &args.command_log_dir {
        middleware = middleware.with_rotating_command_log(
            Path::new(directory),
            "commands",
            args.command_log_max_bytes,
            args.command_log_retain,
        )?;
    }
    let (server, events) = start_gateway(&args.listen)?;
    if args.stats_interval_ms > 0 {
        server.enable_timing();
    }
    let mut gateway = GatewayRuntime::with_replay_capacity(middleware, events, 4096);
    if let Some(token) = args.auth_token {
        gateway = gateway.with_auth_token(token);
    }
    let health_directory = args.redirect_endpoints.as_deref().and_then(|endpoints| {
        trading_system_engine::trading::gateway::directory::GatewayDirectory::new(
            endpoints.split(',').map(str::to_string),
        )
        .ok()
        .map(Arc::new)
    });
    let mut health_monitor = health_directory.as_ref().map(|directory| {
        trading_system_engine::trading::gateway::directory::HealthMonitor::start(
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
        gateway = gateway.with_redirect_target(target);
    }
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
    let mut gateway_events = 0u64;
    let mut private_events = 0u64;
    let mut public_events = 0u64;
    let mut stats_start = std::time::Instant::now();
    loop {
        gateway_events += gateway.poll() as u64;
        for (connection_id, replay) in gateway.take_replay_batches() {
            for event in replay {
                if let trading_system_engine::trading::gateway::private_replay::ReplayEvent::Core(
                    event,
                ) = event
                {
                    let _ = server.send(connection_id, GatewayFrame::PrivateEvent(event.clone()));
                }
            }
        }
        let private: Vec<_> = std::iter::from_fn(|| private_consumer.recv())
            .filter_map(Result::ok)
            .collect();
        private_events += private.len() as u64;
        if let Some(log) = &mut private_log {
            for event in &private {
                log.append(event)?;
            }
            log.sync()?;
        }
        if let Some(log) = &mut rotating_private_log {
            for event in &private {
                log.append(event)?;
            }
        }
        let _ = gateway.dispatch_private(&server, private);
        while let Some(Ok(event)) = public_consumer.recv() {
            public_events += 1;
            if let Some(log) = &mut public_log {
                log.append(&event)?;
            }
            if let Some(log) = &mut rotating_public_log {
                log.append(&event)?;
            }
            if let Some(publisher) = &publisher {
                let _ = publisher.send(&event);
            }
        }
        if let Some(log) = &mut public_log {
            log.sync()?;
        }
        if args.idle_timeout_secs > 0 {
            for connection_id in gateway.expire_idle(Duration::from_secs(args.idle_timeout_secs)) {
                server.close(connection_id);
            }
        }
        ticks += 1;
        if args.stats_interval_ms > 0
            && stats_start.elapsed() >= Duration::from_millis(args.stats_interval_ms)
        {
            let submitted = gateway.middleware_mut().submitted();
            let gateway_stats = server.stats();
            let unrouted = gateway.private_unrouted();
            let send_errors = gateway.private_send_errors();
            eprintln!(
                "middleware_stats ticks={ticks} gateway_events={gateway_events} submitted={submitted} private={private_events} public={public_events} accepted={} frames_read={} writes={} bytes={} write_errors={} unrouted={unrouted} send_errors={send_errors}",
                gateway_stats.accepted,
                gateway_stats.frames_read,
                gateway_stats.writes,
                gateway_stats.bytes_written,
                gateway_stats.write_errors
            );
            eprintln!(
                "gateway_timing read_calls={} read_ns={} write_ns={}",
                gateway_stats.read_calls, gateway_stats.read_ns, gateway_stats.write_ns
            );
            stats_start = std::time::Instant::now();
        }
        if args.iterations > 0 && ticks >= args.iterations {
            break;
        }
        std::hint::spin_loop();
    }
    if let Some(monitor) = &mut health_monitor {
        monitor.stop();
    }
    Ok(())
}
