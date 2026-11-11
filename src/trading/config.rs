//! Trading runtime configuration.
//!
//! Central defaults for gateway replay, idle expiry, and market-data multicast.
//! Transport-specific objects are still constructed by the caller; this struct
//! only validates and carries values.

use crate::domain::orderbook::ContractSpec;
use crate::trading::core::combo_pair::{ComboPair, ComboPairError};
use crate::trading::core::registry::{InstrumentRegistry, RegistryError};
use crate::trading::market_data::MarketDataPublisher;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradingConfig {
    pub private_replay_capacity: usize,
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

/// One ordinary Contract declared by the static combo venue configuration.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboInstrumentConfig {
    /// Wire instrument id used by ordinary commands and Combo Pair legs.
    pub instrument_id: u32,
    /// Contract symbol interned for public and private events.
    pub symbol: String,
    /// Minimum price increment.
    pub tick_size: u64,
    /// Lowest representable price.
    pub min_price: u64,
    /// Highest representable price.
    pub max_price: u64,
}

/// Prior settlement price used to freeze leg margin at Combo Order entry.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboSettlementConfig {
    /// Ordinary Contract whose prior settlement is configured.
    pub instrument_id: u32,
    /// Prior settlement in price ticks.
    pub price_ticks: u64,
}

/// One account's explicit available margin balance.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboMarginBalance {
    /// Account the balance belongs to.
    pub user_id: u64,
    /// Available margin in leg notional units.
    pub balance: u64,
}

/// Static combo venue configuration loaded before the single-process runtime
/// starts matching.
///
/// The configuration is fail-closed: a venue that declares at least one Combo
/// Pair must also declare explicit account margin balances, and every
/// unconfigured account is rejected at Combo Order entry instead of being
/// treated as unlimited.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboVenueConfig {
    /// Ordinary Contracts in the venue.
    #[serde(default)]
    pub instruments: Vec<ComboInstrumentConfig>,
    /// Static Combo Pairs over those Contracts.
    #[serde(default)]
    pub pairs: Vec<ComboPair>,
    /// Prior settlements used for the combo entry margin estimate.
    #[serde(default)]
    pub prior_settlements: Vec<ComboSettlementConfig>,
    /// Explicit account margin balances.
    #[serde(default)]
    pub margin_balances: Vec<ComboMarginBalance>,
}

impl ComboVenueConfig {
    /// Parses a JSON configuration.
    ///
    /// # Errors
    /// Returns a JSON or validation error.
    pub fn from_json(json: &str) -> Result<Self, ComboVenueConfigError> {
        let config: Self = serde_json::from_str(json).map_err(ComboVenueConfigError::Json)?;
        config.validate()?;
        Ok(config)
    }

    /// Loads a JSON configuration from `path`.
    ///
    /// # Errors
    /// Returns an I/O, JSON, or validation error.
    pub fn load(path: &Path) -> Result<Self, ComboVenueConfigError> {
        let json = std::fs::read_to_string(path).map_err(ComboVenueConfigError::Io)?;
        Self::from_json(&json)
    }

    /// Validates cross-references and fail-closed margin configuration.
    ///
    /// # Errors
    /// Returns the first inconsistency found.
    pub fn validate(&self) -> Result<(), ComboVenueConfigError> {
        for instrument in &self.instruments {
            let reason = if instrument.tick_size == 0 {
                Some("tick size must be positive")
            } else if instrument.max_price <= instrument.min_price {
                Some("max price must be above min price")
            } else if !(instrument.max_price - instrument.min_price)
                .is_multiple_of(instrument.tick_size)
            {
                Some("price range must be divisible by tick size")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(ComboVenueConfigError::InvalidInstrument {
                    instrument_id: instrument.instrument_id,
                    reason,
                });
            }
        }
        let mut instrument_ids = HashSet::new();
        for instrument in &self.instruments {
            if !instrument_ids.insert(instrument.instrument_id) {
                return Err(ComboVenueConfigError::DuplicateInstrument {
                    instrument_id: instrument.instrument_id,
                });
            }
        }
        let mut pair_ids = HashSet::new();
        for pair in &self.pairs {
            if !pair_ids.insert(pair.pair_id) {
                return Err(ComboVenueConfigError::DuplicatePair {
                    pair_id: pair.pair_id,
                });
            }
        }
        let mut settlement_ids = HashSet::new();
        for settlement in &self.prior_settlements {
            if !settlement_ids.insert(settlement.instrument_id) {
                return Err(ComboVenueConfigError::DuplicatePriorSettlement {
                    instrument_id: settlement.instrument_id,
                });
            }
        }
        let mut balance_users = HashSet::new();
        for balance in &self.margin_balances {
            if !balance_users.insert(balance.user_id) {
                return Err(ComboVenueConfigError::DuplicateMarginBalance {
                    user_id: balance.user_id,
                });
            }
        }
        if !self.pairs.is_empty() && self.margin_balances.is_empty() {
            return Err(ComboVenueConfigError::MissingMarginBalances);
        }
        Ok(())
    }

