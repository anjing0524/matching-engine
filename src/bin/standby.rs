//! Warm standby: receives committed WAL batches from a primary engine.
//!
//! The standby only persists the WAL. Promotion is manual: stop the standby,
//! start `matching-engine` with the standby data directory, and recovery replays
//! the shipped records. This is not consensus and does not catch up old WAL;
//! seed the standby directory offline when attaching to an existing primary.
use clap::Parser;
use matching_engine::infrastructure::persistence::{
    Journal, PersistenceConfig, ReplicationBatch, replication::read_batch,
};
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "standby")]
struct Config {
    /// Primary replication Unix socket path.
    #[arg(long)]
    primary_uds: PathBuf,
    /// Standby WAL data directory.
    #[arg(long, default_value = "./standby-data")]
    data_dir: PathBuf,
    /// fsync the standby WAL before continuing.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    wal_sync: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("standby error: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let config = Config::parse();
    let persistence = PersistenceConfig {
        data_dir: config.data_dir.clone(),
        wal_sync: config.wal_sync,
        ..Default::default()
    };
    let mut journal =
        Journal::create(&persistence).map_err(|error| io::Error::other(error.to_string()))?;
    println!(
        "standby data={} primary={}",
        config.data_dir.display(),
        config.primary_uds.display()
    );

    let mut stream = connect(&config.primary_uds)?;
    let mut last_seq = 0u64;
    let mut batches = 0u64;
    loop {
        let batch = read_batch(&mut stream).map_err(|error| io::Error::other(error.to_string()))?;
        let Some(ReplicationBatch { records }) = batch else {
            break;
        };
        for record in &records {
            journal
                .append_record(record)
                .map_err(|error| io::Error::other(error.to_string()))?;
            last_seq = last_seq.max(record.seq);
        }
        journal
            .commit()
            .map_err(|error| io::Error::other(error.to_string()))?;
        stream
            .write_all(&last_seq.to_be_bytes())
            .map_err(|error| io::Error::other(error.to_string()))?;
        batches += 1;
        if batches.is_multiple_of(100) {
            println!("standby batches={batches} last_seq={last_seq}");
        }
    }
    println!("standby primary closed: batches={batches} last_seq={last_seq}");
    Ok(())
}

fn connect(path: &PathBuf) -> io::Result<UnixStream> {
    let mut last = None;
    for _ in 0..300 {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                last = Some(error);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "primary unavailable")))
}
