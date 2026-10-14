//! How a large embed is split: texts sorted longest first into batches
//! whose padded size (rows × longest) stays under a token budget, the
//! batches spread over several model sessions, and every vector returned in
//! the caller's order.

use std::collections::HashSet;
use std::time::Duration;
use vex_embed::batching::{default_workers, fan_out, plan, threads_per_worker};

fn flatten(batches: &[Vec<usize>]) -> Vec<usize> {
    batches.iter().flatten().copied().collect()
}

#[test]
fn batches_run_longest_first_and_keep_ties_in_order() {
    let lens = [3, 10, 1, 10, 5];
    let batches = plan(&lens, 20, 64);
    assert_eq!(batches, vec![vec![1, 3], vec![4, 0, 2]]);
}

#[test]
fn a_batch_never_pads_past_the_budget_or_the_row_cap() {
    let lens: Vec<usize> = (0..300).map(|i| 1 + (i * 37) % 512).collect();
    let batches = plan(&lens, 8192, 64);
    for b in &batches {
        let longest = b.iter().map(|&i| lens[i]).max().unwrap();
        assert!(b.len() <= 64, "{} rows", b.len());
        assert!(
            b.len() == 1 || b.len() * longest <= 8192,
            "{} rows × {longest} tokens",
            b.len()
        );
    }
    let mut all = flatten(&batches);
    all.sort_unstable();
    assert_eq!(all, (0..300).collect::<Vec<_>>(), "each text exactly once");
    let order: Vec<usize> = flatten(&batches).iter().map(|&i| lens[i]).collect();
    assert!(order.windows(2).all(|w| w[0] >= w[1]), "not longest first");
}

#[test]
fn a_text_longer_than_the_budget_runs_alone() {
    assert_eq!(plan(&[600, 2, 2], 512, 64), vec![vec![0], vec![1, 2]]);
    assert!(plan(&[], 512, 64).is_empty());
}

#[test]
fn fan_out_uses_every_worker_and_returns_vectors_in_input_order() {
    let mut workers: Vec<usize> = (0..4).collect();
    let batches: Vec<Vec<usize>> = (0..40).rev().map(|i| vec![i]).collect();
    let out = fan_out(&mut workers, &batches, 40, |w: &mut usize, b: &[usize]| {
        std::thread::sleep(Duration::from_millis(5));
        Ok::<_, String>(b.iter().map(|&i| vec![i as f32, *w as f32]).collect())
    })
    .unwrap();
    assert_eq!(out.len(), 40);
    for (i, v) in out.iter().enumerate() {
        assert_eq!(v[0], i as f32, "vector {i} out of place");
    }
    let used: HashSet<u32> = out.iter().map(|v| v[1] as u32).collect();
    assert!(used.len() > 1, "one worker did everything: {used:?}");
}

#[test]
fn fan_out_reports_a_failing_batch() {
    let mut workers = vec![(), ()];
    let batches: Vec<Vec<usize>> = (0..10).map(|i| vec![i]).collect();
    let got = fan_out(&mut workers, &batches, 10, |_: &mut (), b: &[usize]| {
        if b.contains(&7) {
            Err("model failed".to_string())
        } else {
            Ok(vec![vec![0.0]; b.len()])
        }
    });
    assert_eq!(got.unwrap_err(), "model failed");
}

#[test]
fn workers_and_threads_follow_the_core_count() {
    for (cores, workers, threads) in [
        (10, 4, 2),
        (8, 4, 2),
        (16, 4, 4),
        (4, 2, 2),
        (2, 1, 2),
        (1, 1, 1),
    ] {
        assert_eq!(default_workers(cores), workers, "workers for {cores} cores");
        assert_eq!(
            threads_per_worker(cores, workers),
            threads,
            "threads for {cores} cores"
        );
    }
}
