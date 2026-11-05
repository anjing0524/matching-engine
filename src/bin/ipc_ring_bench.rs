//! Microbenchmark for the file-backed SPSC ring.
use matching_engine::shared::spsc_ring::{PushError, SpscRing};
use std::time::Instant;

fn main() {
    let path = std::env::temp_dir().join(format!(
        "matching-engine-spsc-bench-{}.ring",
        std::process::id()
    ));
    let producer = SpscRing::create(&path, 65_536, 256).expect("create ring");
    let consumer = SpscRing::open(&path).expect("open ring");
    let payload = [0x5au8; 128];
    let iterations = 5_000_000u64;
    let mut out = Vec::with_capacity(128);
    let started = Instant::now();
    for _ in 0..iterations {
        loop {
            match producer.try_push(&payload) {
                Ok(()) => break,
                Err(PushError::Full) => std::hint::spin_loop(),
                Err(PushError::TooLarge) => panic!("payload too large"),
            }
        }
        while !consumer.try_pop_into(&mut out) {
            std::hint::spin_loop();
        }
    }
    let elapsed = started.elapsed();
    let ns = elapsed.as_nanos() as f64 / iterations as f64;
    println!(
        "spsc ring: {iterations} roundtrips, {:.1} ns/push+pop, {:.1} Mmsg/s",
        ns,
        iterations as f64 / elapsed.as_secs_f64() / 1e6
    );
    let _ = std::fs::remove_file(path);
}
