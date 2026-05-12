//! PR lifecycle, commit capture, and cost-rollup recomputation.
//!
//! Three entry points:
//! - [`capture_commit`] — store a single commit + its file diffstat, backfill
//!   `attributions.commit_sha` for matching rows.
//! - [`detect_and_record_pr`] — match local branch tip against remote
//!   `refs/pull/*/head` (git only, no `gh`); on first detection persist the
//!   PR + its commits + a `first_seen` snapshot.
//! - [`recompute_rollup`] — recompute and persist `pr_cost_rollups` for one PR
//!   by prorating session cost over the share of session attributions that
//!   landed in the PR's commits.

use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::git;
use crate::store::queries::{self, CommitFileRow, CommitRow, PollState, PrTotals, PullRequestRow};

/// Negative cache window for "we polled and the branch had no PR".
pub const POLL_NEGATIVE_TTL_MS: i64 = 60_000;
/// Positive cache window for "we found a PR; don't repoll until this elapses".
pub const POLL_POSITIVE_TTL_MS: i64 = 5 * 60_000;

/// Snapshot kinds for `pr_cost_snapshots`.
pub const SNAP_FIRST_SEEN: &str = "first_seen";
pub const SNAP_MERGED: &str = "merged";

/// Store one commit + its per-file numstat. Best-effort: any git failure is
/// a no-op (logged by the caller via the audit log) so we never block hooks.
///
/// `now_ms` is used as `captured_at_ms`.
pub fn capture_commit(
    conn: &Connection,
    cwd: &Path,
    sha: &str,
    branch: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    let Some(diff) = git::commit_diffstat(cwd, sha) else {
        return Ok(());
    };
    let cwd_str = cwd.to_string_lossy();
    let prior_ts = queries::last_captured_commit_ts(conn, &cwd_str)?.unwrap_or(0);
    let authored_at_ms = diff.authored_at_unix.map(|s| s * 1000);

    queries::upsert_commit(
        conn,
        &CommitRow {
            cwd: &cwd_str,
            commit_sha: &diff.sha,
            authored_at_ms,
            branch,
            subject: diff.subject.as_deref(),
            additions: diff.additions,
            deletions: diff.deletions,
            files_touched: diff.files.len() as i64,
            captured_at_ms: now_ms,
        },
    )?;
    for f in &diff.files {
        queries::upsert_commit_file(
            conn,
            &CommitFileRow {
                cwd: &cwd_str,
                commit_sha: &diff.sha,
                file_path: &f.path,
                additions: f.additions,
                deletions: f.deletions,
            },
        )?;
    }
    // Backfill attributions for any tool calls between the last captured
    // commit and this one. Use authored_at_ms as the upper bound so PRs we
    // backfill in batch don't pull in unrelated post-commit edits.
    let upper_ms = authored_at_ms.unwrap_or(now_ms);
    queries::backfill_attribution_commits(conn, &cwd_str, &diff.sha, prior_ts, upper_ms)?;
    Ok(())
}

