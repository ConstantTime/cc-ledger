//! Phase 5 — sync engine.
//!
//! Reads local SQLite tables newer than `sync_cursor`, sanitizes each row
//! (drops `cwd`, hashes commit subjects, etc.), and POSTs batches to
//! `/sync/*` on ccledger.dev.
//!
//! Sanitization is the privacy boundary on the client side. The cloud's
//! TypeBox schemas are the second line of defense: they silently drop any
//! field they don't recognize, so a regression here cannot land forbidden
//! fields in Postgres.
//!
//! Triggered three ways:
//! - Manual: `cc-ledger sync` from a shell.
//! - Auto: `SessionEnd` hook forks `cc-ledger sync --background` if the
//!   user is authenticated.
//! - Forced full upload: `cc-ledger sync --reset-cursor`.
//!
//! Cursor model: the local `sync_cursor` table tracks the last successfully
//! pushed `(table, ts)` per syncable table. On startup we also fetch the
//! server's high-water-mark via `GET /sync/cursor` so a fresh laptop can
//! resume without re-uploading data the cloud already has.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::git::{self, RepoIdent};
use crate::{auth, config, paths, store};

/// Hard cap for batch size — must match the server's `MAX_BATCH`.
pub const BATCH_SIZE: usize = 500;

/// HTTP timeout for any single request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Tables we sync, in dependency order.
const SYNC_TABLES: &[&str] = &[
    "sessions",
    "tool_aggregates",
    "attributions",
    "commits",
    "pull_requests",
    "pr_snapshots",
    "agent_turns",
];

/// One pass of the engine. Returns per-table counts.
#[derive(Debug, Default, Serialize)]
pub struct SyncReport {
    pub sessions: usize,
    pub tool_aggregates: usize,
    pub attributions: usize,
    pub commits: usize,
    pub pull_requests: usize,
    pub pr_snapshots: usize,
    pub agent_turns: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SyncOptions {
    /// Print what's about to leave the machine and ask before sending.
    pub interactive: bool,
    /// Don't actually POST — just compute counts and exit. Implies non-interactive.
    pub dry_run: bool,
    /// Ignore local cursors; treat every row as un-synced.
    pub reset_cursor: bool,
}

#[derive(Debug, Deserialize, Default)]
struct ServerCursors {
    #[serde(default)]
    sessions_high_water_ms: i64,
    #[serde(default)]
    tool_aggregates_high_water_ms: i64,
    #[serde(default)]
    commits_high_water_ms: i64,
    #[serde(default)]
    pull_requests_high_water_ms: i64,
    #[serde(default)]
    agent_turns_high_water_ms: i64,
}

pub fn run(conn: &Connection, opts: SyncOptions) -> Result<SyncReport> {
    let tokens = auth::ensure_fresh_token()
        .context("not authenticated — run `cc-ledger auth` to enable sync")?;
    let bearer = tokens.access_token;
    let org_id = tokens.org_id.clone();
    let api = config::api_base();

    // Stamp the watermark at the start of the run so concurrent CLI
    // invocations don't all decide it's time to autosync at once. We stamp
    // again at the end with the post-success time.
    let _ = store::queries::set_config_i64(conn, "last_sync_at_ms", paths::now_ms());

    if opts.reset_cursor {
        clear_local_cursors(conn)?;
    }

    // Cursors: max(local, server) per table to avoid re-uploading rows the
    // cloud already has after a fresh laptop install.
    //
    // `--reset-cursor` skips the server-cursor fetch as well, otherwise a
    // forced re-upload would still be filtered down by `MAX(synced_at)` etc.
    // on the cloud and the user wouldn't actually re-send the data they
    // asked to re-send. Server-side upserts make the full replay safe.
    let server = if opts.reset_cursor {
        ServerCursors::default()
    } else {
        match fetch_server_cursors(&api, &bearer, org_id.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                // Non-fatal — fall back to local cursor only.
                eprintln!("cc-ledger: GET /sync/cursor failed ({e}); using local cursor only");
                ServerCursors::default()
            }
        }
    };

    let mut report = SyncReport::default();
    // One `git remote get-url origin` per cwd, cached for this sync run.
    // Resolves cwd → host/owner/repo_name to attach to every wire row.
    let mut repos = RepoCache::default();

