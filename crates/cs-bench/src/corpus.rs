//! Deterministic corpus and query generation for experiment 005.
//!
//! Relevance comes from what the generator knows (which fact a query asks
//! about, and whether it is current), never from text similarity, so no
//! retrieval strategy can influence its own gold labels.

use serde::Serialize;
use time::{Duration, OffsetDateTime};

use crate::rng::Rng;

pub const CORPUS_SIZE: usize = 2_000;
pub const SERVICES: usize = 150;
pub const REPLICAS: usize = 20;
pub const CHANGING: usize = 40;
pub const BOB_COPIES: usize = 50;
pub const QUERIES_PER_FAMILY: usize = 50;
pub const RECENT_WINDOW: usize = 200;
pub const DAYS: i64 = 365;

const A: &[&str] = &[
    "orion", "atlas", "vega", "lyra", "nova", "helix", "quartz", "ember", "cobalt", "zephyr",
    "aurora", "falcon", "onyx", "polar", "titan", "drift", "ripple", "summit", "harbor", "cinder",
];
const B: &[&str] = &[
    "ledger", "gateway", "relay", "vault", "beacon", "forge", "signal", "pulse", "anchor",
    "lattice", "prism", "harvest", "compass", "spindle", "engine", "router",
];
const STORES: &[&str] = &[
    "postgres",
    "cassandra",
    "redis",
    "mongodb",
    "dynamodb",
    "clickhouse",
    "elasticsearch",
    "mysql",
    "cockroachdb",
    "sqlite",
    "bigtable",
    "scylladb",
];
const TEAMS: &[&str] = &[
    "payments",
    "identity",
    "search",
    "growth",
    "platform",
    "billing",
    "risk",
    "messaging",
    "catalog",
    "analytics",
    "mobile",
    "infra",
];
const REGIONS: &[&str] = &[
    "eu-west-1",
    "eu-central-1",
    "us-east-1",
    "us-west-2",
    "ap-south-1",
    "ap-northeast-1",
    "sa-east-1",
    "ca-central-1",
];
const TOPICS: &[&str] = &[
    "rollout",
    "incident review",
    "latency budget",
    "capacity plan",
    "migration",
    "on-call",
    "retro",
    "load test",
    "schema change",
    "cost report",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    Alice,
    Bob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Store,
    Owner,
    Region,
    Port,
    Depends,
}

