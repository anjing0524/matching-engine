//! WAL shipping server for a warm standby.
//!
//! The primary journaler broadcasts committed batches to connected standby
//! processes. A slow standby is disconnected rather than backpressuring the
//! matching path. This is a warm-standby transport, not consensus: catch-up
//! from a pre-existing primary WAL requires an offline data-directory copy.
use crate::infrastructure::persistence::replication::{encode_batch, write_batch};
use crate::infrastructure::persistence::wal::WalReader;
use crate::infrastructure::persistence::{
    ReplicationBatch, Snapshot, read_start_file, replication::encode_bootstrap,
};
use crate::shared::metrics::METRICS;
use crossbeam::channel::{Receiver, Sender, bounded};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

struct Subscriber {
    sender: Sender<Vec<u8>>,
    acked_seq: Arc<AtomicU64>,
}

/// Starts the replication listener in a background thread.
///
/// # Errors
/// Returns an error if the Unix socket cannot be bound.
pub fn start(
    path: PathBuf,
    receiver: Receiver<ReplicationBatch>,
    data_dir: PathBuf,
    shutdown: Arc<AtomicBool>,
) -> io::Result<thread::JoinHandle<()>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    thread::Builder::new()
        .name("wal-replication".to_string())
        .spawn(move || {
            let mut subscribers: Vec<Subscriber> = Vec::new();
            let sent_seq = Arc::new(AtomicU64::new(0));
            while !shutdown.load(Ordering::Acquire) {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // Accepted sockets may inherit nonblocking mode from
                            // the listener; the writer/ack threads use blocking IO.
                            let _ = stream.set_nonblocking(false);
                            let Ok(bootstrap) = bootstrap_frame(&data_dir) else {
                                tracing::error!("failed to build standby bootstrap");
                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                continue;
                            };
                            let (sender, receiver) = bounded(64);
                            let Ok(ack_reader) = stream.try_clone() else {
                                continue;
                            };
                            let acked_seq = Arc::new(AtomicU64::new(0));
                            let ack_state = Arc::clone(&acked_seq);
                            let sent_state = Arc::clone(&sent_seq);
                            if thread::Builder::new()
                                .name("replication-writer".to_string())
                                .spawn(move || writer(stream, receiver))
                                .is_ok()
                            {
                                let _ = thread::Builder::new()
                                    .name("replication-ack".to_string())
                                    .spawn(move || read_acks(ack_reader, ack_state, sent_state));
                                if sender.try_send(bootstrap).is_ok() {
                                    subscribers.push(Subscriber { sender, acked_seq });
                                    METRICS.replication_connected.set(subscribers.len() as i64);
                                }
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            tracing::error!(%error, "replication accept failed");
                            break;
                        }
                    }
                }

                match receiver.recv_timeout(Duration::from_millis(5)) {
                    Ok(batch) => {
                        if batch.records.is_empty() {
                            continue;
                        }
                        let Ok(encoded) = encode_batch(&batch) else {
                            continue;
                        };
                        let sent = batch
                            .records
                            .last()
                            .map_or(sent_seq.load(Ordering::Acquire), |record| record.seq);
                        sent_seq.store(sent, Ordering::Release);
                        let before = subscribers.len();
                        subscribers.retain(|subscriber| {
                            matches!(subscriber.sender.try_send(encoded.clone()), Ok(()))
                        });
                        if subscribers.len() < before {
                            METRICS
                                .replication_dropped_batches
                                .inc_by((before - subscribers.len()) as u64);
                        }
                        METRICS.replication_connected.set(subscribers.len() as i64);
                        let lag = subscribers
                            .iter()
                            .map(|subscriber| subscriber.acked_seq.load(Ordering::Acquire))
                            .min()
                            .map_or(0, |acked| sent.saturating_sub(acked));
                        METRICS.replication_lag_records.set(lag as i64);
                    }
                    Err(crossbeam::channel::RecvTimeoutError::Timeout) => {
                        let lag = subscribers
                            .iter()
                            .map(|subscriber| subscriber.acked_seq.load(Ordering::Acquire))
                            .min()
                            .map_or(0, |acked| {
                                sent_seq.load(Ordering::Acquire).saturating_sub(acked)
                            });
                        METRICS.replication_lag_records.set(lag as i64);
                    }
                    Err(crossbeam::channel::RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = std::fs::remove_file(&path);
        })
}

fn bootstrap_frame(data_dir: &std::path::Path) -> io::Result<Vec<u8>> {
    let snapshot = std::fs::read(data_dir.join("snapshot.bin")).unwrap_or_default();
    let snapshot_seq = Snapshot::load(data_dir)
        .map_err(|error| io::Error::other(error.to_string()))?
        .map(|snapshot| snapshot.last_seq);
    let start = read_start_file(data_dir).map_err(|error| io::Error::other(error.to_string()))?;
    if snapshot_seq.is_none() && start > 0 {
        return Err(io::Error::other(
            "cannot bootstrap standby: WAL prefix was pruned and no snapshot exists",
        ));
    }
    let scan = WalReader::new(data_dir.to_path_buf())
        .scan_from(snapshot_seq)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if let Some(seq) = snapshot_seq
        && let Some(first) = scan.records.first()
        && first.seq != seq + 1
    {
        return Err(io::Error::other(format!(
            "standby bootstrap WAL gap: snapshot seq {seq}, first tail seq {}",
            first.seq
        )));
    }
    encode_bootstrap(&snapshot, &scan.records).map_err(|error| io::Error::other(error.to_string()))
}

fn read_acks(mut stream: UnixStream, acked_seq: Arc<AtomicU64>, sent_seq: Arc<AtomicU64>) {
    let mut buf = [0u8; 8];
    while stream.read_exact(&mut buf).is_ok() {
        let acked = u64::from_be_bytes(buf);
        acked_seq.store(acked, Ordering::Release);
        let sent = sent_seq.load(Ordering::Acquire);
        METRICS
            .replication_lag_records
            .set(sent.saturating_sub(acked) as i64);
    }
}

fn writer(mut stream: UnixStream, receiver: Receiver<Vec<u8>>) {
    while let Ok(bytes) = receiver.recv() {
        if stream.write_all(&bytes).is_err() {
            break;
        }
    }
}

/// Convenience for a standby: writes one received batch.
pub fn write_batch_to<W: std::io::Write>(
    writer: &mut W,
    batch: &ReplicationBatch,
) -> io::Result<()> {
    write_batch(writer, batch).map_err(|error| io::Error::other(error.to_string()))
}
