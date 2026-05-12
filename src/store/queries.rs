//! Typed write helpers — one function per call site in the handlers.
//!
//! Every helper is idempotent (`INSERT OR IGNORE` or
//! `INSERT … ON CONFLICT … DO UPDATE`) so retried hook calls never duplicate.

use anyhow::Result;
use rusqlite::{params, Connection};

/// Status values for `tool_calls.status`.
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_SUCCESS: &str = "success";
pub const STATUS_FAILURE: &str = "failure";

/// Token breakdown for one assistant turn.
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnTokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write_5m: i64,
    pub cache_write_1h: i64,
}

#[derive(Debug, Clone, Default)]
pub struct SessionInit<'a> {
    pub session_id: &'a str,
    pub agent_id: &'a str,
    pub started_at: Option<i64>,
    pub cwd: Option<&'a str>,
    pub model: Option<&'a str>,
    pub os: Option<&'a str>,
    pub hostname: Option<&'a str>,
    pub cc_version: Option<&'a str>,
    pub billing_mode: Option<&'a str>,
}

pub fn upsert_session(conn: &Connection, s: &SessionInit) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions(session_id, agent_id, started_at, cwd, model,
                              os, hostname, cc_version, billing_mode)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, COALESCE(?9, 'unknown'))
         ON CONFLICT(session_id) DO UPDATE SET
            agent_id     = excluded.agent_id,
            started_at   = COALESCE(sessions.started_at, excluded.started_at),
            cwd          = COALESCE(excluded.cwd,        sessions.cwd),
            model        = COALESCE(excluded.model,      sessions.model),
            os           = COALESCE(excluded.os,         sessions.os),
            hostname     = COALESCE(excluded.hostname,   sessions.hostname),
            cc_version   = COALESCE(excluded.cc_version, sessions.cc_version),
            billing_mode = CASE WHEN sessions.billing_mode = 'unknown'
                                THEN COALESCE(excluded.billing_mode, 'unknown')
                                ELSE sessions.billing_mode END",
        params![
            s.session_id,
            s.agent_id,
            s.started_at,
            s.cwd,
            s.model,
            s.os,
            s.hostname,
            s.cc_version,
            s.billing_mode,
        ],
    )?;
    Ok(())
}

pub fn end_session(conn: &Connection, session_id: &str, ended_at: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions(session_id, agent_id, ended_at)
              VALUES(?1, '', ?2)
         ON CONFLICT(session_id) DO UPDATE SET ended_at = excluded.ended_at",
        params![session_id, ended_at],
    )?;
    Ok(())
}

pub fn record_pre_tool(
    conn: &Connection,
    session_id: &str,
    tool_use_id: &str,
    tool_name: &str,
    file_path: Option<&str>,
    ts_ms: i64,
    pre_blob_sha: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO tool_calls(session_id, tool_use_id, tool_name, file_path,
                                ts_ms, pre_blob_sha, status)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'pending')
         ON CONFLICT(session_id, tool_use_id) DO UPDATE SET
            tool_name    = excluded.tool_name,
            file_path    = excluded.file_path,
            pre_blob_sha = excluded.pre_blob_sha",
        params![
            session_id,
            tool_use_id,
            tool_name,
            file_path,
            ts_ms,
            pre_blob_sha
        ],
    )?;
    Ok(())
}

pub fn record_post_tool(
    conn: &Connection,
    session_id: &str,
    tool_use_id: &str,
    end_ms: i64,
    post_blob_sha: &str,
    lines_added: i64,
    lines_removed: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE tool_calls
            SET end_ms        = ?3,
                post_blob_sha = ?4,
                lines_added   = ?5,
                lines_removed = ?6,
                status        = 'success'
          WHERE session_id  = ?1
            AND tool_use_id = ?2",
        params![
            session_id,
            tool_use_id,
            end_ms,
            post_blob_sha,
            lines_added,
            lines_removed
        ],
    )?;
    Ok(())
}

