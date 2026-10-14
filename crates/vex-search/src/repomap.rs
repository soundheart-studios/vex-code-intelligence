//! Repo map: personalized PageRank over the file reference graph, packed to
//! a token budget (port of Cortex's rank/repomap.py, itself after aider's).
//!
//! Files are nodes and a REFS/IMPORTS/CALLS edge from referencer to definer
//! is a weighted vote; focus hints bias the teleport toward matching files.
//! Within a file, symbols rank by incoming REFS. Float operations follow
//! Cortex's evaluation order exactly so ranks, and with them ties, agree
//! bit for bit.

use rusqlite::Connection;
use std::collections::HashMap;

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 30;
pub const DEFAULT_MAP_TOKEN_BUDGET: i64 = 800;
pub const MAX_MAP_FILES: usize = 12;
const EMPTY: &str = "(index empty \u{2014} run reindex first)";

/// Weighted personalized PageRank via power iteration.
pub fn pagerank(
    nodes: &[String],
    edges: &[(String, String, f64)],
    personalization: Option<&HashMap<String, f64>>,
) -> HashMap<String, f64> {
    if nodes.is_empty() {
        return HashMap::new();
    }
    let mut idx: HashMap<&str, usize> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        idx.insert(node, i);
    }
    let n = nodes.len();
    let mut out_weight = vec![0.0f64; n];
    let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for (src, dst, w) in edges {
        if let (Some(&s), Some(&d)) = (idx.get(src.as_str()), idx.get(dst.as_str()))
            && src != dst
        {
            adj[s].push((d, *w));
            out_weight[s] += w;
        }
    }
    let base: Vec<f64> = match personalization.filter(|p| !p.is_empty()) {
        Some(p) => {
            // Cortex sums the dict in insertion order; every value is 1.0,
            // so the sum is the exact count whatever the order.
            let sum: f64 = p.values().sum();
            let total = if sum == 0.0 { 1.0 } else { sum };
            nodes
                .iter()
                .map(|node| p.get(node).copied().unwrap_or(0.0) / total)
                .map(|b| 0.8 * b + 0.2 / n as f64)
                .collect()
        }
        None => vec![1.0 / n as f64; n],
    };
    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..ITERATIONS {
        let mut nxt = vec![0.0f64; n];
        let mut dangling = 0.0f64;
        for i in 0..n {
            if out_weight[i] > 0.0 {
                let share = rank[i] / out_weight[i];
                for &(j, w) in &adj[i] {
                    nxt[j] += DAMPING * share * w;
                }
            } else {
                dangling += rank[i];
            }
        }
        for i in 0..n {
            nxt[i] += (1.0 - DAMPING + DAMPING * dangling) * base[i];
        }
        rank = nxt;
    }
    nodes.iter().cloned().zip(rank).collect()
}

/// The map text and the files it includes, bounded by both the token
/// budget and [`MAX_MAP_FILES`] (deliberately not caller-configurable).
pub fn repo_map(
    conn: &Connection,
    budget_tokens: i64,
    focus: &[String],
) -> rusqlite::Result<(String, Vec<String>)> {
    let files: Vec<String> = conn
        .prepare("SELECT path FROM files")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let edges: Vec<(String, String, f64)> = conn
        .prepare(
            "SELECT src_file, dst_file, confidence FROM edges \
             WHERE type IN ('REFS', 'IMPORTS', 'CALLS') AND dst_file != ''",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let symbols: Vec<(String, String, String, String, i64)> = conn
        .prepare("SELECT qualified_name, name, kind, file_path, start_line FROM symbols")?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    let indeg: HashMap<String, i64> = conn
        .prepare("SELECT dst, COUNT(*) FROM edges WHERE type = 'REFS' GROUP BY dst")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;

    let personalization: Option<HashMap<String, f64>> = (!focus.is_empty()).then(|| {
        files
            .iter()
            .filter(|f| focus.iter().any(|hint| f.contains(hint.as_str())))
            .map(|f| (f.clone(), 1.0))
            .collect()
    });
    let ranks = pagerank(&files, &edges, personalization.as_ref());

    let mut by_file: HashMap<&str, Vec<(i64, &str, &str, i64)>> = HashMap::new();
    for (qn, name, kind, fp, line) in &symbols {
        by_file.entry(fp).or_default().push((
            indeg.get(qn).copied().unwrap_or(0),
            name,
            kind,
            *line,
        ));
    }

    let mut order: Vec<&String> = files.iter().collect();
    let rank_of = |f: &str| ranks.get(f).copied().unwrap_or(0.0);
    // Stable, so equal ranks keep the files table's order, as Python's
    // sorted(reverse=True) does.
    order.sort_by(|a, b| rank_of(b).partial_cmp(&rank_of(a)).unwrap());

    let mut lines: Vec<String> = Vec::new();
    let mut included: Vec<String> = Vec::new();
    let mut used = 0i64;
    for fp in order {
        if included.len() >= MAX_MAP_FILES {
            break;
        }
        let mut syms = by_file.get(fp.as_str()).cloned().unwrap_or_default();
        syms.sort_by(|a, b| b.cmp(a));
        syms.truncate(8);
        let mut entry = vec![format!("{fp}:")];
        for (deg, name, kind, line) in syms {
            let kind: String = kind.chars().take(2).collect();
            let refs = if deg != 0 {
                format!("  <-{deg} refs")
            } else {
                String::new()
            };
            entry.push(format!("  {kind} {name}{refs}  L{line}"));
        }
        let cost: i64 = entry
            .iter()
            .map(|l| (l.chars().count() as i64 / 3).max(1))
            .sum();
        if used + cost > budget_tokens {
            break;
        }
        lines.extend(entry);
        included.push(fp.clone());
        used += cost;
    }
    let text = if lines.is_empty() {
        EMPTY.to_string()
    } else {
        lines.join("\n")
    };
    Ok((text, included))
}
