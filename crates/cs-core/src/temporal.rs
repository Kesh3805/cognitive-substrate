//! Bitemporal intervals and injectable clocks.
//!
//! * **Valid time** — when a proposition holds in the represented world.
//! * **Transaction time** — when a version was present in the substrate.
//!
//! Intervals are half-open `[start, end)`; `end = None` means unbounded.
//! All timestamps are truncated to microsecond precision so that they
//! round-trip losslessly through PostgreSQL `timestamptz`.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeInterval {
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub end: Option<OffsetDateTime>,
}

impl TimeInterval {
    pub fn from(start: OffsetDateTime) -> Self {
        Self { start, end: None }
    }

    pub fn bounded(start: OffsetDateTime, end: OffsetDateTime) -> Self {
        Self {
            start,
            end: Some(end),
        }
    }

    pub fn contains(&self, at: OffsetDateTime) -> bool {
        self.start <= at && self.end.is_none_or(|end| at < end)
    }

    pub fn overlaps(&self, other: &TimeInterval) -> bool {
        let starts_before_other_ends = other.end.is_none_or(|e| self.start < e);
        let other_starts_before_self_ends = self.end.is_none_or(|e| other.start < e);
        starts_before_other_ends && other_starts_before_self_ends
    }

    pub fn is_open(&self) -> bool {
        self.end.is_none()
    }

    pub fn validate(&self) -> Result<()> {
        match self.end {
            Some(end) if end <= self.start => {
                Err(Error::validation("interval end must be after start"))
            }
            _ => Ok(()),
        }
    }

    /// Normalize both bounds to UTC microseconds.
    pub fn normalized(self) -> Self {
        Self {
            start: truncate_micros(self.start),
            end: self.end.map(truncate_micros),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bitemporal {
    pub valid_time: TimeInterval,
    /// Assigned and closed exclusively by storage transactions.
    pub transaction_time: TimeInterval,
}

/// Truncate to microseconds in UTC (PostgreSQL precision).
pub fn truncate_micros(t: OffsetDateTime) -> OffsetDateTime {
    let t = t.to_offset(UtcOffset::UTC);
    let micros = t.nanosecond() / 1_000 * 1_000;
    t.replace_nanosecond(micros).unwrap_or(t)
}

/// Source of "now". Injected so that tests and replays are deterministic.
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        truncate_micros(OffsetDateTime::now_utc())
    }
}

/// A manually advanced clock for tests and simulations.
#[derive(Debug)]
pub struct FixedClock(Mutex<OffsetDateTime>);

impl FixedClock {
    pub fn new(at: OffsetDateTime) -> Self {
        Self(Mutex::new(truncate_micros(at)))
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.0.lock().unwrap_or_else(|p| p.into_inner());
        *now = truncate_micros(*now + by);
    }

    pub fn set(&self, at: OffsetDateTime) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = truncate_micros(at);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn half_open_semantics() {
        let i = TimeInterval::bounded(
            datetime!(2026-01-01 0:00 UTC),
            datetime!(2026-02-01 0:00 UTC),
        );
        assert!(i.contains(datetime!(2026-01-01 0:00 UTC)));
        assert!(!i.contains(datetime!(2026-02-01 0:00 UTC)));
    }

    #[test]
    fn overlap_is_symmetric_and_touching_is_disjoint() {
        let a = TimeInterval::bounded(
            datetime!(2026-01-01 0:00 UTC),
            datetime!(2026-02-01 0:00 UTC),
        );
        let b = TimeInterval::from(datetime!(2026-02-01 0:00 UTC));
        let c = TimeInterval::from(datetime!(2026-01-15 0:00 UTC));
        assert!(!a.overlaps(&b) && !b.overlaps(&a));
        assert!(a.overlaps(&c) && c.overlaps(&a));
    }

    #[test]
    fn rejects_inverted_interval() {
        let i = TimeInterval::bounded(
            datetime!(2026-02-01 0:00 UTC),
            datetime!(2026-01-01 0:00 UTC),
        );
        assert!(i.validate().is_err());
    }

    #[test]
    fn truncation_is_microsecond() {
        let t = datetime!(2026-01-01 0:00:00.123456789 UTC);
        assert_eq!(truncate_micros(t).nanosecond(), 123_456_000);
    }
}
