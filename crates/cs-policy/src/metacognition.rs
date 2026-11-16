//! Signals for the metacognitive policy (M6.1) that need the records
//! themselves.

use cs_core::MemoryId;
use cs_memory::MemoryRecord;

use crate::resolve::{single_valued, triple};

/// Pairs of records that state different values for the same subject and
/// predicate (single-valued domains only) while valid at the same time:
/// the evidence disagrees, and nothing has resolved it yet. Each pair is
/// ordered by id, and pairs are sorted.
pub fn contradictions(records: &[&MemoryRecord]) -> Vec<(MemoryId, MemoryId)> {
    let mut out = Vec::new();
    for (i, a) in records.iter().enumerate() {
        if !single_valued(a.domain()) {
            continue;
        }
        let Some(ta) = triple(&a.content) else {
            continue;
        };
        for b in &records[i + 1..] {
            if a.id == b.id || a.domain() != b.domain() {
                continue;
            }
            let Some(tb) = triple(&b.content) else {
                continue;
            };
            if ta.0 == tb.0
                && ta.1 == tb.1
                && ta.2 != tb.2
                && a.temporal.valid_time.overlaps(&b.temporal.valid_time)
            {
                out.push(if a.id < b.id {
                    (a.id, b.id)
                } else {
                    (b.id, a.id)
                });
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
