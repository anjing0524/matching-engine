//! Persistent matching service.
//!
//! Wraps [`TickBasedOrderBook`] with a synchronous write-ahead log:
//!
//! 1. Every accepted order and cancellation is appended to the WAL.
//! 2. The WAL is flushed (and fsynced when configured) before success is
//!    reported, so an acknowledgement implies durability.
//! 3. The matching operation is applied in memory and its results are appended.
//!
//! On startup [`PersistentMatchingService::recover`] replays the log, so the
//! in-memory order book always matches the durable state. A torn WAL tail left
//! by a crash is dropped; any other corruption aborts recovery.

use crate::domain::orderbook::{ContractSpec, MatchOutcome, TickBasedOrderBook};
use crate::infrastructure::persistence::{
    Journal, PersistenceConfig, PersistenceError, Snapshot, WalEntry, WalRecord,
};
use crate::shared::metrics::METRICS;
use crate::shared::protocol::{
    CancelOrderRequest, NewOrderRequest, OrderConfirmation, OrderType, TradeNotification,
};
use prometheus::{Counter, Histogram};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tracing::{error, info, warn};

/// Error returned by service operations.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// A persistence operation failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),

    /// The order book rejected the request (for example, a cancel that the
    /// caller is not authorized to perform).
    #[error("order rejected: {0}")]
    Rejected(String),
}

/// Result alias used by [`PersistentMatchingService`].
pub type ServiceResult<T> = Result<T, ServiceError>;

/// Pre-resolved Prometheus handles for this book's symbol.
///
/// Resolving labels per order would hash the label set on every match; building
/// the handles once at startup removes that lookup from the hot path.
struct ServiceMetrics {
    orders_buy: Counter,
    orders_sell: Counter,
    trades: Counter,
    matching_latency_ns: Histogram,
    wal_latency_ns: Histogram,
    commit_latency_ns: Histogram,
    order_size_buy: Histogram,
    order_size_sell: Histogram,
    cancellations: Counter,
}

impl ServiceMetrics {
    fn new(symbol: &str) -> Self {
        Self {
            orders_buy: METRICS.orders_total.with_label_values(&["buy", symbol]),
            orders_sell: METRICS.orders_total.with_label_values(&["sell", symbol]),
            trades: METRICS.trades_total.with_label_values(&[symbol]),
            matching_latency_ns: METRICS.matching_latency_ns.with_label_values(&[symbol]),
            wal_latency_ns: METRICS.wal_latency_ns.with_label_values(&[symbol]),
            commit_latency_ns: METRICS.commit_latency_ns.with_label_values(&[symbol]),
            order_size_buy: METRICS.order_size.with_label_values(&["buy"]),
            order_size_sell: METRICS.order_size.with_label_values(&["sell"]),
            cancellations: METRICS
                .cancellations_total
                .with_label_values(&[symbol, "ok"]),
        }
    }
}

type ClientOrderKey = (u64, u64);

/// Cached result for an idempotent client order.
#[derive(Clone)]
struct CachedOrderResult {
    trades: Vec<TradeNotification>,
    confirmation: Option<OrderConfirmation>,
}

struct DedupEntry {
    /// `None` when the entry was seeded from WAL recovery; a retry then gets a
    /// duplicate rejection instead of a re-execution.
    cached: Option<CachedOrderResult>,
}

/// A single-order-book matching service with synchronous WAL persistence.
pub struct PersistentMatchingService {
    orderbook: TickBasedOrderBook,
    /// Present while the service owns durable I/O. A decoupled runtime moves it
    /// to the journaler thread; the service then only stages records in memory.
    journal: Option<Journal>,
    staged: Vec<WalRecord>,
    staged_next_seq: u64,
    metrics: ServiceMetrics,
    next_trade_id: u64,
    /// Bounded in-memory idempotency table keyed by (user_id, client_order_id).
    dedup_capacity: usize,
    dedup: HashMap<ClientOrderKey, DedupEntry>,
    dedup_order: VecDeque<ClientOrderKey>,
    data_dir: std::path::PathBuf,
    snapshot_every_records: u64,
    last_snapshot_seq: u64,
    /// Held for the service lifetime; prevents a second process from writing the
    /// same WAL. Released when the service is dropped.
    _data_dir_lock: File,
}

