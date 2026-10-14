//! Call-edge resolution: a port of Cortex's `indexer/resolver.py`
//! (`rebuild_call_edges`, `resolve_call`, `resolve_module`). Pure: rows in,
//! edges out.
//!
//! Confidence tiers: 0.95 import-bound, 0.90 lexical / typed / explicit
//! receiver, 0.70 constructor instance, 0.60 unique other-file name, 0.35
//! each for 2-3 defining files, nothing beyond that.
//!
//! Row ORDER matters exactly where it does in Cortex: the name-match tiers
//! take the first symbol a file defines under a name, and edges are stored
//! with INSERT OR REPLACE, so a later duplicate (src, dst) wins. Callers must
//! pass rows in the order Cortex's SELECTs return them.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Everything `rebuild_call_edges` reads from the index.
#[derive(Debug, Clone, Default)]
pub struct Rows {
    pub files: BTreeSet<String>,
    /// (qualified_name, name, kind, file_path), in table order.
    pub symbols: Vec<(String, String, String, String)>,
    /// (file_path, name, module, orig).
    pub bindings: Vec<(String, String, String, String)>,
    /// (file_path, caller, name, count), in table order.
    pub calls: Vec<(String, String, String, i64)>,
    /// Qualified names whose every site is a declaration (interface methods).
    pub abstract_qns: HashSet<String>,
    /// (file_path, owner, member, type).
    pub members: Vec<(String, String, String, String)>,
    /// Workspace package name -> directory (read from package.json files).
    pub packages: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub src: String,
    pub dst: String,
    pub confidence: f64,
    pub src_file: String,
    pub dst_file: String,
}

const JS_EXTS: &[&str] = &[".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs"];
const ESM_RUNTIME_EXTS: &[&str] = &[".js", ".mjs", ".cjs", ".jsx"];
const CPP_EXTS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "inl", "ipp", "mm",
];
const CLASS_KINDS: &[&str] = &["class", "interface", "struct", "trait", "impl"];
const BARE_SKIP_CLASS_EXTS: &[&str] =
    &[".py", ".js", ".jsx", ".mjs", ".ts", ".tsx", ".mts", ".cts"];

const RUNTIME_METHOD_NAMES: &[&str] = &[
    "map",
    "filter",
    "reduce",
    "find",
    "findIndex",
    "forEach",
    "some",
    "every",
    "flat",
    "flatMap",
    "slice",
    "splice",
    "concat",
    "join",
    "split",
    "trim",
    "replace",
    "replaceAll",
    "match",
    "matchAll",
    "startsWith",
    "endsWith",
    "includes",
    "indexOf",
    "lastIndexOf",
    "padStart",
    "padEnd",
    "repeat",
    "toLowerCase",
    "toUpperCase",
    "push",
    "pop",
    "shift",
    "unshift",
    "reverse",
    "sort",
    "keys",
    "values",
    "entries",
    "has",
    "add",
    "delete",
    "clear",
    "then",
    "catch",
    "finally",
    "resolve",
    "reject",
    "all",
    "race",
    "parse",
    "stringify",
    "toString",
    "valueOf",
    "bind",
    "call",
    "apply",
    "assign",
    "freeze",
    "hasOwnProperty",
    "next",
    "return",
    "throw",
    "describe",
    "it",
    "test",
    "expect",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
    "vi",
    "jest",
    "mock",
    "spyOn",
    "get",
    "set",
    "list",
    "dict",
    "len",
    "open",
    "close",
    "read",
    "write",
    "append",
    "extend",
    "items",
    "update",
    "copy",
    "count",
    "format",
    "strip",
    "lower",
    "upper",
    "encode",
    "decode",
    "send",
    "run",
    "start",
    "stop",
    "clone",
    "document",
    "log",
    "error",
    "warn",
    "info",
    "debug",
    "print",
    "id",
    "type",
    "str",
    "int",
    "float",
];

const CPP_STD_METHOD_NAMES: &[&str] = &[
    "size",
    "empty",
    "data",
    "begin",
    "end",
    "cbegin",
    "cend",
    "rbegin",
    "rend",
    "front",
    "back",
    "push_back",
    "pop_back",
    "emplace_back",
    "push_front",
    "pop_front",
    "emplace",
    "insert",
    "erase",
    "clear",
    "reserve",
    "resize",
    "capacity",
    "at",
    "find",
    "count",
    "swap",
    "c_str",
    "substr",
    "length",
    "append",
    "compare",
    "get",
    "reset",
    "release",
    "lock",
    "unlock",
    "first",
    "second",
    "load",
    "store",
    "exchange",
    "value",
    "has_value",
    "value_or",
];

