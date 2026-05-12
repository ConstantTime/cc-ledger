//! Panel — *Session spend distribution*.
//!
//! Question: how bad does a single session get? Reports `p50 / p95 / p99 /
//! max` across per-session cost totals, and counts how many sessions blew
//! past `p95` (the runaway badge threshold — same cut the PR distribution
//! uses, see `LedgerStore.swift::prCostStats`).
//!
//! Returns `None` when fewer than 5 sessions are in the window — percentiles
//! below that are noise. The orchestrator threads the resulting `p95` into
//! `top_sessions` so its `RUNAWAY` column lights up consistently.
//!
//! Percentile method: R-7 linear interpolation on a sorted ascending vector,
//! matching the menubar's SwiftUI implementation so CLI/menubar agree to the
//! cent on the same DB.
//!
//! Cost source: `SUM(cost_usd_api_equiv) GROUP BY session_id` over
//! `agent_turns` — same series the PR rollups use, just bucketed by session
//! instead of PR.
//!
//! No bare session IDs: this panel only emits aggregate stats.

use std::io::Write;

use anyhow::Result;
use owo_colors::OwoColorize;
use rusqlite::{params, Connection};

use super::palette::fmt_usd;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SessionCostStats {
    pub count: i64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub runaway_count: i64,
    pub runaway_threshold: f64,
}

pub(super) fn compute(
    conn: &Connection,
    since_ms: Option<i64>,
) -> Result<Option<SessionCostStats>> {
    let mut stmt = conn.prepare(
        "SELECT SUM(a.cost_usd_api_equiv) AS total
           FROM agent_turns a
          WHERE a.started_at_ms IS NOT NULL
            AND (?1 IS NULL OR a.started_at_ms >= ?1)
          GROUP BY a.session_id
         HAVING total > 0
          ORDER BY total ASC",
    )?;
    let costs: Vec<f64> = stmt
        .query_map(params![since_ms], |r| r.get::<_, f64>(0))?
        .collect::<rusqlite::Result<_>>()?;

    if costs.len() < 5 {
        return Ok(None);
    }

    let p50 = pct_r7(&costs, 0.50);
    let p95 = pct_r7(&costs, 0.95);
    let p99 = pct_r7(&costs, 0.99);
    let max = *costs.last().unwrap_or(&0.0);
    let runaway_count = costs.iter().filter(|c| **c > p95).count() as i64;

    Ok(Some(SessionCostStats {
        count: costs.len() as i64,
        p50,
        p95,
        p99,
        max,
        runaway_count,
        runaway_threshold: p95,
    }))
}

pub(super) fn render<W: Write>(out: &mut W, stats: Option<&SessionCostStats>) -> Result<()> {
    let Some(s) = stats else {
        writeln!(out, "(no data)")?;
        return Ok(());
    };
    writeln!(
        out,
        "{} sessions · p50 {} · p95 {} · p99 {} · max {}",
        s.count,
        fmt_usd(s.p50),
        fmt_usd(s.p95),
        fmt_usd(s.p99),
        fmt_usd(s.max),
    )?;
    if s.runaway_count > 0 {
        let noun = if s.runaway_count == 1 {
            "session"
        } else {
            "sessions"
        };
        let line = format!(
            "⚠ {} runaway {noun} above p95 ({})",
            s.runaway_count,
            fmt_usd(s.runaway_threshold),
        );
        writeln!(out, "{}", line.yellow())?;
    }
    Ok(())
}

/// R-7 linear interpolation percentile. `costs` must be sorted ascending,
/// non-empty. `p` in `[0.0, 1.0]`.
fn pct_r7(costs: &[f64], p: f64) -> f64 {
    debug_assert!(!costs.is_empty());
    let n = costs.len();
    let idx = p * (n as f64 - 1.0);
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        return costs[lo];
    }
    let t = idx - lo as f64;
    costs[lo] * (1.0 - t) + costs[hi] * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::stats::tests::fixture;

    #[test]
    fn pct_r7_matches_known_values() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((pct_r7(&v, 0.0) - 1.0).abs() < 1e-9);
        assert!((pct_r7(&v, 0.5) - 3.0).abs() < 1e-9);
        assert!((pct_r7(&v, 1.0) - 5.0).abs() < 1e-9);
        // p25 of [1..5] under R-7 → 1 + 0.25*(5-1) = 2.0
        assert!((pct_r7(&v, 0.25) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn compute_returns_none_below_five_sessions() {
        // Baseline fixture has 3 sessions; compute should bail.
        let conn = fixture();
        let stats = compute(&conn, None).unwrap();
        assert!(stats.is_none());
    }

    #[test]
    fn compute_flags_outliers_above_p95() {
        // Seed a fresh DB with 10 sessions whose totals are 1, 2, ..., 9, 100.
        // p95 over [1..9, 100] sorted = 9 + 0.95*(10-1) interpolation between
        // costs[8]=9 and costs[9]=100 → 9 + 0.55*(100-9) = 9 + 0.55*91 = 59.05.
        // Wait — R-7 idx = 0.95 * (10 - 1) = 8.55 → lo=8 (=9), hi=9 (=100),
        // t=0.55 → 9*0.45 + 100*0.55 = 4.05 + 55.0 = 59.05.
        // So only the 100 session is strictly above p95.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        let conn = crate::store::open(&path).unwrap();

        let mut session_inserts = String::new();
        let mut turn_inserts = String::new();
        let costs = [1.0_f64, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 100.0];
        for (i, c) in costs.iter().enumerate() {
            let sid = format!("S{i}");
            let start = 1_000_000 + i as i64 * 1000;
            session_inserts.push_str(&format!(
                "INSERT INTO sessions(session_id,agent_id,started_at,cwd,hostname,model)\
                 VALUES('{sid}','claude-code',{start},'/r','h','m');\n"
            ));
            turn_inserts.push_str(&format!(
                "INSERT INTO agent_turns(session_id,user_turn_idx,started_at_ms,ended_at_ms,\
                 api_call_count,cost_usd_api_equiv,input_tokens,output_tokens,cache_read_tokens,\
                 cache_write_5m_tokens,cache_write_1h_tokens,category,classifier_version,\
                 classifier_tier,computed_at_ms)\
                 VALUES('{sid}',0,{start},{start},1,{c},0,0,0,0,0,'coding',1,'tool',{start});\n"
            ));
        }
        conn.execute_batch(&format!("{session_inserts}{turn_inserts}"))
            .unwrap();

        let stats = compute(&conn, None).unwrap().expect("≥5 sessions");
        assert_eq!(stats.count, 10);
        // p95 ≈ 59.05; only the $100 session is above it.
        assert!((stats.p95 - 59.05).abs() < 1e-6, "p95 = {}", stats.p95);
        assert_eq!(stats.runaway_count, 1);
        assert!((stats.max - 100.0).abs() < 1e-9);
        // p50 of 10 sorted values → idx = 0.5 * 9 = 4.5 → between 5 and 6 → 5.5
        assert!((stats.p50 - 5.5).abs() < 1e-9);
    }

    #[test]
    fn render_says_no_data_when_none() {
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, None).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("(no data)"));
    }

    #[test]
    fn render_emits_runaway_line_when_outliers_present() {
        let stats = SessionCostStats {
            count: 10,
            p50: 5.5,
            p95: 59.05,
            p99: 95.95,
            max: 100.0,
            runaway_count: 1,
            runaway_threshold: 59.05,
        };
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, Some(&stats)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("10 sessions"));
        assert!(s.contains("p99"));
        assert!(s.contains("runaway session above p95"));
    }
}