/// One memory to write, in chronological order.
#[derive(Debug, Clone)]
pub struct Insert {
    pub at: OffsetDateTime,
    pub owner: Owner,
    /// Semantic fact `(subject, predicate, object)` or an episodic note.
    pub kind: InsertKind,
    /// Valid-time end, for facts that stopped being true.
    pub valid_until: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub enum InsertKind {
    Fact {
        service: String,
        relation: Relation,
        predicate: &'static str,
        object: String,
    },
    Note(String),
}

impl Insert {
    pub fn text(&self) -> String {
        match &self.kind {
            InsertKind::Fact {
                service,
                predicate,
                object,
                ..
            } => format!("{service} {predicate} {object}"),
            InsertKind::Note(t) => t.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Direct,
    LexicalTrap,
    Semantic,
    Temporal,
    Scope,
}

impl Family {
    pub const ALL: [Self; 5] = [
        Self::Direct,
        Self::LexicalTrap,
        Self::Semantic,
        Self::Temporal,
        Self::Scope,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Age {
    Recent,
    Old,
}

#[derive(Debug, Clone, Serialize)]
pub struct Query {
    pub family: Family,
    pub age: Age,
    pub text: String,
    /// `(insert index, grade)`.
    pub gold: Vec<(usize, u8)>,
    /// Must never be a candidate (out of scope).
    pub forbidden_candidates: Vec<usize>,
    /// Must never be selected (no longer valid).
    pub forbidden_selected: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub inserts: Vec<Insert>,
    pub queries: Vec<Query>,
    /// When queries run.
    pub query_at: OffsetDateTime,
}

pub fn start() -> OffsetDateTime {
    time::macros::datetime!(2025-11-14 0:00 UTC)
}

fn direct(rel: Relation, svc: &str) -> String {
    match rel {
        Relation::Store => format!("where does {svc} store data"),
        Relation::Owner => format!("{svc} is owned by which team"),
        Relation::Region => format!("which region does {svc} run in"),
        Relation::Port => format!("which port does {svc} expose"),
        Relation::Depends => format!("what does {svc} depend on"),
    }
}

fn semantic(rel: Relation, svc: &str) -> String {
    match rel {
        Relation::Store => format!("which database backs {svc}"),
        Relation::Owner => format!("who maintains {svc}"),
        Relation::Region => format!("where is {svc} hosted"),
        Relation::Port => format!("how do clients reach {svc} over the network"),
        Relation::Depends => format!("which upstream systems does {svc} call"),
    }
}

struct Fact {
    service: usize,
    relation: Relation,
    predicate: &'static str,
    object: String,
    owner: Owner,
    /// Index of the fact this one replaces (temporal changes).
    replaces: Option<usize>,
}

pub fn generate(seed: u64) -> Corpus {
    let mut rng = Rng::new(seed);

    // Services: unique two-word names; some get a longer "replica" twin
    // whose name contains theirs (lexical neighbours).
    let mut names: Vec<String> = Vec::new();
    while names.len() < SERVICES - REPLICAS {
        let n = format!("{} {}", rng.pick(A), rng.pick(B));
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let base = names.len();
    let mut twins = Vec::new();
    for i in 0..REPLICAS {
        names.push(format!("{} replica", names[i]));
        twins.push((i, base + i));
    }

    let mut facts: Vec<Fact> = Vec::new();
    let mut store_of = vec![0usize; names.len()];
    for (s, store) in store_of.iter_mut().enumerate() {
        *store = facts.len();
        facts.push(Fact {
            service: s,
            relation: Relation::Store,
            predicate: "stores data in",
            object: (*rng.pick(STORES)).to_owned(),
            owner: Owner::Alice,
            replaces: None,
        });
        facts.push(Fact {
            service: s,
            relation: Relation::Owner,
            predicate: "is owned by the team",
            object: (*rng.pick(TEAMS)).to_owned(),
            owner: Owner::Alice,
            replaces: None,
        });
        facts.push(Fact {
            service: s,
            relation: Relation::Region,
            predicate: "runs in region",
            object: (*rng.pick(REGIONS)).to_owned(),
            owner: Owner::Alice,
            replaces: None,
        });
        facts.push(Fact {
            service: s,
            relation: Relation::Port,
            predicate: "exposes port",
            object: format!("{}", 1024 + rng.below(60_000)),
            owner: Owner::Alice,
            replaces: None,
        });
        for _ in 0..1 + rng.below(2) {
            let mut other = rng.below(names.len());
            if other == s {
                other = (other + 1) % names.len();
            }
            facts.push(Fact {
                service: s,
                relation: Relation::Depends,
                predicate: "depends on",
                object: names[other].clone(),
                owner: Owner::Alice,
                replaces: None,
            });
        }
    }
    // Temporal changes: a later fact replaces a service's datastore.
    let mut changing: Vec<usize> = (0..names.len()).collect();
    rng.shuffle(&mut changing);
    changing.truncate(CHANGING);
    for &s in &changing {
        let old = store_of[s];
        let mut object = (*rng.pick(STORES)).to_owned();
        if object == facts[old].object {
            object = STORES
                [(STORES.iter().position(|x| *x == object).unwrap_or(0) + 1) % STORES.len()]
            .to_owned();
        }
        facts.push(Fact {
            service: s,
            relation: Relation::Store,
            predicate: "stores data in",
            object,
            owner: Owner::Alice,
            replaces: Some(old),
        });
    }
    // Out-of-scope copies in Bob's private scope.
    let mut copies: Vec<usize> = (0..facts.len())
        .filter(|&i| facts[i].replaces.is_none() && facts[i].relation != Relation::Depends)
        .collect();
    rng.shuffle(&mut copies);
    copies.truncate(BOB_COPIES);
    let mut copy_of = Vec::new();
    for &i in &copies {
        copy_of.push((i, facts.len()));
        facts.push(Fact {
            service: facts[i].service,
            relation: facts[i].relation,
            predicate: facts[i].predicate,
            object: facts[i].object.clone(),
            owner: Owner::Bob,
            replaces: None,
        });
    }

    // Timeline: replaced facts in the first half, their replacements in
    // the second; everything else (and the noise) anywhere.
    let notes = CORPUS_SIZE.saturating_sub(facts.len());
    let total = facts.len() + notes;
    let mut slots: Vec<usize> = (0..total).collect();
    rng.shuffle(&mut slots);
    let mut slot_of_fact = vec![0usize; facts.len()];
    for (i, s) in slot_of_fact.iter_mut().enumerate() {
        *s = slots[i];
    }
    for f in 0..facts.len() {
        if let Some(old) = facts[f].replaces
            && slot_of_fact[f] < slot_of_fact[old]
        {
            slot_of_fact.swap(f, old);
        }
    }
    let at =
        |slot: usize| start() + Duration::minutes((slot as i64) * DAYS * 24 * 60 / total as i64);

    let mut order: Vec<(usize, Insert, Option<usize>)> = Vec::with_capacity(total);
    for (f, fact) in facts.iter().enumerate() {
        let valid_until = facts
            .iter()
            .position(|x| x.replaces == Some(f))
            .map(|n| at(slot_of_fact[n]));
        order.push((
            slot_of_fact[f],
            Insert {
                at: at(slot_of_fact[f]),
                owner: fact.owner,
                kind: InsertKind::Fact {
                    service: names[fact.service].clone(),
                    relation: fact.relation,
                    predicate: fact.predicate,
                    object: fact.object.clone(),
                },
                valid_until,
            },
            Some(f),
        ));
    }
    for (n, slot) in slots.iter().skip(facts.len()).enumerate() {
        let word = rng.pick(A);
        let note = format!(
            "standup note {n}: discussed the {} for the {word} {}",
            rng.pick(TOPICS),
            rng.pick(B)
        );
        order.push((
            *slot,
            Insert {
                at: at(*slot),
                owner: Owner::Alice,
                kind: InsertKind::Note(note),
                valid_until: None,
            },
            None,
        ));
    }
    order.sort_by_key(|(slot, _, _)| *slot);
    let mut index_of_fact = vec![0usize; facts.len()];
    let inserts: Vec<Insert> = order
        .into_iter()
        .enumerate()
        .map(|(i, (_, ins, f))| {
            if let Some(f) = f {
                index_of_fact[f] = i;
            }
            ins
        })
        .collect();
    let query_at = start() + Duration::days(DAYS + 1);

    // Age: fewer than RECENT_WINDOW newer, still-valid Alice memories.
    let alice_valid: Vec<bool> = inserts
        .iter()
        .map(|i| i.owner == Owner::Alice && i.valid_until.is_none())
        .collect();
    let mut newer = vec![0usize; inserts.len()];
    let mut seen = 0;
    for i in (0..inserts.len()).rev() {
        newer[i] = seen;
        if alice_valid[i] {
            seen += 1;
        }
    }
    let age = |i: usize| {
        if newer[i] < RECENT_WINDOW {
            Age::Recent
        } else {
            Age::Old
        }
    };

    // ---- queries ------------------------------------------------------------
    let current: Vec<usize> = (0..facts.len())
        .filter(|&f| facts[f].owner == Owner::Alice && !facts.iter().any(|x| x.replaces == Some(f)))
        .collect();
    let gold_for = |f: usize| -> Vec<(usize, u8)> {
        let rel = facts[f].relation;
        let svc = facts[f].service;
        if rel == Relation::Depends {
            // Every current dependency of the service is relevant.
            current
                .iter()
                .filter(|&&g| facts[g].service == svc && facts[g].relation == Relation::Depends)
                .map(|&g| (index_of_fact[g], 2))
                .collect()
        } else {
            vec![(index_of_fact[f], 2)]
        }
    };
    let mut queries = Vec::new();
    let mut pick_stratified =
        |rng: &mut Rng,
         pool: &[usize],
         family: Family,
         text: &dyn Fn(usize) -> String,
         extra: &dyn Fn(usize) -> (Vec<usize>, Vec<usize>)| {
            // Equal numbers from each age stratum (amendment 1): never
            // let a small recent pool tilt a family toward old targets.
            let strata: Vec<(Age, Vec<usize>)> = [Age::Recent, Age::Old]
                .into_iter()
                .map(|want| {
                    let mut cands: Vec<usize> = pool
                        .iter()
                        .copied()
                        .filter(|&f| age(index_of_fact[f]) == want)
                        .collect();
                    rng.shuffle(&mut cands);
                    (want, cands)
                })
                .collect();
            let n = strata
                .iter()
                .map(|(_, c)| c.len())
                .min()
                .unwrap_or(0)
                .min(QUERIES_PER_FAMILY / 2);
            for (want, cands) in strata {
                for &f in cands.iter().take(n) {
                    let (forbidden_candidates, forbidden_selected) = extra(f);
                    queries.push(Query {
                        family,
                        age: want,
                        text: text(f),
                        gold: gold_for(f),
                        forbidden_candidates,
                        forbidden_selected,
                    });
                }
            }
        };
    let none = |_: usize| (Vec::new(), Vec::new());
    let plain: Vec<usize> = current
        .iter()
        .copied()
        .filter(|&f| facts[f].replaces.is_none())
        .collect();
    pick_stratified(
        &mut rng,
        &plain,
        Family::Direct,
        &|f| direct(facts[f].relation, &names[facts[f].service]),
        &none,
    );
    let twin_bases: Vec<usize> = plain
        .iter()
        .copied()
        .filter(|&f| {
            twins.iter().any(|(b, _)| *b == facts[f].service)
                && facts[f].relation != Relation::Depends
        })
        .collect();
    pick_stratified(
        &mut rng,
        &twin_bases,
        Family::LexicalTrap,
        &|f| direct(facts[f].relation, &names[facts[f].service]),
        &none,
    );
    let non_dep: Vec<usize> = plain
        .iter()
        .copied()
        .filter(|&f| facts[f].relation != Relation::Depends)
        .collect();
    pick_stratified(
        &mut rng,
        &non_dep,
        Family::Semantic,
        &|f| semantic(facts[f].relation, &names[facts[f].service]),
        &none,
    );
    let replacements: Vec<usize> = (0..facts.len())
        .filter(|&f| facts[f].replaces.is_some())
        .collect();
    pick_stratified(
        &mut rng,
        &replacements,
        Family::Temporal,
        &|f| direct(Relation::Store, &names[facts[f].service]),
        &|f| {
            (
                Vec::new(),
                facts[f]
                    .replaces
                    .map(|o| vec![index_of_fact[o]])
                    .unwrap_or_default(),
            )
        },
    );
    let copied: Vec<usize> = copy_of
        .iter()
        .map(|(a, _)| *a)
        .filter(|f| current.contains(f))
        .collect();
    pick_stratified(
        &mut rng,
        &copied,
        Family::Scope,
        &|f| direct(facts[f].relation, &names[facts[f].service]),
        &|f| {
            let bob = copy_of
                .iter()
                .find(|(a, _)| *a == f)
                .map(|(_, b)| index_of_fact[*b]);
            (bob.into_iter().collect(), Vec::new())
        },
    );

    Corpus {
        inserts,
        queries,
        query_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic_and_balanced() {
        let a = generate(101);
        let b = generate(101);
        assert_eq!(a.inserts.len(), CORPUS_SIZE);
        assert_eq!(
            a.inserts.iter().map(Insert::text).collect::<Vec<_>>(),
            b.inserts.iter().map(Insert::text).collect::<Vec<_>>()
        );
        assert_eq!(a.queries.len(), b.queries.len());
        for fam in Family::ALL {
            for age in [Age::Recent, Age::Old] {
                let n = a
                    .queries
                    .iter()
                    .filter(|q| q.family == fam && q.age == age)
                    .count();
                assert!(n > 0, "{fam:?} {age:?} has no queries");
            }
        }
        for fam in Family::ALL {
            let count = |age| {
                a.queries
                    .iter()
                    .filter(|q| q.family == fam && q.age == age)
                    .count()
            };
            assert_eq!(count(Age::Recent), count(Age::Old), "{fam:?} is balanced");
        }
        assert!(a.inserts.windows(2).all(|w| w[0].at <= w[1].at));
        assert_ne!(
            generate(102).inserts[0].text(),
            a.inserts[0].text(),
            "seeds differ"
        );
    }

    #[test]
    fn replaced_facts_end_before_their_replacements() {
        let c = generate(7);
        for q in c.queries.iter().filter(|q| q.family == Family::Temporal) {
            let (new, _) = q.gold[0];
            let old = q.forbidden_selected[0];
            assert!(c.inserts[old].at < c.inserts[new].at);
            assert_eq!(c.inserts[old].valid_until, Some(c.inserts[new].at));
        }
    }
}