/// Rows from the JSON shape the row oracle records (tests and xtask).
pub fn rows_from_json(v: &Value) -> Rows {
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let quads = |key: &str| -> Vec<(String, String, String, String)> {
        v[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| (s(&r[0]), s(&r[1]), s(&r[2]), s(&r[3])))
            .collect()
    };
    Rows {
        files: v["files"].as_array().into_iter().flatten().map(s).collect(),
        symbols: quads("symbols"),
        bindings: quads("bindings"),
        calls: v["calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| (s(&r[0]), s(&r[1]), s(&r[2]), r[3].as_i64().unwrap_or(0)))
            .collect(),
        abstract_qns: v["abstract"]
            .as_array()
            .into_iter()
            .flatten()
            .map(s)
            .collect(),
        members: quads("members"),
        packages: v["packages"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, d)| (k.clone(), s(d)))
            .collect(),
    }
}

/// Edges in the golden export's shape: `[src, dst, round(conf, 6),
/// src_file, dst_file]`, sorted.
pub fn edges_json(edges: &[Edge]) -> Value {
    let mut rows: Vec<(String, String, f64, String, String)> = edges
        .iter()
        .map(|e| {
            (
                e.src.clone(),
                e.dst.clone(),
                (e.confidence * 1e6).round() / 1e6,
                e.src_file.clone(),
                e.dst_file.clone(),
            )
        })
        .collect();
    rows.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Value::Array(
        rows.into_iter()
            .map(|(a, b, c, d, e)| serde_json::json!([a, b, c, d, e]))
            .collect(),
    )
}

// ---------------------------------------------------------------- paths

/// `posixpath.dirname`.
fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => {
            let head = &p[..i + 1];
            let trimmed = head.trim_end_matches('/');
            if trimmed.is_empty() {
                &head[..head.len().min(1)]
            } else {
                trimmed
            }
        }
        None => "",
    }
}

