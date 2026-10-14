//! Lazy load with an idle-unload timer: nothing loads until first use, an
//! idle model is dropped once the timer elapses, use keeps it resident, and
//! the next use after an unload loads it again. The bge backend is lazy:
//! constructing it touches no weights.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};
use vex_embed::{BgeEmbedder, Embedder, IdleSlot};

struct Model(Arc<AtomicUsize>);
impl Drop for Model {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn wait_until(limit: Duration, cond: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        sleep(Duration::from_millis(10));
    }
    cond()
}

#[test]
fn the_model_unloads_after_the_idle_timer_elapses() {
    let loads = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let slot = IdleSlot::new(Duration::from_millis(150));
    let load = || -> Result<Model, String> {
        loads.fetch_add(1, Ordering::SeqCst);
        Ok(Model(drops.clone()))
    };
    assert!(!slot.is_loaded(), "loaded before first use");
    assert_eq!(slot.idle_remaining(), None);

    assert_eq!(slot.with(load, |_| 7).unwrap(), 7);
    assert!(slot.is_loaded());
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert!(slot.idle_remaining().unwrap() <= Duration::from_millis(150));
    sleep(Duration::from_millis(60));
    let left = slot.idle_remaining().expect("still loaded");
    assert!(
        left <= Duration::from_millis(95),
        "the idle clock is not counting down: {left:?} left after 60ms of 150ms"
    );

    // Use inside the window keeps it resident and does not reload.
    for _ in 0..4 {
        sleep(Duration::from_millis(60));
        slot.with(load, |_| ()).unwrap();
    }
    assert!(slot.is_loaded(), "unloaded while in use");
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    assert!(
        wait_until(Duration::from_secs(3), || !slot.is_loaded()),
        "never unloaded"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1, "the model was not dropped");
    assert_eq!(slot.idle_remaining(), None);

    slot.with(load, |_| ()).unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 2, "no reload after unload");
    slot.unload();
    assert!(!slot.is_loaded());
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_load_leaves_the_slot_empty_and_retries() {
    let slot: IdleSlot<u8> = IdleSlot::new(Duration::from_secs(60));
    let err = slot
        .with(|| Err("offline".to_string()), |_| ())
        .unwrap_err();
    assert_eq!(err, "offline");
    assert!(!slot.is_loaded());
    assert_eq!(slot.with(|| Ok::<u8, String>(3), |v| *v).unwrap(), 3);
}

#[test]
fn dropping_the_slot_stops_its_timer() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let slot = IdleSlot::new(Duration::from_secs(3600));
        slot.with(|| Ok::<_, String>(Model(drops.clone())), |_| ())
            .unwrap();
    }
    assert!(
        wait_until(Duration::from_secs(2), || drops.load(Ordering::SeqCst) == 1),
        "the model outlived its slot"
    );
}

#[test]
fn the_bge_backend_is_lazy() {
    let dir = std::env::temp_dir().join(format!("vex-embed-lazy-{}", std::process::id()));
    let e = BgeEmbedder::new(&dir, Duration::from_secs(1));
    assert_eq!(e.model_name(), "bge-small-en-v1.5");
    assert_eq!(e.dim(), 384);
    assert_eq!(e.context_tokens(), Some(512));
    assert!(!e.is_loaded());
    assert!(!dir.exists(), "construction touched the models dir");
    // An empty batch needs no model: nothing loads or downloads.
    assert!(e.embed(&[]).unwrap().is_empty());
    assert!(!e.is_loaded());
    assert!(!dir.exists(), "an empty embed touched the models dir");
}
