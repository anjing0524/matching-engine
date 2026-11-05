//! Matching execution runtime.
//!
//! This module is the deep seam between order intake and the single-threaded
//! matching service. It owns the command channel, group-commit batch loop,
//! reply ordering, fatal-state propagation, and worker lifecycle. Callers only
//! see [`MatchingHandle::submit_order`] / [`MatchingHandle::submit_cancel`] and
//! a lifecycle handle.
use crate::application::services::persistent_service::{
    PersistentMatchingService, ServiceError, ServiceResult,
};
use crate::infrastructure::persistence::{Journal, PersistenceError, Snapshot, WalRecord};
use crate::shared::metrics::METRICS;
use crate::shared::protocol::{
    CancelOrderRequest, NewOrderRequest, OrderConfirmation, TradeNotification,
};
use crossbeam::channel::{Receiver, Sender, TryRecvError, bounded};
use prometheus::Histogram;
use std::fs::File;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Instant;

/// Result of one order submission.
pub type OrderOutcome = (Vec<TradeNotification>, Option<OrderConfirmation>);

/// Errors surfaced by the matching execution handle.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The worker is gone and cannot accept commands.
    #[error("matching worker is unavailable")]
    Closed,

    /// The worker panicked.
    #[error("matching worker panicked")]
    WorkerPanicked,

    /// The worker stopped because persistence failed.
    #[error("fatal persistence failure: {0}")]
    Fatal(String),

    /// The matching service rejected or failed the command.
    #[error(transparent)]
    Service(#[from] ServiceError),

    /// Flush/commit failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// A submitted order awaiting the group-commit reply.
pub struct OrderTicket {
    reply: Receiver<ServiceResult<OrderOutcome>>,
    submitted_at: Instant,
}

impl OrderTicket {
    /// Time at which the ticket was enqueued.
    #[must_use]
    pub fn submitted_at(&self) -> Instant {
        self.submitted_at
    }

    /// Waits for the durable reply.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Closed`] if the worker is gone.
    pub fn wait(self) -> Result<OrderOutcome, RuntimeError> {
        let result = self.reply.recv().map_err(|_| RuntimeError::Closed)?;
        result.map_err(RuntimeError::Service)
    }
}

/// A submitted cancellation awaiting the group-commit reply.
pub struct CancelTicket {
    reply: Receiver<ServiceResult<()>>,
    submitted_at: Instant,
}

impl CancelTicket {
    /// Time at which the ticket was enqueued.
    #[must_use]
    pub fn submitted_at(&self) -> Instant {
        self.submitted_at
    }

    /// Waits for the durable reply.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Closed`] if the worker is gone.
    pub fn wait(self) -> Result<(), RuntimeError> {
        let result = self.reply.recv().map_err(|_| RuntimeError::Closed)?;
        result.map_err(RuntimeError::Service)
    }
}

/// Configuration for the matching worker.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Bounded command queue capacity.
    pub queue_capacity: usize,
    /// Maximum commands staged before one WAL flush/fsync.
    pub batch_size: usize,
    /// Optional logical core for the matching thread (requires `cpu-affinity`).
    pub matching_core: Option<usize>,
    /// Spin instead of sleeping when the command queue is empty.
    pub busy_poll: bool,
    /// Flush the WAL on the matching thread, fsync on a dedicated thread.
    ///
    /// Incompatible with automatic snapshots; the CLI rejects that combination.
    pub async_fsync: bool,
    /// Move the entire WAL writer to a journaler thread. The matching thread
    /// only stages sequenced records in memory.
    pub decouple_wal: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 10_000,
            batch_size: 256,
            matching_core: None,
            busy_poll: false,
            async_fsync: false,
            decouple_wal: false,
        }
    }
}

type OrderReply = Sender<ServiceResult<OrderOutcome>>;
type CancelReply = Sender<ServiceResult<()>>;

enum StagedReply {
    Order {
        reply: OrderReply,
        result: ServiceResult<OrderOutcome>,
    },
    Cancel {
        reply: CancelReply,
        result: ServiceResult<()>,
    },
}