/// Recompute totals for one PR without persisting. Used by both the rollup
/// writer and the offline `cc-ledger pr-cost` command.
///
/// Proration: for each session that has attributions on commits in this PR,
/// `share = pr_lines / session_lines`. Cost and token totals are scaled by
/// `share`. AI line counts are absolute (not prorated).
pub fn compute_totals(conn: &Connection, cwd: &str, pr_number: i64) -> Result<PrTotals> {
    let commit_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pr_commits WHERE cwd = ?1 AND pr_number = ?2",
            params![cwd, pr_number],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0);
    let mut totals = PrTotals {
        commit_count,
        ..PrTotals::default()
    };

    let mut sessions_stmt = conn.prepare(
        "SELECT DISTINCT a.session_id
           FROM attributions a
          WHERE a.cwd = ?1
            AND a.commit_sha IN (
                SELECT commit_sha FROM pr_commits WHERE cwd = ?1 AND pr_number = ?2
            )",
    )?;
    let sessions: Vec<String> = sessions_stmt
        .query_map(params![cwd, pr_number], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    totals.session_count = sessions.len() as i64;

    for sid in &sessions {
        let pr_lines: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(line_end - line_start + 1), 0)
                   FROM attributions
                  WHERE cwd = ?1 AND session_id = ?2
                    AND commit_sha IN (
                        SELECT commit_sha FROM pr_commits WHERE cwd = ?1 AND pr_number = ?3
                    )",
                params![cwd, sid, pr_number],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let session_lines: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(line_end - line_start + 1), 0)
                   FROM attributions WHERE session_id = ?1",
                params![sid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if session_lines == 0 || pr_lines == 0 {
            continue;
        }
        let share = pr_lines as f64 / session_lines as f64;

        let (cost, inp, outp, cr, cw5, cw1): (Option<f64>, i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd_api_equiv), 0),
                        COALESCE(SUM(input_tokens), 0),
                        COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cache_read_tokens), 0),
                        COALESCE(SUM(cache_write_5m_tokens), 0),
                        COALESCE(SUM(cache_write_1h_tokens), 0)
                   FROM turns WHERE session_id = ?1",
                params![sid],
                |r| {
                    Ok((
                        r.get::<_, Option<f64>>(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap_or((Some(0.0), 0, 0, 0, 0, 0));

        totals.total_cost_usd_api_equiv += cost.unwrap_or(0.0) * share;
        totals.input_tokens += scale(inp, share);
        totals.output_tokens += scale(outp, share);
        totals.cache_read_tokens += scale(cr, share);
        totals.cache_write_5m_tokens += scale(cw5, share);
        totals.cache_write_1h_tokens += scale(cw1, share);
        totals.ai_lines_added += pr_lines;

        // Removed-line proxy: sum tool_calls.lines_removed for tool calls that
        // produced any attribution rows in this PR. Not prorated — a tool call
        // either touched this PR's commits or it didn't.
        let removed: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(tc.lines_removed), 0)
                   FROM tool_calls tc
                  WHERE tc.session_id = ?1
                    AND EXISTS (
                        SELECT 1 FROM attributions a
                         WHERE a.session_id   = tc.session_id
                           AND a.tool_use_id  = tc.tool_use_id
                           AND a.cwd          = ?2
                           AND a.commit_sha IN (
                               SELECT commit_sha FROM pr_commits
                                WHERE cwd = ?2 AND pr_number = ?3
                           )
                    )",
                params![sid, cwd, pr_number],
                |r| r.get(0),
            )
            .unwrap_or(0);
        totals.ai_lines_removed += removed;
    }
    Ok(totals)
}

fn scale(n: i64, share: f64) -> i64 {
    ((n as f64) * share).round() as i64
}

/// Recompute and persist the rollup row.
pub fn recompute_rollup(
    conn: &Connection,
    cwd: &str,
    pr_number: i64,
    now_ms: i64,
) -> Result<PrTotals> {
    let totals = compute_totals(conn, cwd, pr_number)?;
    queries::upsert_pr_rollup(conn, cwd, pr_number, &totals, now_ms)?;
    Ok(totals)
}

/// Run the `git ls-remote` PR-detection algorithm for `cwd`. Returns the PR
/// number on a hit, `None` for any failure or "no PR found". Has no side
/// effects beyond optional `git_poll_state` updates handled by the caller.
pub fn detect_pr_number(cwd: &Path, branch: &str, base: &str) -> Option<i64> {
    let head = git::head_sha(cwd)?;
    let upstream = git::upstream_sha(cwd);
    let branch_shas = git::rev_list_branch(cwd, branch, base);
    let table = git::list_pr_head_refs(cwd);
    if table.is_empty() {
        return None;
    }
    // Pick the highest PR # whose head sha is in our branch's history.
    let mut best: Option<i64> = None;
    for (sha, pr) in &table {
        let matches = sha == &head
            || upstream.as_deref() == Some(sha.as_str())
            || branch_shas.iter().any(|b| b == sha);
        if matches && best.map(|b| pr > &b).unwrap_or(true) {
            best = Some(*pr);
        }
    }
    best
}

/// Result of [`detect_and_record_pr`]: which PR (if any) we matched, and
/// whether this was the first time we saw it locally.
pub struct PrDetection {
    pub pr_number: i64,
    pub is_new: bool,
}

