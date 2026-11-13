//! Strongly typed identifiers.
//!
//! All identifiers are UUIDs. New values are generated explicitly at the
//! application boundary with [`new_v7`](TenantId::new_v7); domain constructors
//! never mint identifiers implicitly.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a new time-ordered (v7) identifier.
            pub fn new_v7() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    )+};
}

id_type!(
    /// Hard isolation boundary. Nothing crosses tenants.
    TenantId,
    UserId,
    AgentId,
    ProjectId,
    ConversationId,
    /// Logical memory identity, stable across revisions.
    MemoryId,
    /// One immutable revision of a memory.
    MemoryRevisionId,
    /// One revision of a belief. The first revision's id is the lineage id.
    BeliefId,
    EvidenceId,
    EventId,
    EventStreamId,
    RecallTraceId,
    CapabilityId,
    /// A durable background job.
    JobId,
    /// A raw observation submitted for perception.
    ObservationId,
    /// A proposed change awaiting an explicit decision.
    ProposalId,
);
