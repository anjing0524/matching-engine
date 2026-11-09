//! Real-NIC multicast smoke test (ignored by default).
//!
//! `EXCHANGE_MCAST_IFACE=10.92.252.146 cargo test --release --test multicast_nic -- --ignored --nocapture`

use matching_engine::exchange::market_data::{MarketDataPublisher, MarketDataSubscriber};
use matching_engine::exchange::public_events::{PublicEvent, PublicEventType};
use std::time::Duration;

fn event() -> PublicEvent {
    PublicEvent {
        seq: 1,
        event_type: PublicEventType::Trade,
        exchange_ts_ns: 2,
        phase: 3,
        instrument_id: 1,
        order_id: 0,
        trade_id: 1,
        buy_order_id: 0,
        sell_order_id: 0,
        side: None,
        price_ticks: 50_000,
        quantity: 1,
        volume: 0,
    }
}

#[test]
#[ignore = "requires EXCHANGE_MCAST_IFACE with a multicast-capable NIC"]
fn publishes_on_configured_interface() {
    let interface = std::env::var("EXCHANGE_MCAST_IFACE").expect("EXCHANGE_MCAST_IFACE");
    let interface: std::net::Ipv4Addr = interface.parse().expect("IPv4 interface");
    let group = "239.255.42.97";
    let subscriber = MarketDataSubscriber::bind_multicast_on(0, group, interface).unwrap();
    let port = subscriber.local_addr().unwrap().port();
    let publisher =
        MarketDataPublisher::bind_multicast_on("0.0.0.0:0", group, port, interface).unwrap();
    publisher.send(&event()).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(subscriber.recv().unwrap(), event());
}