    let sessions = collect_sessions(
        conn,
        max_cursor(conn, "sessions", server.sessions_high_water_ms)?,
        &mut repos,
    )?;
    if !sessions.is_empty() {
        if opts.interactive {
            print_summary("sessions", sessions.len());
        }
        if !opts.dry_run {
            post_batches(&api, &bearer, org_id.as_deref(), "sessions", &sessions)?;
            advance_cursor(conn, "sessions", paths::now_ms())?;
        }
        report.sessions = sessions.len();
    }

    let tool_aggs = collect_tool_aggregates(
        conn,
        max_cursor(
            conn,
            "tool_aggregates",
            server.tool_aggregates_high_water_ms,
        )?,
    )?;
    if !tool_aggs.is_empty() {
        if opts.interactive {
            print_summary("tool-aggregates", tool_aggs.len());
        }
        if !opts.dry_run {
            post_batches(
                &api,
                &bearer,
                org_id.as_deref(),
                "tool-aggregates",
                &tool_aggs,
            )?;
            advance_cursor(conn, "tool_aggregates", paths::now_ms())?;
        }
        report.tool_aggregates = tool_aggs.len();
    }

    let attrs = collect_attributions(conn, &mut repos)?;
    if !attrs.is_empty() {
        if opts.interactive {
            print_summary("attributions", attrs.len());
        }
        if !opts.dry_run {
            post_batches(&api, &bearer, org_id.as_deref(), "attributions", &attrs)?;
            advance_cursor(conn, "attributions", paths::now_ms())?;
        }
        report.attributions = attrs.len();
    }

    let cmts = collect_commits(
        conn,
        max_cursor(conn, "commits", server.commits_high_water_ms)?,
        &mut repos,
    )?;
    if !cmts.is_empty() {
        if opts.interactive {
            print_summary("commits", cmts.len());
        }
        if !opts.dry_run {
            post_batches(&api, &bearer, org_id.as_deref(), "commits", &cmts)?;
            advance_cursor(conn, "commits", paths::now_ms())?;
        }
        report.commits = cmts.len();
    }

    let prs = collect_pull_requests(
        conn,
        max_cursor(conn, "pull_requests", server.pull_requests_high_water_ms)?,
        &mut repos,
    )?;
    if !prs.is_empty() {
        if opts.interactive {
            print_summary("pull-requests", prs.len());
        }
        if !opts.dry_run {
            post_batches(&api, &bearer, org_id.as_deref(), "pull-requests", &prs)?;
            advance_cursor(conn, "pull_requests", paths::now_ms())?;
        }
        report.pull_requests = prs.len();
    }

    let snaps = collect_pr_snapshots(conn, max_cursor(conn, "pr_snapshots", 0)?, &mut repos)?;
    if !snaps.is_empty() {
        if opts.interactive {
            print_summary("pr-snapshots", snaps.len());
        }
        if !opts.dry_run {
            post_batches(&api, &bearer, org_id.as_deref(), "pr-snapshots", &snaps)?;
            advance_cursor(conn, "pr_snapshots", paths::now_ms())?;
        }
        report.pr_snapshots = snaps.len();
    }

    let agent_turns = collect_agent_turns(
        conn,
        max_cursor(conn, "agent_turns", server.agent_turns_high_water_ms)?,
    )?;
    if !agent_turns.is_empty() {
        if opts.interactive {
            print_summary("agent-turns", agent_turns.len());
        }
        if !opts.dry_run {
            post_batches(
                &api,
                &bearer,
                org_id.as_deref(),
                "agent-turns",
                &agent_turns,
            )?;
            advance_cursor(conn, "agent_turns", paths::now_ms())?;
        }
        report.agent_turns = agent_turns.len();
    }

    // Final watermark stamp — closes out the autosync window.
    let _ = store::queries::set_config_i64(conn, "last_sync_at_ms", paths::now_ms());
    let _ = crate::autosync::release_sync_slot(conn);

    Ok(report)
}

fn print_summary(kind: &str, count: usize) {
    eprintln!("  {} {kind} rows", count);
}

// ─── cursor helpers ────────────────────────────────────────────────────

fn max_cursor(conn: &Connection, table: &str, server_ms: i64) -> Result<i64> {
    let local: Option<i64> = conn
        .query_row(
            "SELECT last_pushed_ts_ms FROM sync_cursor WHERE table_name = ?1",
            params![table],
            |r| r.get(0),
        )
        .ok();
    Ok(local.unwrap_or(0).max(server_ms))
}

fn advance_cursor(conn: &Connection, table: &str, ts_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_cursor(table_name, last_pushed_ts_ms) VALUES(?1, ?2)
         ON CONFLICT(table_name) DO UPDATE SET last_pushed_ts_ms = excluded.last_pushed_ts_ms",
        params![table, ts_ms],
    )?;
    Ok(())
}

