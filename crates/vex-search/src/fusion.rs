//! Reciprocal Rank Fusion: the one implementation (Cortex `rank/fusion.py`,
//! ADR-14). Only the position a channel gives a key matters, never its score.

use std::collections::HashMap;
use std::hash::Hash;

/// The standard RRF constant.
pub const RRF_K: f64 = 60.0;

/// One key's merged standing across the channels that returned it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fused<K> {
    pub key: K,
    /// Raw reciprocal-rank sum; comparable within one `fuse` call only.
    pub score: f64,
    /// `score` relative to the best hit, rounded to three places.
    pub relative: f64,
    /// The channels that found this key, in the order they were supplied.
    pub channels: Vec<String>,
}

/// Merge per-channel rankings, best first. Channels are scanned in the order
/// given; a channel absent from `weights` weighs 1.0. Ties resolve by the
/// order a key was first seen, so the result is reproducible.
pub fn fuse<K: Hash + Eq + Clone>(
    ranked_by_channel: &[(&str, &[K])],
    weights: &[(&str, f64)],
) -> Vec<Fused<K>> {
    let mut keys: Vec<K> = Vec::new();
    let mut index: HashMap<K, usize> = HashMap::new();
    let mut scores: Vec<f64> = Vec::new();
    let mut channels: Vec<Vec<String>> = Vec::new();
    for (name, ranked) in ranked_by_channel {
        let weight = weights
            .iter()
            .find(|(w, _)| w == name)
            .map_or(1.0, |&(_, w)| w);
        for (rank, key) in ranked.iter().enumerate() {
            let i = *index.entry(key.clone()).or_insert_with(|| {
                keys.push(key.clone());
                scores.push(0.0);
                channels.push(Vec::new());
                keys.len() - 1
            });
            scores[i] += weight / (RRF_K + rank as f64);
            channels[i].push((*name).to_string());
        }
    }
    if keys.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..keys.len()).collect();
    // Stable sort on descending score keeps first-seen order among ties.
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    let best = scores[order[0]];
    order
        .into_iter()
        .map(|i| Fused {
            key: keys[i].clone(),
            score: scores[i],
            relative: crate::round3(scores[i] / best),
            channels: std::mem::take(&mut channels[i]),
        })
        .collect()
}
