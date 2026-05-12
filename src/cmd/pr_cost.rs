//! `cc-ledger pr-cost` — terminal-only cost-per-PR view.
//!
//! Pure local read; never opens a network connection or checks auth.
//! Recomputes the rollup for each requested PR before printing so the value
//! is always fresh against the current `attributions` and `turns`.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rusqlite::{params, Connection};

use crate::pr;
use crate::{paths, store};

#[derive(Debug, Parser)]
pub struct Args {
    /// PR number to inspect. Defaults to the PR matching the current branch.
    #[arg(long)]
    pub pr: Option<i64>,

    /// Show every PR ever recorded for the cwd, sorted by cost desc.
    #[arg(long, conflicts_with_all = ["pr", "branch"])]
    pub all: bool,

    /// Branch to look up. Defaults to `git rev-parse --abbrev-ref HEAD`.
    #[arg(long)]
    pub branch: Option<String>,

    /// Working directory to query (default: current dir). Useful for tests.
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Emit one JSON object per line instead of a table.
    #[arg(long)]
    pub json: bool,

    /// Force a recompute even if a fresh rollup row exists.
    #[arg(long)]
    pub recompute: bool,
}

pub fn run(args: Args) -> Result<()> {
    let conn = store::open(&paths::db_path()?)?;
    let mut out = std::io::stdout().lock();
    render(&mut out, &conn, &args, paths::now_ms())
}

fn render<W: Write>(out: &mut W, conn: &Connection, args: &Args, now_ms: i64) -> Result<()> {
    let cwd = match &args.cwd {
        Some(p) => p.clone(),
        None => std::env::current_dir().context("resolving current directory")?,
    };
    let cwd_str = cwd.to_string_lossy().to_string();

    let prs = resolve_prs(conn, &cwd, args)?;
    if prs.is_empty() {
        writeln!(out, "(no matching PRs in {cwd_str})")?;
        return Ok(());
    }

    let mut rows = Vec::with_capacity(prs.len());
    for pr_number in &prs {
        if args.recompute {
            pr::recompute_rollup(conn, &cwd_str, *pr_number, now_ms)?;
        }
        rows.push(load_row(conn, &cwd_str, *pr_number)?);
    }

    if args.json {
        for r in &rows {
            writeln!(out, "{}", serde_json::to_string(r)?)?;
        }
    } else {
        print_table(out, &rows)?;
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct PrRow {
    pr_number: i64,
    branch: String,
    state: String,
    commits: i64,
    sessions: i64,
    ai_lines_added: i64,
    ai_lines_removed: i64,
    total_cost_usd: f64,
    cost_per_ai_line: Option<f64>,
    last_refresh_ms: i64,
    first_seen_at_ms: i64,
    merged_at_ms: Option<i64>,
}

fn load_row(conn: &Connection, cwd: &str, pr_number: i64) -> Result<PrRow> {
    // The pr_cost_rollups row may not exist yet for PRs we just created
    // without --recompute; left-join handles that.
    let row = conn.query_row(
        "SELECT p.branch, p.state, p.first_seen_at_ms, p.merged_at_ms,
                COALESCE(r.total_cost_usd_api_equiv, 0),
                COALESCE(r.ai_lines_added, 0),
                COALESCE(r.ai_lines_removed, 0),
                COALESCE(r.session_count, 0),
                COALESCE(r.commit_count, 0),
                COALESCE(r.last_computed_at_ms, 0)
           FROM pull_requests p
           LEFT JOIN pr_cost_rollups r
             ON r.cwd = p.cwd AND r.pr_number = p.pr_number
          WHERE p.cwd = ?1 AND p.pr_number = ?2",
        params![cwd, pr_number],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
            ))
        },
    )?;
    let (branch, state, first_seen, merged, cost, added, removed, sessions, commits, last) = row;
    let cost_per_ai_line = if added > 0 {
        Some(cost / added as f64)
    } else {
        None
    };
    Ok(PrRow {
        pr_number,
        branch,
        state,
        commits,
        sessions,
        ai_lines_added: added,
        ai_lines_removed: removed,
        total_cost_usd: cost,
        cost_per_ai_line,
        last_refresh_ms: last,
        first_seen_at_ms: first_seen,
        merged_at_ms: merged,
    })
}