fn clear_local_cursors(conn: &Connection) -> Result<()> {
    for t in SYNC_TABLES {
        conn.execute("DELETE FROM sync_cursor WHERE table_name = ?1", params![t])?;
    }
    Ok(())
}

// ─── HTTP ──────────────────────────────────────────────────────────────

fn fetch_server_cursors(api: &str, bearer: &str, org: Option<&str>) -> Result<ServerCursors> {
    let url = format!("{api}/sync/cursor");
    let mut req = ureq::get(&url)
        .timeout(HTTP_TIMEOUT)
        .set("Authorization", &format!("Bearer {bearer}"));
    if let Some(o) = org {
        req = req.set("X-CC-Ledger-Org-Id", o);
    }
    let resp = req.call().map_err(|e| anyhow!("GET {url}: {e}"))?;
    resp.into_json::<ServerCursors>()
        .context("decoding /sync/cursor response")
}

fn post_batches(
    api: &str,
    bearer: &str,
    org: Option<&str>,
    slug: &str,
    rows: &[Value],
) -> Result<()> {
    let url = format!("{api}/sync/{slug}");
    for chunk in rows.chunks(BATCH_SIZE) {
        let body = json!({ "rows": chunk });
        let mut req = ureq::post(&url)
            .timeout(HTTP_TIMEOUT)
            .set("Authorization", &format!("Bearer {bearer}"))
            .set("Content-Type", "application/json");
        if let Some(o) = org {
            req = req.set("X-CC-Ledger-Org-Id", o);
        }
        let resp = req
            .send_json(body)
            .map_err(|e| anyhow!("POST {url}: {e}"))?;
        let status = resp.status();
        if !(200..300).contains(&status) {
            return Err(anyhow!("POST {url}: HTTP {status}"));
        }
    }
    Ok(())
}

// ─── collectors (sanitization happens here) ────────────────────────────

/// One `git remote get-url origin` lookup per cwd, parsed into a
/// `RepoIdent`. Misses (no remote / non-recognized URL) cache the negative
/// result so we don't re-shell out per row.
#[derive(Default)]
pub struct RepoCache {
    seen: HashMap<PathBuf, Option<RepoIdent>>,
}

impl RepoCache {
    pub fn lookup(&mut self, cwd: &str) -> Option<RepoIdent> {
        if cwd.is_empty() {
            return None;
        }
        let key = PathBuf::from(cwd);
        if let Some(v) = self.seen.get(&key) {
            return v.clone();
        }
        let parsed = git::repo_ident(&key);
        self.seen.insert(key, parsed.clone());
        parsed
    }
}

/// Pull `host`/`owner`/`repo_name` for a cwd as JSON-friendly Options. Used
/// inline in every wire row so `serde_json::Value::Null` lands when we don't
/// know.
fn repo_identity_fields(repos: &mut RepoCache, cwd: &str) -> (Value, Value, Value) {
    match repos.lookup(cwd) {
        Some(r) => (
            Value::String(r.host),
            Value::String(r.owner),
            Value::String(r.name),
        ),
        None => (Value::Null, Value::Null, Value::Null),
    }
}

/// SHA-256 of an absolute cwd, or `None` if cwd is missing/empty.
fn cwd_hash(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(cwd.as_bytes());
    Some(hex::encode(h.finalize()))
}

/// Basename of a path. None if cwd is empty.
fn cwd_basename(cwd: &str) -> Option<String> {
    Path::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
}

