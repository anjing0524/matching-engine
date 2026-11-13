//! Admission rules for combo commands.
//!
//! This module owns the command-shape, phase, pair-window, and spread-tick
//! checks. The registry uses the returned terms to build and route an order;
//! it no longer carries the admission policy itself.

use crate::trading::core::combo_book::ComboCommandError;
use crate::trading::core::combo_pair::ComboPair;
use crate::trading::core::protocol::{
    Command, Offset, PriceType, TimeCondition, TriggerType, VolumeCondition,
};
use crate::trading::core::public_events::TradingPhase;

/// Validated combo new-order terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ComboAdmission {
    pub(crate) market: bool,
    pub(crate) protection: Option<i64>,
    pub(crate) ioc: bool,
    pub(crate) fok: bool,
}

/// Validates a combo new order against the venue phase and pair definition.
pub(crate) fn admit_new(
    command: &Command,
    pair: &ComboPair,
    phase: TradingPhase,
) -> Result<ComboAdmission, ComboCommandError> {
    if phase != TradingPhase::Continuous {
        return Err(ComboCommandError::OutsideContinuousTrading { phase });
    }
    if !pair.is_valid_at(command.trading_ts_ns) {
        return Err(ComboCommandError::OutsidePairWindow {
            pair_id: pair.pair_id,
            trading_ts_ns: command.trading_ts_ns,
        });
    }
    if !command
        .price_ticks
        .unsigned_abs()
        .is_multiple_of(pair.spread_tick)
    {
        return Err(ComboCommandError::OffTickSpreadPrice {
            price_ticks: command.price_ticks,
            spread_tick: pair.spread_tick,
        });
    }
    if !matches!(
        command.price_type,
        PriceType::Limit | PriceType::Market | PriceType::AdvancedMarket
    ) {
        return Err(ComboCommandError::UnsupportedPriceType {
            price_type: command.price_type,
        });
    }
    if !matches!(
        command.time_condition,
        TimeCondition::Gfd | TimeCondition::Ioc
    ) {
        return Err(ComboCommandError::UnsupportedTimeCondition {
            time_condition: command.time_condition,
        });
    }
    let valid_volume = matches!(
        (command.time_condition, command.volume_condition),
        (TimeCondition::Gfd, VolumeCondition::Any)
            | (
                TimeCondition::Ioc,
                VolumeCondition::Any | VolumeCondition::All
            )
    );
    if !valid_volume {
        return Err(ComboCommandError::UnsupportedVolumeCondition {
            volume_condition: command.volume_condition,
        });
    }
    if command.trigger_type != TriggerType::None {
        return Err(ComboCommandError::UnsupportedTriggerType {
            trigger_type: command.trigger_type,
        });
    }
    if command.offset != Offset::Open {
        return Err(ComboCommandError::UnsupportedOffset {
            offset: command.offset,
        });
    }
    if command.leg2_offset != Offset::Open {
        return Err(ComboCommandError::UnsupportedOffset {
            offset: command.leg2_offset,
        });
    }
    if command.quantity == 0 {
        return Err(ComboCommandError::ZeroQuantity {
            pair_id: command.combo_id,
        });
    }
    let market = matches!(
        command.price_type,
        PriceType::Market | PriceType::AdvancedMarket
    );
    let protection =
        (command.price_type == PriceType::AdvancedMarket).then_some(command.price_ticks);
    let ioc = market || command.time_condition == TimeCondition::Ioc;
    let fok = ioc && command.volume_condition == VolumeCondition::All;
    Ok(ComboAdmission {
        market,
        protection,
        ioc,
        fok,
    })
}

/// Validates combo cancellation against the venue phase.
pub(crate) fn admit_cancel(phase: TradingPhase) -> Result<(), ComboCommandError> {
    if phase != TradingPhase::Continuous {
        return Err(ComboCommandError::OutsideContinuousTrading { phase });
    }
    Ok(())
}
