//! Panel — *Daily activity*.
//!
//! Question: when have I been using it?
//! Source: `agent_turns` grouped by day (UTC) for the time window.
//! (agent_turns is populated by both the live `Stop` hook *and* the
//! `cc-ledger backfill agent-turns` command, so this panel works whether
//! the user has been live-capturing or not.)

use std::io::Write;

use anyhow::Result;
use comfy_table::{Cell, CellAlignment};
use rusqlite::{params, Connection};

use super::{
    palette::{bar, cell, cost_tier, fmt_tokens, fmt_usd},
    render::{new_table, print_or_no_data},
};

#[derive(Debug)]
pub(super) struct Row {
    pub day: String,
    pub turns: i64,
    pub tokens: i64,
    pub cost: f64,
}

pub(super) fn compute(conn: &Connection, since_ms: Option<i64>) -> Result<Vec<Row>> {
    // `?1 IS NULL` makes the time filter a no-op when `since_ms` is None.
    let mut stmt = conn.prepare(
        "SELECT
           strftime('%Y-%m-%d', started_at_ms / 1000, 'unixepoch')               AS day,
           COUNT(*)                                                              AS turns,
           COALESCE(SUM(input_tokens + output_tokens + cache_read_tokens
                        + cache_write_5m_tokens + cache_write_1h_tokens), 0)     AS tokens,
           COALESCE(SUM(cost_usd_api_equiv), 0)                                  AS cost
         FROM agent_turns
         WHERE started_at_ms IS NOT NULL
           AND (?1 IS NULL OR started_at_ms >= ?1)
         GROUP BY day
         ORDER BY day DESC",
    )?;
    let rows = stmt
        .query_map(params![since_ms], |r| {
            Ok(Row {
                day: r.get(0)?,
                turns: r.get(1)?,
                tokens: r.get(2)?,
                cost: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(super) fn render<W: Write>(out: &mut W, rows: &[Row]) -> Result<()> {
    let mut t = new_table(&["DAY", "TURNS", "TOKENS", "COST", "BAR"]);
    let max_cost = rows.iter().map(|r| r.cost).fold(0.0_f64, f64::max);
    let n = rows.len();
    for r in rows {
        let tier = cost_tier(r.cost, max_cost, n);
        let pct = if max_cost > 0.0 {
            r.cost / max_cost
        } else {
            0.0
        };
        t.add_row(vec![
            Cell::new(&r.day),
            Cell::new(r.turns).set_alignment(CellAlignment::Right),
            Cell::new(fmt_tokens(r.tokens)).set_alignment(CellAlignment::Right),
            cell(fmt_usd(r.cost), tier).set_alignment(CellAlignment::Right),
            Cell::new(bar(pct, 20)),
        ]);
    }
    print_or_no_data(out, t, !rows.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::stats::tests::fixture;

    #[test]
    fn aggregates_by_day_with_no_window() {
        let conn = fixture();
        let rows = compute(&conn, None).unwrap();
        // Shared fixture spans two unixepoch days:
        //   day 1970-01-01: A (2 turns, $1.00) + B (1 turn, $0.10)
        //   day 1970-01-02: C (1 turn,  $0.40)
        // Sorted by day desc.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].day, "1970-01-02");
        assert_eq!(rows[0].turns, 1);
        assert!((rows[0].cost - 0.40).abs() < 1e-9);
        assert_eq!(rows[1].day, "1970-01-01");
        assert_eq!(rows[1].turns, 3);
        assert!((rows[1].cost - 1.10).abs() < 1e-9);
    }

    #[test]
    fn since_window_filters_old_rows() {
        let conn = fixture();
        // Cutoff above all the fixture rows → empty result.
        let rows = compute(&conn, Some(1_000_000_000_000)).unwrap();
        assert!(rows.is_empty());
    }
}