pub fn mark_tool_failure(
    conn: &Connection,
    session_id: &str,
    tool_use_id: &str,
    end_ms: i64,
    error_message: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE tool_calls
            SET status        = 'failure',
                end_ms        = ?3,
                error_message = ?4
          WHERE session_id  = ?1
            AND tool_use_id = ?2",
        params![session_id, tool_use_id, end_ms, error_message],
    )?;
    Ok(())
}

pub fn get_pre_blob_sha(
    conn: &Connection,
    session_id: &str,
    tool_use_id: &str,
) -> Result<Option<String>> {
    let row = conn
        .query_row(
            "SELECT pre_blob_sha FROM tool_calls
              WHERE session_id = ?1 AND tool_use_id = ?2",
            params![session_id, tool_use_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    Ok(row)
}

/// One attribution row to insert. Kept as a struct so the helper stays
/// under clippy's 7-arg threshold and call sites read clearly at a glance.
#[derive(Debug, Clone)]
pub struct AttributionRow<'a> {
    pub cwd: &'a str,
    pub file_path: &'a str,
    pub line_start: i64,
    pub line_end: i64,
    pub session_id: &'a str,
    pub tool_use_id: &'a str,
    pub author_id: &'a str,
}

pub fn insert_attribution(conn: &Connection, a: &AttributionRow) -> Result<()> {
    conn.execute(
        "INSERT INTO attributions(cwd, file_path, line_start, line_end,
                                  session_id, tool_use_id, author_id)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            a.cwd,
            a.file_path,
            a.line_start,
            a.line_end,
            a.session_id,
            a.tool_use_id,
            a.author_id,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_turn(
    conn: &Connection,
    session_id: &str,
    turn_idx: i64,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    model: Option<&str>,
    tokens: &TurnTokens,
    cost_usd_api_equiv: Option<f64>,
    pricing_version: Option<i64>,
    service_tier: Option<&str>,
    web_search_count: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO turns(session_id, turn_idx, started_at_ms, ended_at_ms, model,
                           input_tokens, output_tokens, cache_read_tokens,
                           cache_write_5m_tokens, cache_write_1h_tokens,
                           cost_usd_api_equiv, pricing_version, service_tier,
                           web_search_count)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(session_id, turn_idx) DO UPDATE SET
            started_at_ms          = excluded.started_at_ms,
            ended_at_ms            = excluded.ended_at_ms,
            model                  = excluded.model,
            input_tokens           = excluded.input_tokens,
            output_tokens          = excluded.output_tokens,
            cache_read_tokens      = excluded.cache_read_tokens,
            cache_write_5m_tokens  = excluded.cache_write_5m_tokens,
            cache_write_1h_tokens  = excluded.cache_write_1h_tokens,
            cost_usd_api_equiv     = excluded.cost_usd_api_equiv,
            pricing_version        = excluded.pricing_version,
            service_tier           = excluded.service_tier,
            web_search_count       = excluded.web_search_count",
        params![
            session_id,
            turn_idx,
            started_at_ms,
            ended_at_ms,
            model,
            tokens.input,
            tokens.output,
            tokens.cache_read,
            tokens.cache_write_5m,
            tokens.cache_write_1h,
            cost_usd_api_equiv,
            pricing_version,
            service_tier,
            web_search_count,
        ],
    )?;
    Ok(())
}

// ─── v2: commits + commit_files ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CommitRow<'a> {
    pub cwd: &'a str,
    pub commit_sha: &'a str,
    pub authored_at_ms: Option<i64>,
    pub branch: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub additions: i64,
    pub deletions: i64,
    pub files_touched: i64,
    pub captured_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct CommitFileRow<'a> {
    pub cwd: &'a str,
    pub commit_sha: &'a str,
    pub file_path: &'a str,
    pub additions: i64,
    pub deletions: i64,
}

