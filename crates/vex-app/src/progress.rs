//! Progress of long operations (model download, scanning, parsing,
//! embedding), for a front end that wants to show it. The caller installs
//! a reporter for the current thread; with none installed every call is a
//! no-op, so the daemon and tests pay nothing.

use std::cell::RefCell;
use std::rc::Rc;

/// A long-running phase of `init`, `index` or `embed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Fetching model weights; units are bytes.
    Download,
    /// Reading and hashing the working tree; units are files.
    Scan,
    /// Parsing changed files into chunks and symbols; units are files.
    Parse,
    /// Timing the model on a sample before a long embed; units are chunks.
    Measure,
    /// Computing vectors; units are chunks.
    Embed,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Download => "downloading model",
            Stage::Scan => "scanning files",
            Stage::Parse => "parsing",
            Stage::Measure => "measuring speed",
            Stage::Embed => "embedding",
        }
    }
}

pub trait Reporter {
    /// A stage starts with `total` units of work.
    fn begin(&self, stage: Stage, total: u64);
    /// `n` more units of the current stage are done.
    fn advance(&self, n: u64);
    /// The current stage is over (done, or given up).
    fn end(&self);
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<dyn Reporter>>> = const { RefCell::new(None) };
}

/// Run `f` with `reporter` receiving this thread's progress.
pub fn with<T>(reporter: Rc<dyn Reporter>, f: impl FnOnce() -> T) -> T {
    let previous = CURRENT.with(|c| c.replace(Some(reporter)));
    struct Restore(Option<Rc<dyn Reporter>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CURRENT.with(|c| *c.borrow_mut() = previous);
        }
    }
    let _restore = Restore(previous);
    f()
}

fn current() -> Option<Rc<dyn Reporter>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Report a stage; it ends when the returned guard drops.
pub(crate) fn stage(stage: Stage, total: u64) -> StageGuard {
    let reporter = current();
    if let Some(r) = &reporter {
        r.begin(stage, total);
    }
    StageGuard(reporter)
}

pub(crate) struct StageGuard(Option<Rc<dyn Reporter>>);

impl StageGuard {
    pub(crate) fn advance(&self, n: u64) {
        if let Some(r) = &self.0 {
            r.advance(n);
        }
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if let Some(r) = &self.0 {
            r.end();
        }
    }
}