/// `posixpath.join(a, b)`.
fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `posixpath.normpath`.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let initial = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else if path.starts_with('/') {
        1
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (initial == 0 && comps.is_empty()) || comps.last() == Some(&"..") {
            comps.push(comp);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let joined = comps.join("/");
    let out = format!("{}{joined}", "/".repeat(initial));
    if out.is_empty() { ".".into() } else { out }
}

fn ext_of(fp: &str) -> &str {
    fp.rsplit('.').next().unwrap_or(fp)
}

fn dir_of(fp: &str) -> &str {
    if fp.contains('/') {
        fp.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
    } else {
        ""
    }
}

fn dotted(fp: &str) -> String {
    fp.replace('/', ".")
}

/// Basename and directory-leaf lookups over the file set (`_PathIndex`).
struct PathIndex {
    by_basename: HashMap<String, Vec<String>>,
    by_dir_leaf: HashMap<String, BTreeSet<String>>,
}

impl PathIndex {
    fn new(files: &BTreeSet<String>) -> PathIndex {
        let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_dir_leaf: HashMap<String, BTreeSet<String>> = HashMap::new();
        for f in files {
            let (head, base) = match f.rfind('/') {
                Some(i) => (&f[..i], &f[i + 1..]),
                None => ("", f.as_str()),
            };
            by_basename
                .entry(base.to_string())
                .or_default()
                .push(f.clone());
            let leaf = head.rsplit('/').next().unwrap_or(head);
            by_dir_leaf
                .entry(leaf.to_string())
                .or_default()
                .insert(head.to_string());
        }
        PathIndex {
            by_basename,
            by_dir_leaf,
        }
    }

    fn named(&self, basenames: &[&str]) -> Vec<String> {
        basenames
            .iter()
            .flat_map(|b| self.by_basename.get(*b).cloned().unwrap_or_default())
            .collect()
    }
}

/// `_file_for`: a specifier as written, then as TypeScript means it.
fn file_for(base: &str, files: &BTreeSet<String>) -> String {
    let candidates = |stem: &str| -> Vec<String> {
        JS_EXTS
            .iter()
            .map(|e| format!("{stem}{e}"))
            .chain(JS_EXTS.iter().map(|e| join(stem, &format!("index{e}"))))
            .collect()
    };
    if let Some(c) = candidates(base).into_iter().find(|c| files.contains(c)) {
        return c;
    }
    if files.contains(base) {
        return base.to_string();
    }
    if let Some(ext) = ESM_RUNTIME_EXTS.iter().find(|e| base.ends_with(**e)) {
        let stem = &base[..base.len() - ext.len()];
        if let Some(c) = candidates(stem).into_iter().find(|c| files.contains(c)) {
            return c;
        }
    }
    String::new()
}

/// Cortex `resolve_module`: '' when unresolved, `dir:<path>` for a Go
/// package directory, `pkg:<path>` for a workspace package root.
pub fn resolve_module(
    module: &str,
    importer: &str,
    files: &BTreeSet<String>,
    packages: &BTreeMap<String, String>,
) -> String {
    resolve_module_with(module, importer, files, packages, &PathIndex::new(files))
}

fn resolve_module_with(
    module: &str,
    importer: &str,
    files: &BTreeSet<String>,
    packages: &BTreeMap<String, String>,
    index: &PathIndex,
) -> String {
    if module.is_empty() {
        return String::new();
    }
    if module.starts_with('.') {
        return file_for(&normpath(&join(dirname(importer), module)), files);
    }
    if !packages.is_empty() {
        let mut names: Vec<&String> = packages
            .keys()
            .filter(|p| module == p.as_str() || module.starts_with(&format!("{p}/")))
            .collect();
        names.sort_by_key(|n| std::cmp::Reverse(n.len()));
        if let Some(name) = names.first() {
            let base = &packages[*name];
            let subpath = module[name.len()..].trim_start_matches('/');
            if !subpath.is_empty() {
                for stem in [join(base, subpath), join(&join(base, "src"), subpath)] {
                    let hit = file_for(&stem, files);
                    if !hit.is_empty() {
                        return hit;
                    }
                }
            }
            return format!("pkg:{base}");
        }
    }
    if module == "crate" {
        let mut d = dirname(importer).to_string();
        loop {
            for root_file in ["lib.rs", "main.rs"] {
                let cand = if d.is_empty() {
                    root_file.to_string()
                } else {
                    format!("{d}/{root_file}")
                };
                if files.contains(&cand) {
                    return cand;
                }
            }
            if d.is_empty() {
                return String::new();
            }
            d = dirname(&d).to_string();
        }
    }
    if module.contains("::") {
        let parts: Vec<&str> = module
            .split("::")
            .filter(|x| !matches!(*x, "crate" | "self" | "super"))
            .collect();
        let p = parts.join("/");
        if !p.is_empty() {
            for cand in [
                format!("{p}.rs"),
                format!("{p}/mod.rs"),
                format!("src/{p}.rs"),
                format!("src/{p}/mod.rs"),
            ] {
                if files.contains(&cand) {
                    return cand;
                }
            }
            let leaf = p.rsplit('/').next().unwrap_or(&p);
            let hits: Vec<String> = index
                .named(&[&format!("{leaf}.rs"), "mod.rs"])
                .into_iter()
                .filter(|f| {
                    f.ends_with(&format!("/{p}.rs")) || f.ends_with(&format!("/{p}/mod.rs"))
                })
                .collect();
            if hits.len() == 1 {
                return hits[0].clone();
            }
        }
        return String::new();
    }
    if module.contains('/') && module.split('/').next().is_some_and(|h| h.contains('.')) {
        let pkg = module.rsplit('/').next().unwrap_or(module);
        if let Some(dirs) = index.by_dir_leaf.get(pkg)
            && dirs.len() == 1
        {
            return format!("dir:{}", dirs.iter().next().unwrap());
        }
        return String::new();
    }
    let p = module.replace('.', "/");
    for cand in [format!("{p}.py"), format!("{p}/__init__.py")] {
        if files.contains(&cand) {
            return cand;
        }
    }
    let leaf = p.rsplit('/').next().unwrap_or(&p).to_string();
    for ext in [".py", ".java", ".kt", ".cs"] {
        let suffixes: Vec<String> = if ext == ".py" {
            vec![format!("/{p}.py"), format!("/{p}/__init__.py")]
        } else {
            vec![format!("/{p}{ext}")]
        };
        let cands = if ext == ".py" {
            index.named(&[&format!("{leaf}.py"), "__init__.py"])
        } else {
            index.named(&[&format!("{leaf}{ext}")])
        };
        let hits: Vec<String> = cands
            .into_iter()
            .filter(|f| {
                suffixes.iter().any(|s| f.ends_with(s.as_str())) || *f == format!("{p}{ext}")
            })
            .collect();
        if hits.len() == 1 {
            return hits[0].clone();
        }
    }
    String::new()
}

/// `re.sub(r"<[^.]*>$", "", s)`: the leftmost `<` after the last `.` whose
/// tail runs to a final `>`.
pub fn strip_trailing_template(s: &str) -> String {
    if !s.ends_with('>') {
        return s.to_string();
    }
    let tail_start = s.rfind('.').map_or(0, |i| i + 1);
    match s[tail_start..].find('<') {
        Some(i) if tail_start + i < s.len() - 1 => s[..tail_start + i].to_string(),
        _ => s.to_string(),
    }
}

/// Python `str.isupper()` on a one-character prefix.
fn first_is_upper(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
}

/// `next(iter(set))` when the set has exactly one element.
fn single<T: Clone + Eq + std::hash::Hash>(hits: HashSet<T>) -> Option<T> {
    if hits.len() == 1 {
        hits.into_iter().next()
    } else {
        None
    }
}

// ---------------------------------------------------------------- resolver

struct Resolver<'r> {
    rows: &'r Rows,
    by_file_name: HashMap<(String, String), Vec<String>>,
    by_name_files: HashMap<String, BTreeSet<String>>,
    class_qns: HashSet<String>,
    file_bindings: HashMap<String, HashMap<String, (String, String)>>,
    by_pkg_name: HashMap<(String, String), Vec<(String, String)>>,
    by_dir_name: HashMap<(String, String), Vec<(String, String)>>,
    declared_results: HashMap<(String, String, String), String>,
    fields_by_owner: HashMap<String, Vec<(String, String, String)>>,
    fields_by_type: HashMap<String, Vec<(String, String, String)>>,
}

type Target = (String, f64, String);

