//! The background worker: claims jobs, runs handlers, and commits each
//! handler's effects together with its fenced job transition.
//!
//! The worker is runtime-agnostic: call [`Worker::run_once`] from whatever
//! loop or scheduler the host application uses (the service binary drives
//! it with Tokio).

use std::sync::Arc;

use cs_core::{CognitiveEventType, Error, Result, Scope};
use cs_memory::{ClaimedJob, JobError, JobKind, JobOutcome, job::deterministic_jitter};
use cs_storage::{JobClaim, JobEffects};
use serde_json::json;
use time::Duration;

use crate::{CognitiveEngine, Extractor, processing::HandlerError};

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Lease owner name (shown on leased jobs).
    pub owner: String,
    pub kinds: Vec<JobKind>,
    pub batch_size: u32,
    pub lease: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            owner: format!("worker-{}", uuid::Uuid::now_v7()),
            kinds: JobKind::ALL.to_vec(),
            batch_size: 16,
            lease: Duration::seconds(60),
        }
    }
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkerReport {
    pub claimed: usize,
    pub succeeded: usize,
    pub retried: usize,
    pub dead_lettered: usize,
    /// Leases lost to another worker before completion (effects discarded).
    pub lost: usize,
}

#[derive(Debug, Clone)]
pub struct Worker {
    engine: CognitiveEngine,
    extractor: Arc<dyn Extractor>,
    config: WorkerConfig,
}

impl Worker {
    pub fn new(
        engine: CognitiveEngine,
        extractor: Arc<dyn Extractor>,
        config: WorkerConfig,
    ) -> Self {
        Self {
            engine,
            extractor,
            config,
        }
    }

    pub fn config(&self) -> &WorkerConfig {
        &self.config
    }

    /// Claim and process one batch.
    pub async fn run_once(&self) -> Result<WorkerReport> {
        let now = self.engine.now();
        let claimed = self
            .engine
            .store
            .claim_jobs(&JobClaim {
                kinds: self.config.kinds.clone(),
                owner: self.config.owner.clone(),
                now,
                lease_until: now + self.config.lease,
                limit: self.config.batch_size,
            })
            .await?;
        let mut report = WorkerReport {
            claimed: claimed.len(),
            ..WorkerReport::default()
        };
        for c in claimed {
            match self.process(c).await {
                Ok(Outcome::Succeeded) => report.succeeded += 1,
                Ok(Outcome::Retried) => report.retried += 1,
                Ok(Outcome::DeadLettered) => report.dead_lettered += 1,
                Ok(Outcome::Lost) => report.lost += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(report)
    }

    /// Run passes until a pass claims nothing (useful in tests and tools).
    pub async fn drain(&self) -> Result<WorkerReport> {
        let mut total = WorkerReport::default();
        for _ in 0..10_000 {
            let r = self.run_once().await?;
            total.claimed += r.claimed;
            total.succeeded += r.succeeded;
            total.retried += r.retried;
            total.dead_lettered += r.dead_lettered;
            total.lost += r.lost;
            if r.claimed == 0 {
                break;
            }
        }
        Ok(total)
    }

    async fn process(&self, c: ClaimedJob) -> Result<Outcome> {
        let job = &c.job;
        let handled = match job.kind {
            JobKind::Perceive => self.engine.perceive(job, self.extractor.as_ref()).await,
            JobKind::Resolve => self.engine.resolve(job).await,
            JobKind::Embed => self.engine.embed(job).await,
            JobKind::Project => self.engine.project(job).await,
            JobKind::Compile => self.engine.compile(job).await,
        };
        let now = self.engine.now();
        let (outcome, effects, kind) = match handled {
            Ok((effects, result)) => (
                JobOutcome::Succeeded { result },
                effects,
                Outcome::Succeeded,
            ),
            Err(HandlerError { permanent, message }) => {
                let error = JobError {
                    permanent,
                    message: message.clone(),
                    attempt: job.attempts,
                };
                if !permanent && job.attempts < job.max_attempts {
                    let delay = self
                        .engine
                        .processing
                        .retry
                        .delay(job.attempts, deterministic_jitter(job.id, job.attempts));
                    (
                        JobOutcome::Retry {
                            at: now + delay,
                            error,
                        },
                        JobEffects::default(),
                        Outcome::Retried,
                    )
                } else {
                    tracing::warn!(job = %job.id, kind = job.kind.as_str(), %message, "job dead-lettered");
                    let event = self.engine.draft(
                        &job.principal,
                        job.id.0,
                        &Scope::tenant(job.tenant_id),
                        CognitiveEventType::JobDeadLettered,
                        json!({
                            "job_id": job.id,
                            "kind": job.kind,
                            "attempts": job.attempts,
                            "permanent": permanent,
                        }),
                        now,
                    );
                    (
                        JobOutcome::DeadLetter { error },
                        JobEffects {
                            events: vec![event],
                            ..JobEffects::default()
                        },
                        Outcome::DeadLettered,
                    )
                }
            }
        };
        match self
            .engine
            .store
            .finish_job(c.lease, now, outcome, effects)
            .await
        {
            Ok(_) => Ok(kind),
            Err(Error::Conflict(m)) => {
                // Our lease was superseded; another worker owns the job and
                // nothing we computed was written.
                tracing::info!(job = %job.id, reason = %m, "lease lost; effects discarded");
                Ok(Outcome::Lost)
            }
            Err(e) => Err(e),
        }
    }

    /// Return any still-held lease to the queue (graceful shutdown helper).
    pub async fn release(&self, claimed: &ClaimedJob) -> Result<()> {
        self.engine
            .store
            .release_job(claimed.lease, self.engine.now())
            .await
    }
}

enum Outcome {
    Succeeded,
    Retried,
    DeadLettered,
    Lost,
}