/// Idempotent insert. If a row for `(cwd, sha)` already exists we leave it —
/// commit content never changes after capture.
pub fn upsert_commit(conn: &Connection, c: &CommitRow) -> Result<()> {
    conn.execute(
        "INSERT INTO commits(cwd, commit_sha, authored_at_ms, branch, subject,
                             additions, deletions, files_touched, captured_at_ms)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(cwd, commit_sha) DO UPDATE SET
            authored_at_ms = COALESCE(excluded.authored_at_ms, commits.authored_at_ms),
            branch         = COALESCE(excluded.branch,         commits.branch),
            subject        = COALESCE(excluded.subject,        commits.subject),
            additions      = excluded.additions,
            deletions      = excluded.deletions,
            files_touched  = excluded.files_touched",
        params![
            c.cwd,
            c.commit_sha,
            c.authored_at_ms,
            c.branch,
            c.subject,
            c.additions,
            c.deletions,
            c.files_touched,
            c.captured_at_ms,
        ],
    )?;
    Ok(())
}

pub fn upsert_commit_file(conn: &Connection, f: &CommitFileRow) -> Result<()> {
    conn.execute(
        "INSERT INTO commit_files(cwd, commit_sha, file_path, additions, deletions)
              VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(cwd, commit_sha, file_path) DO UPDATE SET
            additions = excluded.additions,
            deletions = excluded.deletions",
        params![f.cwd, f.commit_sha, f.file_path, f.additions, f.deletions],
    )?;
    Ok(())
}

/// Backfill `attributions.commit_sha` for any rows whose `(cwd, file_path)`
/// matches a file in the just-captured commit, whose `commit_sha IS NULL`, and
/// whose underlying `tool_call` ts falls between `since_ms` (inclusive) and
/// `commit_ts_ms` (inclusive). The session_id chain is `attributions →
/// tool_calls.(session_id, tool_use_id) → tool_calls.ts_ms`.
pub fn backfill_attribution_commits(
    conn: &Connection,
    cwd: &str,
    commit_sha: &str,
    since_ms: i64,
    commit_ts_ms: i64,
) -> Result<usize> {
    // attributions.file_path is absolute (the path Claude Code passed to
    // Edit/Write); commit_files.file_path is repo-relative (`git log
    // --numstat` output). Strip the cwd prefix from the attribution before
    // comparing.
    let n = conn.execute(
        "UPDATE attributions
            SET commit_sha = ?2
          WHERE commit_sha IS NULL
            AND cwd = ?1
            AND EXISTS (
                SELECT 1 FROM commit_files cf
                 WHERE cf.commit_sha = ?2
                   AND cf.cwd        = attributions.cwd
                   AND cf.file_path  = substr(attributions.file_path, length(attributions.cwd) + 2)
            )
            AND EXISTS (
                SELECT 1 FROM tool_calls tc
                 WHERE tc.session_id  = attributions.session_id
                   AND tc.tool_use_id = attributions.tool_use_id
                   AND tc.ts_ms BETWEEN ?3 AND ?4
            )",
        params![cwd, commit_sha, since_ms, commit_ts_ms],
    )?;
    Ok(n)
}

/// Most recent captured commit ts in `cwd`, used as the lower bound for
/// `backfill_attribution_commits`. None → fall back to 0 (capture everything).
pub fn last_captured_commit_ts(conn: &Connection, cwd: &str) -> Result<Option<i64>> {
    let row: Option<i64> = conn
        .query_row(
            "SELECT MAX(captured_at_ms) FROM commits WHERE cwd = ?1",
            params![cwd],
            |r| r.get(0),
        )
        .ok();
    Ok(row)
}

// ─── v2: pull_requests + pr_commits ──────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PullRequestRow<'a> {
    pub cwd: &'a str,
    pub pr_number: i64,
    pub repo_remote_url: Option<&'a str>,
    pub repo_basename: Option<&'a str>,
    pub branch: &'a str,
    pub base_branch: Option<&'a str>,
    pub head_sha: Option<&'a str>,
    pub now_ms: i64,
}

