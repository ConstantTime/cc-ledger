//! Panel — *Top sessions by token burn*.
//!
//! Question: which sessions burned through the most tokens in this window?
//! Source: `agent_turns` × `sessions`, grouped by session, sorted by total
//! tokens.
//!
//! No bare UUIDs — the row identity is `DATE / REPO / MODEL` plus volumes.
//! If you really need the session_id, `sqlite3 ~/.cc-ledger/ledger.db` is
//! one query away.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use comfy_table::{Cell, CellAlignment};
use rusqlite::{params, Connection};

use super::{
    palette::{cell, cost_tier, fmt_tokens, fmt_usd, ColorTier},
    render::{new_table, print_or_no_data},
};

#[derive(Debug)]
pub(super) struct Row {
    pub day: String,
    pub repo: String,
    pub model: String,
    pub turns: i64,
    pub tokens: i64,
    pub cost: f64,
}

pub(super) fn compute(conn: &Connection, since_ms: Option<i64>, limit: i64) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT
           strftime('%Y-%m-%d', MIN(a.started_at_ms) / 1000, 'unixepoch')        AS day,
           COALESCE(s.cwd, '(unknown)')                                          AS repo,
           COALESCE(s.model, '(unknown)')                                        AS model,
           COUNT(*)                                                              AS turns,
           COALESCE(SUM(a.input_tokens + a.output_tokens + a.cache_read_tokens
                        + a.cache_write_5m_tokens + a.cache_write_1h_tokens), 0) AS tokens,
           COALESCE(SUM(a.cost_usd_api_equiv), 0)                                AS cost
         FROM agent_turns a
         LEFT JOIN sessions s USING (session_id)
         WHERE a.started_at_ms IS NOT NULL
           AND (?1 IS NULL OR a.started_at_ms >= ?1)
         GROUP BY a.session_id
         ORDER BY tokens DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![since_ms, limit], |r| {
            Ok(Row {
                day: r.get(0)?,
                repo: r.get(1)?,
                model: r.get(2)?,
                turns: r.get(3)?,
                tokens: r.get(4)?,
                cost: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(super) fn render<W: Write>(
    out: &mut W,
    rows: &[Row],
    runaway_threshold: Option<f64>,
) -> Result<()> {
    let mut t = new_table(&["DAY", "REPO", "MODEL", "TURNS", "TOKENS", "COST", "RUNAWAY"]);
    let max_cost = rows.iter().map(|r| r.cost).fold(0.0_f64, f64::max);
    let n = rows.len();
    for r in rows {
        let tier = cost_tier(r.cost, max_cost, n);
        let is_runaway = runaway_threshold.is_some_and(|thr| r.cost > thr);
        // Hot tier (red) for runaways; muted (no color) for blank cells so a
        // missing threshold doesn't dye the column.
        let runaway_cell = if is_runaway {
            cell("⚠", ColorTier::Hot)
        } else {
            cell("", ColorTier::Muted)
        };
        t.add_row(vec![
            Cell::new(&r.day),
            Cell::new(repo_basename(&r.repo)),
            Cell::new(&r.model),
            Cell::new(r.turns).set_alignment(CellAlignment::Right),
            Cell::new(fmt_tokens(r.tokens)).set_alignment(CellAlignment::Right),
            cell(fmt_usd(r.cost), tier).set_alignment(CellAlignment::Right),
            runaway_cell.set_alignment(CellAlignment::Center),
        ]);
    }
    print_or_no_data(out, t, !rows.is_empty())
}

/// Trim a cwd to its trailing path component for compact display. A path
/// with no separator (or "(unknown)") is returned verbatim.
fn repo_basename(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .map(String::from)
        .unwrap_or_else(|| cwd.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::stats::tests::fixture;

    #[test]
    fn top_sessions_orders_by_tokens_desc() {
        let conn = fixture();
        let rows = compute(&conn, None, 100).unwrap();
        // 3 sessions: A (300 tok), B (1500 tok), C (150 tok).
        // Sorted by tokens desc → B, A, C.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].tokens, 1500);
        assert_eq!(rows[1].tokens, 300);
        assert_eq!(rows[2].tokens, 150);
        // Repo / model carried through from the JOIN to sessions.
        assert_eq!(rows[0].repo, "/repo2");
        assert_eq!(rows[0].model, "claude-sonnet-4-6");
    }

    #[test]
    fn render_emits_runaway_marker_only_above_threshold() {
        let rows = vec![
            Row {
                day: "2025-01-01".into(),
                repo: "/r".into(),
                model: "m".into(),
                turns: 1,
                tokens: 100,
                cost: 1.0,
            },
            Row {
                day: "2025-01-01".into(),
                repo: "/r".into(),
                model: "m".into(),
                turns: 1,
                tokens: 100,
                cost: 50.0,
            },
        ];
        // Threshold 10.0 → only the $50 row gets the marker. ANSI styling
        // wraps the char but doesn't replace it, so plain `contains` works.
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, &rows, Some(10.0)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let warn_count = s.matches('⚠').count();
        assert_eq!(warn_count, 1, "rendered:\n{s}");

        // No threshold → no markers anywhere.
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, &rows, None).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains('⚠'), "rendered:\n{s}");
    }

    #[test]
    fn repo_basename_strips_path_prefix() {
        assert_eq!(repo_basename("/home/me/projects/cc-ledger"), "cc-ledger");
        assert_eq!(repo_basename("/cc-ledger"), "cc-ledger");
        assert_eq!(repo_basename("(unknown)"), "(unknown)");
        assert_eq!(repo_basename(""), "");
    }
}