struct CommitSenders {
    async_fsync: Option<Sender<CommitTask>>,
    journal: Option<Sender<JournalerCommand>>,
    snapshot: Option<Sender<Snapshot>>,
}

enum JournalerCommand {
    Batch(JournalTask),
    Compact { through_seq: u64 },
}

struct JournalTask {
    records: Vec<WalRecord>,
    staged: Vec<StagedReply>,
    barrier: Option<Sender<Result<(), PersistenceError>>>,
}

struct CommitTask {
    file: File,
    staged: Vec<StagedReply>,
    barrier: Option<Sender<Result<(), PersistenceError>>>,
}

enum Command {
    NewOrder(NewOrderRequest, Instant, OrderReply),
    CancelOrder(CancelOrderRequest, CancelReply),
    Flush(Sender<Result<(), PersistenceError>>),
    Shutdown(Sender<()>),
}

struct RuntimeControl {
    alive: AtomicBool,
    fatal: Mutex<Option<String>>,
}

impl RuntimeControl {
    fn new() -> Self {
        Self {
            alive: AtomicBool::new(true),
            fatal: Mutex::new(None),
        }
    }

    fn set_fatal(&self, message: String) {
        let mut fatal = lock(&self.fatal);
        if fatal.is_none() {
            *fatal = Some(message);
        }
    }

