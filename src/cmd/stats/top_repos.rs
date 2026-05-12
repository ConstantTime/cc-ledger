//! Panel — *Top repos by usage*.
//!
//! Question: where am I spending?
//! Source: `sessions.cwd` × `agent_turns`. (agent_turns is populated by
//! both live capture and backfill, so this panel works for backfill-only
//! users too.)

use std::io::Write;

use anyhow::Result;
use comfy_table::{Cell, CellAlignment};
use rusqlite::{params, Connection};

use super::{
    palette::{cell, cost_tier, fmt_tokens, fmt_usd},
    render::{new_table, print_or_no_data},
};

#[derive(Debug)]
pub(super) struct Row {
    pub repo: String,
    pub sessions: i64,
    pub turns: i64,
    pub tokens: i64,
    pub cost: f64,
}

pub(super) fn compute(conn: &Connection, since_ms: Option<i64>, limit: i64) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT
           COALESCE(s.cwd, '(unknown)')                                          AS repo,
           COUNT(DISTINCT a.session_id)                                          AS sessions,
           COUNT(*)                                                              AS turns,
           COALESCE(SUM(a.input_tokens + a.output_tokens + a.cache_read_tokens
                        + a.cache_write_5m_tokens + a.cache_write_1h_tokens), 0) AS tokens,
           COALESCE(SUM(a.cost_usd_api_equiv), 0)                                AS cost
         FROM agent_turns a
         LEFT JOIN sessions s USING (session_id)
         WHERE a.started_at_ms IS NOT NULL
           AND (?1 IS NULL OR a.started_at_ms >= ?1)
         GROUP BY repo
         ORDER BY cost DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![since_ms, limit], |r| {
            Ok(Row {
                repo: r.get(0)?,
                sessions: r.get(1)?,
                turns: r.get(2)?,
                tokens: r.get(3)?,
                cost: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(super) fn render<W: Write>(out: &mut W, rows: &[Row]) -> Result<()> {
    let mut t = new_table(&["REPO", "SESSIONS", "TURNS", "TOKENS", "COST"]);
    let max_cost = rows.iter().map(|r| r.cost).fold(0.0_f64, f64::max);
    let n = rows.len();
    for r in rows {
        let tier = cost_tier(r.cost, max_cost, n);
        t.add_row(vec![
            Cell::new(&r.repo),
            Cell::new(r.sessions).set_alignment(CellAlignment::Right),
            Cell::new(r.turns).set_alignment(CellAlignment::Right),
            Cell::new(fmt_tokens(r.tokens)).set_alignment(CellAlignment::Right),
            cell(fmt_usd(r.cost), tier).set_alignment(CellAlignment::Right),
        ]);
    }
    print_or_no_data(out, t, !rows.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::stats::tests::fixture;

    #[test]
    fn top_repos_aggregates_by_cwd() {
        let conn = fixture();
        // Fixture: /repo1 has sessions A + C ($1.40, 3 turns); /repo2 has B
        // ($0.10, 1 turn).
        let rows = compute(&conn, None, 100).unwrap();
        assert_eq!(rows.len(), 2);

        let r1 = rows.iter().find(|r| r.repo == "/repo1").unwrap();
        assert_eq!(r1.sessions, 2);
        assert_eq!(r1.turns, 3);
        assert!((r1.cost - 1.40).abs() < 1e-9);

        let r2 = rows.iter().find(|r| r.repo == "/repo2").unwrap();
        assert_eq!(r2.sessions, 1);
        assert_eq!(r2.turns, 1);
        assert!((r2.cost - 0.10).abs() < 1e-9);
    }
}
