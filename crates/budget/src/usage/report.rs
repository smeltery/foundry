//! Aggregates over recorded attempts: daily consumption, grouped totals and
//! metric time series. Every sum is checked, so an overflowing report fails
//! instead of wrapping, and every ordering is total, so the same records always
//! produce the same report. Wire names are provisional pending NIC-34 D1/D6.
use crate::amount::{Amount, CostUnit, Currency};
use crate::error::BudgetError;
use crate::price::Usage;
use crate::usage::calendar::{DAY_MS, HOUR_MS, UtcDate, Window};
use crate::usage::record::AttemptUsage;
use std::collections::BTreeMap;

/// A recorded attempt with the time it was recorded.
pub(crate) struct Stored {
    pub record: AttemptUsage,
    pub recorded_at_ms: u64,
}

/// Summed usage and spend. Token and millisecond totals stay unsigned 64-bit
/// integers, as the consumption units require.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Totals {
    pub attempts: u64,
    pub usage: Usage,
    pub spend: Amount,
}

impl Totals {
    pub fn zero(currency: Currency) -> Self {
        Self {
            attempts: 0,
            usage: Usage::default(),
            spend: Amount::zero(CostUnit::Money(currency)),
        }
    }

    /// Adds one attempt, or fails without changing anything. A cost in another
    /// currency is `conflict`: no exchange rate exists to combine them.
    fn add(&mut self, record: &AttemptUsage) -> Result<(), BudgetError> {
        if record.cost.unit() != self.spend.unit() {
            return Err(BudgetError::Conflict { what: "currency" });
        }
        let attempts = self.attempts.checked_add(1).ok_or_else(overflow)?;
        let usage = add_usage(self.usage, record.usage)?;
        let spend = self.spend.checked_add(record.cost)?;
        *self = Self {
            attempts,
            usage,
            spend,
        };
        Ok(())
    }
}

fn overflow() -> BudgetError {
    BudgetError::invalid("total")
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

/// `consumption.daily`: one entry per UTC day with activity, oldest first.
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
    for stored in records {
        let date = UtcDate::containing(stored.recorded_at_ms);
        days.entry(date)
            .or_insert_with(|| Totals::zero(currency))
            .add(&stored.record)?;
        total.add(&stored.record)?;
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
}

/// The group an attempt counts toward. `Unattributed` collects attempts with
/// no session or user and sorts after every named group with equal spend.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GroupKey {
    Session(String),
    User(String),
    Account {
        provider_id: String,
        account_key: String,
    },
    Unattributed,
}

impl GroupKey {
    fn of(record: &AttemptUsage, group_by: GroupBy) -> Self {
        let named = |id: &Option<String>, wrap: fn(String) -> Self| {
            id.clone().map_or(Self::Unattributed, wrap)
        };
        match group_by {
            GroupBy::Session => named(&record.session_id, Self::Session),
            GroupBy::User => named(&record.user_id, Self::User),
            GroupBy::Account => Self::Account {
                provider_id: record.provider_id.clone(),
                account_key: record.account_key.clone(),
            },
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
    for stored in records {
        groups
            .entry(GroupKey::of(&stored.record, group_by))
            .or_insert_with(|| Totals::zero(currency))
            .add(&stored.record)?;
        total.add(&stored.record)?;
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
    pub attempts: u64,
    pub usage: Usage,
}

/// Buckets with activity, oldest first. Metrics carry no spend, so attempts
/// in any currency are counted.
pub(crate) fn metrics(
    bucket: Bucket,
    records: &[Stored],
) -> Result<Vec<MetricsPoint>, BudgetError> {
    let mut points: BTreeMap<u64, MetricsPoint> = BTreeMap::new();
    for stored in records {
        let start_ms = stored.recorded_at_ms - stored.recorded_at_ms % bucket.ms();
        let point = points.entry(start_ms).or_insert(MetricsPoint {
            start_ms,
            attempts: 0,
            usage: Usage::default(),
        });
        let attempts = point.attempts.checked_add(1).ok_or_else(overflow)?;
        point.usage = add_usage(point.usage, stored.record.usage)?;
        point.attempts = attempts;
    }
    Ok(points.into_values().collect())
}
