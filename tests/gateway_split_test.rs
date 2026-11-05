//! End-to-end test for the gateway → engine process split.
use matching_engine::shared::protocol::{
    CancelOrderRequest, ClientMessage, ClientRequest, NewOrderRequest, OrderType, PROTOCOL_VERSION,
    ServerMessage, ServerResponse,
};
use matching_engine::shared::wire::{decode, encode_frame};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_path(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn connect(addr: &str) -> TcpStream {
    let started = Instant::now();
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return stream,
            Err(error) if started.elapsed() < Duration::from_secs(8) => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("timed out connecting to gateway: {error}"),
        }
    }
}

fn read_response(stream: &mut TcpStream) -> ServerResponse {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).expect("response length");
    let len = u32::from_be_bytes(len) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).expect("response payload");
    decode::<ServerResponse>(&payload).expect("decode response")
}

#[test]
fn gateway_forwards_orders_to_separate_engine_process() {
    let dir = tempfile::TempDir::new().unwrap();
    let socket = dir.path().join("engine.sock");
    let data = dir.path().join("data");

    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);

    let _engine = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_matching-engine"))
            .args([
                "--engine-uds",
                socket.to_str().unwrap(),
                "--data-dir",
                data.to_str().unwrap(),
                "--snapshot-interval",
                "0",
                "--async-fsync",
                "--metrics-port",
                "0",
                "--log-level",
                "error",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn engine"),
    );
    wait_for_path(&socket);

    let listen = format!("127.0.0.1:{port}");
    let _gateway = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_gateway"))
            .args([
                "--listen",
                &listen,
                "--engine-uds",
                socket.to_str().unwrap(),
                "--max-in-flight",
                "8",
                "--io-threads",
                "2",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn gateway"),
    );

    let mut stream = connect(&listen);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let order = ClientRequest {
        version: PROTOCOL_VERSION,
        request_id: 1,
        message: ClientMessage::NewOrder(NewOrderRequest {
            user_id: 1,
            client_order_id: 1,
            symbol: matching_engine::shared::SymbolId::intern("BTCUSDT"),
            order_type: OrderType::Buy,
            price: 49_999,
            quantity: 2,
        }),
    };
    stream.write_all(&encode_frame(&order).unwrap()).unwrap();
    let response = read_response(&mut stream);
    assert_eq!(response.request_id, 1);
    let order_id = match response.message {
        ServerMessage::Confirmation(confirmation) => confirmation.order_id,
        other => panic!("expected confirmation, got {other:?}"),
    };

    let cancel = ClientRequest {
        version: PROTOCOL_VERSION,
        request_id: 2,
        message: ClientMessage::CancelOrder(CancelOrderRequest {
            user_id: 1,
            order_id,
        }),
    };
    stream.write_all(&encode_frame(&cancel).unwrap()).unwrap();
    let response = read_response(&mut stream);
    assert_eq!(response.request_id, 2);
    assert!(matches!(
        response.message,
        ServerMessage::CancelAcknowledged(_)
    ));
}
