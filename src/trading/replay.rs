//! Deterministic replay from a persisted core snapshot.

use crate::trading::core::TradingCore;
use crate::trading::plugins::NoPlugins;
use crate::trading::protocol::Command;
use std::path::Path;

/// Loads a snapshot and applies the remaining command log.
///
/// # Errors
/// Returns a reason if the snapshot cannot be loaded or restored.
pub fn replay_from_snapshot(
    snapshot_path: &Path,
    commands: impl IntoIterator<Item = Command>,
) -> Result<TradingCore, String> {
    let snapshot = TradingCore::load_snapshot(snapshot_path).map_err(|error| error.to_string())?;
    let mut core = TradingCore::new(snapshot.spec.clone(), snapshot.symbol);
    core.restore_snapshot(snapshot)?;
    apply(&mut core, commands);
    Ok(core)
}

/// Applies commands through the same plugin path used by the core runner.
pub fn apply(core: &mut TradingCore, commands: impl IntoIterator<Item = Command>) -> usize {
    let plugin = NoPlugins;
    let mut applied = 0;
    for command in commands {
        let _ = core.execute_with_plugins(command, &plugin);
        applied += 1;
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::protocol::{
        CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType, VolumeCondition,
    };
    use crate::trading::public_events::TradingPhase;

    fn command(seq: u64, side: Side, phase: TradingPhase) -> Command {
        let mut command = Command::zeroed();
        command.global_seq = seq;
        command.client_order_id = seq;
        command.session_id = 1;
        command.member_id = 1;
        command.user_id = 1;
        command.seat_id = 1;
        command.instrument_id = 1;
        command.command_type = CommandType::NewOrder;
        command.price_type = PriceType::Limit;
        command.time_condition = TimeCondition::Gfd;
        command.volume_condition = VolumeCondition::Any;
        command.side = side;
        command.offset = Offset::Open;
        command.hedge = Hedge::Spec;
        command.price_ticks = 50_000;
        command.quantity = 1;
        command.flags = phase.code();
        command.trigger_type = TriggerType::None;
        command
    }

    #[test]
    fn replay_matches_continuous_execution() {
        let spec = crate::domain::orderbook::ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let symbol = crate::shared::SymbolId::intern("BTCUSDT");
        let mut expected = TradingCore::new(spec.clone(), symbol);
        let plugin = NoPlugins;
        for seq in 1..=4u64 {
            let side = if seq.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            let _ = expected
                .execute_with_plugins(command(seq, side, TradingPhase::Continuous), &plugin);
        }
        let snapshot = expected.snapshot();
        let mut restored = TradingCore::new(spec, symbol);
        restored.restore_snapshot(snapshot).unwrap();
        apply(
            &mut restored,
            [
                command(5, Side::Sell, TradingPhase::Continuous),
                command(6, Side::Buy, TradingPhase::Continuous),
            ],
        );
        for seq in 5..=6u64 {
            let side = if seq.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            let _ = expected
                .execute_with_plugins(command(seq, side, TradingPhase::Continuous), &plugin);
        }
        assert_eq!(restored.snapshot(), expected.snapshot());

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-replay-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut before = TradingCore::new(
            crate::domain::orderbook::ContractSpec::new("BTCUSDT", 1, 40_000, 70_000),
            symbol,
        );
        let plugin = NoPlugins;
        for seq in 1..=4u64 {
            let side = if seq.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            let _ =
                before.execute_with_plugins(command(seq, side, TradingPhase::Continuous), &plugin);
        }
        before.save_snapshot(&path).unwrap();
        let replayed = replay_from_snapshot(
            &path,
            [
                command(5, Side::Sell, TradingPhase::Continuous),
                command(6, Side::Buy, TradingPhase::Continuous),
            ],
        )
        .unwrap();
        assert_eq!(replayed.snapshot(), expected.snapshot());
        let _ = std::fs::remove_file(&path);
    }
}
