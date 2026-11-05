//! Fixed-size wire records for the exchange core transport.
//!
//! This is the Rust counterpart of `docs/exchange-protocol.md`. The records are
//! deliberately hand-encoded instead of using `serde`/`bincode`: the shared
//! memory layout must be stable, little-endian, and exactly 256/128 bytes.

/// Size of one command slot in the middleware→core command log.
pub const COMMAND_SLOT_SIZE: usize = 256;

/// Command kind in a command-log slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandType {
    NewOrder,
    Cancel,
    Control,
    TimeTick,
}

impl CommandType {
    fn tag(self) -> u8 {
        match self {
            Self::NewOrder => 1,
            Self::Cancel => 2,
            Self::Control => 3,
            Self::TimeTick => 4,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            1 => Ok(Self::NewOrder),
            2 => Ok(Self::Cancel),
            3 => Ok(Self::Control),
            4 => Ok(Self::TimeTick),
            value => Err(ProtocolError::new("command_type", value)),
        }
    }
}

/// Price-selection mechanism. FAK/FOK are not price types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriceType {
    Limit,
    Market,
    AdvancedMarket,
}

impl PriceType {
    fn tag(self) -> u8 {
        match self {
            Self::Limit => 1,
            Self::Market => 2,
            Self::AdvancedMarket => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            1 => Ok(Self::Limit),
            2 => Ok(Self::Market),
            3 => Ok(Self::AdvancedMarket),
            value => Err(ProtocolError::new("price_type", value)),
        }
    }
}

/// Order validity in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeCondition {
    Gfd,
    Ioc,
    Gis,
}

impl TimeCondition {
    fn tag(self) -> u8 {
        match self {
            Self::Gfd => 1,
            Self::Ioc => 2,
            Self::Gis => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            1 => Ok(Self::Gfd),
            2 => Ok(Self::Ioc),
            3 => Ok(Self::Gis),
            value => Err(ProtocolError::new("time_condition", value)),
        }
    }
}

/// How much of the order must fill. FAK = IOC + MV, FOK = IOC + CV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeCondition {
    Any,
    Minimum,
    All,
}

