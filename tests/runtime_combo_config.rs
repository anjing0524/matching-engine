//! Static combo venue configuration and fail-closed margin backing.
//!
//! The production single-process runtime loads instruments, pairs, prior
//! settlements, and margin balances from one configuration value. A config that
//! defines combo pairs must also define explicit account margin balances, and
//! an unconfigured account must be rejected at combo entry rather than treated
//! as unlimited.

use trading_system_engine::trading::config::{ComboVenueConfig, ComboVenueConfigError};
use trading_system_engine::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
    VolumeCondition,
};
use trading_system_engine::trading::core::{
    ComboCommandError, ComboLeg, ComboMarginError, ComboPairKind,
};

const VALID_CONFIG: &str = r#"
{
  "instruments": [
    {"instrument_id": 1, "symbol": "IF2501", "tick_size": 1, "min_price": 30000, "max_price": 70000,
     "product": "IF", "month": 2501},
    {"instrument_id": 2, "symbol": "IF2502", "tick_size": 1, "min_price": 30000, "max_price": 70000,
     "product": "IF", "month": 2502}
  ],
  "pairs": [
    {"pair_id": 10, "leg1_instrument_id": 1, "leg2_instrument_id": 2, "kind": "cross_term", "spread_tick": 1,
     "margin": {"multiplier": 300, "leg1_rate_bps": 1200, "leg2_rate_bps": 1000}}
  ],
  "prior_settlements": [
    {"instrument_id": 1, "price_ticks": 45000},
    {"instrument_id": 2, "price_ticks": 40000}
  ],
  "margin_balances": [
    {"user_id": 1, "balance": 1000000000},
    {"user_id": 2, "balance": 1000000000}
  ]
}
"#;

const CROSS_PRODUCT_CONFIG: &str = r#"
{
  "instruments": [
    {"instrument_id": 1, "symbol": "IF2501", "tick_size": 1, "min_price": 30000, "max_price": 70000,
     "product": "IF", "month": 2501},
    {"instrument_id": 3, "symbol": "IH2501", "tick_size": 1, "min_price": 30000, "max_price": 70000,
     "product": "IH", "month": 2501}
  ],
  "pairs": [
    {"pair_id": 20, "leg1_instrument_id": 1, "leg2_instrument_id": 3, "kind": "cross_product", "spread_tick": 1,
     "margin": {"multiplier": 300, "leg1_rate_bps": 1200, "leg2_rate_bps": 1000}}
  ],
  "prior_settlements": [
    {"instrument_id": 1, "price_ticks": 45000},
    {"instrument_id": 3, "price_ticks": 40000}
  ],
  "margin_balances": [
    {"user_id": 1, "balance": 1000000000}
  ]
}
"#;

fn config_value(json: &str) -> serde_json::Value {
    serde_json::from_str(json).unwrap()
}

fn config_from_value(value: &serde_json::Value) -> Result<ComboVenueConfig, ComboVenueConfigError> {
    ComboVenueConfig::from_json(&value.to_string())
}

fn combo_order(user_id: u32) -> Command {
    let mut command = Command::zeroed();
    command.command_type = CommandType::NewOrder;
    command.combo_id = 10;
    command.client_order_id = 1;
    command.user_id = user_id;
    command.price_type = PriceType::Limit;
    command.time_condition = TimeCondition::Gfd;
    command.volume_condition = VolumeCondition::Any;
    command.side = Side::Buy;
    command.offset = Offset::Open;
    command.hedge = Hedge::Spec;
    command.price_ticks = 100;
    command.quantity = 1;
    command.trigger_type = TriggerType::None;
    command
}

#[test]
fn valid_config_builds_registry_with_pairs_settlements_and_balances() {
    let config = ComboVenueConfig::from_json(VALID_CONFIG).unwrap();
    let registry = config.build_registry().unwrap();

    assert_eq!(registry.len(), 2);
    assert_eq!(registry.pair_count(), 1);
    assert_eq!(registry.prior_settlement(1), Some(45_000));
    assert_eq!(registry.prior_settlement(2), Some(40_000));
    assert_eq!(registry.margin_balance(1), Some(1_000_000_000));
    assert!(registry.requires_explicit_margin());
}

#[test]
fn cross_term_pair_requires_same_product_and_different_month() {
    let mut different_product = config_value(VALID_CONFIG);
    different_product["instruments"][1]["product"] = "IH".into();
    match config_from_value(&different_product) {
        Err(ComboVenueConfigError::PairKindMismatch {
            pair_id: 10,
            kind: ComboPairKind::CrossTerm,
            ..
        }) => {}
        other => panic!("expected cross-term product mismatch, got {other:?}"),
    }

    let mut same_month = config_value(VALID_CONFIG);
    same_month["instruments"][1]["month"] = 2501.into();
    match config_from_value(&same_month) {
        Err(ComboVenueConfigError::PairKindMismatch {
            pair_id: 10,
            kind: ComboPairKind::CrossTerm,
            ..
        }) => {}
        other => panic!("expected cross-term month mismatch, got {other:?}"),
    }
}

