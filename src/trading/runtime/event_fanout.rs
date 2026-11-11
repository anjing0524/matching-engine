//! Output fan-out for public/private projections.
//!
//! One module owns result logs, repair buffers, the market-data publisher, and
//! their counters. The runtime only decides when to publish.

use crate::trading::core::private_events::PrivateEvent;
use crate::trading::core::public_events::PublicEvent;
use crate::trading::market_data::MarketDataPublisher;
#[cfg(test)]
use crate::trading::market_data::repair::NackRequest;
use crate::trading::market_data::repair::{RepairBuffer, RepairServer};
use crate::trading::transport::result_log::{PrivateResultLog, PublicResultLog};

/// Sink set for one runtime's projected events.
#[derive(Default)]
pub struct EventFanout {
    public_log: Option<PublicResultLog>,
    private_log: Option<PrivateResultLog>,
    repair_buffer: Option<RepairBuffer>,
    repair_server: Option<RepairServer>,
    market_data: Option<MarketDataPublisher>,
    public_sent: u64,
    repair_sent: u64,
    sink_errors: u64,
}

/// Optional sinks composed by [`EventFanout`].
#[derive(Default)]
pub struct EventFanoutOptions {
    pub public_log: Option<PublicResultLog>,
    pub private_log: Option<PrivateResultLog>,
    pub repair_buffer_capacity: Option<usize>,
    pub repair_server: Option<RepairServer>,
    pub market_data: Option<MarketDataPublisher>,
}

impl EventFanout {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_options(mut self, options: EventFanoutOptions) -> Self {
        self.public_log = options.public_log;
        self.private_log = options.private_log;
        self.repair_buffer = options.repair_buffer_capacity.map(RepairBuffer::new);
        self.repair_server = options.repair_server;
        self.market_data = options.market_data;
        self
    }

    #[must_use]
    pub fn public_sent(&self) -> u64 {
        self.public_sent
    }

    #[must_use]
    pub fn repair_sent(&self) -> u64 {
        self.repair_sent
    }

    #[must_use]
    pub fn sink_errors(&self) -> u64 {
        self.sink_errors
    }

    /// Logs private events and publishes public events through all sinks.
    ///
    /// Returns how many public events were handed to the market-data publisher.
    pub fn emit(&mut self, private: &[PrivateEvent], public: &[PublicEvent]) -> u64 {
        self.log_private(private);
        let before = self.public_sent;
        self.publish_public(public);
        self.public_sent - before
    }

    fn log_private(&mut self, events: &[PrivateEvent]) {
        let Some(log) = &mut self.private_log else {
            return;
        };
        for event in events {
            if log.append(event).is_err() {
                self.sink_errors += 1;
            }
        }
        if log.sync().is_err() {
            self.sink_errors += 1;
        }
    }

    fn publish_public(&mut self, events: &[PublicEvent]) {
        if let Some(log) = &mut self.public_log {
            for event in events {
                if log.append(event).is_err() {
                    self.sink_errors += 1;
                }
            }
            if log.sync().is_err() {
                self.sink_errors += 1;
            }
        }
        if let Some(buffer) = &mut self.repair_buffer {
            for event in events {
                buffer.push(*event);
            }
        }
        if let Some(server) = &mut self.repair_server {
            for event in events {
                server.push(*event);
            }
        }
        if let Some(publisher) = &self.market_data {
            for event in events {
                match publisher.send(event) {
                    Ok(()) => self.public_sent += 1,
                    Err(_) => self.sink_errors += 1,
                }
            }
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn repair(&self, request: NackRequest) -> Vec<PublicEvent> {
        self.repair_buffer
            .as_ref()
            .map_or_else(Vec::new, |buffer| buffer.repair(request))
    }

    pub fn poll_repair(&mut self) -> u64 {
        let Some(server) = &mut self.repair_server else {
            return 0;
        };
        match server.poll_once() {
            Ok(repaired) => {
                let repaired = repaired as u64;
                self.repair_sent += repaired;
                repaired
            }
            Err(_) => {
                self.sink_errors += 1;
                0
            }
        }
    }
}