/// Takes an exclusive advisory lock on `<data_dir>/.lock`.
fn acquire_data_dir_lock(data_dir: &Path) -> Result<File, PersistenceError> {
    std::fs::create_dir_all(data_dir)?;
    let lock_path = data_dir.join(".lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    file.try_lock().map_err(|error| {
        PersistenceError::Io(std::io::Error::other(format!(
            "failed to lock {}: {error}",
            lock_path.display()
        )))
    })?;
    Ok(file)
}

impl PersistentMatchingService {
    /// Creates a new service backed by `data_dir`.
    pub fn new(spec: ContractSpec, config: PersistenceConfig) -> Result<Self, PersistenceError> {
        let data_dir_lock = acquire_data_dir_lock(&config.data_dir)?;
        let journal = Journal::create(&config)?;

        let orderbook = TickBasedOrderBook::new(spec);
        let metrics = ServiceMetrics::new(orderbook.symbol());

        Ok(Self {
            orderbook,
            journal: Some(journal),
            staged: Vec::new(),
            staged_next_seq: 0,
            metrics,
            next_trade_id: 1,
            dedup_capacity: config.dedup_cache_size,
            dedup: HashMap::new(),
            dedup_order: VecDeque::new(),
            data_dir: config.data_dir,
            snapshot_every_records: config.snapshot_every_records,
            last_snapshot_seq: 0,
            _data_dir_lock: data_dir_lock,
        })
    }

    /// Rebuilds the in-memory order book by replaying the WAL.
    ///
    /// # Errors
    /// Returns an error if the WAL is missing, corrupted, or has sequence gaps.
    pub fn recover(
        spec: ContractSpec,
        config: PersistenceConfig,
    ) -> Result<Self, PersistenceError> {
        info!("recovering state from WAL");

        let data_dir_lock = acquire_data_dir_lock(&config.data_dir)?;
        let snapshot = Snapshot::load(&config.data_dir)?;
        let snapshot_seq = snapshot.as_ref().map(|snapshot| snapshot.last_seq);
        let (journal, recovery) = Journal::recover_after(&config, snapshot_seq)?;
        let base_seq = snapshot_seq.unwrap_or(0);
        let result = recovery;
        if result.last_seq < base_seq {
            return Err(PersistenceError::Corruption(format!(
                "WAL last_seq {} precedes snapshot seq {base_seq}",
                result.last_seq
            )));
        }

        let mut orderbook = match &snapshot {
            Some(snapshot) => {
                if snapshot.contract.symbol != spec.symbol
                    || snapshot.contract.tick_size != spec.tick_size
                    || snapshot.contract.min_price != spec.min_price
                    || snapshot.contract.max_price != spec.max_price
                {
                    return Err(PersistenceError::Corruption(
                        "snapshot contract does not match configured contract".to_string(),
                    ));
                }
                TickBasedOrderBook::restore(spec, &snapshot.orders, snapshot.next_order_id)
                    .map_err(PersistenceError::Corruption)?
            }
            None => TickBasedOrderBook::new(spec),
        };
        let mut next_trade_id = snapshot
            .as_ref()
            .map_or(1, |snapshot| snapshot.next_trade_id);

        for record in &result.records {
            match &record.entry {
                WalEntry::NewOrder(order) => {
                    if let MatchOutcome::Rejected { reason, .. } =
                        orderbook.match_order(order.clone())
                    {
                        warn!(%reason, "recovered order was rejected during replay");
                    }
                }
                WalEntry::CancelOrder(cancel) => {
                    if let Err(error) = orderbook.cancel_order(cancel.order_id, cancel.user_id) {
                        warn!(
                            order_id = cancel.order_id,
                            user_id = cancel.user_id,
                            %error,
                            "failed to replay cancel"
                        );
                    }
                }
                WalEntry::Trade(trade) => {
                    next_trade_id = next_trade_id.max(trade.trade_id + 1);
                }
                WalEntry::Confirmation(_) => {
                    // Results do not affect the recovered order book.
                }
            }
        }

        info!(
            records = result.records.len(),
            last_seq = result.last_seq,
            "WAL replay complete"
        );

        // Seed the idempotency table with every client order id seen in the WAL.
        // The original response cannot be reconstructed cheaply, so a retry
        // after restart is rejected rather than re-executed.
        let mut dedup = HashMap::new();
        let mut dedup_order = VecDeque::new();
        if config.dedup_cache_size > 0 {
            if let Some(snapshot) = &snapshot {
                for &key in &snapshot.dedup_keys {
                    if dedup.insert(key, DedupEntry { cached: None }).is_none() {
                        dedup_order.push_back(key);
                    }
                }
            }
            for record in &result.records {
                if let WalEntry::NewOrder(order) = &record.entry
                    && order.client_order_id != 0
                {
                    let key = (order.user_id, order.client_order_id);
                    if dedup.insert(key, DedupEntry { cached: None }).is_none() {
                        dedup_order.push_back(key);
                    }
                }
            }
            while dedup.len() > config.dedup_cache_size {
                if let Some(oldest) = dedup_order.pop_front() {
                    dedup.remove(&oldest);
                } else {
                    break;
                }
            }
        }

        let metrics = ServiceMetrics::new(orderbook.symbol());

        let staged_next_seq = journal.current_seq();
        Ok(Self {
            orderbook,
            journal: Some(journal),
            staged: Vec::new(),
            staged_next_seq,
            metrics,
            next_trade_id,
            dedup_capacity: config.dedup_cache_size,
            dedup,
            dedup_order,
            data_dir: config.data_dir,
            snapshot_every_records: config.snapshot_every_records,
            last_snapshot_seq: base_seq,
            _data_dir_lock: data_dir_lock,
        })
    }

