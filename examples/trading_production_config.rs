//! Build a validated production trading configuration and multicast publisher.

use std::time::Duration;
use trading_system_engine::trading::config::TradingConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = TradingConfig {
        private_replay_capacity: 4096,
        repair_buffer_capacity: 1_000_000,
        idle_timeout: Some(Duration::from_secs(15)),
        multicast_group: Some("239.10.20.30".to_string()),
        multicast_port: Some(40_000),
        multicast_interface: Some("127.0.0.1".to_string()),
        multicast_interfaces: vec!["127.0.0.1".to_string()],
    };
    config.validate()?;
    let publisher = config.multicast_publisher("0.0.0.0:0")?;
    println!(
        "trading config valid: replay={} repair={} idle={:?} multicast={}",
        config.private_replay_capacity,
        config.repair_buffer_capacity,
        config.idle_timeout,
        publisher.is_some(),
    );
    Ok(())
}
