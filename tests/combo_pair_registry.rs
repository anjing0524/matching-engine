//! Static Combo Pair behavior through the [`InstrumentRegistry`] seam.

use trading_system_engine::domain::orderbook::ContractSpec;
use trading_system_engine::trading::core::protocol::{Command, CommandType};
use trading_system_engine::trading::core::{
    ComboLeg, ComboPair, ComboPairError, ComboPairKind, ComboPairMargin, InstrumentRegistry,
};

fn spec(symbol: &str) -> ContractSpec {
    ContractSpec::new(symbol, 1, 40_000, 70_000)
}

fn registry_with_legs() -> InstrumentRegistry {
    let mut registry = InstrumentRegistry::new();
    registry.register(1, spec("IF2501")).unwrap();
    registry.register(2, spec("IF2502")).unwrap();
    registry.register(3, spec("IC2501")).unwrap();
    registry
}

fn margin() -> ComboPairMargin {
    ComboPairMargin::new(300, 1_200, 1_000)
}

fn pair(pair_id: u32, leg1: u32, leg2: u32, spread_tick: u64) -> ComboPair {
    ComboPair::new(pair_id, leg1, leg2, spread_tick, margin())
}

#[test]
fn cross_term_and_cross_product_pairs_resolve_to_two_registered_legs() {
    let mut registry = registry_with_legs();

    registry.register_pair(pair(10, 1, 2, 1)).unwrap();
    registry.register_pair(pair(20, 1, 3, 5)).unwrap();

    let cross_term = registry.pair(10).unwrap();
    assert_eq!(cross_term.leg1_instrument_id, 1);
    assert_eq!(cross_term.leg2_instrument_id, 2);
    assert_eq!(cross_term.spread_tick, 1);
    assert_eq!(cross_term.margin, margin());

    let cross_product = registry.pair(20).unwrap();
    assert_eq!(cross_product.leg1_instrument_id, 1);
    assert_eq!(cross_product.leg2_instrument_id, 3);
    assert_eq!(cross_product.spread_tick, 5);

    let mut listed: Vec<u32> = registry.pairs().map(|pair| pair.pair_id).collect();
    listed.sort_unstable();
    assert_eq!(listed, vec![10, 20]);
    assert_eq!(registry.pair_count(), 2);
    assert!(registry.contains_pair(10));
    assert!(!registry.contains_pair(11));
}

#[test]
fn duplicate_pair_id_is_rejected_and_keeps_the_first_definition() {
    let mut registry = registry_with_legs();
    registry.register_pair(pair(10, 1, 2, 1)).unwrap();

    let error = registry.register_pair(pair(10, 1, 3, 5)).unwrap_err();
    assert_eq!(error, ComboPairError::DuplicatePairId { pair_id: 10 });

    let stored = registry.pair(10).unwrap();
    assert_eq!(stored.leg2_instrument_id, 2);
    assert_eq!(stored.spread_tick, 1);
    assert_eq!(registry.pair_count(), 1);
}

#[test]
fn pair_with_unknown_leg_instrument_is_rejected() {
    let mut registry = registry_with_legs();

    let error = registry.register_pair(pair(10, 1, 99, 1)).unwrap_err();
    assert_eq!(
        error,
        ComboPairError::UnknownLegInstrument {
            pair_id: 10,
            leg: ComboLeg::Second,
            instrument_id: 99,
        }
    );
    assert!(!registry.contains_pair(10));

    let error = registry.register_pair(pair(11, 99, 2, 1)).unwrap_err();
    assert_eq!(
        error,
        ComboPairError::UnknownLegInstrument {
            pair_id: 11,
            leg: ComboLeg::First,
            instrument_id: 99,
        }
    );
    assert_eq!(registry.pair_count(), 0);
}

#[test]
fn pair_with_identical_legs_is_rejected() {
    let mut registry = registry_with_legs();

    let error = registry.register_pair(pair(10, 2, 2, 1)).unwrap_err();
    assert_eq!(
        error,
        ComboPairError::IdenticalLegs {
            pair_id: 10,
            instrument_id: 2,
        }
    );
    assert!(!registry.contains_pair(10));
    assert_eq!(registry.pair_count(), 0);
}

#[test]
fn pair_with_non_positive_spread_tick_is_rejected() {
    let mut registry = registry_with_legs();

    let error = registry.register_pair(pair(10, 1, 2, 0)).unwrap_err();
    assert_eq!(
        error,
        ComboPairError::InvalidSpreadTick {
            pair_id: 10,
            spread_tick: 0,
        }
    );
    assert!(!registry.contains_pair(10));
    assert_eq!(registry.pair_count(), 0);
}

#[test]
fn pair_with_invalid_margin_is_rejected() {
    let mut registry = registry_with_legs();

    let zero_multiplier = ComboPair::new(10, 1, 2, 1, ComboPairMargin::new(0, 1_200, 1_000));
    assert_eq!(
        registry.register_pair(zero_multiplier).unwrap_err(),
        ComboPairError::InvalidMargin {
            pair_id: 10,
            reason: "multiplier must be positive",
        }
    );
    assert!(!registry.contains_pair(10));

    let first_leg_rate = ComboPair::new(11, 1, 2, 1, ComboPairMargin::new(300, 10_001, 1_000));
    assert_eq!(
        registry.register_pair(first_leg_rate).unwrap_err(),
        ComboPairError::InvalidMargin {
            pair_id: 11,
            reason: "first-leg margin rate must not exceed 10000 bps",
        }
    );
    assert!(!registry.contains_pair(11));

    let second_leg_rate = ComboPair::new(12, 1, 2, 1, ComboPairMargin::new(300, 1_200, 10_001));
    assert_eq!(
        registry.register_pair(second_leg_rate).unwrap_err(),
        ComboPairError::InvalidMargin {
            pair_id: 12,
            reason: "second-leg margin rate must not exceed 10000 bps",
        }
    );
    assert_eq!(registry.pair_count(), 0);
}

#[test]
fn commands_do_not_create_combo_pairs_at_runtime() {
    let mut registry = registry_with_legs();
    registry.register_pair(pair(10, 1, 2, 1)).unwrap();

    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.instrument_id = 99;
    command.combo_id = 77;
    assert!(registry.execute(command).is_err());

    assert_eq!(registry.pair_count(), 1);
    assert!(!registry.contains_pair(77));
}

#[test]
fn pair_id_zero_is_rejected_as_non_combo_sentinel() {
    let mut registry = registry_with_legs();
    let error = registry.register_pair(pair(0, 1, 2, 1)).unwrap_err();
    assert_eq!(error, ComboPairError::InvalidPairId { pair_id: 0 });
    assert_eq!(registry.pair_count(), 0);
}

#[test]
fn pair_kind_defaults_cross_term_and_sorts_before_cross_product() {
    let mut cross_term = pair(10, 1, 2, 1);
    assert_eq!(cross_term.kind, ComboPairKind::CrossTerm);
    cross_term = cross_term.with_kind(ComboPairKind::CrossProduct);
    assert_eq!(cross_term.kind, ComboPairKind::CrossProduct);
    assert!(ComboPairKind::CrossTerm.priority() < ComboPairKind::CrossProduct.priority());
}