/// Insert a brand-new PR row, or update head_sha + last_seen_at on a known one.
/// Returns true if a new row was inserted (the moment to write a `first_seen`
/// snapshot and recompute the rollup).
pub fn upsert_pull_request(conn: &Connection, p: &PullRequestRow) -> Result<bool> {
    let existed: bool = conn
        .query_row(
            "SELECT 1 FROM pull_requests WHERE cwd = ?1 AND pr_number = ?2",
            params![p.cwd, p.pr_number],
            |_| Ok(true),
        )
        .unwrap_or(false);
    conn.execute(
        "INSERT INTO pull_requests(cwd, pr_number, repo_remote_url, repo_basename,
                                   branch, base_branch, head_sha,
                                   first_seen_at_ms, last_seen_at_ms, state)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 'open')
         ON CONFLICT(cwd, pr_number) DO UPDATE SET
            repo_remote_url = COALESCE(excluded.repo_remote_url, pull_requests.repo_remote_url),
            repo_basename   = COALESCE(excluded.repo_basename,   pull_requests.repo_basename),
            branch          = excluded.branch,
            base_branch     = COALESCE(excluded.base_branch,     pull_requests.base_branch),
            head_sha        = COALESCE(excluded.head_sha,        pull_requests.head_sha),
            last_seen_at_ms = excluded.last_seen_at_ms",
        params![
            p.cwd,
            p.pr_number,
            p.repo_remote_url,
            p.repo_basename,
            p.branch,
            p.base_branch,
            p.head_sha,
            p.now_ms,
        ],
    )?;
    Ok(!existed)
}

pub fn upsert_pr_commit(
    conn: &Connection,
    cwd: &str,
    pr_number: i64,
    commit_sha: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pr_commits(cwd, pr_number, commit_sha)
              VALUES(?1, ?2, ?3)",
        params![cwd, pr_number, commit_sha],
    )?;
    Ok(())
}

pub fn mark_pr_merged(
    conn: &Connection,
    cwd: &str,
    pr_number: i64,
    merged_at_ms: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE pull_requests SET state = 'merged', merged_at_ms = ?3
          WHERE cwd = ?1 AND pr_number = ?2",
        params![cwd, pr_number, merged_at_ms],
    )?;
    Ok(())
}

/// Open PRs in `cwd`, optionally filtered to a specific branch.
pub fn open_prs(conn: &Connection, cwd: &str, branch: Option<&str>) -> Result<Vec<(i64, String)>> {
    let mut rows = Vec::new();
    if let Some(b) = branch {
        let mut stmt = conn.prepare(
            "SELECT pr_number, branch FROM pull_requests
              WHERE cwd = ?1 AND state = 'open' AND branch = ?2",
        )?;
        let iter = stmt.query_map(params![cwd, b], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in iter {
            rows.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT pr_number, branch FROM pull_requests
              WHERE cwd = ?1 AND state = 'open'",
        )?;
        let iter = stmt.query_map(params![cwd], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in iter {
            rows.push(row?);
        }
    }
    Ok(rows)
}

// ─── v2: pr_cost_rollups + pr_cost_snapshots ─────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct PrTotals {
    pub total_cost_usd_api_equiv: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_5m_tokens: i64,
    pub cache_write_1h_tokens: i64,
    pub ai_lines_added: i64,
    pub ai_lines_removed: i64,
    pub session_count: i64,
    pub commit_count: i64,
}

pub fn upsert_pr_rollup(
    conn: &Connection,
    cwd: &str,
    pr_number: i64,
    totals: &PrTotals,
    now_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pr_cost_rollups(cwd, pr_number, total_cost_usd_api_equiv,
                input_tokens, output_tokens, cache_read_tokens,
                cache_write_5m_tokens, cache_write_1h_tokens,
                ai_lines_added, ai_lines_removed,
                session_count, commit_count, last_computed_at_ms)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(cwd, pr_number) DO UPDATE SET
            total_cost_usd_api_equiv = excluded.total_cost_usd_api_equiv,
            input_tokens             = excluded.input_tokens,
            output_tokens            = excluded.output_tokens,
            cache_read_tokens        = excluded.cache_read_tokens,
            cache_write_5m_tokens    = excluded.cache_write_5m_tokens,
            cache_write_1h_tokens    = excluded.cache_write_1h_tokens,
            ai_lines_added           = excluded.ai_lines_added,
            ai_lines_removed         = excluded.ai_lines_removed,
            session_count            = excluded.session_count,
            commit_count             = excluded.commit_count,
            last_computed_at_ms      = excluded.last_computed_at_ms",
        params![
            cwd,
            pr_number,
            totals.total_cost_usd_api_equiv,
            totals.input_tokens,
            totals.output_tokens,
            totals.cache_read_tokens,
            totals.cache_write_5m_tokens,
            totals.cache_write_1h_tokens,
            totals.ai_lines_added,
            totals.ai_lines_removed,
            totals.session_count,
            totals.commit_count,
            now_ms,
        ],
    )?;
    Ok(())
}