    fn fatal(&self) -> Option<String> {
        lock(&self.fatal).clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Cloneable command handle for connection threads.
#[derive(Clone)]
pub struct MatchingHandle {
    commands: Sender<Command>,
    control: Arc<RuntimeControl>,
}

impl MatchingHandle {
    /// Enqueues a new order and returns a ticket without waiting.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Closed`] if the worker is gone.
    pub fn submit_order_ticket(&self, order: NewOrderRequest) -> Result<OrderTicket, RuntimeError> {
        let submitted_at = Instant::now();
        let (reply_tx, reply) = bounded(1);
        self.commands
            .send(Command::NewOrder(order, submitted_at, reply_tx))
            .map_err(|_| RuntimeError::Closed)?;
        Ok(OrderTicket {
            reply,
            submitted_at,
        })
    }

    /// Submits a new order and waits for the group-commit reply.
    ///
    /// # Errors
    /// Propagates submission/reply errors.
    pub fn submit_order(&self, order: NewOrderRequest) -> Result<OrderOutcome, RuntimeError> {
        self.submit_order_ticket(order)?.wait()
    }

    /// Enqueues a cancellation and returns a ticket without waiting.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Closed`] if the worker is gone.
    pub fn submit_cancel_ticket(
        &self,
        cancel: CancelOrderRequest,
    ) -> Result<CancelTicket, RuntimeError> {
        let submitted_at = Instant::now();
        let (reply_tx, reply) = bounded(1);
        self.commands
            .send(Command::CancelOrder(cancel, reply_tx))
            .map_err(|_| RuntimeError::Closed)?;
        Ok(CancelTicket {
            reply,
            submitted_at,
        })
    }

    /// Submits a cancellation and waits for the group-commit reply.
    ///
    /// # Errors
    /// Propagates submission/reply errors.
    pub fn submit_cancel(&self, cancel: CancelOrderRequest) -> Result<(), RuntimeError> {
        self.submit_cancel_ticket(cancel)?.wait()
    }

    /// Flushes and fsyncs all staged commands.
    pub fn flush(&self) -> Result<(), RuntimeError> {
        let (reply_tx, reply_rx) = bounded(1);
        self.commands
            .send(Command::Flush(reply_tx))
            .map_err(|_| RuntimeError::Closed)?;
        let result = reply_rx.recv().map_err(|_| RuntimeError::Closed)?;
        result.map_err(RuntimeError::Persistence)
    }

    /// Returns the fatal persistence message, if the worker failed.
    #[must_use]
    pub fn fatal(&self) -> Option<String> {
        self.control.fatal()
    }

    /// Returns whether the worker thread is still alive.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.control.alive.load(Ordering::Acquire)
    }
}

/// Owns the matching worker thread and its command handle.
pub struct MatchingRuntime {
    handle: MatchingHandle,
    worker: Option<JoinHandle<()>>,
    commit_worker: Option<JoinHandle<()>>,
    journaler: Option<JoinHandle<()>>,
    snapshotter: Option<JoinHandle<()>>,
    control: Arc<RuntimeControl>,
}

impl MatchingRuntime {
    /// Starts the matching worker.
    ///
    /// # Errors
    /// Returns an error if the OS thread cannot be spawned.
    pub fn start(
        mut service: PersistentMatchingService,
        config: RuntimeConfig,
    ) -> std::io::Result<Self> {
        let symbol = service.orderbook().symbol().to_string();
        let queue_wait = METRICS.actor_queue_wait_ns.with_label_values(&[&symbol]);
        let control = Arc::new(RuntimeControl::new());
        let worker_control = Arc::clone(&control);
        let (commands, receiver) = bounded(config.queue_capacity);
        let batch_size = config.batch_size.max(1);
        let matching_core = config.matching_core;
        let busy_poll = config.busy_poll;
        let async_fsync = config.async_fsync;
        let decouple_wal = config.decouple_wal;

        let (commit_sender, commit_worker) = if async_fsync && !decouple_wal {
            let (sender, commit_receiver) = bounded(config.queue_capacity.max(1));
            let commit_control = Arc::clone(&control);
            let handle = thread::Builder::new()
                .name("wal-fsync".to_string())
                .spawn(move || run_commit_worker(commit_receiver, commit_control))?;
            (Some(sender), Some(handle))
        } else {
            (None, None)
        };

        let (journal_sender, journaler, snapshot_sender, snapshotter) = if decouple_wal {
            let journal = service
                .take_journal()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let durable_seq = Arc::new(AtomicU64::new(journal.current_seq().saturating_sub(1)));
            let (sender, journal_receiver) = bounded(config.queue_capacity.max(1));
            let journal_control = Arc::clone(&control);
            let journal_durable = Arc::clone(&durable_seq);
            let journal_commit_latency = METRICS
                .commit_latency_ns
                .with_label_values(&[service.orderbook().symbol()]);
            let handle = thread::Builder::new()
                .name("wal-journaler".to_string())
                .spawn(move || {
                    run_journaler(
                        journal,
                        journal_receiver,
                        journal_durable,
                        journal_control,
                        journal_commit_latency,
                    )
                })?;

            let (snapshot_sender, snapshot_receiver) = bounded(64);
            let data_dir = service.data_dir().to_path_buf();
            let snapshot_control = Arc::clone(&control);
            let compact_sender = sender.clone();
            let snapshotter = thread::Builder::new()
                .name("wal-snapshotter".to_string())
                .spawn(move || {
                    run_snapshotter(
                        snapshot_receiver,
                        compact_sender,
                        data_dir,
                        durable_seq,
                        snapshot_control,
                    )
                })?;
            (
                Some(sender),
                Some(handle),
                Some(snapshot_sender),
                Some(snapshotter),
            )
        } else {
            (None, None, None, None)
        };

        let worker = thread::Builder::new()
            .name("matching".to_string())
            .spawn(move || {
                pin_matching_thread(matching_core);
                let _alive_guard = AliveGuard {
                    control: Arc::clone(&worker_control),
                };
                run_worker(
                    service,
                    receiver,
                    worker_control,
                    queue_wait,
                    batch_size,
                    busy_poll,
                    CommitSenders {
                        async_fsync: commit_sender,
                        journal: journal_sender,
                        snapshot: snapshot_sender,
                    },
                );
            })?;

        Ok(Self {
            handle: MatchingHandle {
                commands,
                control: Arc::clone(&control),
            },
            worker: Some(worker),
            commit_worker,
            journaler,
            snapshotter,
            control,
        })
    }

