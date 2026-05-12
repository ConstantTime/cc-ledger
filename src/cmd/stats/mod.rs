//! `cc-ledger stats` — questions a daily user actually asks, one panel each.
//!
//! Each panel module exposes the same contract:
//!
//! ```ignore
//! pub(super) fn compute(conn, since_ms: Option<i64>, limit: i64) -> Result<Vec<Row>>;
//! pub(super) fn render<W: Write>(out, rows: &[Row]) -> Result<()>;
//! ```
//!
//! `compute` is pure SQL + typed-row map (cheap to unit-test); `render` is
//! dumb-string formatting. The orchestrator below threads them together.
//!
//! Panels:
//! 1. `daily_activity`        — when have I been using it?
//! 2. `top_repos`             — where am I spending?
//! 3. `top_models`            — which model is doing the work? (filters `unknown`)
//! 4. `top_sessions`          — heavy-hitter sessions in the window (no UUIDs shown)
//! 5. `session_distribution`  — what does a typical session cost vs. how bad does it get?
//! 6. `activity_category`     — what kind of work?
//!
//! All five are filtered by `--since <duration>` (default `30d`). Use
//! `--since all` to disable the time filter.

use std::io::Write;

use anyhow::Result;
use clap::Parser;
use rusqlite::Connection;

use crate::{paths, store};

mod palette;
mod render;

mod activity_category;
mod daily_activity;
mod session_distribution;
mod top_models;
mod top_repos;
mod top_sessions;

#[derive(Debug, Parser)]
pub struct Args {
    /// Limit rows in each section (where applicable).
    #[arg(long, default_value_t = 20)]
    pub top: usize,

    /// Time window: `<N>d`, `<N>h`, or `all`. Default `30d`.
    #[arg(long, default_value = "30d")]
    pub since: String,
}

pub fn run(args: Args) -> Result<()> {
    let conn = store::open(&paths::db_path()?)?;
    let now_ms = paths::now_ms();
    let since_ms = palette::parse_since(&args.since, now_ms)?;
    let mut out = std::io::stdout().lock();
    render_all(&mut out, &conn, since_ms, args.top as i64, &args.since)
}

