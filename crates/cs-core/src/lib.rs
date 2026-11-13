//! Foundational contracts shared by every Cognitive Substrate crate.
//!
//! This crate has no knowledge of storage, transport or model providers. It
//! defines *who* (identity and scope), *where from* (provenance), *when*
//! (bitemporal intervals and clocks) and *what happened* (cognitive events).

pub mod error;
pub mod event;
pub mod ids;
pub mod provenance;
pub mod scope;
pub mod temporal;

pub use error::{Error, Result};
pub use event::{CognitiveEvent, CognitiveEventType, EventDraft, EventQuery};
pub use ids::*;
pub use provenance::{ActorRef, ExtractionInfo, Provenance, SourceRef};
pub use scope::{AccessContext, Scope, ScopeFilter, Visibility};
pub use temporal::{Bitemporal, Clock, FixedClock, SystemClock, TimeInterval};
