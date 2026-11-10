//! Output fan-out for public/private projections.
//!
//! One module owns result logs, repair buffers, the market-data publisher, and
//! their counters. The runtime only decides when to publish.

use crate::trading::market_data::MarketDataPublisher;
use crate::trading::private_events::PrivateEvent;
use crate::trading::public_events::PublicEvent;
use crate::trading::repair::{NackRequest, RepairBuffer, RepairServer};
use crate::trading::result_log::{PrivateResultLog, PublicResultLog};

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

impl EventFanout {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_public_log(mut self, log: PublicResultLog) -> Self {
        self.public_log = Some(log);
        self
    }

    #[must_use]
    pub fn with_private_log(mut self, log: PrivateResultLog) -> Self {
        self.private_log = Some(log);
        self
    }

    #[must_use]
    pub fn with_repair_buffer(mut self, capacity: usize) -> Self {
        self.repair_buffer = Some(RepairBuffer::new(capacity));
        self
    }

    #[must_use]
    pub fn with_repair_server(mut self, server: RepairServer) -> Self {
        self.repair_server = Some(server);
        self
    }

    #[must_use]
    pub fn with_market_data(mut self, publisher: MarketDataPublisher) -> Self {
        self.market_data = Some(publisher);
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

    pub fn log_private(&mut self, events: &[PrivateEvent]) {
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

    pub fn publish_public(&mut self, events: &[PublicEvent]) {
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

    #[must_use]
    pub fn repair(&self, request: NackRequest) -> Vec<PublicEvent> {
        self.repair_buffer
            .as_ref()
            .map_or_else(Vec::new, |buffer| buffer.repair(request))
    }

    /// Switches the multicast publisher's local interface.
    ///
    /// # Errors
    /// Returns an I/O error when no multicast publisher exists or rebinding fails.
    pub fn switch_interface(&mut self, interface: std::net::Ipv4Addr) -> std::io::Result<()> {
        let publisher = self.market_data.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no market-data publisher")
        })?;
        publisher.switch_interface(interface)
    }
}