fn render_all<W: Write>(
    w: &mut W,
    conn: &Connection,
    since_ms: Option<i64>,
    limit: i64,
    since_label: &str,
) -> Result<()> {
    let window = match since_ms {
        Some(_) => format!("last {since_label}"),
        None => "all time".to_string(),
    };

    render::section_header(w, "DAILY ACTIVITY", &format!("when, over the {window}"))?;
    daily_activity::render(w, &daily_activity::compute(conn, since_ms)?)?;

    render::section_header(w, "TOP REPOS", &format!("where you spent ({window})"))?;
    top_repos::render(w, &top_repos::compute(conn, since_ms, limit)?)?;

    render::section_header(
        w,
        "TOP MODELS",
        &format!("which model did the work ({window}, ignoring `unknown`)"),
    )?;
    top_models::render(w, &top_models::compute(conn, since_ms, limit)?)?;

    // Compute the session-cost distribution once: `top_sessions` needs the
    // p95 threshold for its RUNAWAY column, and the distribution panel
    // renders the percentiles directly.
    let session_stats = session_distribution::compute(conn, since_ms)?;
    let runaway_threshold = session_stats.as_ref().map(|s| s.runaway_threshold);

    render::section_header(
        w,
        "TOP SESSIONS BY TOKEN BURN",
        &format!("the heavy hitters ({window})"),
    )?;
    top_sessions::render(
        w,
        &top_sessions::compute(conn, since_ms, limit)?,
        runaway_threshold,
    )?;

    render::section_header(
        w,
        "SESSION SPEND DISTRIBUTION",
        &format!("how bad does a single session get ({window})"),
    )?;
    session_distribution::render(w, session_stats.as_ref())?;

    render::section_header(
        w,
        "ACTIVITY CATEGORY",
        &format!("what the agent was doing ({window})"),
    )?;
    activity_category::render(w, &activity_category::compute(conn, since_ms, limit)?)?;

    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    //! Shared per-panel test fixture. Each panel's `tests` module imports
    //! `fixture()` from here so the SQL setup lives in one place.

    use rusqlite::Connection;

    use crate::store;

    pub(crate) fn fixture() -> Connection {
        // tempdir survives via leak; conn keeps the file open during test.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        let conn = store::open(&path).unwrap();
        std::mem::forget(dir);

        // Three sessions, exercising the model + repo split that the
        // top-models / top-repos / top-sessions panels need to differentiate.
        // agent_turns is the canonical source for daily activity & per-session
        // aggregates; tokens + cost match what each session totals to.
        //
        //   A: opus-4-7   in /repo1 — 2 user turns, $1.00, 300 tokens, day 1
        //   B: sonnet-4-6 in /repo2 — 1 user turn,  $0.10, 1500 tokens, day 1
        //   C: opus-4-7   in /repo1 — 1 user turn,  $0.40, 150 tokens, day 2
        conn.execute_batch(
            "INSERT INTO sessions(session_id,agent_id,started_at,cwd,hostname,model)
                VALUES('A','claude-code',1000000,'/repo1','alice','claude-opus-4-7'),
                      ('B','claude-code',2000000,'/repo2','alice','claude-sonnet-4-6'),
                      ('C','claude-code',90000000,'/repo1','bob',  'claude-opus-4-7');

             INSERT INTO agent_turns(session_id,user_turn_idx,started_at_ms,ended_at_ms,
                                     api_call_count,cost_usd_api_equiv,
                                     input_tokens,output_tokens,cache_read_tokens,
                                     cache_write_5m_tokens,cache_write_1h_tokens,
                                     category,classifier_version,classifier_tier,computed_at_ms)
                VALUES('A',0,1050000,1100000, 1,0.50, 100,50,0,0,0,
                       'coding',     1,'tool',    1100000),
                      ('A',1,1150000,1200000, 1,0.50, 100,50,0,0,0,
                       'debugging',  1,'tool',    1200000),
                      ('B',0,2050000,2100000, 1,0.10, 1000,500,0,0,0,
                       'coding',     1,'tool',    2100000),
                      ('C',0,90050000,90100000, 1,0.40, 100,50,0,0,0,
                       'exploration',1,'keyword', 90100000);

             INSERT INTO tool_calls(session_id,tool_use_id,tool_name,
                                    ts_ms,status,lines_added,lines_removed)
                VALUES('A','t1','Edit',1050000,'success',5,1),
                      ('A','t2','Edit',1150000,'failure',0,0),
                      ('B','t1','Edit',2050000,'success',3,0),
                      ('B','t2','Edit',2080000,'success',2,0);

             INSERT INTO attributions(cwd,file_path,line_start,line_end,
                                      session_id,tool_use_id,author_id)
                VALUES('/repo1','a.rs',1,5, 'A','t1','ai:claude-code:A'),
                      ('/repo2','b.rs',1,3, 'B','t1','ai:claude-code:B'),
                      ('/repo2','b.rs',1,2, 'B','t2','ai:claude-code:B');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn render_all_runs_clean_on_populated_db() {
        let conn = fixture();
        let mut buf: Vec<u8> = Vec::new();
        super::render_all(&mut buf, &conn, None, 100, "all").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("DAILY ACTIVITY"));
        assert!(s.contains("TOP REPOS"));
        assert!(s.contains("TOP MODELS"));
        assert!(s.contains("TOP SESSIONS"));
        assert!(s.contains("SESSION SPEND DISTRIBUTION"));
        assert!(s.contains("ACTIVITY CATEGORY"));
    }

    #[test]
    fn render_all_says_no_data_on_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        super::render_all(&mut buf, &conn, None, 100, "all").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("(no data)"));
    }
}
