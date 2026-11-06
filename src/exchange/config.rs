//! Exchange runtime configuration.
//!
//! Central defaults for gateway replay, repair retention, idle expiry, and
//! market-data multicast. Transport-specific objects are still constructed by
//! the caller; this struct only validates and carries values.

use crate::exchange::market_data::MarketDataPublisher;
use std::io;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeConfig {
    pub private_replay_capacity: usize,
    pub repair_buffer_capacity: usize,
    pub idle_timeout: Option<Duration>,
    pub multicast_group: Option<String>,
    pub multicast_port: Option<u16>,
}

impl Default for ExchangeConfig {
    fn default() -> Self {
        Self {
            private_replay_capacity: 1024,
            repair_buffer_capacity: 100_000,
            idle_timeout: Some(Duration::from_secs(30)),
            multicast_group: None,
            multicast_port: None,
        }
    }
}

impl ExchangeConfig {
    /// Builds a multicast publisher when both multicast fields are configured.
    ///
    /// # Errors
    /// Returns an I/O error for invalid config or socket bind failure.
    pub fn multicast_publisher(&self, local: &str) -> io::Result<Option<MarketDataPublisher>> {
        self.validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        match (&self.multicast_group, self.multicast_port) {
            (Some(group), Some(port)) => Ok(Some(MarketDataPublisher::bind_multicast(
                local, group, port,
            )?)),
            _ => Ok(None),
        }
    }

    /// Validates cross-field constraints.
    ///
    /// # Errors
    /// Returns a static reason when a field is invalid or incomplete.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.private_replay_capacity == 0 {
            return Err("private_replay_capacity must be positive");
        }
        if self.repair_buffer_capacity == 0 {
            return Err("repair_buffer_capacity must be positive");
        }
        match (&self.multicast_group, self.multicast_port) {
            (Some(group), Some(_)) => {
                if group.parse::<std::net::Ipv4Addr>().is_err() {
                    return Err("multicast_group must be an IPv4 address");
                }
            }
            (None, None) => {}
            _ => return Err("multicast_group and multicast_port must be set together"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        assert!(ExchangeConfig::default().validate().is_ok());
    }

    #[test]
    fn incomplete_multicast_is_rejected() {
        let config = ExchangeConfig {
            multicast_group: Some("239.1.1.1".to_string()),
            ..ExchangeConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let config = ExchangeConfig {
            private_replay_capacity: 0,
            ..ExchangeConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn multicast_publisher_is_built_only_when_configured() {
        assert!(
            ExchangeConfig::default()
                .multicast_publisher("127.0.0.1:0")
                .unwrap()
                .is_none()
        );
        let config = ExchangeConfig {
            multicast_group: Some("239.1.1.1".to_string()),
            multicast_port: Some(5000),
            ..ExchangeConfig::default()
        };
        assert!(config.multicast_publisher("127.0.0.1:0").unwrap().is_some());
    }

    #[test]
    fn complete_multicast_is_valid() {
        let config = ExchangeConfig {
            multicast_group: Some("239.1.1.1".to_string()),
            multicast_port: Some(5000),
            ..ExchangeConfig::default()
        };
        assert!(config.validate().is_ok());
    }
}
