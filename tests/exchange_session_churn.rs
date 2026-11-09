//! Gateway connection churn: active sessions return to zero and closed records
//! remain bounded.

use matching_engine::exchange::gateway::GatewayFrame;
use matching_engine::exchange::gateway_runtime::GatewayRuntime;
use matching_engine::exchange::gateway_server::start;
use matching_engine::exchange::middleware::MiddlewareSequencer;
use matching_engine::exchange::transport::create;
use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[test]
fn reconnect_churn_returns_to_zero_active_sessions() {
    let path = std::env::temp_dir().join(format!("exchange-session-churn-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (producer, _consumer) = create(&path, 8).unwrap();
    let middleware = MiddlewareSequencer::new(producer);
    let (server, events) = start("127.0.0.1:0").unwrap();
    let addr = server.addr();
    let mut runtime = GatewayRuntime::new(middleware, events);

    let login = GatewayFrame::Login {
        member_id: 1,
        user_id: 1,
        seat_id: 1,
        session_epoch: 1,
        last_private_seq: 0,
        auth_token: 0,
    };
    for _ in 0..500 {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(&login.clone().encode()).unwrap();
        runtime.poll();
        drop(stream);
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while runtime.session_counts().0 > 0 && Instant::now() < deadline {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(runtime.session_counts().0, 0);
    runtime.prune_closed_sessions(16);
    assert!(runtime.session_counts().1 <= 16);
    let _ = std::fs::remove_file(&path);
}
