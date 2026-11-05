//! Aggregates over settled usage rows: daily consumption, grouped totals and
//! metric time series. Every sum is checked, so an overflowing report fails
//! instead of wrapping, and every ordering is total, so the same rows always
//! produce the same report. Wire names are provisional pending NIC-34 D1/D6.
//!
//! Spend reports (`daily`, `grouped`) cover only rows charged in the requested
//! currency; rows charged in another currency, or in a token or millisecond
//! unit, are left out because no exchange rate exists. `metrics` carries no
//! spend and counts every row.
use crate::amount::{Amount, CostUnit, Currency};
use crate::error::BudgetError;
use crate::price::Usage;
use crate::usage::calendar::{DAY_MS, HOUR_MS, UtcDate, Window};
use crate::usage::record::Attempt;
use std::collections::BTreeMap;

/// One settlement's usage row.
pub(crate) struct Stored {
    pub attempt: Attempt,
    pub usage: Usage,
    /// The amount the ledger committed, in the budget's unit.
    pub cost: Amount,
    pub settled_at_ms: u64,
}

impl Stored {
    fn charged_in(&self, currency: Currency) -> bool {
        self.cost.unit() == CostUnit::Money(currency)
    }
}

/// Summed usage and spend. Token and millisecond totals stay unsigned 64-bit
/// integers, as the consumption units require.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Totals {
    /// Settled model calls; an attempt with several calls counts each.
    pub settlements: u64,
    pub usage: Usage,
    pub spend: Amount,
}

impl Totals {
    pub fn zero(currency: Currency) -> Self {
        Self {
            settlements: 0,
            usage: Usage::default(),
            spend: Amount::zero(CostUnit::Money(currency)),
        }
    }

    /// Adds one row charged in this total's currency, or fails without
    /// changing anything.
    fn add(&mut self, stored: &Stored) -> Result<(), BudgetError> {
        let settlements = self.settlements.checked_add(1).ok_or_else(overflow)?;
        let usage = add_usage(self.usage, stored.usage)?;
        let spend = self
            .spend
            .atoms()
            .checked_add(stored.cost.atoms())
            .ok_or_else(overflow)?;
        *self = Self {
            settlements,
            usage,
            spend: Amount::from_atoms(self.spend.unit(), spend)?,
        };
        Ok(())
    }
}

/// Stored rows whose sum does not fit the report's integer types. The caller
/// asked for nothing invalid, so this is the store's failure, not theirs.
fn overflow() -> BudgetError {
    BudgetError::Unavailable {
        detail: "usage total overflows".into(),
    }
}

