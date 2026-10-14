//! The regression gate: losses fail unless reviewed with the exact value,
//! silence is a loss, and the baseline only advances when everything passes.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

struct Gate {
    _home: TestHome,
    dir: PathBuf,
}

impl Gate {
    fn new(baseline: Value, review: Value) -> Gate {
        let home = TestHome::new();
        let dir = home.vex_home().join("golden");
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir.join("ported.json"),
            &json!({"languages": [], "packages": []}),
        );
        write(&dir.join("baseline.json"), &baseline);
        write(&dir.join("regression-review.json"), &review);
        Gate { _home: home, dir }
    }

    fn run(&self, measured: Value, extra: &[&str]) -> (i32, String) {
        let m = self.dir.join("measured.json");
        write(&m, &measured);
        let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args([
                "gate",
                "--skip-suite",
                "--golden-dir",
                self.dir.to_str().unwrap(),
            ])
            .args(["--measurements", m.to_str().unwrap()])
            .args(extra)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn baseline(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join("baseline.json")).unwrap()).unwrap()
    }
}

fn write(p: &Path, v: &Value) {
    std::fs::write(p, serde_json::to_string_pretty(v).unwrap()).unwrap();
}

fn base() -> Value {
    json!({"parity": {"python": {"mismatches": 0}}, "bench": {"rack": {"correct": 4, "wrong": 8}}})
}

fn no_review() -> Value {
    json!({"exceptions": []})
}

#[test]
fn an_unchanged_measurement_passes() {
    let g = Gate::new(base(), no_review());
    let (code, text) = g.run(base(), &[]);
    assert_eq!(code, 0, "{text}");
}

#[test]
fn a_ported_language_regressing_fails() {
    let g = Gate::new(base(), no_review());
    let mut m = base();
    m["parity"]["python"]["mismatches"] = json!(3);
    let (code, text) = g.run(m, &[]);
    assert_eq!(code, 1);
    assert!(
        text.contains("LOSS      parity.python.mismatches: 0 -> 3"),
        "{text}"
    );
}

#[test]
fn losing_a_correct_edge_or_gaining_a_wrong_one_fails() {
    let g = Gate::new(base(), no_review());
    let (code, text) = g.run(json!({"parity": {"python": {"mismatches": 0}}, "bench": {"rack": {"correct": 3, "wrong": 9}}}), &[]);
    assert_eq!(code, 1);
    assert!(
        text.contains("bench.rack.correct: 4 -> 3") && text.contains("bench.rack.wrong: 8 -> 9"),
        "{text}"
    );
}

#[test]
fn a_review_declaring_the_exact_new_value_passes() {
    let review = json!({"exceptions": [{"key": "parity.python.mismatches", "value": 3, "reason": "golden regenerated upstream"}]});
    let g = Gate::new(base(), review);
    let mut m = base();
    m["parity"]["python"]["mismatches"] = json!(3);
    let (code, text) = g.run(m, &[]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("reviewed"), "{text}");
}

#[test]
fn a_review_for_a_different_value_or_without_a_reason_does_not_excuse() {
    for exc in [
        json!({"key": "parity.python.mismatches", "value": 2, "reason": "stale review"}),
        json!({"key": "parity.python.mismatches", "value": 3, "reason": "  "}),
    ] {
        let g = Gate::new(base(), json!({"exceptions": [exc]}));
        let mut m = base();
        m["parity"]["python"]["mismatches"] = json!(3);
        assert_eq!(g.run(m, &[]).0, 1);
    }
}

#[test]
fn a_baseline_key_no_longer_measured_is_a_loss() {
    let g = Gate::new(base(), no_review());
    let (code, text) = g.run(
        json!({"parity": {"python": {"mismatches": 0}}, "bench": {}}),
        &[],
    );
    assert_eq!(code, 1);
    assert!(text.contains("no longer measured"), "{text}");
}

#[test]
fn gains_are_reported_not_failed() {
    let g = Gate::new(base(), no_review());
    let (code, text) = g.run(json!({"parity": {"python": {"mismatches": 0}}, "bench": {"rack": {"correct": 6, "wrong": 2}}}), &[]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("gain      bench.rack.correct: 4 -> 6"),
        "{text}"
    );
}

#[test]
fn write_snapshot_refuses_while_a_comparison_fails() {
    let g = Gate::new(base(), no_review());
    let mut m = base();
    m["bench"]["rack"]["wrong"] = json!(12);
    let (code, text) = g.run(m, &["--write-snapshot"]);
    assert_eq!(code, 1);
    assert!(text.contains("refusing to write a snapshot"), "{text}");
    assert_eq!(g.baseline(), base(), "the baseline must not move");
}

#[test]
fn write_snapshot_advances_the_baseline_when_everything_passes() {
    let g = Gate::new(base(), no_review());
    let better = json!({"parity": {"python": {"mismatches": 0}}, "bench": {"rack": {"correct": 6, "wrong": 2}}});
    let (code, _) = g.run(better.clone(), &["--write-snapshot"]);
    assert_eq!(code, 0);
    assert_eq!(g.baseline(), better);
}