    /// Returns a cloneable handle for command submission and liveness checks.
    #[must_use]
    pub fn handle(&self) -> MatchingHandle {
        self.handle.clone()
    }

    /// Flushes, stops the worker, joins it, and reports fatal/panic state.
    pub fn shutdown(mut self) -> Result<(), RuntimeError> {
        let flush_result = self.handle.flush();
        let (done_tx, done_rx) = bounded(1);
        if self
            .handle
            .commands
            .send(Command::Shutdown(done_tx))
            .is_ok()
        {
            let _ = done_rx.recv();
        }
        let mut panicked = false;
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            panicked = true;
        }
        if let Some(commit_worker) = self.commit_worker.take()
            && commit_worker.join().is_err()
        {
            panicked = true;
        }
        if let Some(journaler) = self.journaler.take()
            && journaler.join().is_err()
        {
            panicked = true;
        }
        if let Some(snapshotter) = self.snapshotter.take()
            && snapshotter.join().is_err()
        {
            panicked = true;
        }

        if let Some(message) = self.control.fatal() {
            return Err(RuntimeError::Fatal(message));
        }
        if panicked {
            return Err(RuntimeError::WorkerPanicked);
        }
        flush_result?;
        Ok(())
    }
}

impl Drop for MatchingRuntime {
    fn drop(&mut self) {
        // A runtime dropped during unwinding must not leave a matching thread
        // blocked on its command channel forever.
        if self.worker.is_some() {
            let (done_tx, done_rx) = bounded(1);
            if self
                .handle
                .commands
                .send(Command::Shutdown(done_tx))
                .is_ok()
            {
                let _ = done_rx.recv();
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            if let Some(commit_worker) = self.commit_worker.take() {
                let _ = commit_worker.join();
            }
            if let Some(journaler) = self.journaler.take() {
                let _ = journaler.join();
            }
            if let Some(snapshotter) = self.snapshotter.take() {
                let _ = snapshotter.join();
            }
        }
    }
}

struct AliveGuard {
    control: Arc<RuntimeControl>,
}

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.control.alive.store(false, Ordering::Release);
    }
}

fn next_command(receiver: &Receiver<Command>, busy_poll: bool) -> Option<Command> {
    if !busy_poll {
        return receiver.recv().ok();
    }
    loop {
        match receiver.try_recv() {
            Ok(command) => return Some(command),
            Err(TryRecvError::Empty) => std::hint::spin_loop(),
            Err(TryRecvError::Disconnected) => return None,
        }
    }
}

