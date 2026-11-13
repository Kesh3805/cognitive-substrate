//! Durable background jobs.
//!
//! Delivery is **at-least-once**. Jobs are claimed under a lease; each claim
//! increments `lease_generation`, and every state transition after a claim
//! must present the generation it was claimed with (fencing), so a worker
//! whose lease expired cannot complete or fail a job another worker has
//! since reclaimed. Handlers make their effects idempotent.
//!
//! Every job runs as the principal that submitted it, so background
//! processing can never see or write more than its submitter could.

use cs_core::{AccessContext, Error, JobId, Result, TenantId};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Turn a raw observation into candidate memories.
    Perceive,
    /// Look for duplicates, supersessions and contradictions.
    Resolve,
    /// Compute (or backfill) an embedding for one revision in one space.
    Embed,
}

impl JobKind {
    pub const ALL: [Self; 3] = [Self::Perceive, Self::Resolve, Self::Embed];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Perceive => "perceive",
            Self::Resolve => "resolve",
            Self::Embed => "embed",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Leased,
    Succeeded,
    DeadLettered,
}

impl JobStatus {
    pub const ALL: [Self; 4] = [
        Self::Queued,
        Self::Leased,
        Self::Succeeded,
        Self::DeadLettered,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::Succeeded => "succeeded",
            Self::DeadLettered => "dead_lettered",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == v)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::DeadLettered)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub tenant_id: TenantId,
    pub kind: JobKind,
    /// The principal the job runs as.
    pub principal: AccessContext,
    pub payload: serde_json::Value,
    /// Unique per `(tenant, kind)` for the lifetime of the job record.
    /// Re-processing requires a new key (e.g. a new observation id).
    pub dedupe_key: String,
    pub status: JobStatus,
    pub attempts: u32,
    pub max_attempts: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub available_at: OffsetDateTime,
    #[serde(default)]
    pub lease_owner: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub lease_until: Option<OffsetDateTime>,
    /// Incremented on every claim; required for fenced transitions.
    pub lease_generation: u64,
    #[serde(default)]
    pub last_error: Option<JobError>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Job {
    pub fn validate(&self) -> Result<()> {
        if self.principal.tenant_id != self.tenant_id {
            return Err(Error::validation(
                "job principal must belong to the job tenant",
            ));
        }
        if self.max_attempts == 0 || self.max_attempts > 100 {
            return Err(Error::validation("max_attempts must be in 1..=100"));
        }
        if self.dedupe_key.is_empty() || self.dedupe_key.len() > 255 {
            return Err(Error::validation("dedupe_key must be 1..=255 bytes"));
        }
        Ok(())
    }

    /// Claimable at `now`: queued and due, or leased with an expired lease.
    pub fn is_claimable(&self, now: OffsetDateTime) -> bool {
        match self.status {
            JobStatus::Queued => self.available_at <= now,
            JobStatus::Leased => self.lease_until.is_some_and(|u| u <= now),
            _ => false,
        }
    }
}

/// Structured failure information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobError {
    pub permanent: bool,
    pub message: String,
    pub attempt: u32,
}

/// A fenced claim on a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub job_id: JobId,
    pub generation: u64,
}

/// What a claim returned, plus the token needed for later transitions.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedJob {
    pub job: Job,
    pub lease: Lease,
}

/// How a failed attempt should be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    Succeeded {
        result: serde_json::Value,
    },
    /// Requeue to run again no earlier than `at`.
    Retry {
        at: OffsetDateTime,
        error: JobError,
    },
    /// Give up permanently.
    DeadLetter {
        error: JobError,
    },
}

/// Bounded exponential backoff with deterministic jitter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub multiplier: f64,
    /// Fraction of the delay randomized, in `[0, 1)`.
    pub jitter_fraction: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_delay_ms: 1_000,
            max_delay_ms: 300_000,
            multiplier: 2.0,
            jitter_fraction: 0.2,
        }
    }
}

impl RetryPolicy {
    /// Delay before the next attempt after `attempt` (1-based) failed.
    /// `jitter` is a value in `[0, 1)` supplied by the caller, which keeps
    /// the computation deterministic and testable.
    pub fn delay(&self, attempt: u32, jitter: f64) -> Duration {
        let exp = self
            .multiplier
            .powi(i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX));
        let base = (self.initial_delay_ms as f64 * exp).min(self.max_delay_ms as f64);
        let spread = base * self.jitter_fraction.clamp(0.0, 0.99);
        let ms =
            (base - spread + 2.0 * spread * jitter.clamp(0.0, 1.0)).min(self.max_delay_ms as f64);
        Duration::milliseconds(ms.max(0.0) as i64)
    }
}

/// Deterministic jitter in `[0, 1)` derived from the job and attempt.
pub fn deterministic_jitter(job: JobId, attempt: u32) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in job.0.as_bytes().iter().chain(&attempt.to_le_bytes()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h >> 11) as f64 / (1_u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let p = RetryPolicy {
            jitter_fraction: 0.0,
            ..RetryPolicy::default()
        };
        assert_eq!(p.delay(1, 0.5), Duration::seconds(1));
        assert_eq!(p.delay(3, 0.5), Duration::seconds(4));
        assert_eq!(p.delay(30, 0.5), Duration::seconds(300));
    }

    #[test]
    fn jitter_stays_within_fraction() {
        let p = RetryPolicy::default();
        let lo = p.delay(2, 0.0);
        let hi = p.delay(2, 1.0);
        assert_eq!(lo, Duration::milliseconds(1_600));
        assert_eq!(hi, Duration::milliseconds(2_400));
    }

    #[test]
    fn jitter_is_deterministic() {
        let id = JobId::new_v7();
        assert_eq!(deterministic_jitter(id, 2), deterministic_jitter(id, 2));
        assert!((0.0..1.0).contains(&deterministic_jitter(id, 3)));
    }

    #[test]
    fn terminal_statuses() {
        assert!(JobStatus::Succeeded.is_terminal());
        assert!(!JobStatus::Leased.is_terminal());
    }
}
