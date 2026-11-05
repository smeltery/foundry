//! UTC calendar days and report windows. The specification fixes reports to UTC
//! calendar days: a day is 86,400,000 milliseconds of Unix time, which has no
//! leap seconds, so a day number is simply `ms / DAY_MS` and no time zone data
//! is needed. A window includes its start day and excludes its end day.
use crate::error::BudgetError;
use std::fmt;

pub const DAY_MS: u64 = 86_400_000;
pub const HOUR_MS: u64 = 3_600_000;
/// The longest report window.
pub const WINDOW_MAX_DAYS: u64 = 366;
/// The window used when a report names none.
pub const WINDOW_DEFAULT_DAYS: u64 = 30;

/// Dates before the Unix epoch have no millisecond value in the store.
const YEAR_MIN: u32 = 1970;
const YEAR_MAX: u32 = 9999;

/// A proleptic Gregorian calendar date in UTC, from 1970-01-01 to 9999-12-31.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UtcDate {
    /// Days since 1970-01-01.
    day: u64,
}

impl UtcDate {
    pub fn new(year: u32, month: u32, day: u32) -> Result<Self, BudgetError> {
        let invalid = || BudgetError::invalid("date");
        if !(YEAR_MIN..=YEAR_MAX).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
        {
            return Err(invalid());
        }
        let days = days_from_civil(year, month, day);
        Ok(Self {
            day: u64::try_from(days).map_err(|_| invalid())?,
        })
    }

    /// Parses exactly `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Result<Self, BudgetError> {
        let invalid = || BudgetError::invalid("date");
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(invalid());
        }
        let field = |range: std::ops::Range<usize>| -> Result<u32, BudgetError> {
            let part = &text[range];
            if !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }
            part.parse().map_err(|_| invalid())
        };
        Self::new(field(0..4)?, field(5..7)?, field(8..10)?)
    }

    /// The UTC day containing `ms` since the Unix epoch.
    pub fn containing(ms: u64) -> Self {
        Self { day: ms / DAY_MS }
    }

    /// Milliseconds since the Unix epoch at 00:00:00 UTC on this date.
    pub fn start_ms(&self) -> u64 {
        // At most 9999-12-31, so this stays far below `u64::MAX`.
        self.day * DAY_MS
    }

    fn checked_add_days(self, days: u64) -> Option<Self> {
        let day = self.day.checked_add(days)?;
        let last = days_from_civil(YEAR_MAX, 12, 31) as u64;
        (day <= last + 1).then_some(Self { day })
    }
}

impl fmt::Display for UtcDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.day as i64);
        write!(formatter, "{year:04}-{month:02}-{day:02}")
    }
}

/// A report window of whole UTC days: `start` is included, `end` excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    start: UtcDate,
    end: UtcDate,
}

impl Window {
    /// A start that is not before the end, or a span over 366 days, is
    /// `invalid_request`.
    pub fn new(start: UtcDate, end: UtcDate) -> Result<Self, BudgetError> {
        if start >= end || end.day - start.day > WINDOW_MAX_DAYS {
            return Err(BudgetError::invalid("window"));
        }
        Ok(Self { start, end })
    }

    /// The `days` UTC days ending with the day containing `now_ms`, so today's
    /// activity is included.
    pub fn last_days(now_ms: u64, days: u64) -> Result<Self, BudgetError> {
        let invalid = || BudgetError::invalid("window");
        let end = UtcDate::containing(now_ms)
            .checked_add_days(1)
            .ok_or_else(invalid)?;
        let start = UtcDate {
            day: end.day.checked_sub(days).ok_or_else(invalid)?,
        };
        Self::new(start, end)
    }

    /// The default window: the last 30 UTC days including today.
    pub fn default_at(now_ms: u64) -> Result<Self, BudgetError> {
        Self::last_days(now_ms, WINDOW_DEFAULT_DAYS)
    }

    pub fn start(&self) -> UtcDate {
        self.start
    }

    pub fn end(&self) -> UtcDate {
        self.end
    }

    pub fn days(&self) -> u64 {
        self.end.day - self.start.day
    }
}

fn is_leap(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a Gregorian date, counting eras of 400 years
/// (146,097 days) from a March-based year so February's length falls last.
fn days_from_civil(year: u32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
