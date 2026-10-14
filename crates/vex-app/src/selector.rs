//! The rules-v1 lead selector (Cortex `application/relevance.py`, heuristic
//! path). Per question: keep the best evidence tier, rank by "names the
//! symbol", word overlap, then path and score preferences, and bundle the
//! top two.
//!
//! Cortex builds the candidate pools as Python sets and sorts them with a
//! stable sort, so leads that tie keep the set's iteration order. That order
//! is CPython's hash-table slot order, not insertion order, and lead scores
//! tie often (several queries' top hits are all 1.0), so `PyIntSet`
//! reproduces it exactly rather than approximating it.
//!
//! The local-model path is not ported (ADR: no Ollama); asking for it
//! reports the same fallback Cortex gives when no local model is available.

use crate::pyjson::{Json, py_round};
use std::collections::BTreeSet;
use vex_search::paths::{is_non_source, is_source};

pub const MAX_SELECTED_LEADS: usize = 12;
pub const MAX_ELIGIBLE_PER_QUESTION: usize = 4;
pub const MAX_BUNDLE_PER_QUESTION: usize = 4;
pub const FALLBACK_BUNDLE_SIZE: usize = 2;
const EXECUTABLE_KINDS: &[&str] = &["function", "method", "class", "entrypoint"];

/// A CPython `set` of small non-negative ints, kept only to iterate it in
/// CPython's order. Mirrors `setobject.c` (3.12): open addressing with nine
/// linear probes then perturbation, resizing to the smallest power of two
/// above 4x the used count whenever fill reaches 3/5 of the mask.
#[derive(Debug, Clone)]
pub struct PyIntSet {
    table: Vec<Option<usize>>,
    used: usize,
}

const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;
const MIN_SIZE: usize = 8;

impl Default for PyIntSet {
    fn default() -> Self {
        PyIntSet {
            table: vec![None; MIN_SIZE],
            used: 0,
        }
    }
}

impl PyIntSet {
    pub fn new() -> PyIntSet {
        PyIntSet::default()
    }

    /// `set(values)` from a list: each added in order.
    pub fn from_list(values: &[usize]) -> PyIntSet {
        let mut s = PyIntSet::new();
        for &v in values {
            s.add(v);
        }
        s
    }

    fn slot(table: &[Option<usize>], v: usize) -> Result<usize, usize> {
        let mask = table.len() - 1;
        let mut perturb = v;
        let mut i = v & mask;
        loop {
            let probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for (j, entry) in table.iter().enumerate().skip(i).take(probes + 1) {
                match entry {
                    None => return Err(j),
                    Some(k) if *k == v => return Ok(j),
                    Some(_) => {}
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb)) & mask;
        }
    }

    pub fn add(&mut self, v: usize) {
        let Err(free) = Self::slot(&self.table, v) else {
            return;
        };
        self.table[free] = Some(v);
        self.used += 1;
        let mask = self.table.len() - 1;
        // No deletions, so fill == used.
        if self.used * 5 >= mask * 3 {
            self.resize(self.used * 4);
        }
    }

    fn resize(&mut self, minused: usize) {
        let mut size = MIN_SIZE;
        while size <= minused {
            size <<= 1;
        }
        let old = std::mem::replace(&mut self.table, vec![None; size]);
        for v in old.into_iter().flatten() {
            let Err(free) = Self::slot(&self.table, v) else {
                unreachable!("reinserting into a fresh table")
            };
            self.table[free] = Some(v);
        }
    }

    pub fn contains(&self, v: usize) -> bool {
        Self::slot(&self.table, v).is_ok()
    }

    /// Iteration order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.table.iter().flatten().copied()
    }

    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }
}

/// One deduplicated lead as the selector sees it.
#[derive(Debug, Clone)]
pub struct Lead {
    pub target: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub matched_queries: Vec<usize>,
    pub score: f64,
    pub source_available: bool,
    /// Private ranking evidence; never in the payload.
    pub preview: String,
}

/// `(source_available, is_source, not non-source, round(score, 1),
/// executable, score)`, compared lexicographically.
fn priority(lead: &Lead) -> (bool, bool, bool, f64, bool, f64) {
    (
        lead.source_available,
        is_source(&lead.file),
        !is_non_source(&lead.file),
        py_round(lead.score, 1),
        EXECUTABLE_KINDS.contains(&lead.kind.to_lowercase().as_str()),
        lead.score,
    )
}

/// Collapse a trailing plural `s` (not `ss`) on words longer than three.
fn fold(token: &str) -> String {
    if token.len() > 3 && token.ends_with('s') && !token.ends_with("ss") {
        token[..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}

/// Words as a question and a symbol name both write them: camelCase split,
/// lowercased, `_` as a space, runs of `[a-z0-9]` longer than two, folded.
fn tokens(value: &str) -> BTreeSet<String> {
    let chars: Vec<char> = value.chars().collect();
    let mut spaced = String::with_capacity(value.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0
            && c.is_ascii_uppercase()
            && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit())
        {
            spaced.push(' ');
        }
        spaced.push(c);
    }
    let lowered = spaced.to_lowercase().replace('_', " ");
    lowered
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|t| t.len() > 2)
        .map(fold)
        .collect()
}