    /// Stages a new order: validates, appends to the WAL buffer, and applies it
    /// to the book without flushing.
    ///
    /// Callers that process several commands should call [`Self::commit`] once
    /// after the batch to amortize flush/fsync.
    pub(crate) fn stage_order(
        &mut self,
        order: NewOrderRequest,
    ) -> ServiceResult<(Vec<TradeNotification>, Option<OrderConfirmation>)> {
        let dedup_key = (order.user_id, order.client_order_id);
        let client_order_id = order.client_order_id;
        if client_order_id != 0
            && let Some(entry) = self.dedup.get(&dedup_key)
        {
            return match &entry.cached {
                Some(cached) => Ok((cached.trades.clone(), cached.confirmation.clone())),
                None => Err(ServiceError::Rejected(format!(
                    "duplicate client_order_id {} for user {}: already processed before restart",
                    order.client_order_id, order.user_id
                ))),
            };
        }

        // Reject before persisting so the WAL only contains valid orders.
        self.orderbook
            .can_accept_order(&order)
            .map_err(ServiceError::Rejected)?;

        match order.order_type {
            OrderType::Buy => {
                self.metrics.orders_buy.inc();
                self.metrics.order_size_buy.observe(order.quantity as f64);
            }
            OrderType::Sell => {
                self.metrics.orders_sell.inc();
                self.metrics.order_size_sell.observe(order.quantity as f64);
            }
        }

        let wal_start = Instant::now();
        let timestamp = current_timestamp();

        self.append_wal(WalEntry::NewOrder(order.clone()), timestamp)?;

        let matching_start = Instant::now();
        let outcome = self.orderbook.match_order(order);
        self.metrics
            .matching_latency_ns
            .observe(matching_start.elapsed().as_nanos() as f64);

        let (trades_vec, confirmation) = match outcome {
            MatchOutcome::Filled { trades } => (trades, None),
            MatchOutcome::Rested {
                trades,
                confirmation,
            } => (trades, Some(confirmation)),
            MatchOutcome::Rejected { reason, trades } => {
                // `can_accept_order` ran before the WAL append. Reaching this
                // branch means the book changed in a way the preflight did not
                // model; fail fast instead of acknowledging an order that is
                // not in the book (and whose WAL entry already exists).
                return Err(ServiceError::Persistence(PersistenceError::Corruption(
                    format!(
                        "prevalidated order rejected after WAL append: {reason} ({} trades)",
                        trades.len()
                    ),
                )));
            }
        };

        self.metrics.trades.inc_by(trades_vec.len() as f64);

        let mut trades: Vec<TradeNotification> = trades_vec.into_iter().collect();
        for trade in &mut trades {
            trade.trade_id = self.next_trade_id;
            trade.timestamp = timestamp;
            self.next_trade_id += 1;
        }

        for trade in &trades {
            self.append_wal(WalEntry::Trade(trade.clone()), timestamp)?;
        }

        if let Some(confirmation) = &confirmation {
            self.append_wal(WalEntry::Confirmation(confirmation.clone()), timestamp)?;
        }

        self.metrics
            .wal_latency_ns
            .observe(wal_start.elapsed().as_nanos() as f64);

        if client_order_id != 0 {
            self.remember_client_order(
                dedup_key,
                DedupEntry {
                    cached: Some(CachedOrderResult {
                        trades: trades.clone(),
                        confirmation: confirmation.clone(),
                    }),
                },
            );
        }

        Ok((trades, confirmation))
    }

