//! Retrieval metrics and paired bootstrap confidence intervals.

use serde::Serialize;

use crate::rng::Rng;

/// nDCG@k with gains `2^g − 1`. `ranked` holds the grade of each
/// selected result in rank order; `gold` holds every gold grade.
pub fn ndcg_at(ranked: &[u8], gold: &[u8], k: usize) -> f64 {
    let gain = |g: u8| f64::from((1u32 << g) - 1);
    let dcg = |gs: &mut dyn Iterator<Item = u8>| -> f64 {
        gs.take(k)
            .enumerate()
            .map(|(i, g)| gain(g) / ((i as f64) + 2.0).log2())
            .sum()
    };
    let mut ideal: Vec<u8> = gold.to_vec();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    let idcg = dcg(&mut ideal.into_iter());
    if idcg == 0.0 {
        return 0.0;
    }
    dcg(&mut ranked.iter().copied()) / idcg
}

/// Reciprocal rank of the first relevant (grade ≥ 1) result within `k`.
pub fn mrr_at(ranked: &[u8], k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .position(|g| *g >= 1)
        .map_or(0.0, |i| 1.0 / (i as f64 + 1.0))
}

/// Share of relevant gold items found among `found` (grade ≥ 1 count).
pub fn recall(found_relevant: usize, gold_relevant: usize) -> f64 {
    if gold_relevant == 0 {
        0.0
    } else {
        found_relevant as f64 / gold_relevant as f64
    }
}

pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Nearest-rank percentile (`p` in 0..=100).
pub fn percentile(xs: &[f64], p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    v[rank.min(v.len()) - 1]
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Interval {
    pub mean: f64,
    pub lower: f64,
    pub upper: f64,
}

/// Paired bootstrap of the mean difference `b − a` with a two-sided
/// `1 − alpha` percentile CI, resampling pairs with replacement.
pub fn paired_bootstrap_at(
    a: &[f64],
    b: &[f64],
    resamples: usize,
    seed: u64,
    alpha: f64,
) -> Interval {
    assert_eq!(a.len(), b.len(), "paired samples");
    let d: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    let mut rng = Rng::new(seed);
    let mut means: Vec<f64> = (0..resamples)
        .map(|_| {
            let s: f64 = (0..d.len()).map(|_| d[rng.below(d.len())]).sum();
            s / d.len() as f64
        })
        .collect();
    means.sort_by(f64::total_cmp);
    let at = |q: f64| means[((q * resamples as f64) as usize).min(resamples - 1)];
    Interval {
        mean: mean(&d),
        lower: at(alpha / 2.0),
        upper: at(1.0 - alpha / 2.0),
    }
}

/// Paired bootstrap of the mean difference `b − a` (95% percentile CI),
/// resampling queries with replacement.
pub fn paired_bootstrap(a: &[f64], b: &[f64], resamples: usize, seed: u64) -> Interval {
    assert_eq!(a.len(), b.len(), "paired samples");
    let d: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    let mut rng = Rng::new(seed);
    let mut means: Vec<f64> = (0..resamples)
        .map(|_| {
            let s: f64 = (0..d.len()).map(|_| d[rng.below(d.len())]).sum();
            s / d.len() as f64
        })
        .collect();
    means.sort_by(f64::total_cmp);
    let at = |q: f64| means[((q * resamples as f64) as usize).min(resamples - 1)];
    Interval {
        mean: mean(&d),
        lower: at(0.025),
        upper: at(0.975),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndcg_and_mrr_reference_values() {
        assert!((ndcg_at(&[2], &[2], 10) - 1.0).abs() < 1e-12);
        // Relevant at rank 2: 1/log2(3).
        assert!((ndcg_at(&[0, 2], &[2], 10) - 1.0 / 3f64.log2()).abs() < 1e-12);
        assert_eq!(ndcg_at(&[0, 0], &[2], 10), 0.0);
        assert_eq!(mrr_at(&[0, 0, 2], 10), 1.0 / 3.0);
        assert_eq!(mrr_at(&[0; 10], 10), 0.0);
    }

    #[test]
    fn bootstrap_brackets_a_clear_difference() {
        let a = vec![0.0; 200];
        let b: Vec<f64> = (0..200)
            .map(|i| if i % 2 == 0 { 1.0 } else { 0.0 })
            .collect();
        let ci = paired_bootstrap(&a, &b, 2_000, 7);
        assert!((ci.mean - 0.5).abs() < 1e-12);
        assert!(ci.lower > 0.4 && ci.upper < 0.6);
    }
}
