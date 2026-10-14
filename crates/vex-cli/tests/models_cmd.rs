//! `vex models list / pull / unload`: the embedder's state and idle timer,
//! fetching the pinned weights ahead of time (refusing any download whose
//! sha256 does not match its pin, and never storing it), and unloading.
//! Downloads come from a local server (VEX_MODEL_BASE_URL), never the hub.

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use vex_test_support::TestHome;

struct Env {
    home: TestHome,
}

impl Env {
    fn vex_home(&self) -> PathBuf {
        self.home.vex_home().join("vexhome")
    }

    fn vex(&self, args: &[&str], extra: &[(&str, &str)]) -> (Option<i32>, Value, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vex"));
        cmd.args(args)
            .current_dir(self.home.vex_home())
            .env("VEX_DAEMON_AUTOSTART", "never")
            .env("VEX_HOME", self.vex_home())
            .env_remove("VEX_EMBED_BACKEND")
            .env_remove("VEX_EMBED_IDLE_SECS")
            .env_remove("VEX_MODEL_BASE_URL");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// Serve `body` for every request; counts requests.
fn serve(body: Vec<u8>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
        }
    });
    (url, hits)
}

#[test]
fn models_list_reports_the_embedders_state_and_idle_timer() {
    let env = Env {
        home: TestHome::new(),
    };
    let (code, doc, err) = env.vex(
        &["--json", "models", "list"],
        &[("VEX_EMBED_IDLE_SECS", "123")],
    );
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["embed_backend"], "bge");
    let models = doc["models"].as_array().unwrap();
    assert_eq!(models.len(), 1, "only the embedder (no selector model)");
    let m = &models[0];
    assert_eq!(m["name"], "bge-small-en-v1.5");
    assert_eq!(m["role"], "embedder");
    assert_eq!(m["dim"], 384);
    assert_eq!(m["active"], true);
    assert_eq!(m["downloaded"], false);
    assert_eq!(m["verified"], false);
    assert_eq!(m["loaded"], false);
    assert_eq!(m["idle_unload_s"], 123);
    assert_eq!(m["idle_remaining_s"], Value::Null);
    assert_eq!(m["revision"].as_str().unwrap().len(), 40);
    assert!(
        m["path"]
            .as_str()
            .unwrap()
            .ends_with("models/bge-small-en-v1.5")
    );

    // With hash vectors chosen, the model is listed but not the active one.
    let (_, doc, _) = env.vex(
        &["--json", "models", "list"],
        &[("VEX_EMBED_BACKEND", "hash")],
    );
    assert_eq!(doc["embed_backend"], "hash");
    assert_eq!(doc["models"][0]["active"], false);
    assert_eq!(doc["models"][0]["idle_unload_s"], 300);

    let (code, _, out) = env.vex(&["models", "list"], &[]);
    assert_eq!(code, Some(0));
    let _ = out;
}

#[test]
fn models_pull_refuses_a_corrupted_download_and_stores_nothing() {
    let env = Env {
        home: TestHome::new(),
    };
    let (url, hits) = serve(b"not the model you are looking for".to_vec());
    let (code, doc, err) = env.vex(
        &["--json", "models", "pull"],
        &[("VEX_MODEL_BASE_URL", &url)],
    );
    assert_eq!(
        code,
        Some(1),
        "a corrupted download is refused: {doc} {err}"
    );
    assert_eq!(doc["error"], "model_verification");
    let msg = doc["message"].as_str().unwrap();
    assert!(
        msg.contains("model_optimized.onnx") && msg.contains("sha256"),
        "{msg}"
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "the local server was not asked"
    );
    let dir = env.vex_home().join("models/bge-small-en-v1.5");
    let stored: Vec<_> = std::fs::read_dir(&dir)
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(
        stored.is_empty(),
        "a refused download left files: {stored:?}"
    );
    let (_, doc, _) = env.vex(&["--json", "models", "list"], &[]);
    assert_eq!(doc["models"][0]["downloaded"], false);
}

#[test]
fn files_on_disk_that_fail_their_pins_are_downloaded_but_not_verified() {
    let env = Env {
        home: TestHome::new(),
    };
    let dir = env.vex_home().join("models/bge-small-en-v1.5");
    std::fs::create_dir_all(&dir).unwrap();
    for name in [
        "model_optimized.onnx",
        "tokenizer.json",
        "config.json",
        "tokenizer_config.json",
        "special_tokens_map.json",
    ] {
        std::fs::write(dir.join(name), b"corrupt").unwrap();
    }
    let (_, doc, _) = env.vex(&["--json", "models", "list"], &[]);
    assert_eq!(doc["models"][0]["downloaded"], true);
    assert_eq!(doc["models"][0]["verified"], false);
    assert_eq!(doc["models"][0]["bytes"], 5 * 7);
}

#[test]
fn models_pull_offline_fails_naming_the_file() {
    let env = Env {
        home: TestHome::new(),
    };
    let (code, doc, _) = env.vex(
        &["--json", "models", "pull"],
        &[("VEX_MODEL_BASE_URL", "http://127.0.0.1:9")],
    );
    assert_eq!(code, Some(4));
    assert_eq!(doc["error"], "model_download");
    assert!(
        doc["message"]
            .as_str()
            .unwrap()
            .contains("model_optimized.onnx")
    );
}

#[test]
fn models_unload_reports_what_it_did() {
    let env = Env {
        home: TestHome::new(),
    };
    let (code, doc, err) = env.vex(&["--json", "models", "unload"], &[]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["unloaded"], false);
    assert_eq!(doc["model"], "bge-small-en-v1.5");
    assert!(doc["reason"].as_str().unwrap().contains("not loaded"));
}
