//! A one-line progress display on stderr for `init`, `index` and `embed`,
//! shown only to a person at a terminal: never with `--json`, never when
//! stderr is a pipe or file, never inside the daemon.

use std::cell::RefCell;
use std::io::{IsTerminal, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};
use vex_app::progress::{Reporter, Stage};

/// Redraw at most this often.
const FRAME: Duration = Duration::from_millis(80);
const WIDTH: usize = 24;

/// Whether progress would be shown for this invocation.
pub fn wanted(json: bool) -> bool {
    !json && std::io::stderr().is_terminal()
}

/// Run `f`, showing its progress when `wanted(json)`.
pub fn shown<T>(json: bool, f: impl FnOnce() -> T) -> T {
    if wanted(json) {
        vex_app::progress::with(Rc::new(Bar::default()), f)
    } else {
        f()
    }
}

struct Current {
    stage: Stage,
    total: u64,
    done: u64,
    started: Instant,
    drawn: Option<Instant>,
}

#[derive(Default)]
struct Bar {
    current: RefCell<Option<Current>>,
}

fn amount(stage: Stage, n: u64) -> String {
    match stage {
        Stage::Download => format!("{:.1}", n as f64 / 1e6),
        _ => n.to_string(),
    }
}

fn unit(stage: Stage) -> &'static str {
    match stage {
        Stage::Download => "MB",
        Stage::Scan | Stage::Parse => "files",
        Stage::Measure | Stage::Embed => "chunks",
    }
}

/// A duration as a person would say it.
pub(crate) fn rough(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        "under a minute".to_string()
    } else if secs < 120 * 60 {
        format!("about {} min", secs.div_ceil(60))
    } else {
        format!("about {:.1} h", secs as f64 / 3600.0)
    }
}

/// How long a stage must run before its rate predicts the rest.
const ETA_AFTER: Duration = Duration::from_secs(5);

/// `line`, plus the time left at the rate so far once that means anything.
pub(crate) fn line_eta(stage: Stage, done: u64, total: u64, elapsed: Duration) -> String {
    let base = line(stage, done, total);
    if elapsed < ETA_AFTER || done == 0 || done >= total {
        return base;
    }
    let left = elapsed.mul_f64((total - done) as f64 / done as f64);
    format!("{base}, {} left", rough(left))
}

/// The in-progress line: label, bar, done/total.
pub(crate) fn line(stage: Stage, done: u64, total: u64) -> String {
    let filled = if total == 0 {
        WIDTH
    } else {
        ((done.min(total) as u128 * WIDTH as u128) / total as u128) as usize
    };
    format!(
        "  {:<18} [{}{}] {}/{} {}",
        stage.label(),
        "#".repeat(filled),
        ".".repeat(WIDTH - filled),
        amount(stage, done),
        amount(stage, total),
        unit(stage)
    )
}

/// The line a finished stage leaves behind.
pub(crate) fn summary(stage: Stage, done: u64, total: u64, took: Duration) -> String {
    let stopped = if done < total { " (stopped early)" } else { "" };
    format!(
        "  {:<18} {} {} in {:.1}s{stopped}",
        stage.label(),
        amount(stage, done),
        unit(stage),
        took.as_secs_f64()
    )
}

fn draw(text: &str, newline: bool) {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r\x1b[2K{text}{}", if newline { "\n" } else { "" });
    let _ = err.flush();
}

impl Reporter for Bar {
    fn begin(&self, stage: Stage, total: u64) {
        *self.current.borrow_mut() = Some(Current {
            stage,
            total,
            done: 0,
            started: Instant::now(),
            drawn: None,
        });
    }

    fn advance(&self, n: u64) {
        let mut current = self.current.borrow_mut();
        let Some(c) = current.as_mut() else { return };
        c.done += n;
        // Stages that finish within a frame never draw a bar at all.
        if c.started.elapsed() < FRAME || c.drawn.is_some_and(|t| t.elapsed() < FRAME) {
            return;
        }
        c.drawn = Some(Instant::now());
        draw(
            &line_eta(c.stage, c.done, c.total, c.started.elapsed()),
            false,
        );
    }

    fn end(&self) {
        let Some(c) = self.current.borrow_mut().take() else {
            return;
        };
        if c.total == 0 {
            return;
        }
        draw(
            &summary(c.stage, c.done, c.total, c.started.elapsed()),
            true,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_fills_with_the_work_done() {
        assert_eq!(
            line(Stage::Parse, 0, 4),
            "  parsing            [........................] 0/4 files"
        );
        assert_eq!(
            line(Stage::Parse, 2, 4),
            "  parsing            [############............] 2/4 files"
        );
        assert_eq!(
            line(Stage::Embed, 9, 4),
            "  embedding          [########################] 9/4 chunks"
        );
    }

    #[test]
    fn downloads_count_megabytes() {
        assert_eq!(
            line(Stage::Download, 33_232_562, 66_465_124),
            "  downloading model  [############............] 33.2/66.5 MB"
        );
    }

    #[test]
    fn a_long_stage_shows_the_time_left() {
        // Halfway after 10 minutes: about 10 more.
        assert_eq!(
            line_eta(Stage::Embed, 50, 100, Duration::from_secs(600)),
            "  embedding          [############............] 50/100 chunks, about 10 min left"
        );
        // Too early to tell, or already done: no estimate.
        assert_eq!(
            line_eta(Stage::Embed, 1, 100, Duration::from_secs(2)),
            line(Stage::Embed, 1, 100)
        );
        assert_eq!(
            line_eta(Stage::Embed, 100, 100, Duration::from_secs(600)),
            line(Stage::Embed, 100, 100)
        );
    }

    #[test]
    fn durations_read_roughly() {
        assert_eq!(rough(Duration::from_secs(20)), "under a minute");
        assert_eq!(rough(Duration::from_secs(61)), "about 2 min");
        assert_eq!(rough(Duration::from_secs(17 * 60)), "about 17 min");
        assert_eq!(rough(Duration::from_secs(150 * 60)), "about 2.5 h");
    }

    #[test]
    fn measuring_counts_chunks() {
        assert_eq!(
            summary(Stage::Measure, 256, 256, Duration::from_millis(6200)),
            "  measuring speed    256 chunks in 6.2s"
        );
    }

    #[test]
    fn a_stage_that_stopped_short_says_so() {
        let s = summary(Stage::Embed, 64, 600, Duration::from_millis(1500));
        assert_eq!(s, "  embedding          64 chunks in 1.5s (stopped early)");
        let s = summary(Stage::Scan, 13, 13, Duration::from_millis(20));
        assert_eq!(s, "  scanning files     13 files in 0.0s");
    }
}
