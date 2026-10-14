//! bge-small-en-v1.5 through statically linked ONNX Runtime agrees with
//! fastembed 0.8.0 (Cortex's locked version) on fixed inputs, one batched
//! call as Cortex makes it. Ignored by default: it downloads the pinned
//! weights (~66 MB) into VEX_HOME/models on first run.

use std::time::Duration;
use vex_embed::{BgeEmbedder, Embedder};

fn cosine(a: &[f32], b: &[f64]) -> f64 {
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let x = *x as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[test]
#[ignore = "downloads the bge-small weights"]
fn bge_vectors_agree_with_fastembed() {
    let raw = include_str!("fixtures/bge_oracle.json");
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    let inputs: Vec<String> = serde_json::from_value(v["inputs"].clone()).unwrap();
    let want: Vec<Vec<f64>> = serde_json::from_value(v["vectors"].clone()).unwrap();
    let home = vex_store::home::from_env().expect("VEX_HOME or HOME");
    let e = BgeEmbedder::new(&home.join("models"), Duration::from_secs(60));
    let texts: Vec<&str> = inputs.iter().map(String::as_str).collect();
    let got = e.embed(&texts).expect("embed");
    assert!(e.is_loaded());
    assert_eq!(got.len(), want.len());
    let mut worst = 1f64;
    for ((text, g), w) in inputs.iter().zip(&got).zip(&want) {
        assert_eq!(g.len(), 384);
        let c = cosine(g, w);
        worst = worst.min(c);
        assert!(c >= 0.999, "cosine {c} < 0.999 for {text:?}");
        // Cosine ignores scale: the vectors themselves must be fastembed's,
        // L2-normalised, component by component.
        let norm = g.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm {norm} for {text:?}");
        let diff = g
            .iter()
            .zip(w)
            .map(|(x, y)| (*x as f64 - y).abs())
            .fold(0f64, f64::max);
        assert!(diff < 1e-3, "component off by {diff} for {text:?}");
    }
    eprintln!("worst cosine vs fastembed: {worst}");
    // A single text embeds the same alone as in the batch.
    let alone = e.embed(&[texts[3]]).unwrap();
    assert!(cosine(&alone[0], &want[3]) >= 0.999);

    // Many texts split into length-sorted batches over several sessions
    // come back in order, each still fastembed's vector.
    let parallel = BgeEmbedder::new(&home.join("models"), Duration::from_secs(60)).with_workers(4);
    let n = texts.len() * 12;
    let many: Vec<&str> = texts.iter().copied().cycle().take(n).collect();
    let got = parallel.embed(&many).expect("parallel embed");
    assert_eq!(got.len(), n);
    for (i, g) in got.iter().enumerate() {
        let w = &want[i % want.len()];
        let c = cosine(g, w);
        assert!(
            c >= 0.999,
            "parallel: cosine {c} for text {}",
            i % want.len()
        );
    }
}