/// Convert an absolute path to a repo-relative path. Falls back to the input
/// if it isn't a child of cwd.
fn relativize(file_path: &str, cwd: &str) -> String {
    if cwd.is_empty() {
        return file_path.to_string();
    }
    let prefix = if cwd.ends_with('/') {
        cwd.to_string()
    } else {
        format!("{cwd}/")
    };
    file_path
        .strip_prefix(&prefix)
        .unwrap_or(file_path)
        .to_string()
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

fn collect_sessions(conn: &Connection, since_ms: i64, repos: &mut RepoCache) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        // `end_session` (queries.rs) inserts agent_id='' when SessionEnd
        // fires before SessionStart. The cloud schema requires minLength 1,
        // so coerce empty → 'claude-code' (the only agent today). Sessions
        // with no started_at AND no ended_at are skipped — they're orphans
        // we can't usefully attribute.
        "SELECT session_id,
                COALESCE(NULLIF(agent_id, ''), 'claude-code') AS agent_id,
                started_at,
                ended_at,
                cwd,
                model,
                billing_mode
           FROM sessions
          WHERE COALESCE(ended_at, started_at) IS NOT NULL
            AND COALESCE(ended_at, started_at, 0) > ?1
          ORDER BY COALESCE(ended_at, started_at, 0) ASC",
    )?;
    // sqlite-only loop: query_map yields rows; we then read row data into
    // owned types so we can call into RepoCache (which mutates) outside the
    // closure.
    let mut owned = Vec::new();
    let mut iter = stmt.query(params![since_ms])?;
    while let Some(r) = iter.next()? {
        owned.push((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ));
    }
    let out = owned
        .into_iter()
        .map(
            |(session_id, agent_id, started, ended, cwd, model, billing_mode)| {
                let (host, owner, repo_name) = repo_identity_fields(repos, &cwd);
                json!({
                    "session_id":     session_id,
                    "org_id":         Value::Null,
                    "device_id":      Value::Null,
                    "agent_id":       agent_id,
                    "repo_basename":  cwd_basename(&cwd),
                    "cwd_hash":       cwd_hash(&cwd),
                    "host":           host,
                    "owner":          owner,
                    "repo_name":      repo_name,
                    "model":          model,
                    "billing_mode":   billing_mode,
                    "started_at_ms":  started,
                    "ended_at_ms":    ended,
                })
            },
        )
        .collect();
    Ok(out)
}

fn collect_tool_aggregates(conn: &Connection, since_ms: i64) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, tool_name,
                COUNT(*)                                                   AS calls,
                SUM(CASE WHEN status='success' THEN 1 ELSE 0 END)          AS success_calls,
                SUM(CASE WHEN status='failure' THEN 1 ELSE 0 END)          AS failure_calls,
                COALESCE(SUM(lines_added), 0)                              AS lines_added,
                COALESCE(SUM(lines_removed), 0)                            AS lines_removed,
                MIN(ts_ms)                                                 AS first_ts_ms,
                MAX(COALESCE(end_ms, ts_ms))                               AS last_ts_ms
           FROM tool_calls
          WHERE COALESCE(end_ms, ts_ms) > ?1
          GROUP BY session_id, tool_name
          ORDER BY MAX(COALESCE(end_ms, ts_ms)) ASC",
    )?;
    let rows = stmt.query_map(params![since_ms], |r| {
        Ok(json!({
            "session_id":     r.get::<_, String>(0)?,
            "tool_name":      r.get::<_, String>(1)?,
            "calls":          r.get::<_, i64>(2)?,
            "success_calls":  r.get::<_, i64>(3)?,
            "failure_calls":  r.get::<_, i64>(4)?,
            "lines_added":    r.get::<_, i64>(5)?,
            "lines_removed":  r.get::<_, i64>(6)?,
            "first_ts_ms":    r.get::<_, Option<i64>>(7)?,
            "last_ts_ms":     r.get::<_, Option<i64>>(8)?,
        }))
    })?;
    rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
}

/// Attributions are aggregated per (session, cwd, file). cwd is stripped to
/// `repo_basename` and `file_path` is rewritten to repo-relative.
///
/// The cloud's `attribution_aggregates` pk is
/// `(user_id, session_id, repo_basename, file_path)` — no `commit_sha`. So we
/// must dedupe on (session, cwd, file) here, otherwise a session that edited
/// the same file across two commits produces two rows with the same pk and
/// Postgres rejects the bulk INSERT with 500. We pick the most recent
/// attributed `commit_sha` (by `commits.authored_at_ms` desc) and sum the
/// lines across all of this session's edits to that file.
fn collect_attributions(conn: &Connection, repos: &mut RepoCache) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT a.session_id,
                a.cwd,
                a.file_path,
                (SELECT a2.commit_sha
                   FROM attributions a2
                   LEFT JOIN commits c
                          ON c.cwd = a2.cwd AND c.commit_sha = a2.commit_sha
                  WHERE a2.session_id  = a.session_id
                    AND a2.cwd         = a.cwd
                    AND a2.file_path   = a.file_path
                    AND a2.commit_sha IS NOT NULL
                  ORDER BY COALESCE(c.authored_at_ms, c.captured_at_ms) DESC
                  LIMIT 1)                                  AS commit_sha,
                SUM(a.line_end - a.line_start + 1)          AS ai_lines_added
           FROM attributions a
          GROUP BY a.session_id, a.cwd, a.file_path",
    )?;
    let mut owned = Vec::new();
    let mut iter = stmt.query([])?;
    while let Some(r) = iter.next()? {
        owned.push((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, i64>(4)?,
        ));
    }
    let out = owned
        .into_iter()
        .filter_map(|(session_id, cwd, file_path, commit_sha, ai_lines)| {
            let basename = cwd_basename(&cwd)?;
            let (host, owner, repo_name) = repo_identity_fields(repos, &cwd);
            Some(json!({
                "session_id":     session_id,
                "repo_basename":  basename,
                "host":           host,
                "owner":          owner,
                "repo_name":      repo_name,
                "file_path":      relativize(&file_path, &cwd),
                "commit_sha":     commit_sha,
                "ai_lines_added": ai_lines,
            }))
        })
        .collect();
    Ok(out)
}

