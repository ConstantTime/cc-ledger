//! Panel 6 — *Activity category*.
//!
//! Question: what is the agent actually doing — coding, debugging,
//! refactoring, exploration, …?
//! Source: `agent_turns` (codeburn-style classifier output, populated by
//! the `Stop` hook + `cc-ledger backfill agent-turns`).
//!
//! Empty-table copy points the user at `backfill` because users who haven't
//! run it on a fresh upgrade won't have any rows yet — distinguishing that
//! state from "you really did zero work" is the friendly thing to do.

use std::io::Write;

use anyhow::Result;
use comfy_table::{Cell, CellAlignment};
use rusqlite::{params, Connection};

use super::{
    palette::{bar, cell, cost_tier, fmt_usd},
    render::{new_table, print_or_no_data},
};

#[derive(Debug)]
pub(super) struct Row {
    pub category: String,
    pub turns: i64,
    pub cost: f64,
    /// Percent of the table's total cost (0–100). NULL-safe in SQL: when
    /// the grand total is 0, this is 0 rather than a divide-by-zero.
    pub pct_cost: f64,
}

pub(super) fn compute(conn: &Connection, since_ms: Option<i64>, limit: i64) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "WITH t AS (
            SELECT SUM(cost_usd_api_equiv) AS grand
              FROM agent_turns
             WHERE ?1 IS NULL OR started_at_ms >= ?1
         )
         SELECT
           category,
           COUNT(*)                                             AS turns,
           COALESCE(SUM(cost_usd_api_equiv), 0)                 AS cost,
           CASE WHEN (SELECT grand FROM t) > 0
             THEN 100.0 * SUM(cost_usd_api_equiv) / (SELECT grand FROM t)
             ELSE 0 END                                         AS pct_cost
         FROM agent_turns
         WHERE ?1 IS NULL OR started_at_ms >= ?1
         GROUP BY category
         ORDER BY cost DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![since_ms, limit], |r| {
            Ok(Row {
                category: r.get(0)?,
                turns: r.get(1)?,
                cost: r.get(2)?,
                pct_cost: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(super) fn render<W: Write>(out: &mut W, rows: &[Row]) -> Result<()> {
    if rows.is_empty() {
        writeln!(
            out,
            "(no data — run `cc-ledger backfill agent-turns` to populate from local Claude Code transcripts)"
        )?;
        return Ok(());
    }

    let mut t = new_table(&["CATEGORY", "TURNS", "COST", "% COST", "SHARE"]);
    let max_cost = rows.iter().map(|r| r.cost).fold(0.0_f64, f64::max);
    let n = rows.len();
    for r in rows {
        let tier = cost_tier(r.cost, max_cost, n);
        t.add_row(vec![
            Cell::new(&r.category),
            Cell::new(r.turns).set_alignment(CellAlignment::Right),
            cell(fmt_usd(r.cost), tier).set_alignment(CellAlignment::Right),
            Cell::new(format!("{:.1}%", r.pct_cost)).set_alignment(CellAlignment::Right),
            // Bar width 20 — fits comfortably with the rest of the table.
            Cell::new(bar(r.pct_cost / 100.0, 20)),
        ]);
    }
    print_or_no_data(out, t, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::stats::tests::fixture;
    use crate::store;

    #[test]
    fn aggregates_cost_per_category_and_computes_pct() {
        let conn = fixture();
        // Shared fixture seeds 4 agent_turns rows totalling $1.50:
        //   coding      → 2 turns, $0.50 + $0.10 = $0.60 → 40.0%
        //   debugging   → 1 turn,  $0.50               → 33.3…%
        //   exploration → 1 turn,  $0.40               → 26.6…%
        let rows = compute(&conn, None, 100).unwrap();

        let coding = rows.iter().find(|r| r.category == "coding").unwrap();
        assert_eq!(coding.turns, 2);
        assert!((coding.cost - 0.60).abs() < 1e-9);
        assert!((coding.pct_cost - 40.0).abs() < 1e-6);

        let dbg = rows.iter().find(|r| r.category == "debugging").unwrap();
        assert_eq!(dbg.turns, 1);
        assert!((dbg.cost - 0.50).abs() < 1e-9);

        // Percentages sum to 100 across all returned rows.
        let total_pct: f64 = rows.iter().map(|r| r.pct_cost).sum();
        assert!((total_pct - 100.0).abs() < 1e-6);
    }

    #[test]
    fn render_says_run_backfill_when_empty() {
        // Empty DB → no agent_turns rows → expect the backfill hint.
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        let rows = compute(&conn, None, 100).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, &rows).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("backfill agent-turns"), "got: {s}");
    }

    #[test]
    fn render_includes_top_category_on_populated_db() {
        let conn = fixture();
        let rows = compute(&conn, None, 100).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, &rows).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("coding"));
    }
}
