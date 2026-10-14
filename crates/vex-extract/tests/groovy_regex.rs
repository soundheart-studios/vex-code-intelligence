//! Groovy's lexical fallback against Cortex's own patterns, one test per
//! pattern, over the samples in fixtures/groovy_regex (recorded from
//! Cortex): every match's span and groups, the blanked text, and
//! the declarations scanned from it.

use serde_json::{Value, json};
use std::path::Path;
use vex_extract::lang::groovy::{
    CALL, CTOR_HEAD, FEATURE_HEAD, GRADLE_TASK, METHOD_HEAD, TYPE_DECL, blank_non_code,
    scan_declarations,
};

fn cases() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/groovy_regex/cases.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn span(m: Option<(usize, usize)>) -> Value {
    m.map_or(Value::Null, |(a, b)| json!([a, b]))
}

fn plain(re: &regex::Regex, text: &str) -> Vec<Value> {
    re.captures_iter(text)
        .map(|c| {
            let whole = c.get(0).unwrap();
            let groups: Vec<Value> = (1..c.len())
                .map(|k| span(c.get(k).map(|g| (g.start(), g.end()))))
                .collect();
            json!({"span": [whole.start(), whole.end()], "groups": groups})
        })
        .collect()
}

fn fancy(re: &fancy_regex::Regex, text: &str) -> Vec<Value> {
    re.captures_iter(text)
        .map(|c| {
            let c = c.unwrap();
            let whole = c.get(0).unwrap();
            let groups: Vec<Value> = (1..c.len())
                .map(|k| span(c.get(k).map(|g| (g.start(), g.end()))))
                .collect();
            json!({"span": [whole.start(), whole.end()], "groups": groups})
        })
        .collect()
}

fn check(name: &str, got: impl Fn(&str) -> Vec<Value>) {
    for (i, case) in cases().iter().enumerate() {
        let code = case["blanked"].as_str().unwrap();
        assert_eq!(
            Value::Array(got(code)),
            case["patterns"][name],
            "{name} on sample {i}: {:?}",
            case["text"]
        );
    }
}

#[test]
fn type_decl_matches_like_cortex() {
    check("TYPE_DECL", |t| plain(&TYPE_DECL, t));
}

#[test]
fn method_head_matches_like_cortex() {
    check("METHOD_HEAD", |t| plain(&METHOD_HEAD, t));
}

#[test]
fn ctor_head_matches_like_cortex() {
    check("CTOR_HEAD", |t| plain(&CTOR_HEAD, t));
}

#[test]
fn gradle_task_matches_like_cortex() {
    check("GRADLE_TASK", |t| plain(&GRADLE_TASK, t));
}

#[test]
fn feature_head_backreference_matches_like_cortex() {
    check("FEATURE_HEAD", |t| fancy(&FEATURE_HEAD, t));
}

#[test]
fn call_lookbehind_matches_like_cortex() {
    check("CALL", |t| fancy(&CALL, t));
}

#[test]
fn comments_and_strings_blank_like_cortex() {
    for case in cases() {
        let text = case["text"].as_str().unwrap();
        let want = case["blanked"].as_str().unwrap();
        let got = blank_non_code(text);
        // Cortex blanks per character, vex per byte (keeping offsets): the
        // same text once runs of spaces are disregarded, identical in ASCII.
        assert_eq!(got.len(), text.len(), "byte length kept: {text:?}");
        let words = |s: &str| -> Vec<String> {
            s.split(' ')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        };
        assert_eq!(words(&got), words(want), "{text:?}");
        if text.is_ascii() {
            assert_eq!(got, want, "{text:?}");
        }
    }
}

#[test]
fn declarations_scan_like_cortex() {
    for case in cases() {
        let text = case["text"].as_str().unwrap();
        let code = blank_non_code(text);
        let got: Vec<Value> = scan_declarations(&code, text)
            .into_iter()
            .map(|(k, n, s, o, e)| json!([k, n, s, o, e]))
            .collect();
        assert_eq!(Value::Array(got), case["declarations"], "{text:?}");
    }
}
