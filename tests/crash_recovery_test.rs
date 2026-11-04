//! Crash-recovery scenarios.
//!
//! The service flushes the WAL before acknowledging an order, so every
//! acknowledged operation must survive a crash. A crash during the final append
//! may leave a torn tail; recovery drops only those incomplete bytes and refuses
//! to start on real corruption.
use matching_engine::application::services::PersistentMatchingService;
use matching_engine::domain::orderbook::ContractSpec;
use matching_engine::infrastructure::persistence::PersistenceConfig;
use matching_engine::shared::protocol::{CancelOrderRequest, NewOrderRequest, OrderType};
use tempfile::TempDir;

fn config(dir: &TempDir) -> PersistenceConfig {
    PersistenceConfig {
        data_dir: dir.path().to_path_buf(),
        wal_sync: false,
        ..Default::default()
    }
}

fn spec() -> ContractSpec {
    ContractSpec::new("BTCUSDT", 1, 40_000, 70_000)
}

fn order(user_id: u64, side: OrderType, price: u64, quantity: u64) -> NewOrderRequest {
    NewOrderRequest {
        user_id,
        client_order_id: 0,
        symbol: matching_engine::shared::SymbolId::intern("BTCUSDT"),
        order_type: side,
        price,
        quantity,
    }
}

#[test]
fn test_acknowledged_orders_survive_restart() {
    let temp_dir = TempDir::new().unwrap();

    {
        let mut service = PersistentMatchingService::new(spec(), config(&temp_dir)).unwrap();
        for i in 0..100 {
            let side = if i % 2 == 0 {
                OrderType::Buy
            } else {
                OrderType::Sell
            };
            service
                .match_order(order(i, side, 50_000 + (i % 10), 10))
                .unwrap();
        }
        // No explicit flush: each acknowledged order was already flushed.
    }

    let recovered = PersistentMatchingService::recover(spec(), config(&temp_dir)).unwrap();
    let book = recovered.orderbook();
    assert!(
        book.best_bid().is_some() || book.best_ask().is_some(),
        "acknowledged orders must survive a crash"
    );
}

#[test]
fn test_cancel_survives_restart() {
    let temp_dir = TempDir::new().unwrap();

    {
        let mut service = PersistentMatchingService::new(spec(), config(&temp_dir)).unwrap();
        let (_, confirmation) = service
            .match_order(order(7, OrderType::Buy, 50_000, 10))
            .unwrap();
        let order_id = confirmation.expect("resting order").order_id;
        service
            .cancel_order(CancelOrderRequest {
                user_id: 7,
                order_id,
            })
            .unwrap();
    }

    let recovered = PersistentMatchingService::recover(spec(), config(&temp_dir)).unwrap();
    assert!(recovered.orderbook().best_bid().is_none());
}

#[test]
fn test_torn_tail_is_dropped() {
    let temp_dir = TempDir::new().unwrap();

    {
        let mut service = PersistentMatchingService::new(spec(), config(&temp_dir)).unwrap();
        for i in 0..3 {
            service
                .match_order(order(i, OrderType::Buy, 50_000 + i, 10))
                .unwrap();
        }
    }

    let path = temp_dir.path().join("wal.000000.log");
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let len = file.metadata().unwrap().len();
    file.set_len(len - 3).unwrap();
    drop(file);

    let recovered = PersistentMatchingService::recover(spec(), config(&temp_dir)).unwrap();
    assert!(
        recovered.orderbook().best_bid().is_some(),
        "complete prefix of the WAL must be recovered"
    );
}

#[test]
fn test_corrupt_entry_aborts_recovery() {
    let temp_dir = TempDir::new().unwrap();

    {
        let mut service = PersistentMatchingService::new(spec(), config(&temp_dir)).unwrap();
        for i in 0..3 {
            service
                .match_order(order(i, OrderType::Buy, 50_000 + i, 10))
                .unwrap();
        }
    }

    let path = temp_dir.path().join("wal.000000.log");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    use std::io::{Seek, SeekFrom, Write};
    // Flip a byte inside the first entry's payload: CRC validation must fail.
    file.seek(SeekFrom::Start(8 + 4 + 8 + 8 + 4 + 2)).unwrap();
    file.write_all(&[0xFF]).unwrap();
    file.flush().unwrap();
    drop(file);

    let result = PersistentMatchingService::recover(spec(), config(&temp_dir));
    assert!(result.is_err(), "corruption must abort recovery");
}