impl<'r> Resolver<'r> {
    fn new(rows: &'r Rows) -> Resolver<'r> {
        let mut by_file_name: HashMap<(String, String), Vec<String>> = HashMap::new();
        let mut by_name_files: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut class_qns = HashSet::new();
        for (qn, name, kind, fp) in &rows.symbols {
            by_file_name
                .entry((fp.clone(), name.clone()))
                .or_default()
                .push(qn.clone());
            by_name_files
                .entry(name.clone())
                .or_default()
                .insert(fp.clone());
            if CLASS_KINDS.contains(&kind.as_str()) {
                class_qns.insert(qn.clone());
            }
        }
        let index = PathIndex::new(&rows.files);
        let mut memo: HashMap<String, String> = HashMap::new();
        let mut file_bindings: HashMap<String, HashMap<String, (String, String)>> = HashMap::new();
        for (fp, name, module, orig) in &rows.bindings {
            let resolved = if module.starts_with('.') || module == "crate" {
                resolve_module_with(module, fp, &rows.files, &rows.packages, &index)
            } else {
                memo.entry(module.clone())
                    .or_insert_with(|| {
                        resolve_module_with(module, fp, &rows.files, &rows.packages, &index)
                    })
                    .clone()
            };
            file_bindings
                .entry(fp.clone())
                .or_default()
                .insert(name.clone(), (resolved, orig.clone()));
        }
        let mut by_pkg_name: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        let pkg_dirs: BTreeSet<&String> = rows.packages.values().collect();
        for pkg_dir in pkg_dirs {
            let prefix = format!("{pkg_dir}/");
            for (qn, name, _, fp) in &rows.symbols {
                if fp.starts_with(&prefix) {
                    by_pkg_name
                        .entry((pkg_dir.clone(), name.clone()))
                        .or_default()
                        .push((qn.clone(), fp.clone()));
                }
            }
        }
        let mut by_dir_name: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        for (qn, name, _, fp) in &rows.symbols {
            by_dir_name
                .entry((dir_of(fp).to_string(), name.clone()))
                .or_default()
                .push((qn.clone(), fp.clone()));
        }
        let mut declared_results = HashMap::new();
        let mut fields_by_owner: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        let mut fields_by_type: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        for (mfp, owner, member, kind) in &rows.members {
            if let Some(f) = member.strip_suffix("()") {
                declared_results.insert((mfp.clone(), owner.clone(), f.to_string()), kind.clone());
            }
        }
        for (mfp, owner, member, kind) in &rows.members {
            fields_by_owner.entry(owner.clone()).or_default().push((
                mfp.clone(),
                member.clone(),
                kind.clone(),
            ));
            let short = owner.rsplit('.').next().unwrap_or(owner).to_string();
            fields_by_type.entry(short).or_default().push((
                mfp.clone(),
                member.clone(),
                kind.clone(),
            ));
        }
        Resolver {
            rows,
            by_file_name,
            by_name_files,
            class_qns,
            file_bindings,
            by_pkg_name,
            by_dir_name,
            declared_results,
            fields_by_owner,
            fields_by_type,
        }
    }

    fn file_name(&self, fp: &str, name: &str) -> &[String] {
        self.by_file_name
            .get(&(fp.to_string(), name.to_string()))
            .map_or(&[], Vec::as_slice)
    }

    fn binding(&self, fp: &str, name: &str) -> Option<&(String, String)> {
        self.file_bindings.get(fp).and_then(|b| b.get(name))
    }

    /// `_bound_lookup`: (qn, file) through an import binding.
    fn bound_lookup(&self, fp: &str, binding_name: &str, target: &str) -> Option<(String, String)> {
        let (resolved, orig) = self.binding(fp, binding_name)?;
        if resolved.is_empty() {
            return None;
        }
        let lookup = if !target.is_empty() {
            target
        } else if !orig.is_empty() {
            orig.as_str()
        } else {
            binding_name
        };
        if let Some(d) = resolved.strip_prefix("dir:") {
            let hits = self.by_dir_name.get(&(d.to_string(), lookup.to_string()));
            return hits.filter(|h| h.len() == 1).map(|h| h[0].clone());
        }
        if let Some(d) = resolved.strip_prefix("pkg:") {
            let hits = self.by_pkg_name.get(&(d.to_string(), lookup.to_string()));
            return hits.filter(|h| h.len() == 1).map(|h| h[0].clone());
        }
        let prefix = dotted(resolved);
        let suffix = if !target.is_empty() && !orig.is_empty() && orig != "*" && orig != "default" {
            format!("{orig}.{target}")
        } else {
            lookup.to_string()
        };
        let base = suffix.rsplit('.').next().unwrap_or(&suffix);
        let want = format!("{prefix}.{suffix}");
        let qns: Vec<&String> = self
            .file_name(resolved, base)
            .iter()
            .filter(|q| **q == want)
            .collect();
        (qns.len() == 1).then(|| (qns[0].clone(), resolved.clone()))
    }

    /// `lexical`: nearest scope first; all hits so ambiguity stops lookup.
    fn lexical(&self, fp: &str, caller: &str, name: &str) -> Vec<String> {
        let prefix = dotted(fp);
        let mut scope = if caller.starts_with(&format!("{prefix}.")) {
            caller.to_string()
        } else {
            prefix.clone()
        };
        let skip_class = BARE_SKIP_CLASS_EXTS.iter().any(|e| fp.ends_with(e));
        let local = self.file_name(fp, name);
        loop {
            if !(skip_class && self.class_qns.contains(&scope)) {
                let want = format!("{scope}.{name}");
                let hits: Vec<String> = local.iter().filter(|q| **q == want).cloned().collect();
                if !hits.is_empty() {
                    return hits;
                }
            }
            if scope == prefix || !scope.starts_with(&format!("{prefix}.")) {
                return Vec::new();
            }
            scope = scope
                .rsplit_once('.')
                .map(|(h, _)| h.to_string())
                .unwrap_or_default();
        }
    }

    /// `typed_target`: (qn, file) of `type_name.base` as the caller's
    /// language finds it.
    fn typed_target(
        &self,
        fp: &str,
        type_name: &str,
        base: &str,
        caller: &str,
    ) -> Option<(String, String)> {
        let ext = ext_of(fp);
        let in_family = |dfp: &str| {
            let e = ext_of(dfp);
            if CPP_EXTS.contains(&ext) {
                CPP_EXTS.contains(&e)
            } else {
                e == ext
            }
        };
        let owned = |sites: Vec<(String, String)>| -> Option<(String, String)> {
            let suffix = format!(".{type_name}.{base}");
            single(
                sites
                    .into_iter()
                    .filter(|(qn, dfp)| {
                        in_family(dfp)
                            && qn.ends_with(&suffix)
                            && qn.starts_with(&format!("{}.", dotted(dfp)))
                    })
                    .collect(),
            )
        };
        if CPP_EXTS.contains(&ext) {
            if type_name == "std" {
                return None;
            }
            let visible = |sites: Vec<(String, String)>| -> Vec<(String, String)> {
                sites
                    .into_iter()
                    .filter(|(qn, _)| {
                        let head = &qn[..qn.len() - base.len() - 1];
                        let enclosing =
                            strip_trailing_template(head.rsplit_once('.').map_or("", |(h, _)| h));
                        !self.class_qns.contains(&enclosing)
                            || caller.starts_with(&format!("{enclosing}."))
                            || caller.starts_with(&format!("{enclosing}<"))
                    })
                    .collect()
            };
            let local: Vec<(String, String)> = self
                .file_name(fp, base)
                .iter()
                .map(|q| (q.clone(), fp.to_string()))
                .collect();
            if let Some(hit) = owned(visible(local)) {
                return Some(hit);
            }
            let global: Vec<(String, String)> = self
                .by_name_files
                .get(base)
                .into_iter()
                .flatten()
                .flat_map(|dfp| {
                    self.file_name(dfp, base)
                        .iter()
                        .map(move |q| (q.clone(), dfp.clone()))
                })
                .collect();
            return owned(visible(global));
        }
        let d = dir_of(fp);
        if ext == "go" || ext == "java" {
            let hit = owned(
                self.by_dir_name
                    .get(&(d.to_string(), base.to_string()))
                    .cloned()
                    .unwrap_or_default(),
            );
            if hit.is_some() || ext == "go" {
                return hit;
            }
        }
        if let Some((bound, _)) = self.binding(fp, type_name)
            && !bound.is_empty()
            && !bound.starts_with("dir:")
            && !bound.starts_with("pkg:")
        {
            return owned(
                self.file_name(bound, base)
                    .iter()
                    .map(|q| (q.clone(), bound.clone()))
                    .collect(),
            );
        }
        if ext == "rs" {
            let global: Vec<(String, String)> = self
                .by_name_files
                .get(base)
                .into_iter()
                .flatten()
                .flat_map(|dfp| {
                    self.file_name(dfp, base)
                        .iter()
                        .map(move |q| (q.clone(), dfp.clone()))
                })
                .collect();
            return owned(global);
        }
        None
    }

    /// `field_type`: the one declared type of `member` among visible rows.
    fn field_type(
        &self,
        fp: &str,
        rows: &[(String, String, String)],
        member: &str,
    ) -> Option<String> {
        let ext = ext_of(fp);
        let d = dir_of(fp);
        let kinds: HashSet<String> = rows
            .iter()
            .filter(|(mfp, m, _)| {
                let e = ext_of(mfp);
                let fam = if CPP_EXTS.contains(&ext) {
                    CPP_EXTS.contains(&e)
                } else {
                    e == ext
                };
                m == member && fam && (!(ext == "go" || ext == "java") || dir_of(mfp) == d)
            })
            .map(|(_, _, k)| k.clone())
            .collect();
        single(kinds)
    }

    fn receiver_owner(&self, fp: &str, caller: &str, receiver: &str) -> Option<String> {
        if matches!(receiver, "self" | "this" | "cls") {
            let prefix = format!("{}.", dotted(fp));
            let mut scope = caller.to_string();
            while scope.starts_with(&prefix) {
                if self.class_qns.contains(&scope) {
                    return Some(scope);
                }
                scope = scope
                    .rsplit_once('.')
                    .map(|(h, _)| h.to_string())
                    .unwrap_or_default();
            }
            return None;
        }
        let hits = self.lexical(fp, caller, receiver);
        (hits.len() == 1 && self.class_qns.contains(&hits[0])).then(|| hits[0].clone())
    }

    fn caller_owner(&self, fp: &str, caller: &str) -> String {
        let prefix = format!("{}.", dotted(fp));
        match caller.strip_prefix(&prefix) {
            Some(rest) => rest
                .rsplit_once('.')
                .map(|(h, _)| h.to_string())
                .unwrap_or_default(),
            None => String::new(),
        }
    }

    /// `evaluate`: the type a c14.4 expression denotes.
    fn evaluate(&self, fp: &str, caller: &str, expr: &Value, depth: usize) -> Option<String> {
        let list = expr.as_array().filter(|l| !l.is_empty())?;
        if depth > 8 {
            return None;
        }
        match list[0].as_str()? {
            "T" => list.get(1)?.as_str().map(String::from),
            "self" => {
                let owner = self.caller_owner(fp, caller);
                let short = owner.rsplit('.').next().unwrap_or("").to_string();
                (!short.is_empty()).then_some(short)
            }
            "field" => {
                let rows = if list.get(1) == Some(&serde_json::json!(["self"])) {
                    self.fields_by_owner
                        .get(&self.caller_owner(fp, caller))
                        .cloned()
                        .unwrap_or_default()
                } else {
                    let owner = self.evaluate(fp, caller, list.get(1)?, depth + 1)?;
                    if owner.starts_with("[]") {
                        return None;
                    }
                    self.fields_by_type.get(&owner).cloned().unwrap_or_default()
                };
                self.field_type(fp, &rows, list.get(2)?.as_str()?)
            }
            "elem" => {
                let inner = self.evaluate(fp, caller, list.get(1)?, depth + 1)?;
                if let Some(e) = inner.strip_prefix("[]") {
                    return Some(e.to_string());
                }
                self.field_type(
                    fp,
                    &self.fields_by_type.get(&inner).cloned().unwrap_or_default(),
                    "[]",
                )
            }
            "ret" => {
                let call = list.get(1)?.as_str()?;
                let hits: HashSet<(String, String)> = self
                    .resolve_call(fp, caller, call)
                    .into_iter()
                    .map(|(q, _, d)| (q, d))
                    .collect();
                let (qn, dfp) = single(hits)?;
                let local = &qn[dotted(&dfp).len() + 1..];
                let (owner, function) = local.rsplit_once('.').unwrap_or(("", local));
                let mut results = self
                    .declared_results
                    .get(&(dfp.clone(), owner.to_string(), function.to_string()))?
                    .clone();
                if list.get(3).and_then(Value::as_bool).unwrap_or(false) {
                    results = results.strip_prefix('?')?.to_string();
                } else if results.starts_with('?') {
                    return None;
                }
                let parts: Vec<&str> = results.split(',').collect();
                match list.get(2) {
                    Some(Value::Number(n)) => {
                        let i = n.as_u64()? as usize;
                        parts
                            .get(i)
                            .filter(|p| !p.is_empty())
                            .map(|p| p.to_string())
                    }
                    _ => (parts.len() == 1 && !parts[0].is_empty()).then(|| parts[0].to_string()),
                }
            }
            _ => None,
        }
    }

    fn package_class(&self, cls_file: &str, written: &str) -> Option<(String, String)> {
        let local = format!("{}.{written}", dotted(cls_file));
        if self.class_qns.contains(&local) {
            return Some((local, cls_file.to_string()));
        }
        let (head, tail) = written.split_once('.').unwrap_or((written, ""));
        self.bound_lookup(cls_file, head, tail)
            .filter(|(qn, _)| self.class_qns.contains(qn))
    }

    /// The first definition of `method` along a class's package bases, or
    /// Err(()) ("abstain") once a base is not a package class.
    fn super_target(
        &self,
        cls_qn: &str,
        cls_file: &str,
        method: &str,
        depth: usize,
    ) -> Result<Option<(String, String)>, ()> {
        if depth > 8 {
            return Err(());
        }
        let owner = &cls_qn[dotted(cls_file).len() + 1..];
        let mut bases: Vec<(usize, String)> = self
            .fields_by_owner
            .get(owner)
            .into_iter()
            .flatten()
            .filter(|(mfp, member, _)| mfp == cls_file && member.starts_with("(base)"))
            .filter_map(|(_, member, kind)| {
                member["(base)".len()..]
                    .parse::<usize>()
                    .ok()
                    .map(|n| (n, kind.clone()))
            })
            .collect();
        bases.sort_by_key(|(n, _)| *n);
        for (_, written) in bases {
            let Some(base) = self.package_class(cls_file, &written) else {
                return Err(());
            };
            let want = format!("{}.{method}", base.0);
            if let Some(defined) = self.file_name(&base.1, method).iter().find(|q| **q == want) {
                return Ok(Some((defined.clone(), base.1)));
            }
            match self.super_target(&base.0, &base.1, method, depth + 1) {
                Ok(None) => {}
                found => return found,
            }
        }
        Ok(None)
    }

    /// Targets of one call, every tier in order (Cortex `resolve_call`).
    fn resolve_call(&self, fp: &str, caller: &str, name: &str) -> Vec<Target> {
        let mut out = Vec::new();
        if let Some(method) = name.strip_prefix("(super).") {
            let cls = caller.rsplit_once('.').map_or("", |(h, _)| h);
            if self.class_qns.contains(cls)
                && let Ok(Some((qn, dfp))) = self.super_target(cls, fp, method, 0)
            {
                out.push((qn, 0.90, dfp));
            }
            return out;
        }
        if name.starts_with("(expr:") {
            let Some(close) = name.rfind(").") else {
                return out;
            };
            let Ok(expr) = serde_json::from_str::<Value>(&name["(expr:".len()..close]) else {
                return out;
            };
            if let Some(declared) = self.evaluate(fp, caller, &expr, 0)
                && !declared.starts_with("[]")
                && declared != "std"
                && let Some((qn, dfp)) =
                    self.typed_target(fp, &declared, &name[close + 2..], caller)
                && !self.rows.abstract_qns.contains(&qn)
            {
                out.push((qn, 0.90, dfp));
            }
            return out;
        }
        let mut name = name.to_string();
        if name.starts_with("(thisfield:")
            || name.starts_with("(typefield:")
            || name.starts_with("(implicitfield:")
        {
            let close = name.find(").").expect("marker");
            let (kind, arg) = name[1..close]
                .split_once(':')
                .unwrap_or((&name[1..close], ""));
            let (kind, arg) = (kind.to_string(), arg.to_string());
            let base = name[close + 2..].to_string();
            let prefix = format!("{}.", dotted(fp));
            let declared = if kind == "typefield" {
                let (owner_type, field) = arg.rsplit_once('.').unwrap_or(("", &arg));
                self.field_type(
                    fp,
                    &self
                        .fields_by_type
                        .get(owner_type)
                        .cloned()
                        .unwrap_or_default(),
                    field,
                )
            } else {
                let owner = match caller.strip_prefix(&prefix) {
                    Some(rest) => rest
                        .rsplit_once('.')
                        .map(|(h, _)| h.to_string())
                        .unwrap_or_default(),
                    None => String::new(),
                };
                if owner.is_empty() {
                    None
                } else {
                    self.field_type(
                        fp,
                        &self
                            .fields_by_owner
                            .get(&owner)
                            .cloned()
                            .unwrap_or_default(),
                        &arg,
                    )
                }
            };
            if let Some(declared) = declared {
                if declared != "std"
                    && let Some((qn, dfp)) = self.typed_target(fp, &declared, &base, caller)
                    && !self.rows.abstract_qns.contains(&qn)
                {
                    out.push((qn, 0.90, dfp));
                }
                return out;
            }
            if CPP_EXTS.contains(&ext_of(fp)) {
                name = format!("(member).{base}");
            } else if kind == "implicitfield" {
                name = format!("{arg}.{base}");
            } else {
                return out;
            }
        }
        let member = name.starts_with("(member).");
        if member {
            name = name["(member).".len()..].to_string();
            if CPP_STD_METHOD_NAMES.contains(&name.as_str()) {
                return out;
            }
        }
        if let Some((qual, base)) = name.rsplit_once('.') {
            let (qual, base) = (qual.to_string(), base.to_string());
            if qual.starts_with("(scope:") && qual.ends_with(')') {
                let scope = &qual["(scope:".len()..qual.len() - 1];
                let suffix = format!(".{scope}.{base}");
                if scope != "std" {
                    let scoped = |sites: Vec<(String, String)>| {
                        single(
                            sites
                                .into_iter()
                                .filter(|(q, _)| q.ends_with(&suffix))
                                .collect(),
                        )
                    };
                    let local: Vec<(String, String)> = self
                        .file_name(fp, &base)
                        .iter()
                        .map(|q| (q.clone(), fp.to_string()))
                        .collect();
                    let hit = scoped(local).or_else(|| {
                        scoped(
                            self.by_name_files
                                .get(&base)
                                .into_iter()
                                .flatten()
                                .flat_map(|dfp| {
                                    self.file_name(dfp, &base)
                                        .iter()
                                        .map(move |q| (q.clone(), dfp.clone()))
                                })
                                .collect(),
                        )
                    });
                    if let Some((qn, dfp)) = hit
                        && !self.rows.abstract_qns.contains(&qn)
                    {
                        out.push((qn, 0.90, dfp));
                    }
                }
                return out;
            }
            if (qual.starts_with("(receiver:") || qual.starts_with("(type:")) && qual.ends_with(')')
            {
                let type_name = &qual[qual.find(':').unwrap() + 1..qual.len() - 1];
                if let Some((qn, dfp)) = self.typed_target(fp, type_name, &base, caller)
                    && !self.rows.abstract_qns.contains(&qn)
                {
                    out.push((qn, 0.90, dfp));
                }
                return out;
            }
            let instance = qual.starts_with("(instance:") && qual.ends_with(')');
            let qual = if instance {
                qual["(instance:".len()..qual.len() - 1].to_string()
            } else {
                qual
            };
            if let Some(owner) = self.receiver_owner(fp, caller, &qual) {
                let want = format!("{owner}.{base}");
                let hits: Vec<&String> = self
                    .file_name(fp, &base)
                    .iter()
                    .filter(|q| **q == want)
                    .collect();
                if hits.len() == 1 {
                    out.push((
                        hits[0].clone(),
                        if instance { 0.70 } else { 0.90 },
                        fp.to_string(),
                    ));
                }
                return out;
            }
            let (qualifier, tail) = qual.split_once('.').unwrap_or((&qual, ""));
            if !self.lexical(fp, caller, qualifier).is_empty() {
                return out;
            }
            let target = if tail.is_empty() {
                base.clone()
            } else {
                format!("{tail}.{base}")
            };
            if let Some((qn, dfp)) = self.bound_lookup(fp, qualifier, &target) {
                let parent = qn.rsplit_once('.').map_or("", |(h, _)| h);
                if !instance || self.class_qns.contains(parent) {
                    out.push((qn, if instance { 0.70 } else { 0.95 }, dfp));
                }
            } else if fp.ends_with(".java")
                && !instance
                && tail.is_empty()
                && first_is_upper(qualifier)
            {
                let d = dir_of(fp);
                let hits: HashSet<(String, String)> = self
                    .by_dir_name
                    .get(&(d.to_string(), base.clone()))
                    .into_iter()
                    .flatten()
                    .filter(|(qn, dfp)| {
                        dfp.ends_with(".java")
                            && *qn == format!("{}.{qualifier}.{base}", dotted(dfp))
                            && self
                                .class_qns
                                .contains(qn.rsplit_once('.').map_or("", |(h, _)| h))
                    })
                    .cloned()
                    .collect();
                if let Some((qn, dfp)) = single(hits) {
                    out.push((qn, 0.90, dfp));
                }
            }
            return out;
        }
        let local = if member {
            Vec::new()
        } else {
            self.lexical(fp, caller, &name)
        };
        if !local.is_empty() {
            if local.len() == 1 {
                out.push((local[0].clone(), 0.90, fp.to_string()));
            }
            return out;
        }
        if self.binding(fp, &name).is_some()
            && let Some((qn, dfp)) = self.bound_lookup(fp, &name, "")
        {
            out.push((qn, 0.95, dfp));
            return out;
        }
        if RUNTIME_METHOD_NAMES.contains(&name.as_str()) {
            return out;
        }
        let def_files: Vec<&String> = self
            .by_name_files
            .get(&name)
            .into_iter()
            .flatten()
            .filter(|f| *f != fp)
            .collect();
        if def_files.len() == 1 {
            let dfp = def_files[0];
            out.push((self.file_name(dfp, &name)[0].clone(), 0.60, dfp.clone()));
        } else if (2..=3).contains(&def_files.len()) {
            for dfp in def_files {
                out.push((self.file_name(dfp, &name)[0].clone(), 0.35, dfp.clone()));
            }
        }
        out
    }
}

/// Every resolved edge in the order Cortex appends them, duplicates
/// included: replaying this list through INSERT OR REPLACE reproduces
/// Cortex's edges table row for row, rowids included.
pub fn resolve_all(rows: &Rows) -> Vec<Edge> {
    let r = Resolver::new(rows);
    let mut out = Vec::new();
    for (fp, caller, name, _) in &rows.calls {
        for (qn, confidence, dst_file) in r.resolve_call(fp, caller, name) {
            out.push(Edge {
                src: caller.clone(),
                dst: qn,
                confidence,
                src_file: fp.clone(),
                dst_file,
            });
        }
    }
    out
}

/// Cortex `rebuild_call_edges`: every CALLS edge, one per (caller, target).
/// A later duplicate replaces an earlier one, as INSERT OR REPLACE does.
pub fn rebuild_call_edges(rows: &Rows) -> Vec<Edge> {
    let mut edges: indexmap_lite::OrderedMap = indexmap_lite::OrderedMap::default();
    for e in resolve_all(rows) {
        edges.insert((e.src.clone(), e.dst.clone()), e);
    }
    edges.into_values()
}

mod indexmap_lite {
    //! A tiny insertion-ordered map with replace-in-place, enough for the
    //! INSERT OR REPLACE semantics above without another dependency.
    use super::Edge;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct OrderedMap {
        index: HashMap<(String, String), usize>,
        values: Vec<Edge>,
    }

    impl OrderedMap {
        pub fn insert(&mut self, key: (String, String), value: Edge) {
            match self.index.get(&key) {
                Some(&i) => self.values[i] = value,
                None => {
                    self.index.insert(key, self.values.len());
                    self.values.push(value);
                }
            }
        }

        pub fn into_values(self) -> Vec<Edge> {
            self.values
        }
    }
}
