//! Build a validated production exchange configuration and multicast publisher.

use matching_engine::exchange::config::ExchangeConfig;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExchangeConfig {
        private_replay_capacity: 4096,
        repair_buffer_capacity: 1_000_000,
        idle_timeout: Some(Duration::from_secs(15)),
        multicast_group: Some("239.10.20.30".to_string()),
        multicast_port: Some(40_000),
        multicast_interface: Some("127.0.0.1".to_string()),
    };
    config.validate()?;
    let publisher = config.multicast_publisher("0.0.0.0:0")?;
    println!(
        "exchange config valid: replay={} repair={} idle={:?} multicast={}",
        config.private_replay_capacity,
        config.repair_buffer_capacity,
        config.idle_timeout,
        publisher.is_some(),
    );
    Ok(())
}
