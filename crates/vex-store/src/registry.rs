//! The project registry (`VEX_HOME/projects/registry.json`): project id ->
//! root, remote URL, display name, last use.
//!
//! Ids are stable, not path hashes: a repository that
//! moved re-attaches to its old id by its remote URL, so its index survives
//! the move. The same remote cloned twice is two projects (each clone, and
//! each git worktree, is its own root with its own index; the embedding
//! cache they share is content-addressed).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const REGISTRY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub root: PathBuf,
    /// The remote URL as configured (normalized for matching only).
    pub remote: Option<String>,
    pub name: String,
    /// Seconds since the Unix epoch.
    pub last_used: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Data {
    version: u32,
    projects: BTreeMap<String, Project>,
}

/// How `attach` found the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attached {
    /// The root was already registered.
    Existing(String),
    /// A moved repository, recognised by its remote; the old root is gone.
    Reattached {
        id: String,
        from: PathBuf,
    },
    New(String),
}

impl Attached {
    pub fn id(&self) -> &str {
        match self {
            Attached::Existing(id) | Attached::New(id) => id,
            Attached::Reattached { id, .. } => id,
        }
    }
}

#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    data: Data,
}

/// `git@github.com:o/r.git`, `https://github.com/o/r/` and
/// `ssh://git@github.com/o/r` all name the same repository.
pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_string();
    for scheme in ["https://", "http://", "ssh://", "git://", "git+ssh://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest.to_string();
            break;
        }
    }
    if let Some(at) = s.find('@')
        && !s[..at].contains('/')
    {
        s = s[at + 1..].to_string();
    }
    // scp-like `host:path` (but not a Windows drive or a port number).
    if let Some(colon) = s.find(':') {
        let (host, rest) = s.split_at(colon);
        if !host.contains('/') && !rest[1..].starts_with(|c: char| c.is_ascii_digit()) {
            s = format!("{host}/{}", &rest[1..]);
        }
    }
    let s = s.trim_end_matches('/');
    let s = s.strip_suffix(".git").unwrap_or(s);
    match s.split_once('/') {
        Some((host, path)) => format!("{}/{path}", host.to_lowercase()),
        None => s.to_lowercase(),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!("{}\0{nanos}\0{}", root.display(), std::process::id());
    let digest = Sha256::digest(seed.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// `VEX_HOME/projects/<id>/`.
pub fn project_dir(home: &Path, id: &str) -> PathBuf {
    home.join("projects").join(id)
}

pub fn index_path(home: &Path, id: &str) -> PathBuf {
    project_dir(home, id).join("index.db")
}

pub fn lock_path(home: &Path, id: &str) -> PathBuf {
    project_dir(home, id).join("write.lock")
}

impl Registry {
    pub fn path_in(home: &Path) -> PathBuf {
        home.join("projects").join("registry.json")
    }

    /// Load the registry (empty when absent or unreadable as JSON).
    pub fn load(home: &Path) -> Registry {
        let path = Registry::path_in(home);
        let data = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Data>(&b).ok())
            .unwrap_or(Data {
                version: REGISTRY_VERSION,
                projects: BTreeMap::new(),
            });
        Registry { path, data }
    }

    /// Write atomically (temp file, then rename), owner-only.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        let mut bytes = serde_json::to_vec_pretty(&self.data).expect("registry serialises");
        bytes.push(b'\n');
        std::fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)
    }

    pub fn get(&self, id: &str) -> Option<&Project> {
        self.data.projects.get(id)
    }

    pub fn projects(&self) -> impl Iterator<Item = (&String, &Project)> {
        self.data.projects.iter()
    }

    pub fn find_by_root(&self, root: &Path) -> Option<&str> {
        self.data
            .projects
            .iter()
            .find(|(_, p)| p.root == root)
            .map(|(id, _)| id.as_str())
    }

    /// Register `root` (or recognise it), with its remote URL if it has one.
    pub fn attach(&mut self, root: &Path, remote: Option<&str>) -> Attached {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(id) = self.find_by_root(root).map(String::from) {
            let p = self.data.projects.get_mut(&id).expect("found");
            p.last_used = now();
            if remote.is_some() {
                p.remote = remote.map(String::from);
            }
            return Attached::Existing(id);
        }
        if let Some(url) = remote {
            let want = normalize_remote(url);
            let moved = self
                .data
                .projects
                .iter()
                .find(|(_, p)| {
                    p.remote.as_deref().map(normalize_remote).as_deref() == Some(want.as_str())
                        && !p.root.exists()
                })
                .map(|(id, p)| (id.clone(), p.root.clone()));
            if let Some((id, from)) = moved {
                let p = self.data.projects.get_mut(&id).expect("found");
                p.root = root.to_path_buf();
                p.name = name;
                p.remote = Some(url.to_string());
                p.last_used = now();
                return Attached::Reattached { id, from };
            }
        }
        let mut id = new_id(root);
        while self.data.projects.contains_key(&id) {
            id = new_id(&root.join(&id));
        }
        self.data.projects.insert(
            id.clone(),
            Project {
                root: root.to_path_buf(),
                remote: remote.map(String::from),
                name,
                last_used: now(),
            },
        );
        Attached::New(id)
    }

    /// Remove a project's registry entry (its index directory is the
    /// caller's to delete).
    pub fn forget(&mut self, id: &str) -> Option<Project> {
        self.data.projects.remove(id)
    }
}
