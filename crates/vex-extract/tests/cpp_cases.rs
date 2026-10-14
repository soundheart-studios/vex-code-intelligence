//! Cortex's C++ edge-case tests, translated: tests/test_cpp_annotations.py
//! and the test_cpp_* tests of test_call_receivers.py and
//! test_call_fields.py (pinned revision), with the same sources and the
//! same expectations. Resolution runs vex-resolve over rows built from vex's
//! own extraction, as Cortex's fixtures build them from chunk_file.

use std::collections::{BTreeSet, HashMap, HashSet};
use vex_extract::engine::chunk_bytes;
use vex_extract::lang::cpp::mask_annotations;
use vex_extract::language::{HEAD_BYTES, language_of};

fn parse(rel: &str, source: &str) -> vex_core::Parsed {
    let bytes = source.as_bytes();
    let lang = language_of(rel, Some(&bytes[..bytes.len().min(HEAD_BYTES)]));
    chunk_bytes(rel, lang, bytes)
}

// ---- test_cpp_annotations.py ----------------------------------------------

#[test]
fn exported_class_keeps_real_owner_and_original_source() {
    for annotation in ["BASE_EXPORT", "URL_IPC_EXPORT", "COMPONENT_EXPORT(URL)"] {
        let source = format!(
            "// π keeps byte offsets honest\nclass {annotation} Widget {{\n public:\n int value() {{ return helper(); }}\n}};\n"
        );
        let result = parse("sample.cc", &source);
        let symbols: Vec<(String, usize, usize)> = result
            .symbols
            .iter()
            .map(|s| (s.name.clone(), s.start_line, s.end_line))
            .collect();
        assert_eq!(
            symbols,
            [("Widget".to_string(), 2, 5), ("value".to_string(), 4, 4)],
            "{annotation}"
        );
        assert_eq!(
            result
                .calls
                .get(&("sample.cc.Widget.value".to_string(), "helper".to_string())),
            Some(&1),
            "{annotation}"
        );
        let lines: Vec<&str> = source.lines().collect();
        for c in &result.chunks {
            assert_eq!(c.content, lines[c.start_line - 1..c.end_line].join("\n"));
        }
        assert!(result.chunks.iter().any(|c| c.content.contains(annotation)));
    }
}

