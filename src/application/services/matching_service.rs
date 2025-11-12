/// Matching Service - Single-threaded Matching Engine
///
/// This service coordinates order matching operations using the domain layer's
/// orderbook implementation. It handles command processing and output distribution.
///
/// ## Architecture
/// - Receives commands via MPSC channel (NewOrder, CancelOrder)
/// - Processes orders sequentially using the orderbook
/// - Sends results (Trades, Confirmations) via output channel
///
/// ## Usage
/// ```rust
/// use matching_engine::application::services::MatchingService;
/// use tokio::sync::mpsc;
///
/// let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
/// let (out_tx, out_rx) = mpsc::unbounded_channel();
///
/// let mut service = MatchingService::new(cmd_rx, out_tx);
/// service.run();
/// ```

use crate::orderbook::OrderBook; // TODO: Replace with domain layer trait
use crate::shared::protocol::{CancelOrderRequest, NewOrderRequest, OrderConfirmation, TradeNotification};
use crate::shared::timestamp::get_fast_timestamp;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Commands that the matching engine can receive
#[derive(Debug)]
pub enum EngineCommand {
    NewOrder(NewOrderRequest),
    CancelOrder(CancelOrderRequest),
}

/// Output results from the matching engine
#[derive(Debug)]
pub enum EngineOutput {
    Trade(TradeNotification),
    Confirmation(OrderConfirmation),
}

/// Single-threaded Matching Service
///
/// Processes orders sequentially using a single orderbook instance.
/// Suitable for single-symbol or low-throughput scenarios.
pub struct MatchingService {
    orderbook: OrderBook,
    command_receiver: UnboundedReceiver<EngineCommand>,
    output_sender: UnboundedSender<EngineOutput>,
    next_trade_id: u64,
}

impl MatchingService {
    /// Creates a new matching service
    ///
    /// # Arguments
    /// * `command_receiver` - Channel to receive commands
    /// * `output_sender` - Channel to send results
    pub fn new(
        command_receiver: UnboundedReceiver<EngineCommand>,
        output_sender: UnboundedSender<EngineOutput>,
    ) -> Self {
        MatchingService {
            orderbook: OrderBook::new(),
            command_receiver,
            output_sender,
            next_trade_id: 1,
        }
    }

    /// Runs the main event loop
    ///
    /// This method blocks and processes commands until the channel is closed.
    /// It's the primary entry point for the matching service.
    pub fn run(&mut self) {
        println!("撮合引擎启动...");
        while let Some(command) = self.command_receiver.blocking_recv() {
            match command {
                EngineCommand::NewOrder(request) => {
                    self.process_new_order(request);
                }
                EngineCommand::CancelOrder(request) => {
                    self.process_cancel_order(request);
                }
            }
        }
        println!("撮合引擎关闭。");
    }

    /// Processes a new order request
    #[inline]
    fn process_new_order(&mut self, request: NewOrderRequest) {
        let (trades, confirmation_opt) = self.orderbook.match_order(request);

        // Batch timestamp generation - use cached timestamp for performance
        let timestamp = get_fast_timestamp();

        for mut trade in trades {
            trade.trade_id = self.next_trade_id;
            trade.timestamp = timestamp;
            self.next_trade_id += 1;

            // Send trade notification
            if self.output_sender.send(EngineOutput::Trade(trade)).is_err() {
                eprintln!("输出通道已关闭，无法发送成交回报");
            }
        }

        if let Some(confirmation) = confirmation_opt {
            // If order is not fully matched, send confirmation for the resting order
            if self.output_sender.send(EngineOutput::Confirmation(confirmation)).is_err() {
                eprintln!("输出通道已关闭，无法发送订单确认");
            }
        }
    }

    /// Processes a cancel order request
    #[inline]
    fn process_cancel_order(&mut self, request: CancelOrderRequest) {
        // TODO: Implement cancel order logic in orderbook
        // self.orderbook.cancel_order(request.order_id);
        println!("收到取消订单请求: {:?}", request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    #[test]
    fn test_matching_service_creation() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (out_tx, _out_rx) = mpsc::unbounded_channel();

        let service = MatchingService::new(cmd_rx, out_tx);
        assert_eq!(service.next_trade_id, 1);

        drop(cmd_tx); // Close channel to prevent blocking
    }

    #[test]
    fn test_matching_service_basic_match() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();

        let mut service = MatchingService::new(cmd_rx, out_tx);

        // Send buy order
        cmd_tx.send(EngineCommand::NewOrder(NewOrderRequest {
            user_id: 1,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Buy,
            price: 50000,
            quantity: 10,
        })).unwrap();

        // Send sell order that matches
        cmd_tx.send(EngineCommand::NewOrder(NewOrderRequest {
            user_id: 2,
            symbol: Arc::from("BTC/USD"),
            order_type: OrderType::Sell,
            price: 50000,
            quantity: 10,
        })).unwrap();

        drop(cmd_tx); // Close command channel

        // Run service in background
        std::thread::spawn(move || {
            service.run();
        });

        // Verify we get confirmation and trade
        let mut confirmation_count = 0;
        let mut trade_count = 0;

        while let Some(output) = out_rx.blocking_recv() {
            match output {
                EngineOutput::Confirmation(_) => confirmation_count += 1,
                EngineOutput::Trade(_) => trade_count += 1,
            }
        }

        assert_eq!(confirmation_count, 1, "Should have 1 confirmation (buy order)");
        assert_eq!(trade_count, 1, "Should have 1 trade");
    }
}
