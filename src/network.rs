use crate::engine::{EngineCommand, EngineOutput};
use crate::protocol::{CancelOrderRequest, NewOrderRequest};
use futures::stream::StreamExt;
use futures::SinkExt;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

// 启动网络服务器
pub async fn run_server(
    addr: SocketAddr,
    command_sender: mpsc::UnboundedSender<EngineCommand>,
    mut output_receiver: mpsc::UnboundedReceiver<EngineOutput>,
) {
    let listener = TcpListener::bind(&addr).await.expect("无法绑定地址");
    println!("服务器正在监听: {}", addr);

    // 创建一个广播通道用于分发引擎的输出
    let (broadcast_tx, _) = broadcast::channel::<String>(1024);

    // 这个任务负责将引擎的输出广播给所有连接的客户端
    let broadcaster_tx_clone = broadcast_tx.clone();
    tokio::spawn(async move {
        while let Some(output) = output_receiver.recv().await {
            let msg = match output {
                EngineOutput::Trade(trade) => serde_json::to_string(&trade).unwrap(),
                EngineOutput::Confirmation(conf) => serde_json::to_string(&conf).unwrap(),
            };
            if broadcaster_tx_clone.send(msg).is_err() {
                // 当没有客户端连接时，发送会失败，这是正常现象
            }
        }
    });

    while let Ok((stream, _)) = listener.accept().await {
        println!("接受新连接: {}", stream.peer_addr().unwrap());
        let command_sender_clone = command_sender.clone();
        let broadcast_rx = broadcast_tx.subscribe();

        tokio::spawn(async move {
            handle_connection(stream, command_sender_clone, broadcast_rx).await;
        });
    }
}

// 处理单个客户端连接
async fn handle_connection(
    stream: TcpStream,
    command_sender: mpsc::UnboundedSender<EngineCommand>,
    mut broadcast_rx: broadcast::Receiver<String>,
) {
    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());

    loop {
        tokio::select! {
            // 从客户端接收数据
            result = framed.next() => {
                match result {
                    Some(Ok(data)) => {
                        let data_str = String::from_utf8_lossy(&data);
                        if let Ok(new_order) = serde_json::from_str::<NewOrderRequest>(&data_str) {
                            if command_sender.send(EngineCommand::NewOrder(new_order)).is_err() {
                                eprintln!("命令通道已关闭");
                                break;
                            }
                        } else if let Ok(cancel_order) = serde_json::from_str::<CancelOrderRequest>(&data_str) {
                            if command_sender.send(EngineCommand::CancelOrder(cancel_order)).is_err() {
                                eprintln!("命令通道已关闭");
                                break;
                            }
                        } else {
                            println!("无法解析的请求: {}", data_str);
                        }
                    }
                    Some(Err(e)) => {
                        println!("处理连接时出错: {}", e);
                        break;
                    }
                    None => break, // 连接已关闭
                }
            }
            // 从广播通道接收数据并发送给客户端
            Ok(msg) = broadcast_rx.recv() => {
                if framed.send(msg.into()).await.is_err() {
                    println!("发送数据到客户端失败");
                    break;
                }
            }
        }
    }
    println!("连接 {} 已关闭", framed.get_ref().peer_addr().unwrap());
}