//! WAL shipping server for a warm standby.
//!
//! The primary journaler broadcasts committed batches to connected standby
//! processes. A slow standby is disconnected rather than backpressuring the
//! matching path. This is a warm-standby transport, not consensus: catch-up
//! from a pre-existing primary WAL requires an offline data-directory copy.
use crate::infrastructure::persistence::ReplicationBatch;
use crate::infrastructure::persistence::replication::{encode_batch, write_batch};
use crossbeam::channel::{Receiver, Sender, bounded};
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

struct Subscriber {
    sender: Sender<Vec<u8>>,
}

/// Starts the replication listener in a background thread.
///
/// # Errors
/// Returns an error if the Unix socket cannot be bound.
pub fn start(
    path: PathBuf,
    receiver: Receiver<ReplicationBatch>,
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
            let mut started = false;
            while !shutdown.load(Ordering::Acquire) {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if started {
                                // No catch-up yet: a late standby would silently
                                // miss the earlier WAL prefix.
                                tracing::warn!(
                                    "rejecting late standby connection (no WAL catch-up)"
                                );
                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                continue;
                            }
                            let (sender, receiver) = bounded(64);
                            if thread::Builder::new()
                                .name("replication-writer".to_string())
                                .spawn(move || writer(stream, receiver))
                                .is_ok()
                            {
                                subscribers.push(Subscriber { sender });
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
                        started = true;
                        subscribers.retain(|subscriber| {
                            matches!(subscriber.sender.try_send(encoded.clone()), Ok(()))
                        });
                    }
                    Err(crossbeam::channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam::channel::RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = std::fs::remove_file(&path);
        })
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