impl VolumeCondition {
    fn tag(self) -> u8 {
        match self {
            Self::Any => 1,
            Self::Minimum => 2,
            Self::All => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            1 => Ok(Self::Any),
            2 => Ok(Self::Minimum),
            3 => Ok(Self::All),
            value => Err(ProtocolError::new("volume_condition", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    fn tag(self) -> u8 {
        match self {
            Self::Buy => 1,
            Self::Sell => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            1 => Ok(Self::Buy),
            2 => Ok(Self::Sell),
            value => Err(ProtocolError::new("side", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Offset {
    Open,
    Close,
    CloseToday,
}

impl Offset {
    fn tag(self) -> u8 {
        match self {
            Self::Open => 1,
            Self::Close => 2,
            Self::CloseToday => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            1 => Ok(Self::Open),
            2 => Ok(Self::Close),
            3 => Ok(Self::CloseToday),
            value => Err(ProtocolError::new("offset", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hedge {
    Spec,
    Hedge,
    Arb,
}

impl Hedge {
    fn tag(self) -> u8 {
        match self {
            Self::Spec => 1,
            Self::Hedge => 2,
            Self::Arb => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            1 => Ok(Self::Spec),
            2 => Ok(Self::Hedge),
            3 => Ok(Self::Arb),
            value => Err(ProtocolError::new("hedge", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerType {
    None,
    Stop,
}

impl TriggerType {
    fn tag(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Stop => 1,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            0 => Ok(Self::None),
            1 => Ok(Self::Stop),
            value => Err(ProtocolError::new("trigger_type", value)),
        }
    }
}

/// A malformed wire tag. The field name makes rejects actionable in logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub field: &'static str,
    pub value: u8,
}

impl ProtocolError {
    const fn new(field: &'static str, value: u8) -> Self {
        Self { field, value }
    }
}

/// One middleware→core command-log slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub global_seq: u64,
    pub gateway_seq: u64,
    pub ingress_ts_ns: u64,
    pub exchange_ts_ns: u64,
    pub client_order_id: u64,
    pub session_id: u64,
    pub gateway_id: u32,
    pub member_id: u32,
    pub user_id: u32,
    pub seat_id: u32,
    pub instrument_id: u32,
    pub combo_id: u32,
    pub session_epoch: u32,
    pub command_type: CommandType,
    pub price_type: PriceType,
    pub time_condition: TimeCondition,
    pub volume_condition: VolumeCondition,
    pub side: Side,
    pub offset: Offset,
    pub hedge: Hedge,
    pub flags: u8,
    pub leg2_ratio: u32,
    pub price_ticks: i64,
    pub quantity: u64,
    pub trigger_price_ticks: i64,
    pub leg2_instrument_id: u32,
    pub leg2_offset: Offset,
    pub leg2_hedge: Hedge,
    pub trigger_type: TriggerType,
    pub cancel_order_id: u64,
}

impl Command {
    /// Encodes the command into its fixed 256-byte slot.
    #[must_use]
    pub fn encode(&self) -> [u8; COMMAND_SLOT_SIZE] {
        let mut out = [0u8; COMMAND_SLOT_SIZE];
        put_u64(&mut out, 0, self.global_seq);
        put_u64(&mut out, 8, self.gateway_seq);
        put_u64(&mut out, 16, self.ingress_ts_ns);
        put_u64(&mut out, 24, self.exchange_ts_ns);
        put_u64(&mut out, 32, self.client_order_id);
        put_u64(&mut out, 40, self.session_id);
        put_u32(&mut out, 48, self.gateway_id);
        put_u32(&mut out, 52, self.member_id);
        put_u32(&mut out, 56, self.user_id);
        put_u32(&mut out, 60, self.seat_id);
        put_u32(&mut out, 64, self.instrument_id);
        put_u32(&mut out, 68, self.combo_id);
        put_u32(&mut out, 72, self.session_epoch);
        out[76] = self.command_type.tag();
        out[77] = self.price_type.tag();
        out[78] = self.time_condition.tag();
        out[79] = self.volume_condition.tag();
        out[80] = self.side.tag();
        out[81] = self.offset.tag();
        out[82] = self.hedge.tag();
        out[83] = self.flags;
        put_u32(&mut out, 84, self.leg2_ratio);
        put_i64(&mut out, 88, self.price_ticks);
        put_u64(&mut out, 96, self.quantity);
        put_i64(&mut out, 104, self.trigger_price_ticks);
        put_u32(&mut out, 112, self.leg2_instrument_id);
        out[116] = self.leg2_offset.tag();
        out[117] = self.leg2_hedge.tag();
        out[118] = self.trigger_type.tag();
        out[119] = 0;
        put_u64(&mut out, 120, self.cancel_order_id);
        out
    }

    /// Decodes a fixed slot and rejects unknown enum tags.
    pub fn decode(slot: &[u8; COMMAND_SLOT_SIZE]) -> Result<Self, ProtocolError> {
        Ok(Self {
            global_seq: get_u64(slot, 0),
            gateway_seq: get_u64(slot, 8),
            ingress_ts_ns: get_u64(slot, 16),
            exchange_ts_ns: get_u64(slot, 24),
            client_order_id: get_u64(slot, 32),
            session_id: get_u64(slot, 40),
            gateway_id: get_u32(slot, 48),
            member_id: get_u32(slot, 52),
            user_id: get_u32(slot, 56),
            seat_id: get_u32(slot, 60),
            instrument_id: get_u32(slot, 64),
            combo_id: get_u32(slot, 68),
            session_epoch: get_u32(slot, 72),
            command_type: CommandType::from_tag(slot[76])?,
            price_type: PriceType::from_tag(slot[77])?,
            time_condition: TimeCondition::from_tag(slot[78])?,
            volume_condition: VolumeCondition::from_tag(slot[79])?,
            side: Side::from_tag(slot[80])?,
            offset: Offset::from_tag(slot[81])?,
            hedge: Hedge::from_tag(slot[82])?,
            flags: slot[83],
            leg2_ratio: get_u32(slot, 84),
            price_ticks: get_i64(slot, 88),
            quantity: get_u64(slot, 96),
            trigger_price_ticks: get_i64(slot, 104),
            leg2_instrument_id: get_u32(slot, 112),
            leg2_offset: Offset::from_tag(slot[116])?,
            leg2_hedge: Hedge::from_tag(slot[117])?,
            trigger_type: TriggerType::from_tag(slot[118])?,
            cancel_order_id: get_u64(slot, 120),
        })
    }
}

fn put_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_i64(out: &mut [u8], offset: usize, value: i64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(slot: &[u8; COMMAND_SLOT_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes(slot[offset..offset + 4].try_into().expect("4 bytes"))
}

fn get_u64(slot: &[u8; COMMAND_SLOT_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes(slot[offset..offset + 8].try_into().expect("8 bytes"))
}

fn get_i64(slot: &[u8; COMMAND_SLOT_SIZE], offset: usize) -> i64 {
    i64::from_le_bytes(slot[offset..offset + 8].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> Command {
        Command {
            global_seq: 7,
            gateway_seq: 3,
            ingress_ts_ns: 11,
            exchange_ts_ns: 13,
            client_order_id: 17,
            session_id: 19,
            gateway_id: 2,
            member_id: 23,
            user_id: 29,
            seat_id: 31,
            instrument_id: 37,
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
            quantity: 10,
            trigger_price_ticks: 0,
            leg2_instrument_id: 0,
            leg2_offset: Offset::Open,
            leg2_hedge: Hedge::Spec,
            trigger_type: TriggerType::None,
            cancel_order_id: 0,
        }
    }

    #[test]
    fn command_roundtrip_has_exact_slot_size() {
        let command = command();
        let encoded = command.encode();
        assert_eq!(encoded.len(), COMMAND_SLOT_SIZE);
        assert_eq!(Command::decode(&encoded), Ok(command));
    }

    #[test]
    fn command_is_little_endian_at_known_offsets() {
        let encoded = command().encode();
        assert_eq!(&encoded[0..8], &7u64.to_le_bytes());
        assert_eq!(&encoded[76..80], &[1, 1, 1, 1]);
    }

    #[test]
    fn unknown_enum_tag_is_rejected() {
        let mut encoded = command().encode();
        encoded[77] = 0xff;
        let error = Command::decode(&encoded).unwrap_err();
        assert_eq!(error.field, "price_type");
        assert_eq!(error.value, 0xff);
    }

    #[test]
    fn cancel_command_uses_cancel_order_id() {
        let mut command = command();
        command.command_type = CommandType::Cancel;
        command.cancel_order_id = 123;
        let decoded = Command::decode(&command.encode()).unwrap();
        assert_eq!(decoded.command_type, CommandType::Cancel);
        assert_eq!(decoded.cancel_order_id, 123);
    }
}