/// Full PR-discovery flow. On a new hit:
/// 1. Captures every branch commit that wasn't already in `commits`.
/// 2. Inserts `pull_requests` + `pr_commits`.
/// 3. Recomputes the rollup.
/// 4. Inserts the `first_seen` snapshot (no-op if one already exists).
///
/// All git failures collapse to `Ok(None)` — never blocks the caller.
pub fn detect_and_record_pr(
    conn: &Connection,
    cwd: &Path,
    now_ms: i64,
) -> Result<Option<PrDetection>> {
    let Some(branch) = git::current_branch(cwd) else {
        return Ok(None);
    };
    let Some(base) = git::default_branch(cwd) else {
        return Ok(None);
    };
    let Some(pr_number) = detect_pr_number(cwd, &branch, &format!("origin/{base}")) else {
        // Cache the negative result so Stop-hook polls don't hammer ls-remote.
        let cwd_str = cwd.to_string_lossy();
        queries::set_poll_state(
            conn,
            &cwd_str,
            &branch,
            &PollState {
                last_polled_at_ms: now_ms,
                last_remote_head_sha: git::upstream_sha(cwd),
                last_pr_number: None,
                last_negative_at_ms: Some(now_ms),
            },
        )?;
        return Ok(None);
    };
    let cwd_str = cwd.to_string_lossy();
    let head_sha = git::head_sha(cwd);
    let remote_url = git::origin_url(cwd);
    let basename = git::repo_basename(cwd);

    // Capture every branch commit (idempotent; pre-existing rows untouched).
    let branch_shas = git::rev_list_branch(cwd, &branch, &format!("origin/{base}"));
    for sha in &branch_shas {
        capture_commit(conn, cwd, sha, Some(&branch), now_ms)?;
    }

    let row = PullRequestRow {
        cwd: &cwd_str,
        pr_number,
        repo_remote_url: remote_url.as_deref(),
        repo_basename: basename.as_deref(),
        branch: &branch,
        base_branch: Some(&base),
        head_sha: head_sha.as_deref(),
        now_ms,
    };
    let is_new = queries::upsert_pull_request(conn, &row)?;
    for sha in &branch_shas {
        queries::upsert_pr_commit(conn, &cwd_str, pr_number, sha)?;
    }

    let totals = recompute_rollup(conn, &cwd_str, pr_number, now_ms)?;
    if is_new {
        queries::insert_pr_snapshot(conn, &cwd_str, pr_number, SNAP_FIRST_SEEN, now_ms, &totals)?;
    }

    queries::set_poll_state(
        conn,
        &cwd_str,
        &branch,
        &PollState {
            last_polled_at_ms: now_ms,
            last_remote_head_sha: head_sha,
            last_pr_number: Some(pr_number),
            last_negative_at_ms: None,
        },
    )?;

    Ok(Some(PrDetection { pr_number, is_new }))
}

/// TTL-gated wrapper: only invokes `git ls-remote` if the cached state is
/// stale or missing. Used by the `Stop` hook so every assistant turn doesn't
/// hit the remote.
pub fn poll_if_stale(conn: &Connection, cwd: &Path, now_ms: i64) -> Result<Option<PrDetection>> {
    let Some(branch) = git::current_branch(cwd) else {
        return Ok(None);
    };
    let cwd_str = cwd.to_string_lossy();
    if let Some(prev) = queries::get_poll_state(conn, &cwd_str, &branch)? {
        let upstream_now = git::upstream_sha(cwd);
        let upstream_changed = match (&prev.last_remote_head_sha, &upstream_now) {
            (Some(a), Some(b)) => a != b,
            (None, _) | (_, None) => true,
        };
        if !upstream_changed {
            // Honor positive cache OR negative cache, depending on which we hit.
            if prev.last_pr_number.is_some()
                && now_ms - prev.last_polled_at_ms < POLL_POSITIVE_TTL_MS
            {
                return Ok(None);
            }
            if let Some(neg) = prev.last_negative_at_ms {
                if now_ms - neg < POLL_NEGATIVE_TTL_MS {
                    return Ok(None);
                }
            }
        }
    }
    detect_and_record_pr(conn, cwd, now_ms)
}

