//! Gateway wire frames.
//!
//! One fixed 264-byte frame carries login, command, or heartbeat messages
//! between a gateway and middleware. TCP transport and session lifecycle sit
//! above this codec; this slice only fixes the byte layout.

use crate::trading::core::combo_book::ComboPrivateFill;
use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};
use crate::trading::core::protocol::{COMMAND_SLOT_SIZE, Command};

pub const GATEWAY_FRAME_SIZE: usize = 264;
const TAG_LOGIN: u8 = 1;
const TAG_COMMAND: u8 = 2;
const TAG_HEARTBEAT: u8 = 3;
const TAG_PRIVATE_EVENT: u8 = 4;
const TAG_COMBO_PRIVATE: u8 = 5;
const TAG_ACK: u8 = 6;
const TAG_REDIRECT: u8 = 7;
const REDIRECT_MAX: usize = 128;
const PAYLOAD_OFFSET: usize = 4;
const PRIVATE_REASON_MAX: usize = 48;

/// One gateway→middleware frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayFrame {
    Login {
        member_id: u32,
        user_id: u32,
        seat_id: u32,
        session_epoch: u32,
        last_private_seq: u64,
        auth_token: u64,
    },
    Command(Command),
    Heartbeat {
        seq: u64,
    },
    PrivateEvent(PrivateEvent),
    ComboPrivate(ComboPrivateFill),
    Ack {
        private_seq: u64,
    },
    Redirect {
        address: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GatewayFrameError {
    pub field: &'static str,
    pub value: u8,
}

impl GatewayFrame {
    #[must_use]
    pub fn encode(self) -> [u8; GATEWAY_FRAME_SIZE] {
        let mut out = [0u8; GATEWAY_FRAME_SIZE];
        match self {
            Self::Login {
                member_id,
                user_id,
                seat_id,
                session_epoch,
                last_private_seq,
                auth_token,
            } => {
                out[0] = TAG_LOGIN;
                out[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 4].copy_from_slice(&member_id.to_le_bytes());
                out[PAYLOAD_OFFSET + 4..PAYLOAD_OFFSET + 8].copy_from_slice(&user_id.to_le_bytes());
                out[PAYLOAD_OFFSET + 8..PAYLOAD_OFFSET + 12]
                    .copy_from_slice(&seat_id.to_le_bytes());
                out[PAYLOAD_OFFSET + 12..PAYLOAD_OFFSET + 16]
                    .copy_from_slice(&session_epoch.to_le_bytes());
                out[PAYLOAD_OFFSET + 16..PAYLOAD_OFFSET + 24]
                    .copy_from_slice(&last_private_seq.to_le_bytes());
                out[PAYLOAD_OFFSET + 24..PAYLOAD_OFFSET + 32]
                    .copy_from_slice(&auth_token.to_le_bytes());
            }
            Self::Command(command) => {
                out[0] = TAG_COMMAND;
                out[PAYLOAD_OFFSET..PAYLOAD_OFFSET + COMMAND_SLOT_SIZE]
                    .copy_from_slice(&command.encode());
            }
            Self::Heartbeat { seq } => {
                out[0] = TAG_HEARTBEAT;
                out[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 8].copy_from_slice(&seq.to_le_bytes());
            }
            Self::Redirect { address } => {
                out[0] = TAG_REDIRECT;
                let bytes = address.as_bytes();
                let len = bytes.len().min(REDIRECT_MAX);
                out[PAYLOAD_OFFSET] = len as u8;
                out[PAYLOAD_OFFSET + 1..PAYLOAD_OFFSET + 1 + len].copy_from_slice(&bytes[..len]);
            }
            Self::Ack { private_seq } => {
                out[0] = TAG_ACK;
                out[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 8].copy_from_slice(&private_seq.to_le_bytes());
            }
            Self::ComboPrivate(fill) => {
                out[0] = TAG_COMBO_PRIVATE;
                let at = PAYLOAD_OFFSET;
                out[at..at + 8].copy_from_slice(&fill.seq.to_le_bytes());
                out[at + 8..at + 16].copy_from_slice(&fill.order_id.to_le_bytes());
                out[at + 16..at + 24].copy_from_slice(&fill.user_id.to_le_bytes());
                out[at + 24] = match fill.side {
                    crate::trading::core::protocol::Side::Buy => 1,
                    crate::trading::core::protocol::Side::Sell => 2,
                };
                out[at + 32..at + 40].copy_from_slice(&fill.quantity.to_le_bytes());
                out[at + 40..at + 48].copy_from_slice(&fill.leg1_price_ticks.to_le_bytes());
                out[at + 48..at + 56].copy_from_slice(&fill.leg2_price_ticks.to_le_bytes());
                out[at + 56..at + 64].copy_from_slice(&fill.spread_price.to_le_bytes());
                out[at + 64..at + 68].copy_from_slice(&fill.pair_id.to_le_bytes());
                out[at + 72..at + 80].copy_from_slice(&fill.client_order_id.to_le_bytes());
                out[at + 80..at + 88].copy_from_slice(&fill.session_id.to_le_bytes());
            }
            Self::PrivateEvent(event) => {
                out[0] = TAG_PRIVATE_EVENT;
                let at = PAYLOAD_OFFSET;
                out[at..at + 8].copy_from_slice(&event.seq.to_le_bytes());
                out[at + 8..at + 16].copy_from_slice(&event.order_id.to_le_bytes());
                out[at + 16..at + 24].copy_from_slice(&event.client_order_id.to_le_bytes());
                out[at + 24..at + 32].copy_from_slice(&event.account_id.to_le_bytes());
                out[at + 32] = match event.status {
                    PrivateStatus::Accepted => 1,
                    PrivateStatus::PartiallyFilled => 2,
                    PrivateStatus::Filled => 3,
                    PrivateStatus::Canceled => 4,
                    PrivateStatus::Rejected => 5,
                };
                out[at + 40..at + 48].copy_from_slice(&event.filled_delta.to_le_bytes());
                out[at + 48..at + 56].copy_from_slice(&event.remaining.to_le_bytes());
                if let Some(reason) = &event.reason {
                    let bytes = reason.as_bytes();
                    let len = bytes.len().min(PRIVATE_REASON_MAX);
                    out[at + 56] = len as u8;
                    out[at + 57..at + 57 + len].copy_from_slice(&bytes[..len]);
                }
            }
        }
        out
    }

    pub fn decode(frame: &[u8; GATEWAY_FRAME_SIZE]) -> Result<Self, GatewayFrameError> {
        match frame[0] {
            TAG_LOGIN => Ok(Self::Login {
                member_id: u32::from_le_bytes(
                    frame[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 4]
                        .try_into()
                        .expect("4 bytes"),
                ),
                user_id: u32::from_le_bytes(
                    frame[PAYLOAD_OFFSET + 4..PAYLOAD_OFFSET + 8]
                        .try_into()
                        .expect("4 bytes"),
                ),
                seat_id: u32::from_le_bytes(
                    frame[PAYLOAD_OFFSET + 8..PAYLOAD_OFFSET + 12]
                        .try_into()
                        .expect("4 bytes"),
                ),
                session_epoch: u32::from_le_bytes(
                    frame[PAYLOAD_OFFSET + 12..PAYLOAD_OFFSET + 16]
                        .try_into()
                        .expect("4 bytes"),
                ),
                last_private_seq: u64::from_le_bytes(
                    frame[PAYLOAD_OFFSET + 16..PAYLOAD_OFFSET + 24]
                        .try_into()
                        .expect("8 bytes"),
                ),
                auth_token: u64::from_le_bytes(
                    frame[PAYLOAD_OFFSET + 24..PAYLOAD_OFFSET + 32]
                        .try_into()
                        .expect("8 bytes"),
                ),
            }),
            TAG_COMMAND => {
                let mut slot = [0u8; COMMAND_SLOT_SIZE];
                slot.copy_from_slice(&frame[PAYLOAD_OFFSET..PAYLOAD_OFFSET + COMMAND_SLOT_SIZE]);
                Command::decode(&slot)
                    .map(Self::Command)
                    .map_err(|_| GatewayFrameError {
                        field: "command",
                        value: TAG_COMMAND,
                    })
            }
            TAG_HEARTBEAT => Ok(Self::Heartbeat {
                seq: u64::from_le_bytes(
                    frame[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 8]
                        .try_into()
                        .expect("8 bytes"),
                ),
            }),
            TAG_PRIVATE_EVENT => {
                let at = PAYLOAD_OFFSET;
                let status = match frame[at + 32] {
                    1 => PrivateStatus::Accepted,
                    2 => PrivateStatus::PartiallyFilled,
                    3 => PrivateStatus::Filled,
                    4 => PrivateStatus::Canceled,
                    5 => PrivateStatus::Rejected,
                    value => {
                        return Err(GatewayFrameError {
                            field: "private_status",
                            value,
                        });
                    }
                };
                let reason_len = usize::from(frame[at + 56]).min(PRIVATE_REASON_MAX);
                let reason = if reason_len == 0 {
                    None
                } else {
                    Some(
                        String::from_utf8_lossy(&frame[at + 57..at + 57 + reason_len]).into_owned(),
                    )
                };
                Ok(Self::PrivateEvent(PrivateEvent {
                    seq: u64::from_le_bytes(frame[at..at + 8].try_into().expect("8 bytes")),
                    order_id: u64::from_le_bytes(
                        frame[at + 8..at + 16].try_into().expect("8 bytes"),
                    ),
                    client_order_id: u64::from_le_bytes(
                        frame[at + 16..at + 24].try_into().expect("8 bytes"),
                    ),
                    account_id: u64::from_le_bytes(
                        frame[at + 24..at + 32].try_into().expect("8 bytes"),
                    ),
                    status,
                    filled_delta: u64::from_le_bytes(
                        frame[at + 40..at + 48].try_into().expect("8 bytes"),
                    ),
                    remaining: u64::from_le_bytes(
                        frame[at + 48..at + 56].try_into().expect("8 bytes"),
                    ),
                    reason,
                }))
            }
            TAG_ACK => Ok(Self::Ack {
                private_seq: u64::from_le_bytes(
                    frame[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 8]
                        .try_into()
                        .expect("8 bytes"),
                ),
            }),
            TAG_REDIRECT => {
                let len = usize::from(frame[PAYLOAD_OFFSET]).min(REDIRECT_MAX);
                let address =
                    String::from_utf8_lossy(&frame[PAYLOAD_OFFSET + 1..PAYLOAD_OFFSET + 1 + len])
                        .into_owned();
                Ok(Self::Redirect { address })
            }
            TAG_COMBO_PRIVATE => {
                let at = PAYLOAD_OFFSET;
                let side = match frame[at + 24] {
                    1 => crate::trading::core::protocol::Side::Buy,
                    2 => crate::trading::core::protocol::Side::Sell,
                    value => {
                        return Err(GatewayFrameError {
                            field: "combo_side",
                            value,
                        });
                    }
                };
                Ok(Self::ComboPrivate(ComboPrivateFill {
                    seq: u64::from_le_bytes(frame[at..at + 8].try_into().expect("8 bytes")),
                    pair_id: u32::from_le_bytes(
                        frame[at + 64..at + 68].try_into().expect("4 bytes"),
                    ),
                    order_id: u64::from_le_bytes(
                        frame[at + 8..at + 16].try_into().expect("8 bytes"),
                    ),
                    client_order_id: u64::from_le_bytes(
                        frame[at + 72..at + 80].try_into().expect("8 bytes"),
                    ),
                    user_id: u64::from_le_bytes(
                        frame[at + 16..at + 24].try_into().expect("8 bytes"),
                    ),
                    session_id: u64::from_le_bytes(
                        frame[at + 80..at + 88].try_into().expect("8 bytes"),
                    ),
                    side,
                    quantity: u64::from_le_bytes(
                        frame[at + 32..at + 40].try_into().expect("8 bytes"),
                    ),
                    spread_price: i64::from_le_bytes(
                        frame[at + 56..at + 64].try_into().expect("8 bytes"),
                    ),
                    leg1_price_ticks: u64::from_le_bytes(
                        frame[at + 40..at + 48].try_into().expect("8 bytes"),
                    ),
                    leg2_price_ticks: u64::from_le_bytes(
                        frame[at + 48..at + 56].try_into().expect("8 bytes"),
                    ),
                }))
            }
            value => Err(GatewayFrameError {
                field: "gateway_frame_tag",
                value,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::core::protocol::{
        CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType, VolumeCondition,
    };

    fn command() -> Command {
        Command {
            global_seq: 1,
            gateway_seq: 2,
            ingress_ts_ns: 0,
            trading_ts_ns: 0,
            client_order_id: 3,
            session_id: 4,
            gateway_id: 5,
            member_id: 6,
            user_id: 7,
            seat_id: 8,
            instrument_id: 9,
            combo_id: 0,
            session_epoch: 1,
            command_type: CommandType::NewOrder,
            price_type: PriceType::Limit,
            time_condition: TimeCondition::Gfd,
            volume_condition: VolumeCondition::Any,
            side: Side::Buy,
            offset: Offset::Open,
            hedge: Hedge::Spec,
            flags: 0,
            leg2_ratio: 0,
            price_ticks: 50_000,
            quantity: 1,
            trigger_price_ticks: 0,
            leg2_instrument_id: 0,
            leg2_offset: Offset::Open,
            leg2_hedge: Hedge::Spec,
            trigger_type: TriggerType::None,
            cancel_order_id: 0,
        }
    }

    #[test]
    fn login_roundtrips() {
        let frame = GatewayFrame::Login {
            member_id: 1,
            user_id: 2,
            seat_id: 3,
            session_epoch: 4,
            last_private_seq: 5,
            auth_token: 6,
        };
        assert_eq!(GatewayFrame::decode(&frame.clone().encode()), Ok(frame));
    }

    #[test]
    fn command_roundtrips() {
        let frame = GatewayFrame::Command(command());
        assert_eq!(GatewayFrame::decode(&frame.clone().encode()), Ok(frame));
    }

    #[test]
    fn heartbeat_roundtrips() {
        let frame = GatewayFrame::Heartbeat { seq: 42 };
        assert_eq!(GatewayFrame::decode(&frame.clone().encode()), Ok(frame));
    }

    #[test]
    fn private_event_roundtrips() {
        let event = PrivateEvent {
            seq: 11,
            order_id: 12,
            client_order_id: 13,
            account_id: 14,
            status: PrivateStatus::Rejected,
            filled_delta: 0,
            remaining: 0,
            reason: Some("insufficient closeable position".to_string()),
        };
        let frame = GatewayFrame::PrivateEvent(event.clone());
        assert_eq!(GatewayFrame::decode(&frame.clone().encode()), Ok(frame));
    }

    #[test]
    fn combo_private_roundtrips() {
        let fill = ComboPrivateFill {
            seq: 3,
            pair_id: 10,
            order_id: 4,
            client_order_id: 40,
            user_id: 5,
            session_id: 50,
            side: crate::trading::core::protocol::Side::Sell,
            quantity: 6,
            spread_price: 49_900,
            leg1_price_ticks: 50_000,
            leg2_price_ticks: 100,
        };
        let frame = GatewayFrame::ComboPrivate(fill);
        assert_eq!(GatewayFrame::decode(&frame.clone().encode()), Ok(frame));
    }

    #[test]
    fn redirect_roundtrips() {
        let frame = GatewayFrame::Redirect {
            address: "127.0.0.1:9000".to_string(),
        };
        assert_eq!(GatewayFrame::decode(&frame.clone().encode()), Ok(frame));
    }

    #[test]
    fn ack_roundtrips() {
        let frame = GatewayFrame::Ack { private_seq: 42 };
        assert_eq!(GatewayFrame::decode(&frame.clone().encode()), Ok(frame));
    }

    #[test]
    fn unknown_tag_is_rejected() {
        let mut frame = [0u8; GATEWAY_FRAME_SIZE];
        frame[0] = 99;
        assert_eq!(
            GatewayFrame::decode(&frame),
            Err(GatewayFrameError {
                field: "gateway_frame_tag",
                value: 99
            })
        );
    }
}