    /// Bounded insertion into the idempotency table.
    fn remember_client_order(&mut self, key: ClientOrderKey, entry: DedupEntry) {
        if self.dedup_capacity == 0 {
            return;
        }
        if self.dedup.insert(key, entry).is_none() {
            self.dedup_order.push_back(key);
        }
        while self.dedup.len() > self.dedup_capacity {
            if let Some(oldest) = self.dedup_order.pop_front() {
                self.dedup.remove(&oldest);
            } else {
                break;
            }
        }
    }

    /// Persists and applies a new order (stages it and commits immediately).
    pub fn match_order(
        &mut self,
        order: NewOrderRequest,
    ) -> ServiceResult<(Vec<TradeNotification>, Option<OrderConfirmation>)> {
        let result = self.stage_order(order)?;
        self.commit()?;
        Ok(result)
    }

    /// Commits all staged WAL records (flush + optional fsync).
    ///
    /// This is the group-commit point: one call amortizes the flush/fsync over
    /// every order staged since the previous commit.
    pub(crate) fn commit(&mut self) -> Result<(), PersistenceError> {
        let started = Instant::now();
        let journal = self.journal.as_mut().ok_or_else(journal_owned)?;
        journal.commit()?;
        self.metrics
            .commit_latency_ns
            .observe(started.elapsed().as_nanos() as f64);
        self.maybe_snapshot()?;
        Ok(())
    }

    /// Moves the WAL writer to a journaler thread.
    ///
    /// After this call the matching thread only appends `WalRecord`s to an
    /// in-memory staging buffer; a separate journaler owns all file/fsync I/O.
    pub(crate) fn take_journal(&mut self) -> Result<Journal, PersistenceError> {
        let journal = self.journal.take().ok_or_else(journal_owned)?;
        self.staged_next_seq = journal.current_seq();
        Ok(journal)
    }

    /// Removes staged records for the journaler to encode and persist.
    pub(crate) fn take_staged_wal(&mut self) -> Vec<WalRecord> {
        std::mem::take(&mut self.staged)
    }

    fn append_wal(&mut self, entry: WalEntry, timestamp: u64) -> Result<u64, PersistenceError> {
        if let Some(journal) = self.journal.as_mut() {
            journal.append(entry, timestamp)
        } else {
            let seq = self.staged_next_seq;
            self.staged_next_seq += 1;
            self.staged.push(WalRecord {
                seq,
                timestamp,
                entry,
            });
            Ok(seq)
        }
    }

    /// Flushes WAL bytes to the OS and returns a file handle for async fsync.
    ///
    /// Snapshotting is intentionally not performed here: it needs a durability
    /// barrier and is incompatible with the async-fsync runtime mode.
    pub(crate) fn commit_to_page_cache(&mut self) -> Result<File, PersistenceError> {
        let started = Instant::now();
        let journal = self.journal.as_mut().ok_or_else(journal_owned)?;
        let handle = journal.flush_handle()?;
        self.metrics
            .commit_latency_ns
            .observe(started.elapsed().as_nanos() as f64);
        Ok(handle)
    }

    /// Whether the async-fsync worker should take a snapshot after the next
    /// durability barrier.
    pub(crate) fn needs_snapshot(&self) -> bool {
        self.snapshot_every_records > 0
            && self.journal.as_ref().is_some_and(|journal| {
                journal.current_seq().saturating_sub(self.last_snapshot_seq)
                    >= self.snapshot_every_records
            })
    }

    /// Writes a snapshot after the current WAL state is durable.
    ///
    /// # Errors
    /// Returns an error if the WAL flush or snapshot write fails.
    pub fn snapshot(&mut self) -> Result<(), PersistenceError> {
        self.journal.as_mut().ok_or_else(journal_owned)?.commit()?;
        self.write_snapshot()
    }