#[test]
fn lifetime_annotation_does_not_become_method_name() {
    for annotation in ["LIFETIME_BOUND", "LIFETIME_CAPTURE_BY_THIS"] {
        let source = format!(
            "class Widget {{\n const char* get() const {annotation} {{ return helper(); }}\n}};\n"
        );
        let result = parse("sample.cc", &source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Widget", "get"], "{annotation}");
        assert_eq!(
            result
                .calls
                .get(&("sample.cc.Widget.get".to_string(), "helper".to_string())),
            Some(&1)
        );
    }
}

#[test]
fn annotation_mask_is_lexical_and_constrained() {
    let source: &[u8] = b"#define MAKE class BASE_EXPORT Generated {};
#define MULTI \\
 class BASE_EXPORT Generated {};
// class BASE_EXPORT Comment {};
/* class COMPONENT_EXPORT(URL) Comment {}; */
const char* text = \"class BASE_EXPORT String {};\";
const char* raw = R\"tag(class BASE_EXPORT Raw {}; \" //)tag\";
class BASE_EXPORT {};
void LIFETIME_BOUND() {}
int BASE_EXPORT = 3;
UNKNOWN(class BASE_EXPORT Generated {});
class UNKNOWN_EXPORT {};
class UNKNOWN_EXPORT Widget;
class COMPONENT_EXPORT(make_code()) Widget {};
";
    assert_eq!(mask_annotations(source), source);
    let annotated: &[u8] =
        b"class COMPONENT_EXPORT(\n URL\n) Widget {\n int get() const LIFETIME_BOUND {return 1;}\n};";
    let masked = mask_annotations(annotated);
    assert_eq!(masked.len(), annotated.len());
    let newlines = |b: &[u8]| -> Vec<usize> {
        b.iter()
            .enumerate()
            .filter(|(_, c)| matches!(**c, b'\n' | b'\r'))
            .map(|(i, _)| i)
            .collect()
    };
    assert_eq!(newlines(&masked), newlines(annotated));
    let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    assert!(!contains(&masked, b"COMPONENT_EXPORT") && !contains(&masked, b"LIFETIME_BOUND"));
    // c13.4: any macro of the *_EXPORT convention between `class` and a
    // class name that opens a body or base list.
    for declaration in [
        &b"class LEVELDB_EXPORT Status {"[..],
        b"struct GPU_EXPORT_PRIVATE Opts : Base {",
        b"class CONTENT_EXPORT Host final {",
    ] {
        let masked = mask_annotations(declaration);
        assert!(masked.len() == declaration.len() && !contains(&masked, b"_EXPORT"));
    }
}

#[test]
fn remaining_candidates_are_not_all_definition_misses() {
    // tests/fixtures/cpp_annotations/categories.json
    for (category, source, symbols) in [
        (
            "annotation_artifact",
            "class BASE_EXPORT Actual { int run() { return 1; } };",
            &["Actual", "run"][..],
        ),
        (
            "elaborated_tag",
            "using Durable = Wrapper<class DurableTag>;",
            &[],
        ),
        (
            "forward_typedef",
            "typedef struct UConverter UConverter;",
            &[],
        ),
        (
            "explicit_instantiation",
            "template class EXPORT_TEMPLATE_DEFINE(COMPONENT_EXPORT(URL)) CanonOutputT<char>;",
            &[],
        ),
    ] {
        let result = parse("sample.cc", source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, symbols, "{category}");
    }
}

// ---- resolution (test_call_resolution_edges.resolve / test_call_fields.index)

/// Edges `{caller: {callee}}` over vex's extraction of `sources`. With
/// `full`, member rows and bodiless sites are included as the pipeline
/// stores them (test_call_fields' `index`); without, only files, symbols,
/// bindings and calls (test_call_resolution_edges' `resolve`).
fn resolve(sources: &[(&str, &str)], full: bool) -> HashMap<String, HashSet<String>> {
    let mut rows = vex_resolve::Rows::default();
    let mut sites: HashMap<String, Vec<bool>> = HashMap::new();
    let mut seen_qns = HashSet::new();
    for (rel, source) in sources {
        let parsed = parse(rel, source);
        rows.files.insert(rel.to_string());
        for s in &parsed.symbols {
            sites
                .entry(s.qualified_name.clone())
                .or_default()
                .push(s.declaration);
            if !full || seen_qns.insert(s.qualified_name.clone()) {
                rows.symbols.push((
                    s.qualified_name.clone(),
                    s.name.clone(),
                    s.kind.clone(),
                    rel.to_string(),
                ));
            }
        }
        for (local, (module, original)) in &parsed.bindings {
            rows.bindings.push((
                rel.to_string(),
                local.clone(),
                module.clone(),
                original.clone(),
            ));
        }
        for ((caller, name), count) in &parsed.calls {
            rows.calls
                .push((rel.to_string(), caller.clone(), name.clone(), *count as i64));
        }
        if full {
            for (owner, member, ty) in &parsed.members {
                rows.members
                    .push((rel.to_string(), owner.clone(), member.clone(), ty.clone()));
            }
        }
    }
    if full {
        rows.abstract_qns = sites
            .into_iter()
            .filter(|(_, d)| d.iter().all(|x| *x))
            .map(|(qn, _)| qn)
            .collect();
    }
    let mut edges: HashMap<String, HashSet<String>> = HashMap::new();
    for e in vex_resolve::resolve_all(&rows) {
        edges.entry(e.src).or_default().insert(e.dst);
    }
    edges
}

fn targets(edges: &HashMap<String, HashSet<String>>, caller: &str) -> BTreeSet<String> {
    edges
        .get(caller)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

const RECEIVERS: &[(&str, &str)] = &[
    (
        "db/iterator.h",
        "namespace leveldb {\nclass Iterator {\n public:\n  virtual Slice key() const = 0;\n};\n}\n",
    ),
    (
        "db/block.cc",
        "namespace leveldb {\nclass Block {\n public:\n  size_t size() const { return size_; }\n  Slice key() const { return Slice(); }\n};\nvoid Block::Reset() {}\n}\n",
    ),
    (
        "db/table.cc",
        "namespace leveldb {\nclass Table {\n public:\n  size_t size() const { return 0; }\n};\n}\n",
    ),
    (
        "db/version.h",
        "namespace leveldb {\nclass VersionSet {\n public:\n  int LogNumber() const;\n};\n}\n",
    ),
    (
        "db/version.cc",
        "namespace leveldb {\nint VersionSet::LogNumber() const { return 1; }\n}\n",
    ),
    (
        "db/db_iter.cc",
        "#include <vector>\nnamespace leveldb {\nclass DBIter {\n public:\n  Slice key() const {\n    return iter_->key();\n  }\n  size_t Count() const {\n    Block block;\n    Block* other = nullptr;\n    other->Reset();\n    files_.size();\n    return block.size() + this->key().size();\n  }\n  int Log() { return versions_->LogNumber(); }\n  size_t Files() const { return files_.size(); }\n private:\n  Iterator* const iter_;\n  std::vector<int> files_;\n};\n}\n",
    ),
];

#[test]
fn cpp_member_calls_through_other_types_are_not_this_calls() {
    let edges = resolve(RECEIVERS, false);
    // `iter_` is an Iterator*: DBIter::key does not call itself.
    assert_eq!(
        targets(&edges, "db.db_iter.cc.leveldb.DBIter.key"),
        set(&[])
    );
}

#[test]
fn cpp_member_calls_on_std_types_yield_no_edge() {
    let edges = resolve(RECEIVERS, false);
    let count = targets(&edges, "db.db_iter.cc.leveldb.DBIter.Count");
    // `files_.size()` is std::vector::size, not Block's or Table's.
    assert_eq!(
        targets(&edges, "db.db_iter.cc.leveldb.DBIter.Files"),
        set(&[])
    );
    // Only `block.size()` is a package size(), and it is Block's.
    assert_eq!(
        count,
        set(&[
            "db.block.cc.leveldb.Block.size",
            "db.block.cc.leveldb.Block.Reset",
            "db.db_iter.cc.leveldb.DBIter.key"
        ])
    );
}

#[test]
fn cpp_member_calls_whose_receiver_is_the_candidates_class_resolve() {
    let edges = resolve(RECEIVERS, false);
    let count = targets(&edges, "db.db_iter.cc.leveldb.DBIter.Count");
    assert!(count.contains("db.block.cc.leveldb.Block.size")); // typed local
    assert!(!count.contains("db.table.cc.leveldb.Table.size"));
    assert!(count.contains("db.block.cc.leveldb.Block.Reset")); // typed pointer
    // A header field of unknown type keeps the old name match.
    assert_eq!(
        targets(&edges, "db.db_iter.cc.leveldb.DBIter.Log"),
        set(&["db.version.cc.leveldb.VersionSet.LogNumber"])
    );
}

#[test]
fn cpp_a_type_defined_in_the_callers_file_wins() {
    // leveldb defines a function-local `struct LogReporter` in three files.
    let local = |f: &str| {
        format!(
            "namespace leveldb {{\nvoid {f}() {{\n  struct LogReporter {{ void Corruption(int n) {{}} }};\n  LogReporter reporter;\n  reporter.Corruption(1);\n}}\n}}\n"
        )
    };
    let (repair, recover) = (local("Repair"), local("Recover"));
    let edges = resolve(
        &[("db/repair.cc", &repair), ("db/version_set.cc", &recover)],
        false,
    );
    assert_eq!(
        targets(&edges, "db.repair.cc.leveldb.Repair"),
        set(&["db.repair.cc.leveldb.Repair.LogReporter.Corruption"])
    );
}

const SCOPED: &[(&str, &str)] = &[
    (
        "include/status.h",
        "namespace leveldb {\nclass Status {\n public:\n  static Status OK() { return Status(); }\n  static Status Corruption(int msg) { return Status(); }\n};\n}\n",
    ),
    (
        "db/db.cc",
        "namespace leveldb {\nclass DB {\n public:\n  virtual Status Delete(int key);\n};\nStatus DB::Delete(int key) { return Status::OK(); }\nnamespace log {\nint Encode(int x) { return x; }\n}\nclass VersionSet {\n public:\n  class Builder {\n   public:\n    void Apply(int e);\n  };\n};\nvoid VersionSet::Builder::Apply(int e) {}\n}\n",
    ),
    (
        "db/db_impl.cc",
        "namespace leveldb {\nclass DBImpl : public DB {\n public:\n  Status Delete(int key) override;\n};\nstruct Reporter { void Corruption(int n) {} };\nStatus DBImpl::Delete(int key) {\n  log::Encode(key);\n  VersionSet::Builder::Apply(key);\n  std::max(1, 2);\n  Unknown::Thing(key);\n  Status::Corruption(key);\n  return DB::Delete(key);\n}\n}\n",
    ),
];

#[test]
fn cpp_scope_qualified_calls_resolve_through_their_scope() {
    let edges = resolve(SCOPED, false);
    assert_eq!(
        targets(&edges, "db.db.cc.leveldb.DB.Delete"),
        set(&["include.status.h.leveldb.Status.OK"])
    );
    assert_eq!(
        targets(&edges, "db.db_impl.cc.leveldb.DBImpl.Delete"),
        set(&[
            "db.db.cc.leveldb.DB.Delete", // explicit base call, not itself
            "db.db.cc.leveldb.log.Encode",
            "db.db.cc.leveldb.VersionSet.Builder.Apply",
            "include.status.h.leveldb.Status.Corruption", // not Reporter::Corruption
        ])
    );
}

#[test]
fn cpp_scope_qualified_calls_to_std_or_unknown_scopes_yield_no_edge() {
    let util =
        "namespace leveldb {\nint max(int a, int b) { return a; }\nvoid Thing(int k) {}\n}\n";
    let mut sources = SCOPED.to_vec();
    sources.push(("db/util.cc", util));
    let edges = resolve(&sources, false);
    let impl_ = targets(&edges, "db.db_impl.cc.leveldb.DBImpl.Delete");
    assert!(!impl_.contains("db.util.cc.leveldb.max")); // std::max
    assert!(!impl_.contains("db.util.cc.leveldb.Thing")); // Unknown::Thing
}

#[test]
fn cpp_calls_through_fields_declared_in_a_header_resolve() {
    let sources: &[(&str, &str)] = &[
        (
            "db/comparator.h",
            "namespace leveldb {\nclass Comparator {\n public:\n  virtual int Compare(int a, int b) const = 0;\n};\n}\n",
        ),
        (
            "db/version_set.h",
            "namespace leveldb {\nclass VersionSet {\n public:\n  int LogNumber() const;\n  void Mark();\n};\n}\n",
        ),
        (
            "db/version_set.cc",
            "namespace leveldb {\nint VersionSet::LogNumber() const { return 1; }\nvoid VersionSet::Mark() {}\n}\n",
        ),
        (
            "db/memtable.cc",
            "namespace leveldb {\nclass MemTable {\n public:\n  int LogNumber() const { return 2; }\n  int Compare(int a, int b) const { return 0; }\n  size_t size() const { return 0; }\n};\n}\n",
        ),
        (
            "db/db_impl.h",
            "#include <vector>\nnamespace leveldb {\nclass DBImpl {\n public:\n  void Run();\n private:\n  VersionSet* const versions_;\n  const Comparator* user_comparator_;\n  std::vector<int> files_;\n};\n}\n",
        ),
        (
            "db/db_impl.cc",
            "namespace leveldb {\nvoid DBImpl::Run() {\n  versions_->LogNumber();\n  this->versions_->Mark();\n  user_comparator_->Compare(1, 2);\n  files_.size();\n}\n}\n",
        ),
    ];
    let edges = resolve(sources, true);
    // LogNumber and Compare also exist on MemTable: only the field's type counts.
    assert_eq!(
        targets(&edges, "db.db_impl.cc.leveldb.DBImpl.Run"),
        set(&[
            "db.version_set.cc.leveldb.VersionSet.LogNumber",
            "db.version_set.cc.leveldb.VersionSet.Mark"
        ])
    );
}

#[test]
fn annotation_masking_matches_cortex_byte_for_byte() {
    // fixtures/cpp_mask: Cortex's mask_annotations on crafted inputs.
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cpp_mask/cases.json");
    let cases: Vec<(String, String)> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for (source, want) in cases {
        let got = String::from_utf8(mask_annotations(source.as_bytes())).unwrap();
        assert_eq!(got, want, "mask_annotations({source:?})");
    }
}
