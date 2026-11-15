//! Experiment 006: the lexical channel in one store round trip.
//!
//! Everything here is fixed by `benchmarks/006-single-round-trip-lexical/
//! PREREGISTRATION.md`. It reuses 005's generator, metrics and gates, on
//! new held-out seeds, with the strategy order rotated per query.

use std::sync::Arc;

use cs_memory::CandidateGeneration;
use cs_storage::CognitiveStore;

use crate::exp005::{Row, Summary, run_seed_with, summarize_for};

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [201, 202, 203, 204, 205];
pub const PRIMARY: &str = "L";

pub fn strategies() -> Vec<(&'static str, CandidateGeneration)> {
    vec![
        ("v1", CandidateGeneration::RecencyV1),
        (
            PRIMARY,
            CandidateGeneration::Channels {
                lexical: 100,
                dense: 0,
            },
        ),
    ]
}

pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<Vec<Row>, String> {
    run_seed_with(store, seed, &strategies(), true).await
}

pub fn summarize(rows: &[Row]) -> Summary {
    let names: Vec<&'static str> = strategies().into_iter().map(|(n, _)| n).collect();
    summarize_for(rows, &names, PRIMARY)
}