    fn maybe_snapshot(&mut self) -> Result<(), PersistenceError> {
        if self.snapshot_every_records == 0 {
            return Ok(());
        }
        let Some(journal) = self.journal.as_ref() else {
            return Ok(());
        };
        let current = journal.current_seq();
        if current.saturating_sub(self.last_snapshot_seq) >= self.snapshot_every_records {
            self.write_snapshot()?;
        }
        Ok(())
    }

    fn write_snapshot(&mut self) -> Result<(), PersistenceError> {
        let journal = self.journal.as_mut().ok_or_else(journal_owned)?;
        let last_seq = journal.current_seq().saturating_sub(1);
        let snapshot = Snapshot {
            contract: self.orderbook.spec().clone(),
            last_seq,
            next_order_id: self.orderbook.next_order_id(),
            next_trade_id: self.next_trade_id,
            orders: self.orderbook.snapshot_orders(),
            dedup_keys: self.dedup.keys().copied().collect(),
        };
        Snapshot::save(&self.data_dir, &snapshot)?;
        journal.rotate()?;
        journal.prune_through(last_seq)?;
        self.last_snapshot_seq = last_seq;
        info!(last_seq, orders = snapshot.orders.len(), "snapshot written");
        Ok(())
    }

    /// Stages a cancellation without flushing.
    pub(crate) fn stage_cancel(&mut self, cancel: CancelOrderRequest) -> ServiceResult<()> {
        // Check ownership before writing so rejected cancels never reach the WAL.
        match self.orderbook.order_owner(cancel.order_id) {
            Some(owner) if owner == cancel.user_id => {}
            Some(_) => {
                return Err(ServiceError::Rejected(format!(
                    "user {} is not allowed to cancel order {}",
                    cancel.user_id, cancel.order_id
                )));
            }
            None => {
                return Err(ServiceError::Rejected(format!(
                    "order {} not found",
                    cancel.order_id
                )));
            }
        }

        let wal_start = Instant::now();
        let timestamp = current_timestamp();

        self.append_wal(WalEntry::CancelOrder(cancel.clone()), timestamp)?;

        self.orderbook
            .cancel_order(cancel.order_id, cancel.user_id)
            .map_err(ServiceError::Rejected)?;

        self.metrics
            .wal_latency_ns
            .observe(wal_start.elapsed().as_nanos() as f64);

        self.metrics.cancellations.inc();

        Ok(())
    }

    /// Persists and applies a cancellation (stages it and commits immediately).
    pub fn cancel_order(&mut self, cancel: CancelOrderRequest) -> ServiceResult<()> {
        self.stage_cancel(cancel)?;
        self.commit()?;
        Ok(())
    }

    /// Flushes buffered WAL records and, when enabled, fsyncs the log.
    pub(crate) fn flush(&mut self) -> Result<(), PersistenceError> {
        self.commit()
    }

    /// Returns a reference to the order book.
    #[must_use]
    pub fn orderbook(&self) -> &TickBasedOrderBook {
        &self.orderbook
    }
}

impl Drop for PersistentMatchingService {
    fn drop(&mut self) {
        if self.journal.is_none() {
            return;
        }
        if let Err(error) = self.flush() {
            error!(%error, "failed to flush WAL on drop");
        }
    }
}

fn journal_owned() -> PersistenceError {
    PersistenceError::Io(std::io::Error::other(
        "WAL writer is owned by the journaler thread",
    ))
}