fn resolve_prs(conn: &Connection, cwd: &std::path::Path, args: &Args) -> Result<Vec<i64>> {
    let cwd_str = cwd.to_string_lossy();

    if let Some(pr) = args.pr {
        return Ok(vec![pr]);
    }
    if args.all {
        let mut stmt = conn.prepare(
            "SELECT p.pr_number FROM pull_requests p
               LEFT JOIN pr_cost_rollups r
                 ON r.cwd = p.cwd AND r.pr_number = p.pr_number
              WHERE p.cwd = ?1
              ORDER BY COALESCE(r.total_cost_usd_api_equiv, 0) DESC",
        )?;
        let rows = stmt
            .query_map(params![cwd_str], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<_>>()?;
        return Ok(rows);
    }

    let branch = args
        .branch
        .clone()
        .or_else(|| crate::git::current_branch(cwd))
        .context("could not resolve branch (pass --branch or run inside a git repo)")?;
    let prs = crate::store::queries::open_prs(conn, &cwd_str, Some(&branch))?;
    if prs.is_empty() {
        // Fall back to the most recent PR on this branch (open or merged).
        let mut stmt = conn.prepare(
            "SELECT pr_number FROM pull_requests
              WHERE cwd = ?1 AND branch = ?2
              ORDER BY first_seen_at_ms DESC LIMIT 1",
        )?;
        let row: Option<i64> = stmt.query_row(params![cwd_str, branch], |r| r.get(0)).ok();
        Ok(row.into_iter().collect())
    } else {
        Ok(prs.into_iter().map(|(n, _)| n).collect())
    }
}

fn print_table<W: Write>(out: &mut W, rows: &[PrRow]) -> Result<()> {
    let headers = [
        "PR",
        "BRANCH",
        "STATE",
        "COMMITS",
        "SESSIONS",
        "AI_LINES",
        "TOTAL_USD",
        "USD/LINE",
    ];
    let mut text_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                format!("#{}", r.pr_number),
                r.branch.clone(),
                r.state.clone(),
                r.commits.to_string(),
                r.sessions.to_string(),
                format!("+{}", r.ai_lines_added),
                fmt_usd(r.total_cost_usd),
                r.cost_per_ai_line
                    .map(fmt_usd)
                    .unwrap_or_else(|| "—".into()),
            ]
        })
        .collect();

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in &text_rows {
        for (i, w) in widths.iter_mut().enumerate() {
            if let Some(cell) = row.get(i) {
                *w = (*w).max(cell.len());
            }
        }
    }
    let join = |row: &[String]| -> String {
        row.iter()
            .enumerate()
            .map(|(i, cell)| format!("{:<w$}", cell, w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
    };
    writeln!(
        out,
        "{}",
        join(&headers.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    )?;
    writeln!(
        out,
        "{}",
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  ")
    )?;
    for row in &mut text_rows {
        writeln!(out, "{}", join(row))?;
    }
    Ok(())
}

fn fmt_usd(n: f64) -> String {
    if n.abs() < 0.01 && n != 0.0 {
        format!("${n:.6}")
    } else {
        format!("${n:.4}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Connection, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        // One session, $0.50, 5 lines on commit X (in PR 7).
        conn.execute(
            "INSERT INTO sessions(session_id, agent_id, hostname, model)
                VALUES('A', 'claude-code', 'h', 'm')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO turns(session_id, turn_idx, model,
                              input_tokens, output_tokens, cache_read_tokens,
                              cache_write_5m_tokens, cache_write_1h_tokens,
                              cost_usd_api_equiv, web_search_count)
                VALUES('A', 0, 'm', 100, 100, 0, 0, 0, 0.50, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_calls(session_id, tool_use_id, tool_name, file_path, ts_ms,
                                    status, lines_added, lines_removed)
                VALUES('A', 'tu1', 'Edit', 'a.rs', 0, 'success', 5, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attributions(commit_sha, cwd, file_path, line_start, line_end,
                                      session_id, tool_use_id, author_id)
                VALUES('X', '/r', 'a.rs', 1, 5, 'A', 'tu1', 'ai:claude-code:A')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pull_requests(cwd, pr_number, branch,
                                       first_seen_at_ms, last_seen_at_ms)
                VALUES('/r', 7, 'feat', 100, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pr_commits(cwd, pr_number, commit_sha) VALUES('/r', 7, 'X')",
            [],
        )
        .unwrap();
        (conn, dir)
    }

    #[test]
    fn explicit_pr_renders_with_recompute() {
        let (conn, _d) = fixture();
        let args = Args {
            pr: Some(7),
            all: false,
            branch: None,
            cwd: Some(PathBuf::from("/r")),
            json: false,
            recompute: true,
        };
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, &conn, &args, 1000).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("#7"), "{s}");
        assert!(s.contains("$0.5"), "{s}");
    }

    #[test]
    fn json_mode_emits_one_object_per_pr() {
        let (conn, _d) = fixture();
        let args = Args {
            pr: None,
            all: true,
            branch: None,
            cwd: Some(PathBuf::from("/r")),
            json: true,
            recompute: true,
        };
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, &conn, &args, 1000).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(v["pr_number"], 7);
        assert!(v["total_cost_usd"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn empty_state_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        let args = Args {
            pr: None,
            all: true,
            branch: None,
            cwd: Some(PathBuf::from("/empty")),
            json: false,
            recompute: false,
        };
        let mut buf: Vec<u8> = Vec::new();
        render(&mut buf, &conn, &args, 1000).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("no matching PRs"), "{s}");
    }
}