fn run_worker(
    mut service: PersistentMatchingService,
    receiver: Receiver<Command>,
    control: Arc<RuntimeControl>,
    queue_wait: Histogram,
    batch_size: usize,
    busy_poll: bool,
    senders: CommitSenders,
) {
    let CommitSenders {
        async_fsync: commit_sender,
        journal: journal_sender,
        snapshot: snapshot_sender,
    } = senders;
    'outer: while let Some(first) = next_command(&receiver, busy_poll) {
        let mut staged: Vec<StagedReply> = Vec::with_capacity(batch_size.min(64));
        let mut control_command: Option<Command> = None;
        let mut current = Some(first);
        let mut fatal_error: Option<String> = None;
        let mut channel_closed = false;

        while let Some(command) = current.take() {
            match command {
                Command::NewOrder(order, enqueued, reply) => {
                    queue_wait.observe(enqueued.elapsed().as_nanos() as f64);
                    let result = service.stage_order(order);
                    if let Err(ServiceError::Persistence(error)) = &result {
                        fatal_error = Some(error.to_string());
                    }
                    staged.push(StagedReply::Order { reply, result });
                }
                Command::CancelOrder(cancel, reply) => {
                    let result = service.stage_cancel(cancel);
                    if let Err(ServiceError::Persistence(error)) = &result {
                        fatal_error = Some(error.to_string());
                    }
                    staged.push(StagedReply::Cancel { reply, result });
                }
                Command::Flush(reply) => {
                    control_command = Some(Command::Flush(reply));
                    break;
                }
                Command::Shutdown(reply) => {
                    control_command = Some(Command::Shutdown(reply));
                    break;
                }
            }

            if fatal_error.is_some() || staged.len() >= batch_size {
                break;
            }
            current = match receiver.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    channel_closed = true;
                    None
                }
            };
        }

        let mut commit_result: Result<(), PersistenceError> = Ok(());
        if let Some(journal_sender) = journal_sender.as_ref() {
            let records = service.take_staged_wal();
            let (barrier, barrier_rx) = if control_command.is_some() {
                let (tx, rx) = bounded(1);
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };
            let task = JournalTask {
                records,
                staged,
                barrier,
            };
            if journal_sender.send(JournalerCommand::Batch(task)).is_err() {
                fatal_error.get_or_insert_with(|| "journaler stopped".to_string());
            } else if let Some(barrier_rx) = barrier_rx {
                match barrier_rx.recv() {
                    Ok(result) => commit_result = result,
                    Err(_) => {
                        fatal_error.get_or_insert_with(|| "journaler stopped".to_string());
                        commit_result = Err(PersistenceError::Io(std::io::Error::other(
                            "journaler stopped",
                        )));
                    }
                }
            }
            if let Some(message) = control.fatal() {
                fatal_error.get_or_insert(message);
            }
            if service.needs_snapshot() {
                let snapshot = service.prepare_snapshot();
                if let Some(snapshot_sender) = snapshot_sender.as_ref()
                    && snapshot_sender.send(snapshot).is_err()
                {
                    fatal_error.get_or_insert_with(|| "snapshotter stopped".to_string());
                }
            }
        } else if let Some(commit_sender) = commit_sender.as_ref() {
            let file = service.commit_to_page_cache();
            match file {
                Ok(file) => {
                    // Snapshots require a durability barrier for all earlier
                    // batches; append one when a snapshot is due.
                    let wants_snapshot = service.needs_snapshot();
                    let want_barrier = control_command.is_some() || wants_snapshot;
                    let (barrier, barrier_rx) = if want_barrier {
                        let (tx, rx) = bounded(1);
                        (Some(tx), Some(rx))
                    } else {
                        (None, None)
                    };
                    let task = CommitTask {
                        file,
                        staged,
                        barrier,
                    };
                    let mut barrier_ok = false;
                    if commit_sender.send(task).is_err() {
                        fatal_error.get_or_insert_with(|| "async fsync worker stopped".to_string());
                    } else if let Some(barrier_rx) = barrier_rx {
                        match barrier_rx.recv() {
                            Ok(result) => {
                                if result.is_ok() {
                                    barrier_ok = true;
                                }
                                commit_result = result;
                            }
                            Err(_) => {
                                fatal_error.get_or_insert_with(|| {
                                    "async fsync worker stopped".to_string()
                                });
                                commit_result = Err(PersistenceError::Io(std::io::Error::other(
                                    "async fsync worker stopped",
                                )));
                            }
                        }
                    }
                    if wants_snapshot
                        && barrier_ok
                        && let Err(error) = service.snapshot()
                    {
                        fatal_error.get_or_insert_with(|| error.to_string());
                        if commit_result.is_ok() {
                            commit_result = Err(error);
                        }
                    }
                    if let Some(message) = control.fatal() {
                        fatal_error.get_or_insert(message);
                    }
                }
                Err(error) => {
                    fatal_error.get_or_insert_with(|| error.to_string());
                    commit_result = Err(copy_persistence_error(&error));
                    for item in staged {
                        send_staged_reply(item, &commit_result);
                    }
                }
            }
        } else {
            commit_result = service.commit();
            if let Err(error) = &commit_result {
                fatal_error.get_or_insert_with(|| error.to_string());
            }
            for item in staged {
                send_staged_reply(item, &commit_result);
            }
        }

        let mut shutting_down = false;
        if let Some(command) = control_command {
            match command {
                Command::Flush(reply) => {
                    let _ = reply.send(commit_result);
                }
                Command::Shutdown(reply) => {
                    let _ = reply.send(());
                    shutting_down = true;
                }
                Command::NewOrder(..) | Command::CancelOrder(..) => {}
            }
        }

        if channel_closed {
            break;
        }
        if let Some(message) = fatal_error {
            control.set_fatal(message);
            break 'outer;
        }
        if shutting_down {
            break;
        }
    }
    // Dropping the service flushes any remaining WAL buffer.
}