fn collect_commits(conn: &Connection, since_ms: i64, repos: &mut RepoCache) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT cwd, commit_sha, authored_at_ms, branch, subject,
                additions, deletions, files_touched
           FROM commits
          WHERE COALESCE(authored_at_ms, captured_at_ms) > ?1
          ORDER BY COALESCE(authored_at_ms, captured_at_ms) ASC",
    )?;
    let mut owned = Vec::new();
    let mut iter = stmt.query(params![since_ms])?;
    while let Some(r) = iter.next()? {
        owned.push((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
        ));
    }
    let out = owned
        .into_iter()
        .filter_map(|(cwd, sha, authored, branch, subject, adds, dels, files)| {
            let basename = cwd_basename(&cwd)?;
            let (host, owner, repo_name) = repo_identity_fields(repos, &cwd);
            Some(json!({
                "repo_basename":   basename,
                "host":            host,
                "owner":           owner,
                "repo_name":       repo_name,
                "commit_sha":      sha,
                "authored_at_ms":  authored,
                "branch":          branch,
                "subject_hash":    subject.as_deref().map(sha256_hex),
                "additions":       adds,
                "deletions":       dels,
                "files_touched":   files,
            }))
        })
        .collect();
    Ok(out)
}

fn collect_pull_requests(
    conn: &Connection,
    since_ms: i64,
    repos: &mut RepoCache,
) -> Result<Vec<Value>> {
    let mut prs_stmt = conn.prepare(
        "SELECT cwd, pr_number, branch, base_branch, head_sha,
                first_seen_at_ms, merged_at_ms, state
           FROM pull_requests
          WHERE last_seen_at_ms > ?1
          ORDER BY last_seen_at_ms ASC",
    )?;
    let mut commits_stmt =
        conn.prepare("SELECT commit_sha FROM pr_commits WHERE cwd = ?1 AND pr_number = ?2")?;

    let prs = prs_stmt.query_map(params![since_ms], |r| {
        Ok((
            r.get::<_, String>(0)?,         // cwd
            r.get::<_, i64>(1)?,            // pr_number
            r.get::<_, Option<String>>(2)?, // branch
            r.get::<_, Option<String>>(3)?, // base_branch
            r.get::<_, Option<String>>(4)?, // head_sha
            r.get::<_, i64>(5)?,            // first_seen_at_ms
            r.get::<_, Option<i64>>(6)?,    // merged_at_ms
            r.get::<_, String>(7)?,         // state
        ))
    })?;

    let mut out = Vec::new();
    for pr in prs {
        let (cwd, pr_n, branch, base, head, first_seen, merged, state) = pr?;
        let Some(basename) = cwd_basename(&cwd) else {
            continue;
        };
        let shas: Vec<String> = commits_stmt
            .query_map(params![cwd, pr_n], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        let (host, owner, repo_name) = repo_identity_fields(repos, &cwd);
        out.push(json!({
            "repo_basename":     basename,
            "host":              host,
            "owner":             owner,
            "repo_name":         repo_name,
            "pr_number":         pr_n,
            "branch":            branch,
            "base_branch":       base,
            "head_sha":          head,
            "first_seen_at_ms":  first_seen,
            "merged_at_ms":      merged,
            "state":             state,
            "commit_shas":       shas,
        }));
    }
    Ok(out)
}

fn collect_agent_turns(conn: &Connection, since_ms: i64) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, user_turn_idx, started_at_ms, ended_at_ms,
                api_call_count, cost_usd_api_equiv,
                input_tokens, output_tokens, cache_read_tokens,
                cache_write_5m_tokens, cache_write_1h_tokens,
                category, classifier_version
           FROM agent_turns
          WHERE COALESCE(started_at_ms, computed_at_ms) > ?1
          ORDER BY COALESCE(started_at_ms, computed_at_ms) ASC",
    )?;
    let rows = stmt.query_map(params![since_ms], |r| {
        Ok(json!({
            "session_id":            r.get::<_, String>(0)?,
            "user_turn_idx":         r.get::<_, i64>(1)?,
            "started_at_ms":         r.get::<_, Option<i64>>(2)?,
            "ended_at_ms":           r.get::<_, Option<i64>>(3)?,
            "api_call_count":        r.get::<_, i64>(4)?,
            "cost_usd_api_equiv":    r.get::<_, f64>(5)?,
            "input_tokens":          r.get::<_, i64>(6)?,
            "output_tokens":         r.get::<_, i64>(7)?,
            "cache_read_tokens":     r.get::<_, i64>(8)?,
            "cache_write_5m_tokens": r.get::<_, i64>(9)?,
            "cache_write_1h_tokens": r.get::<_, i64>(10)?,
            "category":              r.get::<_, String>(11)?,
            "classifier_version":    r.get::<_, i64>(12)?,
        }))
    })?;
    rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
}

