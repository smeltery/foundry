mod support;

use foundry_budget::{Amount, CostUnit, Currency, PriceTable, Usage};
use support::{money, xts};

#[test]
fn decimal_money_is_exact() {
    let sum = money("0.1").checked_add(money("0.2")).unwrap();
    assert_eq!(sum, money("0.3"));
    assert_eq!(sum.to_string(), "0.3");
    assert_eq!(money("0.60"), money("0.6"));
    assert_eq!(money("0.000000001").atoms(), 1);
    assert_eq!(money("12").to_string(), "12");
    assert_eq!(money("0").to_string(), "0");
}

#[test]
fn malformed_amounts_are_invalid_requests() {
    for text in [
        "",
        "1.5e3",
        "-1",
        "+1",
        " 1",
        "1 ",
        ".5",
        "5.",
        "1.2.3",
        "0.0000000001",
        "1,5",
        "NaN",
    ] {
        let error = Amount::parse(xts(), text).unwrap_err();
        assert_eq!(error.class(), "invalid_request", "{text:?}");
    }
    let fractional_tokens = Amount::parse(CostUnit::OutputTokens, "1.5").unwrap_err();
    assert_eq!(fractional_tokens.class(), "invalid_request");
}

#[test]
fn integer_units_are_bounded_to_u64() {
    let max = u64::MAX.to_string();
    let parsed = Amount::parse(CostUnit::InputTokens, &max).unwrap();
    assert_eq!(parsed.atoms(), u128::from(u64::MAX));
    assert_eq!(
        Amount::parse(CostUnit::InputTokens, "18446744073709551616")
            .unwrap_err()
            .class(),
        "invalid_request"
    );
    let one = Amount::parse(CostUnit::InputTokens, "1").unwrap();
    assert!(parsed.checked_add(one).is_err());
}

#[test]
fn amounts_in_different_units_never_combine() {
    let other = Amount::parse(CostUnit::ComputeMs, "1").unwrap();
    assert_eq!(
        money("1").checked_add(other).unwrap_err().class(),
        "invalid_request"
    );
    let other_currency = CostUnit::Money(Currency::parse("XXX").unwrap());
    let elsewhere = Amount::parse(other_currency, "1").unwrap();
    assert!(money("1").checked_add(elsewhere).is_err());
}

#[test]
fn currency_codes_are_three_uppercase_letters() {
    assert_eq!(Currency::parse("XTS").unwrap().code(), "XTS");
    for code in ["xts", "XT", "XTSS", "X1S", ""] {
        assert!(Currency::parse(code).is_err(), "{code:?}");
    }
}

#[test]
fn fixture_price_table_charges_tokens_exactly() {
    let table = PriceTable::fixture();
    let usage = Usage {
        input_tokens: 1_000,
        output_tokens: 250,
        compute_ms: 1_200,
    };
    let cost = table
        .cost("fixture", "fixture-small", xts(), usage)
        .unwrap();
    assert_eq!(cost, money("0.002"));
    let large = table
        .cost("fixture", "fixture-large", xts(), usage)
        .unwrap();
    assert_eq!(large, money("0.0225"));
    let output = table
        .cost("fixture", "fixture-small", CostUnit::OutputTokens, usage)
        .unwrap();
    assert_eq!(output.atoms(), 250);
    let compute = table
        .cost("absent", "absent", CostUnit::ComputeMs, usage)
        .unwrap();
    assert_eq!(compute.atoms(), 1_200);
}

#[test]
fn price_lookups_refuse_unknown_models_and_foreign_currency() {
    let table = PriceTable::fixture();
    let usage = Usage::default();
    let missing = table.cost("fixture", "absent", xts(), usage).unwrap_err();
    assert_eq!(missing.class(), "not_found");
    let foreign = CostUnit::Money(Currency::parse("XXX").unwrap());
    let refused = table
        .cost("fixture", "fixture-small", foreign, usage)
        .unwrap_err();
    assert_eq!(refused.class(), "invalid_request");
}
