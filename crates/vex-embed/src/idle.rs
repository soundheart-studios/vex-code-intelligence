//! A lazily loaded value dropped after an idle period: the model is built
//! on first use, a timer thread drops it once no use has touched it for
//! `idle`, and the next use builds it again.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

struct Inner<T> {
    value: Option<T>,
    last_used: Instant,
    timer_running: bool,
    closed: bool,
}

type Shared<T> = Arc<(Mutex<Inner<T>>, Condvar)>;

pub struct IdleSlot<T: Send + 'static> {
    shared: Shared<T>,
    idle: Duration,
}

fn lock<T>(m: &Mutex<Inner<T>>) -> MutexGuard<'_, Inner<T>> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl<T: Send + 'static> IdleSlot<T> {
    pub fn new(idle: Duration) -> Self {
        IdleSlot {
            shared: Arc::new((
                Mutex::new(Inner {
                    value: None,
                    last_used: Instant::now(),
                    timer_running: false,
                    closed: false,
                }),
                Condvar::new(),
            )),
            idle,
        }
    }

    /// Run `f` on the value, building it with `load` first if it is not
    /// resident. A failed load leaves the slot empty for the next attempt.
    /// The idle clock restarts when `f` returns, so a long call never
    /// counts as idleness.
    pub fn with<R, E>(
        &self,
        load: impl FnOnce() -> Result<T, E>,
        f: impl FnOnce(&mut T) -> R,
    ) -> Result<R, E> {
        let (mutex, cvar) = &*self.shared;
        let mut inner = lock(mutex);
        if inner.value.is_none() {
            inner.value = Some(load()?);
        }
        let out = f(inner.value.as_mut().expect("loaded above"));
        inner.last_used = Instant::now();
        if !inner.timer_running {
            inner.timer_running = true;
            let shared = Arc::clone(&self.shared);
            let idle = self.idle;
            std::thread::Builder::new()
                .name("vex-embed-idle".into())
                .spawn(move || timer(shared, idle))
                .expect("spawn idle timer");
        }
        cvar.notify_all();
        Ok(out)
    }

    pub fn is_loaded(&self) -> bool {
        lock(&self.shared.0).value.is_some()
    }

    /// Time left before an idle unload, None when nothing is loaded.
    pub fn idle_remaining(&self) -> Option<Duration> {
        let inner = lock(&self.shared.0);
        inner
            .value
            .as_ref()
            .map(|_| self.idle.saturating_sub(inner.last_used.elapsed()))
    }

    /// Drop the value now.
    pub fn unload(&self) {
        let taken = lock(&self.shared.0).value.take();
        self.shared.1.notify_all();
        drop(taken);
    }
}

impl<T: Send + 'static> Drop for IdleSlot<T> {
    fn drop(&mut self) {
        let taken = {
            let mut inner = lock(&self.shared.0);
            inner.closed = true;
            inner.value.take()
        };
        self.shared.1.notify_all();
        drop(taken);
    }
}

/// Sleep until the value has been idle for `idle`, then drop it. Exits
/// when the value is gone (unloaded or the slot dropped); the next load
/// starts a new timer.
fn timer<T>(shared: Shared<T>, idle: Duration) {
    let (mutex, cvar) = &*shared;
    let mut inner = lock(mutex);
    loop {
        if inner.closed || inner.value.is_none() {
            inner.timer_running = false;
            return;
        }
        let since = inner.last_used.elapsed();
        if since >= idle {
            let taken = inner.value.take();
            inner.timer_running = false;
            drop(inner);
            drop(taken);
            return;
        }
        inner = cvar
            .wait_timeout(inner, idle - since)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
}
