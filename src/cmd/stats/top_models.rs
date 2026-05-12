//! Panel — *Top models by usage*.
//!
//! Question: which model is doing the work?
//! Source: `sessions.model` × `agent_turns` (filters out NULL and
//! `'unknown'` — those rows don't help anyone decide between Opus /
//! Sonnet / Haiku).
//!
//! Note: `sessions.model` reflects what Claude Code reported at
//! SessionStart. Older Claude Code releases didn't pass the model field,
//! so historical sessions may bucket as `'unknown'` and get filtered out.

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
    pub model: String,
    pub sessions: i64,
    pub turns: i64,
    pub tokens: i64,
    pub cost: f64,
}

pub(super) fn compute(conn: &Connection, since_ms: Option<i64>, limit: i64) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT
           s.model                                                               AS model,
           COUNT(DISTINCT a.session_id)                                          AS sessions,
           COUNT(*)                                                              AS turns,
           COALESCE(SUM(a.input_tokens + a.output_tokens + a.cache_read_tokens
                        + a.cache_write_5m_tokens + a.cache_write_1h_tokens), 0) AS tokens,
           COALESCE(SUM(a.cost_usd_api_equiv), 0)                                AS cost
         FROM agent_turns a
         JOIN sessions s USING (session_id)
         WHERE a.started_at_ms IS NOT NULL
           AND s.model IS NOT NULL
           AND s.model != 'unknown'
           AND (?1 IS NULL OR a.started_at_ms >= ?1)
         GROUP BY model
         ORDER BY cost DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![since_ms, limit], |r| {
            Ok(Row {
                model: r.get(0)?,
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
    let mut t = new_table(&["MODEL", "SESSIONS", "TURNS", "TOKENS", "COST"]);
    let max_cost = rows.iter().map(|r| r.cost).fold(0.0_f64, f64::max);
    let n = rows.len();
    for r in rows {
        let tier = cost_tier(r.cost, max_cost, n);
        t.add_row(vec![
            Cell::new(&r.model),
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
    fn top_models_filters_unknown_and_aggregates() {
        let conn = fixture();
        // Fixture: opus-4-7 across sessions A+C (3 user turns, $1.40);
        //          sonnet-4-6 in B (1 user turn, $0.10).
        let rows = compute(&conn, None, 100).unwrap();
        assert_eq!(rows.len(), 2);
        let opus = rows.iter().find(|r| r.model == "claude-opus-4-7").unwrap();
        assert_eq!(opus.turns, 3);
        assert!((opus.cost - 1.40).abs() < 1e-9);
        let sonnet = rows
            .iter()
            .find(|r| r.model == "claude-sonnet-4-6")
            .unwrap();
        assert_eq!(sonnet.turns, 1);
        assert!((sonnet.cost - 0.10).abs() < 1e-9);
    }

    #[test]
    fn top_models_excludes_unknown_sessions() {
        let conn = fixture();
        // Inject a session whose `model` is 'unknown' plus an agent_turn.
        // It must NOT influence the result.
        conn.execute_batch(
            "INSERT INTO sessions(session_id,agent_id,started_at,model)
                  VALUES('UNK','claude-code',5000000,'unknown');
             INSERT INTO agent_turns(session_id,user_turn_idx,started_at_ms,ended_at_ms,
                                     api_call_count,cost_usd_api_equiv,
                                     input_tokens,output_tokens,cache_read_tokens,
                                     cache_write_5m_tokens,cache_write_1h_tokens,
                                     category,classifier_version,classifier_tier,computed_at_ms)
                  VALUES('UNK',0,5050000,5100000, 1,99.99, 100,50,0,0,0,
                         'coding', 1,'tool', 5100000);",
        )
        .unwrap();
        let rows = compute(&conn, None, 100).unwrap();
        assert!(rows.iter().all(|r| r.model != "unknown"));
        assert!(!rows.iter().any(|r| (r.cost - 99.99).abs() < 1e-9));
    }
}
