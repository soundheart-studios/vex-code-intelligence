//! Builds the declarative walker fixture (tests/fixtures/walk/tree.json)
//! exactly as its Cortex oracle does.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn fixture(name: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/walk")
        .join(name);
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

pub fn build_tree(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    for e in fixture("tree.json")["entries"].as_array().unwrap() {
        if let Some(file) = e["file"].as_str() {
            let p = root.join(file);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let bytes = match e["content"].as_str() {
                Some(c) => c.as_bytes().to_vec(),
                None => {
                    let prefix = e["prefix"].as_str().unwrap_or("").as_bytes().to_vec();
                    let size = e["size"].as_u64().unwrap() as usize;
                    let fill = e["fill"].as_str().unwrap().as_bytes()[0];
                    let mut b = prefix.clone();
                    b.resize(size, fill);
                    b
                }
            };
            std::fs::write(p, bytes).unwrap();
        } else if let Some(dir) = e["dir"].as_str() {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        } else {
            let link = root.join(e["symlink"].as_str().unwrap());
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            let target = match e["target"].as_str().unwrap() {
                "ROOT_PARENT" => root.parent().unwrap().to_path_buf(),
                t => PathBuf::from(t),
            };
            std::os::unix::fs::symlink(target, link).unwrap();
        }
    }
}