#[test]
fn cross_product_pair_requires_different_products_in_same_month() {
    let config = config_from_value(&config_value(CROSS_PRODUCT_CONFIG)).unwrap();
    let registry = config.build_registry().unwrap();
    assert_eq!(registry.pair(20).unwrap().kind, ComboPairKind::CrossProduct);

    let mut same_product = config_value(CROSS_PRODUCT_CONFIG);
    same_product["instruments"][1]["product"] = "IF".into();
    match config_from_value(&same_product) {
        Err(ComboVenueConfigError::PairKindMismatch {
            pair_id: 20,
            kind: ComboPairKind::CrossProduct,
            ..
        }) => {}
        other => panic!("expected cross-product product mismatch, got {other:?}"),
    }

    let mut different_month = config_value(CROSS_PRODUCT_CONFIG);
    different_month["instruments"][1]["month"] = 2502.into();
    match config_from_value(&different_month) {
        Err(ComboVenueConfigError::PairKindMismatch {
            pair_id: 20,
            kind: ComboPairKind::CrossProduct,
            ..
        }) => {}
        other => panic!("expected cross-product month mismatch, got {other:?}"),
    }
}

#[test]
fn pair_leg_without_product_or_month_metadata_is_rejected_at_startup() {
    let mut missing_product = config_value(VALID_CONFIG);
    missing_product["instruments"][0]
        .as_object_mut()
        .unwrap()
        .remove("product");
    match config_from_value(&missing_product) {
        Err(ComboVenueConfigError::MissingPairLegMetadata {
            pair_id: 10,
            leg: ComboLeg::First,
            instrument_id: 1,
        }) => {}
        other => panic!("expected missing first-leg product metadata, got {other:?}"),
    }

    let mut empty_product = config_value(VALID_CONFIG);
    empty_product["instruments"][0]["product"] = "".into();
    match config_from_value(&empty_product) {
        Err(ComboVenueConfigError::MissingPairLegMetadata {
            pair_id: 10,
            leg: ComboLeg::First,
            instrument_id: 1,
        }) => {}
        other => panic!("expected empty first-leg product metadata, got {other:?}"),
    }

    let mut missing_month = config_value(VALID_CONFIG);
    missing_month["instruments"][1]
        .as_object_mut()
        .unwrap()
        .remove("month");
    match config_from_value(&missing_month) {
        Err(ComboVenueConfigError::MissingPairLegMetadata {
            pair_id: 10,
            leg: ComboLeg::Second,
            instrument_id: 2,
        }) => {}
        other => panic!("expected missing second-leg month metadata, got {other:?}"),
    }
}

#[test]
fn instrument_not_used_as_a_pair_leg_may_omit_product_and_month() {
    let mut value = config_value(VALID_CONFIG);
    value["instruments"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "instrument_id": 4,
            "symbol": "AU2512",
            "tick_size": 1,
            "min_price": 30000,
            "max_price": 70000
        }));
    let config = config_from_value(&value).unwrap();
    assert_eq!(config.build_registry().unwrap().len(), 3);
}

#[test]
fn combo_pairs_require_explicit_margin_balances() {
    let json = VALID_CONFIG.replace(
        r#""margin_balances": [
    {"user_id": 1, "balance": 1000000000},
    {"user_id": 2, "balance": 1000000000}
  ]"#,
        r#""margin_balances": []"#,
    );
    match ComboVenueConfig::from_json(&json) {
        Err(ComboVenueConfigError::MissingMarginBalances) => {}
        _ => panic!("expected missing margin balances"),
    }
}

#[test]
fn config_rejects_pair_leg_without_instrument() {
    let json = VALID_CONFIG.replace(r#""leg2_instrument_id": 2"#, r#""leg2_instrument_id": 99"#);
    let config = ComboVenueConfig::from_json(&json).unwrap();
    match config.build_registry() {
        Err(ComboVenueConfigError::Pair(_)) => {}
        _ => panic!("expected pair validation error"),
    }
}

#[test]
fn config_rejects_invalid_instrument_spec() {
    let json = VALID_CONFIG.replace(
        r#""tick_size": 1, "min_price": 30000, "max_price": 70000"#,
        r#""tick_size": 0, "min_price": 30000, "max_price": 70000"#,
    );
    match ComboVenueConfig::from_json(&json) {
        Err(ComboVenueConfigError::InvalidInstrument {
            instrument_id: 1, ..
        }) => {}
        _ => panic!("expected invalid instrument"),
    }
}

#[test]
fn strict_margin_rejects_unconfigured_account_at_combo_entry() {
    let config = ComboVenueConfig::from_json(VALID_CONFIG).unwrap();
    let mut registry = config.build_registry().unwrap();

    let error = registry.execute_combo(combo_order(9)).unwrap_err();
    assert_eq!(
        error,
        ComboCommandError::Margin(ComboMarginError::MissingMarginBalance { account_id: 9 })
    );
    assert_eq!(registry.next_combo_order_id(), 1);
    assert!(registry.combo_book(10).unwrap().snapshot().bids.is_empty());
}

#[test]
fn configured_account_can_enter_a_combo_order() {
    let config = ComboVenueConfig::from_json(VALID_CONFIG).unwrap();
    let mut registry = config.build_registry().unwrap();

    registry.execute_combo(combo_order(1)).unwrap();
    assert_eq!(registry.next_combo_order_id(), 2);
    assert_eq!(registry.combo_book(10).unwrap().snapshot().bids.len(), 1);
}

#[test]
fn config_file_load_rejects_combo_pairs_without_balances() {
    let path = std::env::temp_dir().join(format!(
        "trading-combo-config-reject-{}.json",
        std::process::id()
    ));
    let json = VALID_CONFIG.replace(
        r#""margin_balances": [
    {"user_id": 1, "balance": 1000000000},
    {"user_id": 2, "balance": 1000000000}
  ]"#,
        r#""margin_balances": []"#,
    );
    std::fs::write(&path, json).unwrap();
    match ComboVenueConfig::load(&path) {
        Err(ComboVenueConfigError::MissingMarginBalances) => {}
        _ => panic!("expected missing margin balances from startup load"),
    }
    let _ = std::fs::remove_file(path);
}
