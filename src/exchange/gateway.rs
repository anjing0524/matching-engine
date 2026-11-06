//! Gateway wire frames.
//!
//! One fixed 264-byte frame carries login, command, or heartbeat messages
//! between a gateway and middleware. TCP transport and session lifecycle sit
//! above this codec; this slice only fixes the byte layout.

use crate::exchange::protocol::{COMMAND_SLOT_SIZE, Command};

pub const GATEWAY_FRAME_SIZE: usize = 264;
const TAG_LOGIN: u8 = 1;
const TAG_COMMAND: u8 = 2;
const TAG_HEARTBEAT: u8 = 3;
const PAYLOAD_OFFSET: usize = 4;

/// One gateway→middleware frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayFrame {
    Login {
        member_id: u32,
        user_id: u32,
        seat_id: u32,
        session_epoch: u32,
        last_private_seq: u64,
    },
    Command(Command),
    Heartbeat {
        seq: u64,
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
    use crate::exchange::protocol::{
        CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType, VolumeCondition,
    };

    fn command() -> Command {
        Command {
            global_seq: 1,
            gateway_seq: 2,
            ingress_ts_ns: 0,
            exchange_ts_ns: 0,
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
        };
        assert_eq!(GatewayFrame::decode(&frame.encode()), Ok(frame));
    }

    #[test]
    fn command_roundtrips() {
        let frame = GatewayFrame::Command(command());
        assert_eq!(GatewayFrame::decode(&frame.encode()), Ok(frame));
    }

    #[test]
    fn heartbeat_roundtrips() {
        let frame = GatewayFrame::Heartbeat { seq: 42 };
        assert_eq!(GatewayFrame::decode(&frame.encode()), Ok(frame));
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