/// The question wrote this lead's symbol name out (paths never count).
fn names_the_symbol(question: &str, lead: &Lead) -> bool {
    let name = &lead.name;
    if name.is_empty() || name.contains('/') || name.contains('.') {
        return false;
    }
    let Some(folded) = vex_search::retriever::casefold_ascii(name) else {
        return false;
    };
    identifiers(question).any(|t| t.to_ascii_lowercase() == folded)
}

/// `re.findall(r"[A-Za-z_][A-Za-z0-9_]*", s)`.
fn identifiers(s: &str) -> impl Iterator<Item = &str> {
    let bytes = s.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() && !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        Some(&s[start..i])
    })
}

type QuestionKey = (bool, usize, (bool, bool, bool, f64, bool, f64));

fn question_priority(question: &str, lead: &Lead) -> QuestionKey {
    let evidence = format!("{} {} {}", lead.target, lead.name, lead.preview);
    let q = tokens(question);
    let overlap = tokens(&evidence).intersection(&q).count();
    (names_the_symbol(question, lead), overlap, priority(lead))
}

fn cmp_key(a: &QuestionKey, b: &QuestionKey) -> std::cmp::Ordering {
    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
}

/// Sort indexes best first, keeping the given order among equals (Python's
/// `sort(key=..., reverse=True)` is stable).
fn sort_best_first(indexes: &mut [usize], question: &str, leads: &[Lead]) {
    indexes.sort_by(|&a, &b| {
        cmp_key(
            &question_priority(question, &leads[b]),
            &question_priority(question, &leads[a]),
        )
    });
}

fn quality_tier(lead: &Lead) -> u8 {
    match (
        lead.source_available,
        is_source(&lead.file),
        is_non_source(&lead.file),
    ) {
        (true, true, _) => 3,
        (true, false, false) => 2,
        (true, _, _) => 1,
        _ => 0,
    }
}

fn eligible_candidates(questions: &[String], leads: &[Lead]) -> Vec<PyIntSet> {
    questions
        .iter()
        .enumerate()
        .map(|(q, question)| {
            let matches = PyIntSet::from_list(
                &(0..leads.len())
                    .filter(|&i| leads[i].matched_queries.contains(&q))
                    .collect::<Vec<_>>(),
            );
            if matches.is_empty() {
                return PyIntSet::new();
            }
            let best = matches
                .iter()
                .map(|i| quality_tier(&leads[i]))
                .max()
                .unwrap_or(0);
            let mut best_matches: Vec<usize> = matches
                .iter()
                .filter(|&i| quality_tier(&leads[i]) == best)
                .collect();
            sort_best_first(&mut best_matches, question, leads);
            best_matches.truncate(MAX_ELIGIBLE_PER_QUESTION);
            PyIntSet::from_list(&best_matches)
        })
        .collect()
}

/// The selected leads (in bundle order) and the `selector` block.
pub fn select_leads(
    questions: &[String],
    leads: &[Lead],
    use_model: bool,
    selector_ms: i64,
) -> (Vec<usize>, Json) {
    let eligible = eligible_candidates(questions, leads);
    let mut assignments: Vec<(usize, Vec<usize>)> = Vec::new();
    for (q, question) in questions.iter().enumerate() {
        let mut matches: Vec<usize> = eligible[q].iter().collect();
        sort_best_first(&mut matches, question, leads);
        if !matches.is_empty() {
            matches.truncate(FALLBACK_BUNDLE_SIZE);
            assignments.push((q, matches));
        }
    }
    let mut selected: Vec<usize> = Vec::new();
    for (_, candidates) in &assignments {
        for &i in candidates {
            if !selected.contains(&i) && selected.len() < MAX_SELECTED_LEADS {
                selected.push(i);
            }
        }
    }
    let public: Vec<Json> = assignments
        .iter()
        .map(|(q, candidates)| {
            Json::obj(vec![
                ("question", Json::from(*q)),
                (
                    "targets",
                    Json::from(
                        candidates
                            .iter()
                            .filter(|i| selected.contains(i))
                            .map(|&i| leads[i].target.clone())
                            .collect::<Vec<_>>(),
                    ),
                ),
            ])
        })
        .collect();
    let union: BTreeSet<usize> = eligible.iter().flat_map(|s| s.iter()).collect();
    let filtered = (0..questions.len())
        .map(|q| {
            (0..leads.len())
                .filter(|&i| leads[i].matched_queries.contains(&q) && !eligible[q].contains(i))
                .count()
        })
        .sum::<usize>();
    let reason = if use_model {
        "no local selector available"
    } else {
        "selection is rule-based"
    };
    let block = Json::obj(vec![
        ("provider", Json::from("heuristic")),
        ("model", Json::from("rules-v1")),
        ("status", Json::from("fallback")),
        ("selector_ms", Json::from(selector_ms)),
        ("attempts", Json::from(if use_model { 1usize } else { 0 })),
        ("fallback_reason", Json::from(reason)),
        ("rejected_ids", Json::from(0usize)),
        ("bundle_limit", Json::from(MAX_BUNDLE_PER_QUESTION)),
        ("eligible_candidates", Json::from(union.len())),
        ("guardrail_filtered_matches", Json::from(filtered)),
        ("assignments", Json::Arr(public)),
    ]);
    (selected, block)
}
