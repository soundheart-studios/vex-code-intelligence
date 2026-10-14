//! Weights download on first need, are sha256-verified, and only verified
//! bytes ever reach their final name.

use vex_embed::weights::{self, ModelFile, WeightsError};

const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("vex-weights-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn a_verified_download_is_stored_and_not_refetched() {
    let d = dir("ok");
    let files = [ModelFile {
        name: "a.bin",
        sha256: ABC,
        size: 3,
    }];
    let calls = std::cell::Cell::new(0);
    let fetch = |name: &str| {
        calls.set(calls.get() + 1);
        assert_eq!(name, "a.bin");
        Ok(b"abc".to_vec())
    };
    weights::ensure_files(&d, &files, &fetch).unwrap();
    assert_eq!(std::fs::read(d.join("a.bin")).unwrap(), b"abc");
    weights::ensure_files(&d, &files, &fetch).unwrap();
    assert_eq!(calls.get(), 1, "a verified file was fetched again");
    weights::verify_files(&d, &files).unwrap();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_corrupted_download_is_refused_and_leaves_nothing() {
    let d = dir("bad");
    let files = [ModelFile {
        name: "a.bin",
        sha256: ABC,
        size: 3,
    }];
    let err = weights::ensure_files(&d, &files, &|_: &str| Ok(b"abd".to_vec())).unwrap_err();
    match &err {
        WeightsError::Checksum {
            file,
            expected,
            actual,
        } => {
            assert_eq!(file, "a.bin");
            assert_eq!(expected, ABC);
            assert_ne!(actual, ABC);
        }
        other => panic!("wrong error {other:?}"),
    }
    assert!(err.to_string().contains("a.bin"));
    assert!(!d.join("a.bin").exists());
    let leftovers: Vec<_> = std::fs::read_dir(&d)
        .map(|r| r.count())
        .into_iter()
        .collect();
    assert!(leftovers.iter().all(|n| *n == 0), "temp file left behind");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_file_corrupted_on_disk_fails_verification_and_is_refetched() {
    let d = dir("disk");
    let files = [ModelFile {
        name: "a.bin",
        sha256: ABC,
        size: 3,
    }];
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("a.bin"), b"xyz").unwrap();
    assert!(matches!(
        weights::verify_files(&d, &files),
        Err(WeightsError::Checksum { .. })
    ));
    weights::ensure_files(&d, &files, &|_: &str| Ok(b"abc".to_vec())).unwrap();
    assert_eq!(std::fs::read(d.join("a.bin")).unwrap(), b"abc");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_failed_fetch_names_the_file() {
    let d = dir("net");
    let files = [ModelFile {
        name: "a.bin",
        sha256: ABC,
        size: 3,
    }];
    let err =
        weights::ensure_files(&d, &files, &|_: &str| Err("connection refused".into())).unwrap_err();
    assert!(matches!(err, WeightsError::Fetch { .. }));
    let msg = err.to_string();
    assert!(
        msg.contains("a.bin") && msg.contains("connection refused"),
        "{msg}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_bge_pin_is_complete() {
    let names: Vec<&str> = weights::BGE_FILES.iter().map(|f| f.name).collect();
    for want in [
        "model_optimized.onnx",
        "tokenizer.json",
        "config.json",
        "tokenizer_config.json",
        "special_tokens_map.json",
    ] {
        assert!(names.contains(&want), "{want} not pinned");
    }
    assert!(weights::BGE_FILES.iter().all(|f| f.sha256.len() == 64));
    assert_eq!(weights::BGE_REVISION.len(), 40);
    assert!(weights::url(None, "tokenizer.json").ends_with(&format!(
        "/Qdrant/bge-small-en-v1.5-onnx-Q/resolve/{}/tokenizer.json",
        weights::BGE_REVISION
    )));
    assert_eq!(
        weights::url(Some("http://127.0.0.1:9"), "config.json"),
        format!(
            "http://127.0.0.1:9/Qdrant/bge-small-en-v1.5-onnx-Q/resolve/{}/config.json",
            weights::BGE_REVISION
        )
    );
}
