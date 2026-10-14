//! Symbol lookup shared by the query commands (Cortex
//! `application/symbols.py` resolve/at_span, `store/graph.py` find_symbol).

use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
    /// A bodiless declaration (a prototype, an @interface method).
    pub declaration: bool,
}

fn symbol(r: &rusqlite::Row) -> rusqlite::Result<Symbol> {
    Ok(Symbol {
        qualified_name: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        file_path: r.get(3)?,
        start_line: r.get(4)?,
        end_line: r.get(5)?,
        declaration: false,
    })
}

fn rows(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<Symbol>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let out = stmt
        .query_map(params, symbol)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// Exact name or qualified name (`GraphStore.find_symbol`).
pub fn find_symbol(conn: &Connection, name: &str) -> rusqlite::Result<Vec<Symbol>> {
    rows(
        conn,
        "SELECT * FROM symbols WHERE name = ?1 OR qualified_name = ?1 LIMIT 25",
        &[&name],
    )
}

/// Exact match, then for a dotted target a case-insensitive suffix match on
/// the qualified name, else the bare name in any case; shortest first.
pub fn resolve(conn: &Connection, target: &str) -> rusqlite::Result<Vec<Symbol>> {
    let exact = find_symbol(conn, target)?;
    if !exact.is_empty() {
        return Ok(exact);
    }
    if target.contains('.') {
        let esc = target
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        rows(
            conn,
            r"SELECT * FROM symbols WHERE qualified_name LIKE '%.' || ?1 ESCAPE '\' ORDER BY LENGTH(qualified_name) LIMIT 10",
            &[&esc],
        )
    } else {
        rows(
            conn,
            "SELECT * FROM symbols WHERE name = ?1 COLLATE NOCASE ORDER BY LENGTH(qualified_name) LIMIT 10",
            &[&target],
        )
    }
}

/// The innermost symbol site enclosing the span, else the last one
/// overlapping it.
pub fn at_span(
    conn: &Connection,
    file_path: &str,
    start: i64,
    end: i64,
) -> rusqlite::Result<Option<Symbol>> {
    let site = |r: &rusqlite::Row| -> rusqlite::Result<Symbol> {
        let mut s = symbol(r)?;
        s.declaration = r.get::<_, i64>(6)? != 0;
        Ok(s)
    };
    let enclosing = conn
        .prepare_cached(
            "SELECT * FROM symbol_sites WHERE file_path = ?1 AND start_line <= ?2 AND end_line >= ?3 \
             ORDER BY (end_line - start_line) ASC LIMIT 1",
        )?
        .query_row(rusqlite::params![file_path, start, end], site)
        .optional()?;
    if enclosing.is_some() {
        return Ok(enclosing);
    }
    conn.prepare_cached(
        "SELECT * FROM symbol_sites WHERE file_path = ?1 AND start_line <= ?2 AND end_line >= ?3 \
         ORDER BY start_line DESC LIMIT 1",
    )?
    .query_row(rusqlite::params![file_path, end, start], site)
    .optional()
}

/// Indexed symbol names containing `token`, shortest first.
pub fn near_symbols(conn: &Connection, token: &str, limit: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name FROM symbols WHERE name LIKE ?1 AND name <> ?2 GROUP BY name ORDER BY LENGTH(name) LIMIT ?3",
    )?;
    let out = stmt
        .query_map(rusqlite::params![format!("%{token}%"), token, limit], |r| {
            r.get(0)
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(out)
}
