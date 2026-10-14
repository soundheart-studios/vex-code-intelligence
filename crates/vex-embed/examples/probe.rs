//! Release probe for `cargo xtask check-static-deps`: `--ort` reports the
//! ONNX Runtime linked into this binary. `probe MODELS_DIR TEXT...` embeds
//! with bge-small and prints each vector's first components.

use vex_embed::{BgeEmbedder, Embedder};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--ort") {
        println!("onnxruntime linked: {}", ort::info());
        return;
    }
    let Some((models, texts)) = args.split_first() else {
        eprintln!("usage: probe --ort | probe MODELS_DIR TEXT...");
        std::process::exit(2);
    };
    let e = BgeEmbedder::new(
        std::path::Path::new(models),
        std::time::Duration::from_secs(5),
    );
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    match e.embed(&texts) {
        Ok(vs) => {
            for (t, v) in texts.iter().zip(vs) {
                println!("{t:?}: {:?}", &v[..4]);
            }
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