    /// Builds a registry from this configuration.
    ///
    /// Combo-enabled registries require an explicit balance for every account
    /// that submits a Combo Order. Registrations are all-or-none: an error
    /// leaves no partially configured registry with the caller.
    ///
    /// # Errors
    /// Returns a validation, pair, or registry error.
    pub fn build_registry(&self) -> Result<InstrumentRegistry, ComboVenueConfigError> {
        self.validate()?;
        let mut registry = InstrumentRegistry::new();
        for instrument in &self.instruments {
            let spec = ContractSpec::new(
                &instrument.symbol,
                instrument.tick_size,
                instrument.min_price,
                instrument.max_price,
            );
            registry
                .register(instrument.instrument_id, spec)
                .map_err(ComboVenueConfigError::Registry)?;
        }
        for pair in &self.pairs {
            registry
                .register_pair(*pair)
                .map_err(ComboVenueConfigError::Pair)?;
        }
        for settlement in &self.prior_settlements {
            registry
                .set_prior_settlement(settlement.instrument_id, settlement.price_ticks)
                .map_err(ComboVenueConfigError::Registry)?;
        }
        for balance in &self.margin_balances {
            registry.set_margin_balance(balance.user_id, balance.balance);
        }
        registry.set_require_explicit_margin(!self.pairs.is_empty());
        Ok(registry)
    }
}

/// Why a static combo venue configuration was rejected.
#[derive(Debug)]
#[non_exhaustive]
pub enum ComboVenueConfigError {
    /// The configuration file could not be read.
    Io(io::Error),
    /// The configuration was not valid JSON or had the wrong shape.
    Json(serde_json::Error),
    /// One instrument declares an unusable Contract specification.
    InvalidInstrument {
        /// Instrument whose specification is unusable.
        instrument_id: u32,
        /// Static reason the specification is unusable.
        reason: &'static str,
    },
    /// One instrument id appears twice.
    DuplicateInstrument {
        /// Repeated instrument id.
        instrument_id: u32,
    },
    /// One pair id appears twice.
    DuplicatePair {
        /// Repeated pair id.
        pair_id: u32,
    },
    /// One instrument's prior settlement appears twice.
    DuplicatePriorSettlement {
        /// Repeated instrument id.
        instrument_id: u32,
    },
    /// One account's margin balance appears twice.
    DuplicateMarginBalance {
        /// Repeated account id.
        user_id: u64,
    },
    /// Combo Pairs were declared without any explicit account margin balance.
    MissingMarginBalances,
    /// A pair failed static validation when registered.
    Pair(ComboPairError),
    /// A registry registration failed.
    Registry(RegistryError),
}

impl Display for ComboVenueConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
            Self::InvalidInstrument {
                instrument_id,
                reason,
            } => write!(
                formatter,
                "combo config instrument {instrument_id} is invalid: {reason}"
            ),
            Self::DuplicateInstrument { instrument_id } => {
                write!(formatter, "combo config repeats instrument {instrument_id}")
            }
            Self::DuplicatePair { pair_id } => {
                write!(formatter, "combo config repeats pair {pair_id}")
            }
            Self::DuplicatePriorSettlement { instrument_id } => write!(
                formatter,
                "combo config repeats prior settlement for instrument {instrument_id}"
            ),
            Self::DuplicateMarginBalance { user_id } => {
                write!(
                    formatter,
                    "combo config repeats margin balance for user {user_id}"
                )
            }
            Self::MissingMarginBalances => {
                formatter.write_str("combo config defines pairs without explicit margin balances")
            }
            Self::Pair(error) => Display::fmt(error, formatter),
            Self::Registry(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ComboVenueConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Pair(error) => Some(error),
            Self::Registry(error) => Some(error),
            _ => None,
        }
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