fn run_snapshotter(
    receiver: Receiver<Snapshot>,
    compact_sender: Sender<JournalerCommand>,
    data_dir: std::path::PathBuf,
    durable_seq: Arc<AtomicU64>,
    control: Arc<RuntimeControl>,
) {
    while let Ok(snapshot) = receiver.recv() {
        while durable_seq.load(Ordering::Acquire) < snapshot.last_seq {
            if control.fatal().is_some() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        if let Err(error) = Snapshot::save(&data_dir, &snapshot) {
            control.set_fatal(error.to_string());
            return;
        }
        if compact_sender
            .send(JournalerCommand::Compact {
                through_seq: snapshot.last_seq,
            })
            .is_err()
        {
            control.set_fatal("journaler stopped before WAL compaction".to_string());
            return;
        }
    }
}

fn run_journaler(
    mut journal: Journal,
    receiver: Receiver<JournalerCommand>,
    durable_seq: Arc<AtomicU64>,
    control: Arc<RuntimeControl>,
    commit_latency: Histogram,
) {
    let mut failed: Option<PersistenceError> = None;
    while let Ok(command) = receiver.recv() {
        match command {
            JournalerCommand::Compact { through_seq } => {
                if failed.is_some() {
                    continue;
                }
                if let Err(error) = journal
                    .rotate()
                    .and_then(|()| journal.prune_through(through_seq))
                {
                    control.set_fatal(error.to_string());
                    failed = Some(copy_persistence_error(&error));
                }
            }
            JournalerCommand::Batch(task) => {
                let started = Instant::now();
                let result = if let Some(error) = &failed {
                    Err(copy_persistence_error(error))
                } else {
                    let mut result = Ok(());
                    for record in &task.records {
                        if let Err(error) = journal.append_record(record) {
                            result = Err(error);
                            break;
                        }
                    }
                    if result.is_ok() {
                        result = journal.commit();
                    }
                    result
                };
                if result.is_ok() {
                    commit_latency.observe(started.elapsed().as_nanos() as f64);
                    durable_seq.store(journal.current_seq().saturating_sub(1), Ordering::Release);
                } else if failed.is_none()
                    && let Err(error) = &result
                {
                    control.set_fatal(error.to_string());
                    failed = Some(copy_persistence_error(error));
                }
                for item in task.staged {
                    send_staged_reply(item, &result);
                }
                if let Some(barrier) = task.barrier {
                    let _ = barrier.send(result);
                }
            }
        }
    }
}

fn run_commit_worker(receiver: Receiver<CommitTask>, control: Arc<RuntimeControl>) {
    let mut failed: Option<PersistenceError> = None;
    while let Ok(task) = receiver.recv() {
        let result = if let Some(error) = &failed {
            Err(copy_persistence_error(error))
        } else {
            task.file.sync_all().map_err(PersistenceError::from)
        };
        if let Err(error) = &result
            && failed.is_none()
        {
            control.set_fatal(error.to_string());
            failed = Some(copy_persistence_error(error));
        }
        for item in task.staged {
            send_staged_reply(item, &result);
        }
        if let Some(barrier) = task.barrier {
            let _ = barrier.send(result);
        }
    }
}

fn send_staged_reply(item: StagedReply, commit: &Result<(), PersistenceError>) {
    match item {
        StagedReply::Order { reply, result } => {
            let result = match (result, commit) {
                (Ok(value), Ok(())) => Ok(value),
                (Ok(_), Err(error)) => {
                    Err(ServiceError::Persistence(copy_persistence_error(error)))
                }
                (Err(error), _) => Err(error),
            };
            let _ = reply.send(result);
        }
        StagedReply::Cancel { reply, result } => {
            let result = match (result, commit) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(error)) => {
                    Err(ServiceError::Persistence(copy_persistence_error(error)))
                }
                (Err(error), _) => Err(error),
            };
            let _ = reply.send(result);
        }
    }
}

