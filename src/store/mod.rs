//! Local sqlite ledger.
//!
//! `open(db_path)` returns a connection with WAL journaling enabled and the
//! schema migrated to the current version. The schema covers the cc-ledger
//! capture model: sessions, tool_calls, attributions, turns (+ the v2 PR
//! rollups and the v3 codeburn-style agent_turns).

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

pub mod blobs;
pub mod queries;

/// Schema version applied by `open`. Bump whenever migrations are added.
pub const SCHEMA_VERSION: i64 = 4;

/// Open `db_path` (creating it if missing), enable WAL + sane pragmas, and
/// run migrations idempotently.
pub fn open(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("open sqlite at {}", db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;

    let current: Option<i64> = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM schema_meta WHERE key = 'version'",
            [],
            |r| r.get(0),
        )
        .ok();

    match current {
        None => {
            // Fresh database — apply migrations in order so all tables exist.
            // v4 drops `prompts`, so we skip creating it on a fresh DB.
            conn.execute_batch(SCHEMA_V1)?;
            conn.execute_batch(SCHEMA_V2)?;
            conn.execute_batch(SCHEMA_V3)?;
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "INSERT INTO schema_meta(key, value) VALUES('version', ?1)",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Some(1) => {
            conn.execute_batch(SCHEMA_V2)?;
            conn.execute_batch(SCHEMA_V3)?;
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "UPDATE schema_meta SET value = ?1 WHERE key = 'version'",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Some(2) => {
            conn.execute_batch(SCHEMA_V3)?;
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "UPDATE schema_meta SET value = ?1 WHERE key = 'version'",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Some(3) => {
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "UPDATE schema_meta SET value = ?1 WHERE key = 'version'",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Some(v) if v == SCHEMA_VERSION => {}
        Some(v) => {
            anyhow::bail!("ledger schema version {v} is newer than this binary supports ({SCHEMA_VERSION}); upgrade cc-ledger");
        }
    }
    Ok(())
}

const SCHEMA_V1: &str = r#"
CREATE TABLE sessions (
    session_id    TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL,
    started_at    INTEGER,
    ended_at      INTEGER,
    cwd           TEXT,
    model         TEXT,
    os            TEXT,
    hostname      TEXT,
    cc_version    TEXT,
    billing_mode  TEXT NOT NULL DEFAULT 'unknown'
);

CREATE TABLE tool_calls (
    session_id     TEXT NOT NULL,
    tool_use_id    TEXT NOT NULL,
    tool_name      TEXT NOT NULL,
    file_path      TEXT,
    ts_ms          INTEGER NOT NULL,
    end_ms         INTEGER,
    pre_blob_sha   TEXT,
    post_blob_sha  TEXT,
    status         TEXT NOT NULL,
    lines_added    INTEGER,
    lines_removed  INTEGER,
    error_message  TEXT,
    PRIMARY KEY (session_id, tool_use_id)
);
CREATE INDEX idx_tool_calls_session ON tool_calls(session_id);
CREATE INDEX idx_tool_calls_status  ON tool_calls(status);

CREATE TABLE attributions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    commit_sha    TEXT,
    cwd           TEXT NOT NULL,
    file_path     TEXT NOT NULL,
    line_start    INTEGER NOT NULL,
    line_end      INTEGER NOT NULL,
    session_id    TEXT NOT NULL,
    tool_use_id   TEXT NOT NULL,
    author_id     TEXT NOT NULL
);
CREATE INDEX idx_attr_commit  ON attributions(commit_sha);
CREATE INDEX idx_attr_file    ON attributions(cwd, file_path);
CREATE INDEX idx_attr_session ON attributions(session_id);

CREATE TABLE turns (
    session_id             TEXT NOT NULL,
    turn_idx               INTEGER NOT NULL,
    started_at_ms          INTEGER,
    ended_at_ms            INTEGER,
    model                  TEXT,
    input_tokens           INTEGER,
    output_tokens          INTEGER,
    cache_read_tokens      INTEGER,
    cache_write_5m_tokens  INTEGER,
    cache_write_1h_tokens  INTEGER,
    cost_usd_api_equiv     REAL,
    pricing_version        INTEGER,
    service_tier           TEXT,
    web_search_count       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, turn_idx)
);
CREATE INDEX idx_turns_model ON turns(model);

CREATE TABLE prompts (
    session_id   TEXT NOT NULL,
    turn_idx     INTEGER NOT NULL,
    text         TEXT NOT NULL,
    truncated    INTEGER NOT NULL,
    ts_ms        INTEGER NOT NULL,
    PRIMARY KEY (session_id, turn_idx)
);
"#;

/// V2 — git layer + PR cost rollups + sync state + local config.
///
/// Captures everything the local CLI needs to answer "cost per PR" without a
/// network call. Cloud sync (Phase 5) consumes the same tables, sanitized.
const SCHEMA_V2: &str = r#"
CREATE TABLE commits (
    cwd              TEXT NOT NULL,
    commit_sha       TEXT NOT NULL,
    authored_at_ms   INTEGER,
    branch           TEXT,
    subject          TEXT,
    additions        INTEGER NOT NULL DEFAULT 0,
    deletions        INTEGER NOT NULL DEFAULT 0,
    files_touched    INTEGER NOT NULL DEFAULT 0,
    captured_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (cwd, commit_sha)
);
CREATE INDEX idx_commits_cwd_branch ON commits(cwd, branch);
CREATE INDEX idx_commits_authored   ON commits(authored_at_ms);

CREATE TABLE commit_files (
    cwd          TEXT NOT NULL,
    commit_sha   TEXT NOT NULL,
    file_path    TEXT NOT NULL,
    additions    INTEGER NOT NULL,
    deletions    INTEGER NOT NULL,
    PRIMARY KEY (cwd, commit_sha, file_path)
);

CREATE TABLE pull_requests (
    cwd                  TEXT NOT NULL,
    pr_number            INTEGER NOT NULL,
    repo_remote_url      TEXT,
    repo_basename        TEXT,
    branch               TEXT NOT NULL,
    base_branch          TEXT,
    head_sha             TEXT,
    first_seen_at_ms     INTEGER NOT NULL,
    last_seen_at_ms      INTEGER NOT NULL,
    merged_at_ms         INTEGER,
    state                TEXT NOT NULL DEFAULT 'open',
    PRIMARY KEY (cwd, pr_number)
);
CREATE INDEX idx_prs_cwd_state ON pull_requests(cwd, state);
CREATE INDEX idx_prs_branch    ON pull_requests(cwd, branch);

CREATE TABLE pr_commits (
    cwd          TEXT NOT NULL,
    pr_number    INTEGER NOT NULL,
    commit_sha   TEXT NOT NULL,
    PRIMARY KEY (cwd, pr_number, commit_sha)
);

CREATE TABLE pr_cost_rollups (
    cwd                       TEXT NOT NULL,
    pr_number                 INTEGER NOT NULL,
    total_cost_usd_api_equiv  REAL NOT NULL DEFAULT 0,
    input_tokens              INTEGER NOT NULL DEFAULT 0,
    output_tokens             INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_write_5m_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_write_1h_tokens     INTEGER NOT NULL DEFAULT 0,
    ai_lines_added            INTEGER NOT NULL DEFAULT 0,
    ai_lines_removed          INTEGER NOT NULL DEFAULT 0,
    session_count             INTEGER NOT NULL DEFAULT 0,
    commit_count              INTEGER NOT NULL DEFAULT 0,
    last_computed_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (cwd, pr_number)
);

CREATE TABLE pr_cost_snapshots (
    cwd                       TEXT NOT NULL,
    pr_number                 INTEGER NOT NULL,
    snapshot_kind             TEXT NOT NULL,
    snapshot_at_ms            INTEGER NOT NULL,
    total_cost_usd_api_equiv  REAL NOT NULL,
    input_tokens              INTEGER NOT NULL,
    output_tokens             INTEGER NOT NULL,
    cache_read_tokens         INTEGER NOT NULL,
    cache_write_5m_tokens     INTEGER NOT NULL,
    cache_write_1h_tokens     INTEGER NOT NULL,
    ai_lines_added            INTEGER NOT NULL,
    ai_lines_removed          INTEGER NOT NULL,
    PRIMARY KEY (cwd, pr_number, snapshot_kind)
);

CREATE TABLE git_poll_state (
    cwd                       TEXT NOT NULL,
    branch                    TEXT NOT NULL,
    last_polled_at_ms         INTEGER NOT NULL,
    last_remote_head_sha      TEXT,
    last_pr_number            INTEGER,
    last_negative_at_ms       INTEGER,
    PRIMARY KEY (cwd, branch)
);

CREATE TABLE sync_cursor (
    table_name        TEXT PRIMARY KEY,
    last_pushed_pk    TEXT,
    last_pushed_ts_ms INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE config (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
INSERT INTO config(key, value) VALUES('git_notes.enabled', 'false');
"#;

/// V3 — codeburn-style agent turns + categorization.
///
/// One row per (user message + agent's response to it). Aggregates the
/// per-API-call `turns` data (tokens, cost) and adds a category (one of the
/// 13 codeburn categories) classified from the user prompt + tools used.
const SCHEMA_V3: &str = r#"
CREATE TABLE agent_turns (
    session_id            TEXT NOT NULL,
    user_turn_idx         INTEGER NOT NULL,
    started_at_ms         INTEGER,
    ended_at_ms           INTEGER,
    api_call_count        INTEGER NOT NULL,
    cost_usd_api_equiv    REAL    NOT NULL DEFAULT 0,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_write_5m_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0,
    category              TEXT NOT NULL,
    classifier_version    INTEGER NOT NULL,
    classifier_tier       TEXT NOT NULL,
    computed_at_ms        INTEGER NOT NULL,
    PRIMARY KEY (session_id, user_turn_idx)
);
CREATE INDEX idx_agent_turns_started ON agent_turns(started_at_ms);
CREATE INDEX idx_agent_turns_cat     ON agent_turns(category, started_at_ms);
"#;

/// V4 — drop `prompts`. Prompt text already lives in
/// `~/.claude/projects/<cwd>/<session>.jsonl`; cc-ledger no longer captures a
/// duplicate via the `UserPromptSubmit` hook. Existing rows are discarded.
const SCHEMA_V4: &str = r#"
DROP TABLE IF EXISTS prompts;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        Connection::open_in_memory().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn schema_applies_to_fresh_db() {
        let conn = fresh();
        migrate(&conn).unwrap();
        let v: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = fresh();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_meta WHERE key = 'version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn open_creates_db_file_and_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("ledger.db");
        let _ = open(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn all_tables_exist_after_open() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("ledger.db")).unwrap();
        for t in [
            // v1 (note: `prompts` was dropped in v4)
            "sessions",
            "tool_calls",
            "attributions",
            "turns",
            "schema_meta",
            // v2
            "commits",
            "commit_files",
            "pull_requests",
            "pr_commits",
            "pr_cost_rollups",
            "pr_cost_snapshots",
            "git_poll_state",
            "sync_cursor",
            "config",
            // v3
            "agent_turns",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {t}");
        }
        // `prompts` must not exist after v4.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='prompts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "prompts table should have been dropped in v4");
    }

    #[test]
    fn v3_to_v4_drops_prompts_table() {
        let conn = fresh();
        // Simulate an existing v3 database — apply v1+v2+v3 manually.
        conn.execute_batch("CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.execute_batch(SCHEMA_V3).unwrap();
        conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES('version', '3')",
            [],
        )
        .unwrap();
        // Drop a row into the soon-to-be-deleted prompts table.
        conn.execute(
            "INSERT INTO prompts(session_id, turn_idx, text, truncated, ts_ms)
                  VALUES('s', 0, 'hi', 0, 100)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let v: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key='version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='prompts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn v1_to_v2_upgrade_keeps_existing_data() {
        let conn = fresh();
        // Simulate an existing v1 database — apply v1 manually and tag it.
        conn.execute_batch("CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES('version', '1')",
            [],
        )
        .unwrap();
        // Drop a row in v1 territory.
        conn.execute(
            "INSERT INTO sessions(session_id, agent_id) VALUES('keepme', 'claude-code')",
            [],
        )
        .unwrap();

        // Run the migrate function — it should bump us to v2.
        migrate(&conn).unwrap();

        let v: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());

        // Pre-existing row survives.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE session_id='keepme'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);

        // V2 tables exist.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pr_cost_rollups'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);

        // Default config row seeded.
        let v: String = conn
            .query_row(
                "SELECT value FROM config WHERE key='git_notes.enabled'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "false");
    }
}
