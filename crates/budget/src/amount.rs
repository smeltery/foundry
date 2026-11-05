//! Cost units and exact amounts. Floats are never used: integer units count whole
//! tokens or milliseconds, and money is held as integer nano-units (9 fractional
//! digits), so `0.1` plus `0.2` is exactly `0.3`.
use crate::error::BudgetError;
use std::fmt;

/// Fractional digits a money amount may carry.
pub const MONEY_FRACTION_DIGITS: usize = 9;
const MONEY_SCALE: u128 = 1_000_000_000;

/// ISO 4217 alphabetic currency code: exactly three uppercase ASCII letters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Currency([u8; 3]);

impl Currency {
    pub fn parse(code: &str) -> Result<Self, BudgetError> {
        let bytes: [u8; 3] = code
            .as_bytes()
            .try_into()
            .map_err(|_| BudgetError::invalid("currency"))?;
        if !bytes.iter().all(u8::is_ascii_uppercase) {
            return Err(BudgetError::invalid("currency"));
        }
        Ok(Self(bytes))
    }

    pub fn code(&self) -> &str {
        std::str::from_utf8(&self.0).expect("currency codes are validated ASCII")
    }
}

/// The unit one budget ceiling is denominated in. Wire names are provisional.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CostUnit {
    InputTokens,
    OutputTokens,
    ComputeMs,
    Money(Currency),
}

impl CostUnit {
    pub fn name(&self) -> &'static str {
        match self {
            Self::InputTokens => "input_tokens",
            Self::OutputTokens => "output_tokens",
            Self::ComputeMs => "compute_ms",
            Self::Money(_) => "money",
        }
    }

    fn fraction_digits(&self) -> usize {
        match self {
            Self::Money(_) => MONEY_FRACTION_DIGITS,
            _ => 0,
        }
    }

    /// Token and millisecond counts are unsigned 64-bit integers by specification.
    fn max_atoms(&self) -> u128 {
        match self {
            Self::Money(_) => u128::MAX,
            _ => u128::from(u64::MAX),
        }
    }
}

/// A nonnegative quantity in one cost unit, stored as integer atoms
/// (whole counts, or nano-units for money).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Amount {
    unit: CostUnit,
    atoms: u128,
}

impl Amount {
    pub fn zero(unit: CostUnit) -> Self {
        Self { unit, atoms: 0 }
    }

    pub fn from_atoms(unit: CostUnit, atoms: u128) -> Result<Self, BudgetError> {
        if atoms > unit.max_atoms() {
            return Err(BudgetError::invalid("amount"));
        }
        Ok(Self { unit, atoms })
    }

    /// Parses a plain decimal string: ASCII digits, optionally one `.` followed by
    /// at most 9 digits for money. Signs, exponents, whitespace and fractional
    /// token counts are `invalid_request`.
    pub fn parse(unit: CostUnit, text: &str) -> Result<Self, BudgetError> {
        let invalid = || BudgetError::invalid("amount");
        let is_digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        let (whole, fraction) = match text.split_once('.') {
            Some((whole, fraction)) => (whole, fraction),
            None => (text, ""),
        };
        let has_fraction = text.contains('.');
        if !is_digits(whole)
            || (has_fraction && !is_digits(fraction))
            || fraction.len() > unit.fraction_digits()
        {
            return Err(invalid());
        }
        let padding = unit.fraction_digits() - fraction.len();
        let digits = whole
            .bytes()
            .chain(fraction.bytes())
            .chain(std::iter::repeat_n(b'0', padding));
        let mut atoms: u128 = 0;
        for digit in digits {
            atoms = atoms
                .checked_mul(10)
                .and_then(|value| value.checked_add(u128::from(digit - b'0')))
                .ok_or_else(invalid)?;
        }
        Self::from_atoms(unit, atoms)
    }

    pub fn unit(&self) -> CostUnit {
        self.unit
    }

    pub fn atoms(&self) -> u128 {
        self.atoms
    }

    pub fn checked_add(self, other: Amount) -> Result<Amount, BudgetError> {
        if self.unit != other.unit {
            return Err(BudgetError::invalid("unit"));
        }
        let atoms = self
            .atoms
            .checked_add(other.atoms)
            .ok_or(BudgetError::invalid("amount"))?;
        Self::from_atoms(self.unit, atoms)
    }
}

/// Formats the canonical decimal string without the currency code: integer units
/// as plain digits, money with trailing fractional zeros removed.
impl fmt::Display for Amount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !matches!(self.unit, CostUnit::Money(_)) {
            return write!(formatter, "{}", self.atoms);
        }
        let whole = self.atoms / MONEY_SCALE;
        let fraction = self.atoms % MONEY_SCALE;
        if fraction == 0 {
            return write!(formatter, "{whole}");
        }
        let fraction = format!("{fraction:09}");
        write!(formatter, "{whole}.{}", fraction.trim_end_matches('0'))
    }
}