fn copy_persistence_error(error: &PersistenceError) -> PersistenceError {
    PersistenceError::Io(std::io::Error::other(error.to_string()))
}

/// Pins the matching thread when the `cpu-affinity` feature is on.
fn pin_matching_thread(core_index: Option<usize>) {
    #[cfg(feature = "cpu-affinity")]
    {
        let Some(core_index) = core_index else {
            return;
        };
        let Some(cores) = core_affinity::get_core_ids() else {
            tracing::warn!("core affinity is not supported on this platform");
            return;
        };
        let Some(core_id) = cores.get(core_index) else {
            tracing::warn!(
                core_index,
                cores = cores.len(),
                "matching core is out of range"
            );
            return;
        };
        if core_affinity::set_for_current(*core_id) {
            tracing::info!(core_index, "matching thread pinned");
        } else {
            tracing::warn!(core_index, "failed to pin matching thread");
        }
    }

    #[cfg(not(feature = "cpu-affinity"))]
    {
        let _ = core_index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;
    use crate::infrastructure::persistence::PersistenceConfig;
    use crate::shared::protocol::OrderType;
    use tempfile::TempDir;

    fn start_runtime() -> (TempDir, MatchingRuntime) {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let service = PersistentMatchingService::new(spec, config).unwrap();
        let runtime = MatchingRuntime::start(service, RuntimeConfig::default()).unwrap();
        (temp_dir, runtime)
    }

    fn order(user_id: u64, client_order_id: u64, side: OrderType) -> NewOrderRequest {
        NewOrderRequest {
            user_id,
            client_order_id,
            symbol: crate::shared::SymbolId::intern("BTCUSDT"),
            order_type: side,
            price: 50_000,
            quantity: 5,
        }
    }

    #[test]
    fn test_async_fsync_ack_survives_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: true,
            ..Default::default()
        };
        let service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
        let runtime = MatchingRuntime::start(
            service,
            RuntimeConfig {
                async_fsync: true,
                ..Default::default()
            },
        )
        .unwrap();
        let order_id = runtime
            .handle()
            .submit_order(order(1, 1, OrderType::Buy))
            .expect("order")
            .1
            .expect("rests")
            .order_id;
        runtime.shutdown().expect("shutdown");

        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert!(recovered.orderbook().order_owner(order_id).is_some());
    }

    #[test]
    fn test_decoupled_journaler_ack_survives_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: true,
            ..Default::default()
        };
        let service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
        let runtime = MatchingRuntime::start(
            service,
            RuntimeConfig {
                decouple_wal: true,
                ..Default::default()
            },
        )
        .unwrap();
        let order_id = runtime
            .handle()
            .submit_order(order(1, 1, OrderType::Buy))
            .expect("order")
            .1
            .expect("rests")
            .order_id;
        runtime.shutdown().expect("shutdown");

        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert!(recovered.orderbook().order_owner(order_id).is_some());
    }

    #[test]
    fn test_decoupled_snapshotter_writes_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: true,
            snapshot_every_records: 2,
            ..Default::default()
        };
        let service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
        let runtime = MatchingRuntime::start(
            service,
            RuntimeConfig {
                decouple_wal: true,
                ..Default::default()
            },
        )
        .unwrap();
        let handle = runtime.handle();
        handle
            .submit_order(order(1, 1, OrderType::Buy))
            .expect("first");
        handle
            .submit_order(order(2, 2, OrderType::Buy))
            .expect("second");
        runtime.shutdown().expect("shutdown");

        assert!(temp_dir.path().join("snapshot.bin").exists());
        assert!(
            temp_dir.path().join("wal.start").exists(),
            "snapshotter must trigger WAL compaction"
        );
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert!(recovered.orderbook().order_owner(1).is_some());
        assert!(recovered.orderbook().order_owner(2).is_some());
    }

    #[test]
    fn test_async_fsync_snapshot_barrier() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: true,
            snapshot_every_records: 2,
            ..Default::default()
        };
        let service = PersistentMatchingService::new(spec.clone(), config.clone()).unwrap();
        let runtime = MatchingRuntime::start(
            service,
            RuntimeConfig {
                async_fsync: true,
                ..Default::default()
            },
        )
        .unwrap();
        let handle = runtime.handle();
        handle
            .submit_order(order(1, 1, OrderType::Buy))
            .expect("first order");
        handle
            .submit_order(order(2, 2, OrderType::Buy))
            .expect("second order");
        runtime.shutdown().expect("shutdown");

        assert!(temp_dir.path().join("snapshot.bin").exists());
        assert!(
            temp_dir.path().join("wal.start").exists(),
            "async snapshot must rotate and advance the WAL manifest"
        );
        let recovered = PersistentMatchingService::recover(spec, config).unwrap();
        assert!(recovered.orderbook().order_owner(1).is_some());
        assert!(recovered.orderbook().order_owner(2).is_some());
    }

    #[test]
    fn test_busy_poll_worker_accepts_orders() {
        let temp_dir = TempDir::new().unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let config = PersistenceConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync: false,
            ..Default::default()
        };
        let service = PersistentMatchingService::new(spec, config).unwrap();
        let runtime = MatchingRuntime::start(
            service,
            RuntimeConfig {
                busy_poll: true,
                ..Default::default()
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let confirmation = handle
            .submit_order(order(1, 1, OrderType::Buy))
            .expect("order")
            .1
            .expect("rests");
        assert_eq!(confirmation.order_id, 1);
        runtime.shutdown().expect("clean shutdown");
    }

    #[test]
    fn test_submit_and_cancel_through_handle() {
        let (_temp_dir, runtime) = start_runtime();
        let handle = runtime.handle();

        let confirmation = handle
            .submit_order(order(1, 1, OrderType::Buy))
            .expect("order submits")
            .1
            .expect("order rests");
        assert_eq!(confirmation.order_id, 1);
        assert!(handle.is_alive());

        handle
            .submit_cancel(CancelOrderRequest {
                user_id: 1,
                order_id: confirmation.order_id,
            })
            .expect("cancel succeeds");

        runtime.shutdown().expect("clean shutdown");
    }

    #[test]
    fn test_async_tickets_can_be_pipelined() {
        let (_temp_dir, runtime) = start_runtime();
        let handle = runtime.handle();
        let first = handle
            .submit_order_ticket(order(1, 1, OrderType::Buy))
            .expect("first ticket");
        let second = handle
            .submit_order_ticket(order(2, 2, OrderType::Buy))
            .expect("second ticket");

        let first_conf = first.wait().expect("first reply").1.expect("rest");
        let second_conf = second.wait().expect("second reply").1.expect("rest");
        assert_ne!(first_conf.order_id, second_conf.order_id);
        runtime.shutdown().expect("clean shutdown");
    }

    #[test]
    fn test_duplicate_order_is_idempotent_through_handle() {
        let (_temp_dir, runtime) = start_runtime();
        let handle = runtime.handle();
        let first = handle
            .submit_order(order(1, 7, OrderType::Buy))
            .expect("first order")
            .1
            .expect("rests");
        let second = handle
            .submit_order(order(1, 7, OrderType::Buy))
            .expect("duplicate")
            .1
            .expect("cached confirmation");
        assert_eq!(first.order_id, second.order_id);
        runtime.shutdown().expect("clean shutdown");
    }
}
