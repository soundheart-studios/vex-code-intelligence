//! Splitting a large embed across model sessions. Every row of a batch pads
//! to the batch's longest text, so texts run longest first and a batch
//! closes once its padded size (rows × longest) would pass a token budget;
//! the batches then spread over the sessions. The vectors do not depend on
//! which batch a text lands in: padding is masked.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Sessions to run for a machine with `cores` cores: one per two cores,
/// at most four (more sessions stop paying once the cores are busy, and
/// each holds its own copy of the model).
pub fn default_workers(cores: usize) -> usize {
    (cores / 2).clamp(1, 4)
}

/// Intra-op threads per session when `workers` sessions share `cores`.
pub fn threads_per_worker(cores: usize, workers: usize) -> usize {
    (cores / workers.max(1)).max(1)
}

/// Batches of indices into `lens` (each text's token count): longest
/// first, ties in input order, each batch's rows × longest at most
/// `budget` (a longer text runs alone) and at most `max_rows` rows.
pub fn plan(lens: &[usize], budget: usize, max_rows: usize) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by(|&a, &b| lens[b].cmp(&lens[a]));
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    for i in order {
        // Longest first: the batch's first text is its longest.
        let longest = current.first().map_or(lens[i], |&f| lens[f]);
        if !current.is_empty()
            && (current.len() >= max_rows || (current.len() + 1) * longest > budget)
        {
            batches.push(std::mem::take(&mut current));
        }
        current.push(i);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// Run `batches` over `workers`, each worker taking the next batch as it
/// frees up, and return the `n` vectors in input order. The first failure
/// stops every worker and is returned.
pub fn fan_out<W, E>(
    workers: &mut [W],
    batches: &[Vec<usize>],
    n: usize,
    run: impl Fn(&mut W, &[usize]) -> Result<Vec<Vec<f32>>, E> + Sync,
) -> Result<Vec<Vec<f32>>, E>
where
    W: Send,
    E: Send,
{
    let next = AtomicUsize::new(0);
    let failed: Mutex<Option<E>> = Mutex::new(None);
    let out: Mutex<Vec<Vec<f32>>> = Mutex::new(vec![Vec::new(); n]);
    let work = |w: &mut W| {
        loop {
            let b = next.fetch_add(1, Ordering::Relaxed);
            if b >= batches.len() || failed.lock().unwrap_or_else(|p| p.into_inner()).is_some() {
                return;
            }
            match run(w, &batches[b]) {
                Ok(vecs) => {
                    let mut out = out.lock().unwrap_or_else(|p| p.into_inner());
                    for (&i, v) in batches[b].iter().zip(vecs) {
                        out[i] = v;
                    }
                }
                Err(e) => {
                    failed
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get_or_insert(e);
                    return;
                }
            }
        }
    };
    match workers {
        [] => {}
        // One session: no thread to spawn.
        [only] => work(only),
        many => std::thread::scope(|scope| {
            let work = &work;
            for w in many.iter_mut() {
                scope.spawn(move || work(w));
            }
        }),
    }
    if let Some(e) = failed.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(e);
    }
    Ok(out.into_inner().unwrap_or_else(|p| p.into_inner()))
}