fn add_usage(left: Usage, right: Usage) -> Result<Usage, BudgetError> {
    let sum = |a: u64, b: u64| a.checked_add(b).ok_or_else(overflow);
    Ok(Usage {
        input_tokens: sum(left.input_tokens, right.input_tokens)?,
        output_tokens: sum(left.output_tokens, right.output_tokens)?,
        compute_ms: sum(left.compute_ms, right.compute_ms)?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyEntry {
    pub date: UtcDate,
    pub totals: Totals,
}

/// `consumption.daily`: one entry per UTC settlement day with activity,
/// oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyReport {
    pub window: Window,
    pub currency: Currency,
    pub daily: Vec<DailyEntry>,
    pub total: Totals,
}

pub(crate) fn daily(
    window: Window,
    currency: Currency,
    records: &[Stored],
) -> Result<DailyReport, BudgetError> {
    let mut total = Totals::zero(currency);
    let mut days: BTreeMap<UtcDate, Totals> = BTreeMap::new();
    for stored in records.iter().filter(|stored| stored.charged_in(currency)) {
        let date = UtcDate::containing(stored.settled_at_ms);
        days.entry(date)
            .or_insert_with(|| Totals::zero(currency))
            .add(stored)?;
        total.add(stored)?;
    }
    let daily = days
        .into_iter()
        .map(|(date, totals)| DailyEntry { date, totals })
        .collect();
    Ok(DailyReport {
        window,
        currency,
        daily,
        total,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupBy {
    Session,
    User,
    /// One provider account slot: `provider_id` and `account_key`.
    Account,
    /// Every settlement of one attempt.
    Attempt,
}

/// The group a settlement counts toward. `Unattributed` collects rows with no
/// session or user and sorts after every named group with equal spend.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GroupKey {
    Session(String),
    User(String),
    Account {
        provider_id: String,
        account_key: String,
    },
    Attempt(String),
    Unattributed,
}

impl GroupKey {
    fn of(attempt: &Attempt, group_by: GroupBy) -> Self {
        let named = |id: &Option<String>, wrap: fn(String) -> Self| {
            id.clone().map_or(Self::Unattributed, wrap)
        };
        match group_by {
            GroupBy::Session => named(&attempt.session_id, Self::Session),
            GroupBy::User => named(&attempt.user_id, Self::User),
            GroupBy::Account => Self::Account {
                provider_id: attempt.provider_id.clone(),
                account_key: attempt.account_key.clone(),
            },
            GroupBy::Attempt => Self::Attempt(attempt.attempt_id.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupTotal {
    pub key: GroupKey,
    pub totals: Totals,
}

/// Whole-window totals per group, highest spend first, then by key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupedReport {
    pub window: Window,
    pub currency: Currency,
    pub group_by: GroupBy,
    pub items: Vec<GroupTotal>,
    pub total: Totals,
}

pub(crate) fn grouped(
    window: Window,
    currency: Currency,
    group_by: GroupBy,
    records: &[Stored],
) -> Result<GroupedReport, BudgetError> {
    let mut total = Totals::zero(currency);
    let mut groups: BTreeMap<GroupKey, Totals> = BTreeMap::new();
    for stored in records.iter().filter(|stored| stored.charged_in(currency)) {
        groups
            .entry(GroupKey::of(&stored.attempt, group_by))
            .or_insert_with(|| Totals::zero(currency))
            .add(stored)?;
        total.add(stored)?;
    }
    let mut items: Vec<GroupTotal> = groups
        .into_iter()
        .map(|(key, totals)| GroupTotal { key, totals })
        .collect();
    // The map already ordered keys, and the sort is stable.
    items.sort_by_key(|item| std::cmp::Reverse(item.totals.spend.atoms()));
    Ok(GroupedReport {
        window,
        currency,
        group_by,
        items,
        total,
    })
}

/// The width of one metrics bucket; buckets align to UTC midnight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Bucket {
    Hour,
    #[default]
    Day,
}

impl Bucket {
    pub fn ms(&self) -> u64 {
        match self {
            Self::Hour => HOUR_MS,
            Self::Day => DAY_MS,
        }
    }
}

/// One bucket of `consumption.metrics`. `start_ms` is the bucket's first
/// millisecond since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetricsPoint {
    pub start_ms: u64,
    pub settlements: u64,
    pub usage: Usage,
}

/// Buckets with activity, oldest first. Metrics carry no spend, so rows
/// charged in any unit are counted.
pub(crate) fn metrics(
    bucket: Bucket,
    records: &[Stored],
) -> Result<Vec<MetricsPoint>, BudgetError> {
    let mut points: BTreeMap<u64, MetricsPoint> = BTreeMap::new();
    for stored in records {
        let start_ms = stored.settled_at_ms - stored.settled_at_ms % bucket.ms();
        let point = points.entry(start_ms).or_insert(MetricsPoint {
            start_ms,
            settlements: 0,
            usage: Usage::default(),
        });
        let settlements = point.settlements.checked_add(1).ok_or_else(overflow)?;
        point.usage = add_usage(point.usage, stored.usage)?;
        point.settlements = settlements;
    }
    Ok(points.into_values().collect())
}