fn collect_pr_snapshots(
    conn: &Connection,
    since_ms: i64,
    repos: &mut RepoCache,
) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT cwd, pr_number, snapshot_kind, snapshot_at_ms,
                total_cost_usd_api_equiv,
                input_tokens, output_tokens, cache_read_tokens,
                cache_write_5m_tokens, cache_write_1h_tokens,
                ai_lines_added, ai_lines_removed
           FROM pr_cost_snapshots
          WHERE snapshot_at_ms > ?1
          ORDER BY snapshot_at_ms ASC",
    )?;
    let rows = stmt.query_map(params![since_ms], |r| {
        let cwd: String = r.get(0)?;
        let basename = match cwd_basename(&cwd) {
            Some(b) => b,
            None => return Ok(None),
        };
        Ok(Some((
            cwd,
            basename,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, f64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, i64>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, i64>(10)?,
            r.get::<_, i64>(11)?,
        )))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let Some(tuple) = row? else { continue };
        let (cwd, basename, pr_n, kind, snap_at, cost, input, output, cr, cw5, cw1, added, removed) =
            tuple;
        let (host, owner, repo_name) = repo_identity_fields(repos, &cwd);
        out.push(json!({
            "repo_basename":            basename,
            "host":                     host,
            "owner":                    owner,
            "repo_name":                repo_name,
            "pr_number":                pr_n,
            "snapshot_kind":            kind,
            "snapshot_at_ms":           snap_at,
            "total_cost_usd_api_equiv": cost,
            "input_tokens":             input,
            "output_tokens":            output,
            "cache_read_tokens":        cr,
            "cache_write_5m_tokens":    cw5,
            "cache_write_1h_tokens":    cw1,
            "ai_lines_added":           added,
            "ai_lines_removed":         removed,
        }));
    }
    Ok(out)
}

/// Spawn `cc-ledger sync --background` and detach. Used by `SessionEnd` so
/// the hook returns immediately. Failures are silent — sync will retry on
/// the next session.
pub fn spawn_background_sync(binary_path: &Path) -> Result<()> {
    use std::process::Command;
    Command::new(binary_path)
        .arg("sync")
        .arg("--background")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .with_context(|| format!("spawning background sync via {}", binary_path.display()))
}

/// Spawn `cc-ledger sync --background --full-backfill` and detach. Used by
/// `auth::login` after a successful sign-in so the dashboard has data right
/// away. The child runs the codeburn-style backfill first, then the
/// regular sync pass.
pub fn spawn_background_full_backfill(binary_path: &Path) -> Result<()> {
    use std::process::Command;
    Command::new(binary_path)
        .arg("sync")
        .arg("--background")
        .arg("--full-backfill")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .with_context(|| format!("spawning post-login backfill via {}", binary_path.display()))
}

