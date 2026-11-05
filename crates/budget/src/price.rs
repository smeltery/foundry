//! Per-call cost estimation against a price table. Only the fixture table exists:
//! its rates are synthetic, denominated in `XTS` (the ISO 4217 code reserved for
//! testing) and do not describe any real service.
use crate::amount::{Amount, CostUnit, Currency};
use crate::error::BudgetError;

/// Usage of one model call, or its maximum when estimating a reservation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub compute_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPrice {
    pub provider_id: &'static str,
    pub model: &'static str,
    pub currency: Currency,
    /// Nano-units of `currency` per token.
    pub input_nanos_per_token: u128,
    pub output_nanos_per_token: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceTable {
    entries: Vec<ModelPrice>,
}

impl PriceTable {
    pub fn new(entries: Vec<ModelPrice>) -> Self {
        Self { entries }
    }

    /// Synthetic rates for the `fixture` provider. Tests never charge real money.
    pub fn fixture() -> Self {
        let currency = Currency::parse("XTS").expect("XTS is a valid currency code");
        Self::new(vec![
            ModelPrice {
                provider_id: "fixture",
                model: "fixture-small",
                currency,
                input_nanos_per_token: 1_000,
                output_nanos_per_token: 4_000,
            },
            ModelPrice {
                provider_id: "fixture",
                model: "fixture-large",
                currency,
                input_nanos_per_token: 10_000,
                output_nanos_per_token: 50_000,
            },
        ])
    }

    pub fn price(&self, provider_id: &str, model: &str) -> Result<&ModelPrice, BudgetError> {
        self.entries
            .iter()
            .find(|entry| entry.provider_id == provider_id && entry.model == model)
            .ok_or(BudgetError::NotFound {
                what: "model_price",
            })
    }

    /// The cost of `usage` in `unit`. Token and millisecond units read the usage
    /// directly; money prices it from the table in the table's own currency, and a
    /// different currency is refused because no exchange rate exists.
    pub fn cost(
        &self,
        provider_id: &str,
        model: &str,
        unit: CostUnit,
        usage: Usage,
    ) -> Result<Amount, BudgetError> {
        let count = |value: u64| Amount::from_atoms(unit, u128::from(value));
        match unit {
            CostUnit::InputTokens => count(usage.input_tokens),
            CostUnit::OutputTokens => count(usage.output_tokens),
            CostUnit::ComputeMs => count(usage.compute_ms),
            CostUnit::Money(currency) => {
                let price = self.price(provider_id, model)?;
                if price.currency != currency {
                    return Err(BudgetError::invalid("currency"));
                }
                let overflow = || BudgetError::invalid("amount");
                let input = price
                    .input_nanos_per_token
                    .checked_mul(u128::from(usage.input_tokens))
                    .ok_or_else(overflow)?;
                let output = price
                    .output_nanos_per_token
                    .checked_mul(u128::from(usage.output_tokens))
                    .ok_or_else(overflow)?;
                Amount::from_atoms(unit, input.checked_add(output).ok_or_else(overflow)?)
            }
        }
    }
}
