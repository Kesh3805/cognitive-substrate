//! The "crowded" query family for experiment 009.
//!
//! It extends 005's corpus: for a few services, many recent notes mention
//! the service's data store in words that match a store question better
//! than the store fact itself (4 shared query terms against the fact's 3),
//! so a small lexical budget fills with notes and cuts the fact off. Gold
//! labels still come from the generator, never from text similarity.

use serde::Serialize;
use time::Duration;

use crate::{
    corpus::{self, Family, Insert, InsertKind, Owner, Relation},
    rng::Rng,
};

/// Services that get crowding notes.
pub const CROWDED_SERVICES: usize = 15;
/// Notes per crowded service: more than a 25-slot first pass holds.
pub const NOTES_PER_SERVICE: usize = 30;
/// Notes fall in the second half of the year, after their service's fact.
const SECOND_HALF_DAYS: i64 = corpus::DAYS / 2;

/// A query family of experiment 009: 005's five families plus `crowded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family9 {
    Direct,
    LexicalTrap,
    Semantic,
    Temporal,
    Scope,
    Crowded,
}

impl Family9 {
    pub const ALL: [Self; 6] = [
        Self::Direct,
        Self::LexicalTrap,
        Self::Semantic,
        Self::Temporal,
        Self::Scope,
        Self::Crowded,
    ];

    fn from_base(f: Family) -> Self {
        match f {
            Family::Direct => Self::Direct,
            Family::LexicalTrap => Self::LexicalTrap,
            Family::Semantic => Self::Semantic,
            Family::Temporal => Self::Temporal,
            Family::Scope => Self::Scope,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Query9 {
    pub family: Family9,
    pub text: String,
    /// `(insert index, grade)`.
    pub gold: Vec<(usize, u8)>,
    pub forbidden_candidates: Vec<usize>,
    pub forbidden_selected: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Corpus9 {
    pub inserts: Vec<Insert>,
    pub queries: Vec<Query9>,
    pub query_at: time::OffsetDateTime,
}

pub fn crowding_note(n: usize, service: &str) -> String {
    format!("standup note c{n}: the {service} data store came up again")
}

/// 005's corpus for `seed`, plus crowding notes and one crowded query per
/// crowded service.
pub fn generate(seed: u64) -> Corpus9 {
    let base = corpus::generate(seed);
    let mut rng = Rng::new(seed ^ 0x0009_0009_0009_0009);

    // Services with exactly one Alice store fact that is still valid (no
    // temporal change) and that are not a replica twin.
    let store_facts = |svc: &str| -> Vec<usize> {
        base.inserts
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                i.owner == Owner::Alice
                    && matches!(&i.kind, InsertKind::Fact { service, relation: Relation::Store, .. } if service == svc)
            })
            .map(|(n, _)| n)
            .collect()
    };
    let mut services: Vec<String> = base
        .inserts
        .iter()
        .filter_map(|i| match &i.kind {
            InsertKind::Fact {
                service,
                relation: Relation::Store,
                ..
            } if i.owner == Owner::Alice && !service.ends_with(" replica") => Some(service.clone()),
            _ => None,
        })
        .collect();
    services.sort();
    services.dedup();
    services.retain(|s| {
        let f = store_facts(s);
        f.len() == 1 && base.inserts[f[0]].valid_until.is_none()
    });
    rng.shuffle(&mut services);
    services.truncate(CROWDED_SERVICES);

    // Old inserts keep their tag (Some(index)); crowding notes have none.
    let half = corpus::start() + Duration::days(SECOND_HALF_DAYS);
    let end = corpus::start() + Duration::days(corpus::DAYS);
    let mut all: Vec<(Insert, Option<usize>)> = base
        .inserts
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, ins)| (ins, Some(i)))
        .collect();
    let mut crowded_facts = Vec::new();
    let mut n = 0;
    for svc in &services {
        let fact = store_facts(svc)[0];
        crowded_facts.push((svc.clone(), fact));
        let from = base.inserts[fact].at.max(half);
        let span = (end - from).whole_minutes().max(1) as usize;
        for _ in 0..NOTES_PER_SERVICE {
            all.push((
                Insert {
                    at: from + Duration::minutes(1 + rng.below(span) as i64),
                    owner: Owner::Alice,
                    kind: InsertKind::Note(crowding_note(n, svc)),
                    valid_until: None,
                },
                None,
            ));
            n += 1;
        }
    }
    // Stable: equal times keep the base order, then note order.
    all.sort_by_key(|(ins, _)| ins.at);
    let mut new_index = vec![0usize; base.inserts.len()];
    for (i, (_, old)) in all.iter().enumerate() {
        if let Some(old) = old {
            new_index[*old] = i;
        }
    }
    let remap = |v: &[usize]| v.iter().map(|i| new_index[*i]).collect::<Vec<_>>();

    let mut queries: Vec<Query9> = base
        .queries
        .iter()
        .map(|q| Query9 {
            family: Family9::from_base(q.family),
            text: q.text.clone(),
            gold: q.gold.iter().map(|(i, g)| (new_index[*i], *g)).collect(),
            forbidden_candidates: remap(&q.forbidden_candidates),
            forbidden_selected: remap(&q.forbidden_selected),
        })
        .collect();
    for (svc, fact) in crowded_facts {
        queries.push(Query9 {
            family: Family9::Crowded,
            text: corpus::direct(Relation::Store, &svc),
            gold: vec![(new_index[fact], 2)],
            forbidden_candidates: Vec::new(),
            forbidden_selected: Vec::new(),
        });
    }
    Corpus9 {
        inserts: all.into_iter().map(|(i, _)| i).collect(),
        queries,
        query_at: base.query_at,
    }
}

#[cfg(test)]
mod tests {
    use cs_memory::lexical::{overlap, terms};

    use super::*;

    #[test]
    fn crowding_is_deterministic_and_outranks_the_fact() {
        let a = generate(1);
        let b = generate(1);
        assert_eq!(
            a.inserts.iter().map(Insert::text).collect::<Vec<_>>(),
            b.inserts.iter().map(Insert::text).collect::<Vec<_>>()
        );
        assert_eq!(
            a.inserts.len(),
            corpus::CORPUS_SIZE + CROWDED_SERVICES * NOTES_PER_SERVICE
        );
        assert!(a.inserts.windows(2).all(|w| w[0].at <= w[1].at));
        let crowded: Vec<&Query9> = a
            .queries
            .iter()
            .filter(|q| q.family == Family9::Crowded)
            .collect();
        assert_eq!(crowded.len(), CROWDED_SERVICES);
        for q in crowded {
            let qt = terms(&q.text);
            let fact = &a.inserts[q.gold[0].0];
            let fact_overlap = overlap(&qt, &terms(&fact.text()));
            assert_eq!(fact_overlap, 3, "{}", fact.text());
            let svc = match &fact.kind {
                InsertKind::Fact { service, .. } => service.clone(),
                InsertKind::Note(_) => unreachable!(),
            };
            let louder: Vec<&Insert> = a
                .inserts
                .iter()
                .filter(|i| overlap(&qt, &terms(&i.text())) > fact_overlap)
                .collect();
            assert!(louder.len() >= NOTES_PER_SERVICE, "{svc}");
            assert!(louder.iter().all(|i| i.at > fact.at));
        }
        // Remapping kept every base query pointing at the same text.
        let base = corpus::generate(1);
        for (q, bq) in a.queries.iter().zip(&base.queries) {
            for ((i, _), (j, _)) in q.gold.iter().zip(&bq.gold) {
                assert_eq!(a.inserts[*i].text(), base.inserts[*j].text());
            }
        }
    }
}