/// Insert-only — snapshots are frozen at lifecycle moments. No-op if a row for
/// `(cwd, pr_number, snapshot_kind)` already exists.
pub fn insert_pr_snapshot(
    conn: &Connection,
    cwd: &str,
    pr_number: i64,
    snapshot_kind: &str,
    snapshot_at_ms: i64,
    totals: &PrTotals,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pr_cost_snapshots(cwd, pr_number, snapshot_kind, snapshot_at_ms,
                total_cost_usd_api_equiv,
                input_tokens, output_tokens, cache_read_tokens,
                cache_write_5m_tokens, cache_write_1h_tokens,
                ai_lines_added, ai_lines_removed)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            cwd,
            pr_number,
            snapshot_kind,
            snapshot_at_ms,
            totals.total_cost_usd_api_equiv,
            totals.input_tokens,
            totals.output_tokens,
            totals.cache_read_tokens,
            totals.cache_write_5m_tokens,
            totals.cache_write_1h_tokens,
            totals.ai_lines_added,
            totals.ai_lines_removed,
        ],
    )?;
    Ok(())
}

// ─── v2: git_poll_state ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PollState {
    pub last_polled_at_ms: i64,
    pub last_remote_head_sha: Option<String>,
    pub last_pr_number: Option<i64>,
    pub last_negative_at_ms: Option<i64>,
}

pub fn get_poll_state(conn: &Connection, cwd: &str, branch: &str) -> Result<Option<PollState>> {
    let row = conn
        .query_row(
            "SELECT last_polled_at_ms, last_remote_head_sha, last_pr_number, last_negative_at_ms
               FROM git_poll_state WHERE cwd = ?1 AND branch = ?2",
            params![cwd, branch],
            |r| {
                Ok(PollState {
                    last_polled_at_ms: r.get(0)?,
                    last_remote_head_sha: r.get(1)?,
                    last_pr_number: r.get(2)?,
                    last_negative_at_ms: r.get(3)?,
                })
            },
        )
        .ok();
    Ok(row)
}

pub fn set_poll_state(conn: &Connection, cwd: &str, branch: &str, state: &PollState) -> Result<()> {
    conn.execute(
        "INSERT INTO git_poll_state(cwd, branch, last_polled_at_ms,
                last_remote_head_sha, last_pr_number, last_negative_at_ms)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(cwd, branch) DO UPDATE SET
            last_polled_at_ms    = excluded.last_polled_at_ms,
            last_remote_head_sha = excluded.last_remote_head_sha,
            last_pr_number       = excluded.last_pr_number,
            last_negative_at_ms  = excluded.last_negative_at_ms",
        params![
            cwd,
            branch,
            state.last_polled_at_ms,
            state.last_remote_head_sha,
            state.last_pr_number,
            state.last_negative_at_ms,
        ],
    )?;
    Ok(())
}