/// Background entry point: runs `run` non-interactively and exits silently
/// on any error. Logs to `~/.cc-ledger/audit/` when CC_LEDGER_AUDIT=1.
pub fn run_background(full_backfill: bool) -> Result<()> {
    if full_backfill {
        // Best-effort — even if the categorization step fails (e.g. no
        // ~/.claude directory yet), we still try to push whatever's
        // already local.
        let _ = crate::cmd::backfill::run_agent_turns(crate::cmd::backfill::AgentTurnsArgs {
            background: true,
            all: false,
        });
    }
    let conn = store::open(&paths::db_path()?)?;
    let report = run(
        &conn,
        SyncOptions {
            interactive: false,
            dry_run: false,
            reset_cursor: false,
        },
    )?;
    // Drop the report on the floor — background mode is silent.
    let _ = report;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        std::mem::forget(dir);
        conn
    }

    #[test]
    fn cwd_hash_matches_sha256() {
        let h = cwd_hash("/Users/x/repo").unwrap();
        assert_eq!(h.len(), 64);
        // Empty cwd -> no hash.
        assert!(cwd_hash("").is_none());
    }

    #[test]
    fn cwd_basename_strips_path() {
        assert_eq!(cwd_basename("/Users/x/repo").as_deref(), Some("repo"));
        assert_eq!(cwd_basename("repo").as_deref(), Some("repo"));
        assert!(cwd_basename("").is_none());
        assert!(cwd_basename("/").is_none());
    }

    #[test]
    fn relativize_strips_cwd_prefix() {
        assert_eq!(
            relativize("/Users/x/repo/src/a.rs", "/Users/x/repo"),
            "src/a.rs"
        );
        assert_eq!(
            relativize("/Users/x/repo/src/a.rs", "/Users/x/repo/"),
            "src/a.rs"
        );
        // Path outside cwd: unchanged. We never actually want to sync those
        // (the schema rejects long paths via maxLength), but the function
        // shouldn't crash.
        assert_eq!(
            relativize("/elsewhere/a.rs", "/Users/x/repo"),
            "/elsewhere/a.rs"
        );
    }

    #[test]
    fn collect_sessions_omits_cwd() {
        let conn = db();
        conn.execute(
            "INSERT INTO sessions(session_id, agent_id, started_at, ended_at, cwd, model)
                VALUES('s1', 'claude-code', 1000, 2000, '/Users/x/myrepo', 'claude-opus')",
            [],
        )
        .unwrap();
        let mut repos = RepoCache::default();
        let rows = collect_sessions(&conn, 0, &mut repos).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        // The full path must not appear in the wire payload.
        let s = serde_json::to_string(row).unwrap();
        assert!(!s.contains("/Users/x/myrepo"), "cwd leaked: {s}");
        assert_eq!(row["repo_basename"], "myrepo");
        assert_eq!(row["cwd_hash"].as_str().unwrap().len(), 64);
        assert_eq!(row["session_id"], "s1");
        // Tempdir under /Users/x/myrepo isn't a real git repo, so identity
        // fields land null. Verifying they're present in the wire row.
        assert!(row.as_object().unwrap().contains_key("host"));
        assert!(row.as_object().unwrap().contains_key("owner"));
        assert!(row.as_object().unwrap().contains_key("repo_name"));
    }

    #[test]
    fn collect_commits_hashes_subject() {
        let conn = db();
        conn.execute(
            "INSERT INTO commits(cwd, commit_sha, authored_at_ms, branch, subject,
                                 additions, deletions, files_touched, captured_at_ms)
                VALUES('/r/foo', 'abc123', 1000, 'main',
                       'feat: super-secret merger details', 5, 1, 1, 999)",
            [],
        )
        .unwrap();
        let mut repos = RepoCache::default();
        let rows = collect_commits(&conn, 0, &mut repos).unwrap();
        assert_eq!(rows.len(), 1);
        let s = serde_json::to_string(&rows[0]).unwrap();
        assert!(!s.contains("super-secret"), "subject leaked: {s}");
        assert!(!s.contains("/r/foo"), "cwd leaked: {s}");
        assert_eq!(rows[0]["subject_hash"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn collect_attributions_relativizes_paths() {
        let conn = db();
        conn.execute(
            "INSERT INTO sessions(session_id, agent_id) VALUES('s', 'claude-code')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_calls(session_id, tool_use_id, tool_name, ts_ms, status)
                VALUES('s', 'tu', 'Edit', 0, 'success')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attributions(commit_sha, cwd, file_path, line_start, line_end,
                                      session_id, tool_use_id, author_id)
                VALUES('abc', '/Users/x/repo', '/Users/x/repo/src/a.rs', 1, 5,
                       's', 'tu', 'ai:claude-code:s')",
            [],
        )
        .unwrap();
        let mut repos = RepoCache::default();
        let rows = collect_attributions(&conn, &mut repos).unwrap();
        assert_eq!(rows.len(), 1);
        let s = serde_json::to_string(&rows[0]).unwrap();
        assert!(!s.contains("/Users/x/repo"), "absolute path leaked: {s}");
        assert_eq!(rows[0]["file_path"], "src/a.rs");
        assert_eq!(rows[0]["repo_basename"], "repo");
        assert_eq!(rows[0]["ai_lines_added"], 5);
    }

    #[test]
    fn no_forbidden_fields_in_any_collector_output() {
        let conn = db();
        // Seed every table.
        conn.execute_batch(
            "INSERT INTO sessions(session_id, agent_id, started_at, ended_at, cwd, model)
                VALUES('s1', 'claude-code', 100, 200, '/r/foo', 'm');
             INSERT INTO tool_calls(session_id, tool_use_id, tool_name, ts_ms, end_ms, status, lines_added, lines_removed)
                VALUES('s1', 'tu1', 'Edit', 100, 200, 'success', 3, 1);
             INSERT INTO attributions(commit_sha, cwd, file_path, line_start, line_end,
                                      session_id, tool_use_id, author_id)
                VALUES('sha', '/r/foo', '/r/foo/a.rs', 1, 3, 's1', 'tu1', 'ai:claude-code:s1');
             INSERT INTO commits(cwd, commit_sha, authored_at_ms, branch, subject,
                                 additions, deletions, files_touched, captured_at_ms)
                VALUES('/r/foo', 'sha', 100, 'main', 'feat: x', 3, 1, 1, 100);
             INSERT INTO pull_requests(cwd, pr_number, branch, first_seen_at_ms, last_seen_at_ms, state)
                VALUES('/r/foo', 7, 'main', 100, 100, 'open');
             INSERT INTO pr_commits(cwd, pr_number, commit_sha) VALUES('/r/foo', 7, 'sha');
             INSERT INTO pr_cost_snapshots(cwd, pr_number, snapshot_kind, snapshot_at_ms,
                                           total_cost_usd_api_equiv, input_tokens, output_tokens,
                                           cache_read_tokens, cache_write_5m_tokens, cache_write_1h_tokens,
                                           ai_lines_added, ai_lines_removed)
                VALUES('/r/foo', 7, 'first_seen', 100, 0.5, 100, 50, 0, 0, 0, 3, 0);",
        ).unwrap();

        let mut repos = RepoCache::default();
        let everything = format!(
            "{}{}{}{}{}{}",
            serde_json::to_string(&collect_sessions(&conn, 0, &mut repos).unwrap()).unwrap(),
            serde_json::to_string(&collect_tool_aggregates(&conn, 0).unwrap()).unwrap(),
            serde_json::to_string(&collect_attributions(&conn, &mut repos).unwrap()).unwrap(),
            serde_json::to_string(&collect_commits(&conn, 0, &mut repos).unwrap()).unwrap(),
            serde_json::to_string(&collect_pull_requests(&conn, 0, &mut repos).unwrap()).unwrap(),
            serde_json::to_string(&collect_pr_snapshots(&conn, 0, &mut repos).unwrap()).unwrap(),
        );

        for forbidden in [
            "/r/foo",       // cwd (full path)
            "feat: x",      // raw subject
            "\"cwd\":",     // field name
            "\"subject\":", // field name
            "\"prompt\":",
            "\"transcript\":",
            "\"file_content\":",
            "\"pre_blob\":",
            "\"post_blob\":",
            "\"repo_remote_url\":",
        ] {
            assert!(
                !everything.contains(forbidden),
                "forbidden token {forbidden:?} leaked in payload: {everything}",
            );
        }
    }

    #[test]
    fn cursor_round_trips() {
        let conn = db();
        assert_eq!(max_cursor(&conn, "sessions", 0).unwrap(), 0);
        advance_cursor(&conn, "sessions", 1234).unwrap();
        assert_eq!(max_cursor(&conn, "sessions", 0).unwrap(), 1234);
        // Server cursor wins if higher.
        assert_eq!(max_cursor(&conn, "sessions", 9999).unwrap(), 9999);
    }

    #[test]
    fn clear_local_cursors_removes_only_known_tables() {
        let conn = db();
        advance_cursor(&conn, "sessions", 100).unwrap();
        advance_cursor(&conn, "commits", 200).unwrap();
        clear_local_cursors(&conn).unwrap();
        assert_eq!(max_cursor(&conn, "sessions", 0).unwrap(), 0);
        assert_eq!(max_cursor(&conn, "commits", 0).unwrap(), 0);
    }
}
