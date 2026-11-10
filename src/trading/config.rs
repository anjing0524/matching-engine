//! Trading runtime configuration.
//!
//! Central defaults for gateway replay, repair retention, idle expiry, and
//! market-data multicast. Transport-specific objects are still constructed by
//! the caller; this struct only validates and carries values.

use crate::trading::market_data::MarketDataPublisher;
use std::io;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradingConfig {
    pub private_replay_capacity: usize,
    pub repair_buffer_capacity: usize,
    pub idle_timeout: Option<Duration>,
    pub multicast_group: Option<String>,
    pub multicast_port: Option<u16>,
    pub multicast_interface: Option<String>,
    /// Additional interfaces tried in order if the primary cannot be used.
    pub multicast_interfaces: Vec<String>,
}

impl Default for TradingConfig {
    fn default() -> Self {
        Self {
            private_replay_capacity: 1024,
            repair_buffer_capacity: 100_000,
            idle_timeout: Some(Duration::from_secs(30)),
            multicast_group: None,
            multicast_port: None,
            multicast_interface: None,
            multicast_interfaces: Vec::new(),
        }
    }
}

impl TradingConfig {
    /// Builds a multicast publisher when both multicast fields are configured.
    ///
    /// # Errors
    /// Returns an I/O error for invalid config or socket bind failure.
    pub fn multicast_publisher(&self, local: &str) -> io::Result<Option<MarketDataPublisher>> {
        self.validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let (Some(group), Some(port)) = (&self.multicast_group, self.multicast_port) else {
            return Ok(None);
        };
        let mut candidates = Vec::new();
        if let Some(interface) = &self.multicast_interface {
            candidates.push(interface.clone());
        }
        candidates.extend(self.multicast_interfaces.iter().cloned());
        if candidates.is_empty() {
            candidates.push("127.0.0.1".to_string());
        }
        let mut last_error = None;
        for interface in candidates {
            let interface = interface
                .parse::<std::net::Ipv4Addr>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            match MarketDataPublisher::bind_multicast_on(local, group, port, interface) {
                Ok(publisher) => return Ok(Some(publisher)),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "no multicast interface")
        }))
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
        if let Some(interface) = &self.multicast_interface
            && interface.parse::<std::net::Ipv4Addr>().is_err()
        {
            return Err("multicast_interface must be an IPv4 address");
        }
        if self
            .multicast_interfaces
            .iter()
            .any(|interface| interface.parse::<std::net::Ipv4Addr>().is_err())
        {
            return Err("multicast_interfaces must contain IPv4 addresses");
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
        assert!(TradingConfig::default().validate().is_ok());
    }

    #[test]
    fn incomplete_multicast_is_rejected() {
        let config = TradingConfig {
            multicast_group: Some("239.1.1.1".to_string()),
            ..TradingConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn invalid_multicast_interface_list_is_rejected() {
        let config = TradingConfig {
            multicast_interfaces: vec!["not-an-ip".to_string()],
            ..TradingConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn invalid_multicast_interface_is_rejected() {
        let config = TradingConfig {
            multicast_interface: Some("not-an-ip".to_string()),
            ..TradingConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let config = TradingConfig {
            private_replay_capacity: 0,
            ..TradingConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn multicast_publisher_is_built_only_when_configured() {
        assert!(
            TradingConfig::default()
                .multicast_publisher("127.0.0.1:0")
                .unwrap()
                .is_none()
        );
        let config = TradingConfig {
            multicast_group: Some("239.1.1.1".to_string()),
            multicast_port: Some(5000),
            ..TradingConfig::default()
        };
        assert!(config.multicast_publisher("127.0.0.1:0").unwrap().is_some());
    }

    #[test]
    fn multicast_publisher_uses_fallback_interfaces() {
        let config = TradingConfig {
            multicast_group: Some("239.1.1.1".to_string()),
            multicast_port: Some(5000),
            multicast_interface: Some("203.0.113.1".to_string()),
            multicast_interfaces: vec!["127.0.0.1".to_string()],
            ..TradingConfig::default()
        };
        assert!(config.multicast_publisher("0.0.0.0:0").unwrap().is_some());
    }

    #[test]
    fn complete_multicast_is_valid() {
        let config = TradingConfig {
            multicast_group: Some("239.1.1.1".to_string()),
            multicast_port: Some(5000),
            ..TradingConfig::default()
        };
        assert!(config.validate().is_ok());
    }
}