/// Mark the cached remote head sha stale so the next poll doesn't short-circuit.
pub fn invalidate_poll_state(conn: &Connection, cwd: &str, branch: &str) -> Result<()> {
    conn.execute(
        "UPDATE git_poll_state
            SET last_remote_head_sha = NULL,
                last_negative_at_ms  = NULL
          WHERE cwd = ?1 AND branch = ?2",
        params![cwd, branch],
    )?;
    Ok(())
}

// ─── v2: config ──────────────────────────────────────────────────────────

pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>> {
    let row = conn
        .query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .ok();
    Ok(row)
}

pub fn set_config(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO config(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn list_config(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT key, value FROM config ORDER BY key")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Read an i64-valued config key. Returns `None` if the key is missing or
/// the stored value is not parseable as an integer.
pub fn get_config_i64(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(get_config(conn, key)?.and_then(|v| v.parse::<i64>().ok()))
}

pub fn set_config_i64(conn: &Connection, key: &str, value: i64) -> Result<()> {
    set_config(conn, key, &value.to_string())
}

pub fn delete_config(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM config WHERE key = ?1", params![key])?;
    Ok(())
}

// ─── v3: agent_turns ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct AgentTurnRow<'a> {
    pub session_id: &'a str,
    pub user_turn_idx: i64,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub api_call_count: i64,
    pub cost_usd_api_equiv: f64,
    pub tokens: TurnTokens,
    pub category: &'a str,
    pub classifier_version: i64,
    pub classifier_tier: &'a str,
    pub computed_at_ms: i64,
}

#[allow(clippy::too_many_arguments)]
pub fn upsert_agent_turn(conn: &Connection, r: &AgentTurnRow) -> Result<()> {
    conn.execute(
        "INSERT INTO agent_turns(session_id, user_turn_idx, started_at_ms, ended_at_ms,
                                 api_call_count, cost_usd_api_equiv,
                                 input_tokens, output_tokens, cache_read_tokens,
                                 cache_write_5m_tokens, cache_write_1h_tokens,
                                 category, classifier_version, classifier_tier, computed_at_ms)
              VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(session_id, user_turn_idx) DO UPDATE SET
            started_at_ms          = excluded.started_at_ms,
            ended_at_ms            = excluded.ended_at_ms,
            api_call_count         = excluded.api_call_count,
            cost_usd_api_equiv     = excluded.cost_usd_api_equiv,
            input_tokens           = excluded.input_tokens,
            output_tokens          = excluded.output_tokens,
            cache_read_tokens      = excluded.cache_read_tokens,
            cache_write_5m_tokens  = excluded.cache_write_5m_tokens,
            cache_write_1h_tokens  = excluded.cache_write_1h_tokens,
            category               = excluded.category,
            classifier_version     = excluded.classifier_version,
            classifier_tier        = excluded.classifier_tier,
            computed_at_ms         = excluded.computed_at_ms",
        params![
            r.session_id,
            r.user_turn_idx,
            r.started_at_ms,
            r.ended_at_ms,
            r.api_call_count,
            r.cost_usd_api_equiv,
            r.tokens.input,
            r.tokens.output,
            r.tokens.cache_read,
            r.tokens.cache_write_5m,
            r.tokens.cache_write_1h,
            r.category,
            r.classifier_version,
            r.classifier_tier,
            r.computed_at_ms,
        ],
    )?;
    Ok(())
}

pub fn delete_agent_turns_for_session(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM agent_turns WHERE session_id = ?1",
        params![session_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open;

    fn db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        let conn = open(&path).unwrap();
        // tempdir lives until end of test process; conn keeps the path open.
        std::mem::forget(dir);
        conn
    }

    #[test]
    fn upsert_session_creates_then_updates() {
        let conn = db();
        upsert_session(
            &conn,
            &SessionInit {
                session_id: "s1",
                agent_id: "claude-code",
                started_at: Some(1000),
                cwd: Some("/tmp"),
                ..SessionInit::default()
            },
        )
        .unwrap();
        upsert_session(
            &conn,
            &SessionInit {
                session_id: "s1",
                agent_id: "claude-code",
                model: Some("claude-opus-4-7"),
                ..SessionInit::default()
            },
        )
        .unwrap();
        let (started, model): (i64, String) = conn
            .query_row(
                "SELECT started_at, model FROM sessions WHERE session_id='s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(started, 1000);
        assert_eq!(model, "claude-opus-4-7");
    }

    #[test]
    fn end_session_works_without_prior_start() {
        let conn = db();
        end_session(&conn, "s2", 5000).unwrap();
        let ended: i64 = conn
            .query_row(
                "SELECT ended_at FROM sessions WHERE session_id='s2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ended, 5000);
    }

    #[test]
    fn pre_then_post_tool_round_trips() {
        let conn = db();
        record_pre_tool(&conn, "s", "t1", "Edit", Some("a.rs"), 100, Some("aaa")).unwrap();
        assert_eq!(
            get_pre_blob_sha(&conn, "s", "t1").unwrap().as_deref(),
            Some("aaa")
        );
        record_post_tool(&conn, "s", "t1", 200, "bbb", 5, 1).unwrap();
        let (status, post, end_ms): (String, String, i64) = conn
            .query_row(
                "SELECT status, post_blob_sha, end_ms FROM tool_calls
                  WHERE session_id='s' AND tool_use_id='t1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "success");
        assert_eq!(post, "bbb");
        assert_eq!(end_ms, 200);
    }

    #[test]
    fn mark_tool_failure_overrides_status() {
        let conn = db();
        record_pre_tool(&conn, "s", "t1", "Edit", Some("a.rs"), 100, None).unwrap();
        mark_tool_failure(&conn, "s", "t1", 200, Some("boom")).unwrap();
        let (status, err): (String, String) = conn
            .query_row(
                "SELECT status, error_message FROM tool_calls
                  WHERE session_id='s' AND tool_use_id='t1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failure");
        assert_eq!(err, "boom");
    }

    #[test]
    fn insert_turn_is_idempotent_via_pk() {
        let conn = db();
        let t = TurnTokens {
            input: 100,
            output: 50,
            ..TurnTokens::default()
        };
        insert_turn(
            &conn,
            "s",
            0,
            Some(1000),
            Some(2000),
            Some("claude-opus-4-7"),
            &t,
            Some(0.005),
            Some(1),
            Some("standard"),
            0,
        )
        .unwrap();
        // Second call updates in place (model changed).
        insert_turn(
            &conn,
            "s",
            0,
            Some(1000),
            Some(2000),
            Some("claude-opus-4-7-20260301"),
            &t,
            Some(0.0051),
            Some(1),
            Some("standard"),
            0,
        )
        .unwrap();
        let (model, cost): (String, f64) = conn
            .query_row(
                "SELECT model, cost_usd_api_equiv FROM turns
                  WHERE session_id='s' AND turn_idx=0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "claude-opus-4-7-20260301");
        assert!((cost - 0.0051).abs() < 1e-9);
    }

    #[test]
    fn commit_upsert_is_idempotent_and_updates_diffstats() {
        let conn = db();
        let row = CommitRow {
            cwd: "/r",
            commit_sha: "abc",
            authored_at_ms: Some(100),
            branch: Some("main"),
            subject: Some("init"),
            additions: 5,
            deletions: 0,
            files_touched: 1,
            captured_at_ms: 200,
        };
        upsert_commit(&conn, &row).unwrap();
        upsert_commit(
            &conn,
            &CommitRow {
                additions: 7,
                deletions: 1,
                ..row
            },
        )
        .unwrap();
        let (a, d, n): (i64, i64, i64) = conn
            .query_row(
                "SELECT additions, deletions, COUNT(*) FROM commits WHERE commit_sha='abc'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((a, d, n), (7, 1, 1));
    }

    #[test]
    fn upsert_pull_request_returns_true_only_on_first_insert() {
        let conn = db();
        let row = PullRequestRow {
            cwd: "/r",
            pr_number: 7,
            repo_remote_url: Some("git@github.com:o/r.git"),
            repo_basename: Some("r"),
            branch: "feat/x",
            base_branch: Some("main"),
            head_sha: Some("abc"),
            now_ms: 1000,
        };
        assert!(upsert_pull_request(&conn, &row).unwrap());
        // Second call updates last_seen_at but is not a "new" PR.
        assert!(!upsert_pull_request(
            &conn,
            &PullRequestRow {
                now_ms: 2000,
                ..row
            }
        )
        .unwrap());
        let last: i64 = conn
            .query_row(
                "SELECT last_seen_at_ms FROM pull_requests WHERE pr_number=7",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last, 2000);
    }

    #[test]
    fn pr_snapshot_is_frozen_after_first_insert() {
        let conn = db();
        let totals_a = PrTotals {
            total_cost_usd_api_equiv: 1.0,
            input_tokens: 100,
            ..PrTotals::default()
        };
        insert_pr_snapshot(&conn, "/r", 7, "first_seen", 1000, &totals_a).unwrap();
        // A "later" snapshot of the same kind must not overwrite — it's frozen.
        let totals_b = PrTotals {
            total_cost_usd_api_equiv: 2.0,
            ..totals_a
        };
        insert_pr_snapshot(&conn, "/r", 7, "first_seen", 2000, &totals_b).unwrap();
        let cost: f64 = conn
            .query_row(
                "SELECT total_cost_usd_api_equiv FROM pr_cost_snapshots
                  WHERE pr_number=7 AND snapshot_kind='first_seen'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((cost - 1.0).abs() < 1e-9);
    }

    #[test]
    fn config_round_trip() {
        let conn = db();
        // Default-seeded by the migration.
        assert_eq!(
            get_config(&conn, "git_notes.enabled").unwrap().as_deref(),
            Some("false"),
        );
        set_config(&conn, "git_notes.enabled", "true").unwrap();
        assert_eq!(
            get_config(&conn, "git_notes.enabled").unwrap().as_deref(),
            Some("true"),
        );
        let all = list_config(&conn).unwrap();
        assert!(all
            .iter()
            .any(|(k, v)| k == "git_notes.enabled" && v == "true"));
    }

    #[test]
    fn poll_state_round_trip() {
        let conn = db();
        assert!(get_poll_state(&conn, "/r", "main").unwrap().is_none());
        set_poll_state(
            &conn,
            "/r",
            "main",
            &PollState {
                last_polled_at_ms: 100,
                last_remote_head_sha: Some("abc".into()),
                last_pr_number: Some(7),
                last_negative_at_ms: None,
            },
        )
        .unwrap();
        let s = get_poll_state(&conn, "/r", "main").unwrap().unwrap();
        assert_eq!(s.last_polled_at_ms, 100);
        assert_eq!(s.last_remote_head_sha.as_deref(), Some("abc"));
        assert_eq!(s.last_pr_number, Some(7));

        invalidate_poll_state(&conn, "/r", "main").unwrap();
        let s = get_poll_state(&conn, "/r", "main").unwrap().unwrap();
        assert!(s.last_remote_head_sha.is_none());
    }

    #[test]
    fn attribution_rows_independent() {
        let conn = db();
        let row = AttributionRow {
            cwd: "/r",
            file_path: "a.rs",
            line_start: 1,
            line_end: 5,
            session_id: "s",
            tool_use_id: "t",
            author_id: "ai:claude-code:s",
        };
        insert_attribution(&conn, &row).unwrap();
        insert_attribution(
            &conn,
            &AttributionRow {
                line_start: 10,
                line_end: 20,
                ..row
            },
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attributions WHERE session_id='s'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }
}
