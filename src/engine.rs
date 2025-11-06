use crate::orderbook::OrderBook;
use crate::protocol::{CancelOrderRequest, NewOrderRequest, OrderConfirmation, TradeNotification};
use crate::timestamp::get_fast_timestamp;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

// 定义引擎可以接收的命令
pub enum EngineCommand {
    NewOrder(NewOrderRequest),
    CancelOrder(CancelOrderRequest),
}

// 定义引擎的输出结果
pub enum EngineOutput {
    Trade(TradeNotification),
    Confirmation(OrderConfirmation),
}

// 撮合引擎
pub struct MatchingEngine {
    orderbook: OrderBook,
    command_receiver: UnboundedReceiver<EngineCommand>,
    output_sender: UnboundedSender<EngineOutput>,
    next_trade_id: u64,
}

impl MatchingEngine {
    pub fn new(
        command_receiver: UnboundedReceiver<EngineCommand>,
        output_sender: UnboundedSender<EngineOutput>,
    ) -> Self {
        MatchingEngine {
            orderbook: OrderBook::new(),
            command_receiver,
            output_sender,
            next_trade_id: 1,
        }
    }

    // 引擎的主事件循环
    pub fn run(&mut self) {
        println!("撮合引擎启动...");
        while let Some(command) = self.command_receiver.blocking_recv() {
            match command {
                EngineCommand::NewOrder(request) => {
                    let (trades, confirmation_opt) = self.orderbook.match_order(request);

                    // Batch timestamp generation - use cached timestamp for performance
                    let timestamp = get_fast_timestamp();

                    for mut trade in trades {
                        trade.trade_id = self.next_trade_id;
                        trade.timestamp = timestamp;
                        self.next_trade_id += 1;
                        // 将成交结果发送出去
                        if self.output_sender.send(EngineOutput::Trade(trade)).is_err() {
                            eprintln!("输出通道已关闭，无法发送成交回报");
                        }
                    }

                    if let Some(confirmation) = confirmation_opt {
                        // 如果订单未完全成交，会有一个新挂单
                        // 发送这个新挂单的确认信息
                        if self.output_sender.send(EngineOutput::Confirmation(confirmation)).is_err() {
                            eprintln!("输出通道已关闭，无法发送订单确认");
                        }
                    }
                }
                EngineCommand::CancelOrder(request) => {
                    // TODO: 实现取消订单逻辑
                    // self.orderbook.remove_order(request.order_id);
                    println!("收到取消订单请求: {:?}", request);
                }
            }
        }
        println!("撮合引擎关闭。");
    }
}