/// Squash-merge detection. Walks `git log --first-parent <base>` for each
/// open PR and flips `state='merged'` when a `(#<n>)` subject is found.
/// Recomputes the rollup and writes a `merged` snapshot per PR.
pub fn reconcile_squash_merges(conn: &Connection, cwd: &Path, now_ms: i64) -> Result<Vec<i64>> {
    let cwd_str = cwd.to_string_lossy();
    let Some(base) = git::default_branch(cwd) else {
        return Ok(Vec::new());
    };
    let prs = queries::open_prs(conn, &cwd_str, None)?;
    let mut merged = Vec::new();
    for (pr_number, _branch) in prs {
        if let Some((sha, ts)) = git::find_squash_merge(cwd, &format!("origin/{base}"), pr_number) {
            queries::mark_pr_merged(conn, &cwd_str, pr_number, ts * 1000)?;
            queries::upsert_pr_commit(conn, &cwd_str, pr_number, &sha)?;
            // Capture the squash commit itself so its files are linkable.
            capture_commit(conn, cwd, &sha, None, now_ms)?;
            let totals = recompute_rollup(conn, &cwd_str, pr_number, now_ms)?;
            queries::insert_pr_snapshot(conn, &cwd_str, pr_number, SNAP_MERGED, now_ms, &totals)?;
            merged.push(pr_number);
        }
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    use rusqlite::Connection;

    fn db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        std::mem::forget(dir);
        conn
    }

    fn seed_session(conn: &Connection, sid: &str, model: &str, cost: f64, input: i64, output: i64) {
        conn.execute(
            "INSERT INTO sessions(session_id, agent_id, hostname, model)
                VALUES(?1, 'claude-code', 'h', ?2)",
            params![sid, model],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO turns(session_id, turn_idx, model, input_tokens, output_tokens,
                              cache_read_tokens, cache_write_5m_tokens, cache_write_1h_tokens,
                              cost_usd_api_equiv, web_search_count)
                VALUES(?1, 0, ?2, ?3, ?4, 0, 0, 0, ?5, 0)",
            params![sid, model, input, output, cost],
        )
        .unwrap();
    }

    fn seed_attribution(
        conn: &Connection,
        sid: &str,
        cwd: &str,
        file: &str,
        commit_sha: Option<&str>,
        line_start: i64,
        line_end: i64,
    ) {
        // Match tool_calls(session_id, tool_use_id) for backfill tests.
        let tu = format!("tu_{}_{}_{}", sid, line_start, line_end);
        conn.execute(
            "INSERT INTO tool_calls(session_id, tool_use_id, tool_name, file_path, ts_ms,
                                    status, lines_added, lines_removed)
                VALUES(?1, ?2, 'Edit', ?3, 0, 'success', ?4, 0)",
            params![sid, tu, file, line_end - line_start + 1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attributions(commit_sha, cwd, file_path, line_start, line_end,
                                      session_id, tool_use_id, author_id)
                VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                commit_sha,
                cwd,
                file,
                line_start,
                line_end,
                sid,
                tu,
                format!("ai:claude-code:{sid}"),
            ],
        )
        .unwrap();
    }

    #[test]
    fn compute_totals_prorates_when_session_split_across_prs() {
        let conn = db();
        // Session A: $1.00 across 10 lines — 6 lines on commitX (PR 7), 4 on commitY (out of PR).
        seed_session(&conn, "A", "claude-opus-4-7", 1.00, 1000, 500);
        seed_attribution(&conn, "A", "/r", "a.rs", Some("X"), 1, 6); // 6 lines
        seed_attribution(&conn, "A", "/r", "b.rs", Some("Y"), 1, 4); // 4 lines

        // Register PR 7 + commit X.
        conn.execute(
            "INSERT INTO pull_requests(cwd, pr_number, branch, first_seen_at_ms, last_seen_at_ms)
                VALUES('/r', 7, 'feat', 100, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pr_commits(cwd, pr_number, commit_sha) VALUES('/r', 7, 'X')",
            [],
        )
        .unwrap();

        let totals = compute_totals(&conn, "/r", 7).unwrap();
        // share = 6/10 → cost = $0.60, input = 600, output = 300, ai_lines = 6
        assert!(
            (totals.total_cost_usd_api_equiv - 0.60).abs() < 1e-9,
            "{}",
            totals.total_cost_usd_api_equiv
        );
        assert_eq!(totals.input_tokens, 600);
        assert_eq!(totals.output_tokens, 300);
        assert_eq!(totals.ai_lines_added, 6);
        assert_eq!(totals.session_count, 1);
        assert_eq!(totals.commit_count, 1);
    }

    #[test]
    fn compute_totals_zero_when_no_pr_attributions() {
        let conn = db();
        seed_session(&conn, "A", "m", 1.00, 100, 100);
        // Register PR with a commit but no attributions on it.
        conn.execute(
            "INSERT INTO pull_requests(cwd, pr_number, branch, first_seen_at_ms, last_seen_at_ms)
                VALUES('/r', 7, 'feat', 100, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pr_commits(cwd, pr_number, commit_sha) VALUES('/r', 7, 'X')",
            [],
        )
        .unwrap();
        let totals = compute_totals(&conn, "/r", 7).unwrap();
        assert!((totals.total_cost_usd_api_equiv - 0.0).abs() < 1e-9);
        assert_eq!(totals.session_count, 0);
        assert_eq!(totals.commit_count, 1);
    }

    #[test]
    fn recompute_rollup_persists_row() {
        let conn = db();
        seed_session(&conn, "A", "m", 0.50, 100, 100);
        seed_attribution(&conn, "A", "/r", "a.rs", Some("X"), 1, 4);
        conn.execute(
            "INSERT INTO pull_requests(cwd, pr_number, branch, first_seen_at_ms, last_seen_at_ms)
                VALUES('/r', 1, 'feat', 100, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pr_commits(cwd, pr_number, commit_sha) VALUES('/r', 1, 'X')",
            [],
        )
        .unwrap();
        recompute_rollup(&conn, "/r", 1, 999).unwrap();
        let (cost, last): (f64, i64) = conn
            .query_row(
                "SELECT total_cost_usd_api_equiv, last_computed_at_ms
                   FROM pr_cost_rollups WHERE cwd='/r' AND pr_number=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // Single session entirely on the PR → share = 1.0.
        assert!((cost - 0.50).abs() < 1e-9);
        assert_eq!(last, 999);
    }
}