/// Returns the current time in nanoseconds since the Unix epoch.
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;
    use tempfile::TempDir;

    fn test_service(dir: &TempDir) -> PersistentMatchingService {
        let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
        let config = PersistenceConfig {
            data_dir: dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        PersistentMatchingService::new(spec, config).expect("failed to create service")
    }

    fn buy_order(user_id: u64, price: u64, quantity: u64) -> NewOrderRequest {
        NewOrderRequest {
            user_id,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Buy,
            price,
            quantity,
        }
    }

    fn match_side(
        service: &mut PersistentMatchingService,
        side: OrderType,
        user_id: u64,
        price: u64,
    ) -> ServiceResult<(Vec<TradeNotification>, Option<OrderConfirmation>)> {
        let order = NewOrderRequest {
            user_id,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: side,
            price,
            quantity: 10,
        };
        service.match_order(order)
    }

    #[test]
    fn test_duplicate_client_order_returns_cached_result() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let mut service = PersistentMatchingService::new(spec, config).unwrap();

        let order = NewOrderRequest {
            user_id: 1,
            client_order_id: 7,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 50_000,
            quantity: 5,
        };
        let first = service.match_order(order.clone()).unwrap();
        let first_order_id = first.1.expect("order must rest").order_id;

        let duplicate = service.match_order(order).unwrap();
        assert_eq!(duplicate.1.expect("cached ack").order_id, first_order_id);
        assert!(service.orderbook.order_owner(first_order_id).is_some());
    }

    #[test]
    fn test_duplicate_after_recovery_is_rejected_without_reexecution() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let order = NewOrderRequest {
            user_id: 1,
            client_order_id: 9,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 50_000,
            quantity: 5,
        };
        {
            let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
            service.match_order(order.clone()).unwrap();
        }

        let mut recovered = PersistentMatchingService::recover(spec, config).unwrap();
        let error = recovered
            .match_order(order)
            .expect_err("retry after restart must not create a second order");
        assert!(
            matches!(error, ServiceError::Rejected(ref reason) if reason.contains("duplicate"))
        );
        assert!(recovered.orderbook.order_owner(1).is_some());
    }

    #[test]
    fn test_client_order_id_zero_disables_dedup() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let mut service = PersistentMatchingService::new(spec, config).unwrap();
        let mut order = buy_order(1, 50_000, 1);
        order.client_order_id = 0;
        let first = service.match_order(order.clone()).unwrap().1.unwrap();
        let second = service.match_order(order).unwrap().1.unwrap();
        assert_ne!(first.order_id, second.order_id);
        assert!(service.orderbook.order_owner(first.order_id).is_some());
        assert!(service.orderbook.order_owner(second.order_id).is_some());
    }

    #[test]
    fn test_snapshot_recovery_skips_old_wal_and_continues_ids() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };

        let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
        service.match_order(buy_order(1, 50_000, 7)).unwrap();
        service.snapshot().expect("snapshot");
        let sell = NewOrderRequest {
            user_id: 2,
            client_order_id: 2,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Sell,
            price: 50_000,
            quantity: 3,
        };
        service.match_order(sell).unwrap();
        drop(service);

        let mut recovered = PersistentMatchingService::recover(spec, config).unwrap();
        let status = recovered
            .orderbook()
            .get_order_status(1)
            .expect("snapshot order survives");
        assert_eq!(status.remaining_quantity, 4);
        let next = recovered
            .match_order(buy_order(3, 49_999, 1))
            .expect("new order")
            .1
            .expect("rests");
        assert_eq!(next.order_id, 3, "order ids continue after snapshot + tail");
    }

    #[test]
    fn test_snapshot_preserves_idempotency_keys() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let order = NewOrderRequest {
            user_id: 1,
            client_order_id: 7,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 50_000,
            quantity: 5,
        };
        {
            let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
            service.match_order(order.clone()).unwrap();
            service.snapshot().unwrap();
        }

        let mut recovered = PersistentMatchingService::recover(spec, config).unwrap();
        let error = recovered
            .match_order(order)
            .expect_err("snapshot must preserve client order ids");
        assert!(matches!(error, ServiceError::Rejected(_)));
    }

    #[test]
    fn test_snapshot_pruning_and_recovery_with_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            wal_max_size: 200,
            snapshot_every_records: 4,
            ..Default::default()
        };
        {
            let mut service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
            for i in 0..20 {
                service
                    .match_order(buy_order(i + 1, 50_000 + i, 1))
                    .unwrap();
            }
        }

        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert_eq!(recovered.orderbook().best_bid(), Some(50_019));
    }

    #[test]
    fn test_automatic_snapshot_is_written() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            snapshot_every_records: 2,
            ..Default::default()
        };
        let mut service = PersistentMatchingService::new(spec, config).unwrap();
        service.match_order(buy_order(1, 50_000, 1)).unwrap();
        assert!(temp_dir.path().join("snapshot.bin").exists());
    }

    #[test]
    fn test_group_commit_persists_whole_batch() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let mut service = PersistentMatchingService::new(spec, config.clone()).unwrap();

        let bid = service
            .stage_order(buy_order(1, 50_000, 10))
            .expect("bid stages");
        assert!(bid.1.is_some());
        let ask = NewOrderRequest {
            user_id: 2,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Sell,
            price: 50_000,
            quantity: 4,
        };
        let ask = service.stage_order(ask).expect("ask stages");
        assert_eq!(ask.0.len(), 1, "ask should match the staged bid");

        service.commit().expect("one flush covers the batch");
        drop(service);

        let recovered = PersistentMatchingService::recover(
            ContractSpec::new("BTCUSDT", 1, 40_000, 70_000),
            config,
        )
        .unwrap();
        let status = recovered
            .orderbook()
            .get_order_status(1)
            .expect("partially filled bid must survive recovery");
        assert_eq!(status.remaining_quantity, 6);
    }

    #[test]
    fn test_data_dir_is_exclusively_locked() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let _first = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();

        let second = PersistentMatchingService::new(spec, config);
        assert!(second.is_err(), "a second process must not share the WAL");
    }

    #[test]
    fn test_order_survives_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let mut service = test_service(&temp_dir);

        let (trades, confirmation) = service
            .match_order(buy_order(1, 50000, 10))
            .expect("order rejected");
        assert!(trades.is_empty());
        assert!(confirmation.is_some());

        service.flush().unwrap();
        drop(service);

        let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();

        assert_eq!(recovered.orderbook().best_bid(), Some(50000));
        assert_eq!(recovered.orderbook().best_ask(), None);
    }

    #[test]
    fn test_cancel_is_persisted() {
        let temp_dir = TempDir::new().unwrap();
        let mut service = test_service(&temp_dir);

        let (_, confirmation) = service
            .match_order(buy_order(7, 50000, 10))
            .expect("order rejected");
        let order_id = confirmation
            .expect("resting order must be confirmed")
            .order_id;

        service
            .cancel_order(CancelOrderRequest {
                user_id: 7,
                order_id,
            })
            .expect("cancel failed");
        assert!(service.orderbook().best_bid().is_none());

        service.flush().unwrap();
        drop(service);

        let spec = ContractSpec::new("BTCUSDT", 1, 40000, 70000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert!(recovered.orderbook().best_bid().is_none());
    }

    #[test]
    fn test_invalid_price_is_rejected_without_wal_entry() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 5, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let mut service = PersistentMatchingService::new(spec, config).unwrap();

        let error = service
            .match_order(buy_order(1, 50_002, 10))
            .expect_err("off-grid price must be rejected");
        assert!(matches!(error, ServiceError::Rejected(_)));

        // Rejected orders must not be persisted: recovery yields an empty book.
        service.flush().unwrap();
        drop(service);

        let spec = ContractSpec::new("BTCUSDT", 5, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert!(recovered.orderbook().best_bid().is_none());
    }

    #[test]
    fn test_full_price_level_is_rejected_without_ghost_order() {
        let temp_dir = TempDir::new().unwrap();
        let mut spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        spec.queue_capacity = 2;
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let mut service = PersistentMatchingService::new(spec, config.clone()).unwrap();

        for user_id in 1..=2 {
            match_side(&mut service, OrderType::Sell, user_id, 50_000)
                .expect("first orders must rest");
        }
        let error = match_side(&mut service, OrderType::Sell, 3, 50_000)
            .expect_err("full queue must reject");
        assert!(matches!(error, ServiceError::Rejected(_)));

        // The rejected order must not be in the WAL or the book.
        service.flush().unwrap();
        drop(service);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert_eq!(recovered.orderbook().best_ask(), Some(50_000));
        assert!(recovered.orderbook().order_owner(3).is_none());
    }

    #[test]
    fn test_unknown_symbol_is_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let mut service = test_service(&temp_dir);

        let order = NewOrderRequest {
            user_id: 1,
            client_order_id: 0,
            symbol: crate::shared::SymbolId::intern("ETHUSDT"),
            order_type: OrderType::Buy,
            price: 50_000,
            quantity: 10,
        };
        let error = service
            .match_order(order)
            .expect_err("symbol must be rejected");
        assert!(matches!(error, ServiceError::Rejected(_)));
    }

    #[test]
    fn test_unauthorized_cancel_is_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let mut service = test_service(&temp_dir);

        let (_, confirmation) = service
            .match_order(buy_order(7, 50000, 10))
            .expect("order rejected");
        let order_id = confirmation
            .expect("resting order must be confirmed")
            .order_id;

        let result = service.cancel_order(CancelOrderRequest {
            user_id: 99,
            order_id,
        });
        assert!(matches!(result, Err(ServiceError::Rejected(_))));
        assert_eq!(service.orderbook().best_bid(), Some(50000));
    }
}
