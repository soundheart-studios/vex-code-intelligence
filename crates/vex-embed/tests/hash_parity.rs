//! The hash backend reproduces Cortex 54c557ae `Embedder._hash_embed` bit for
//! bit: every component's IEEE-754 bits, over inputs that probe lowercasing
//! into ASCII (U+0130, U+212A), non-ASCII letters the token regex skips,
//! one-character runs it rejects, repeats, controls and a 2000-char token.
//! Fixtures: tests/fixtures/hash_oracle.json, recorded from Cortex.

use std::collections::BTreeMap;
use vex_embed::{Embedder, HASH_DIM, HashEmbedder, hash_embed};

fn oracle() -> (Vec<String>, Vec<BTreeMap<String, String>>) {
    let raw = include_str!("fixtures/hash_oracle.json");
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    let inputs = serde_json::from_value(v["inputs"].clone()).unwrap();
    let vectors = serde_json::from_value(v["vectors"].clone()).unwrap();
    (inputs, vectors)
}

fn expected(row: &BTreeMap<String, String>) -> Vec<u64> {
    let mut bits = vec![0f64.to_bits(); HASH_DIM];
    for (i, hex) in row {
        bits[i.parse::<usize>().unwrap()] = u64::from_str_radix(hex, 16).unwrap();
    }
    bits
}

#[test]
fn hash_vectors_equal_cortex_bit_for_bit() {
    let (inputs, vectors) = oracle();
    assert!(inputs.len() >= 18, "oracle lost inputs");
    for (text, row) in inputs.iter().zip(&vectors) {
        let got: Vec<u64> = hash_embed(text).iter().map(|x| x.to_bits()).collect();
        assert_eq!(got, expected(row), "hash vector differs for {text:?}");
    }
}

#[test]
fn the_embedder_trait_serves_the_same_vectors_as_f32() {
    let (inputs, vectors) = oracle();
    let texts: Vec<&str> = inputs.iter().map(String::as_str).collect();
    let e = HashEmbedder;
    assert_eq!(e.model_name(), "hash-256");
    assert_eq!(e.dim(), HASH_DIM);
    assert_eq!(e.context_tokens(), None);
    let got = e.embed(&texts).unwrap();
    assert_eq!(got.len(), texts.len());
    for ((text, row), vec) in inputs.iter().zip(&vectors).zip(&got) {
        let want: Vec<f32> = expected(row)
            .into_iter()
            .map(|b| f64::from_bits(b) as f32)
            .collect();
        assert_eq!(vec, &want, "f32 vector differs for {text:?}");
    }
    assert!(e.embed(&[]).unwrap().is_empty());
}
