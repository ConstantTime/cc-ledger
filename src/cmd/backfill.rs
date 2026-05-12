//! `cc-ledger backfill agent-turns` — scan `~/.claude/projects/*/<session>.jsonl`
//! and (re-)classify codeburn-style agent turns for every session into the
//! local `agent_turns` table.
//!
//! Idempotent. Re-runs after a `CLASSIFIER_VERSION` bump pick up the new
//! taxonomy because we delete-and-replace per session.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::agents::claude_code::{agent_turns, categorize};
use crate::pricing::PricingTable;
use crate::store::queries::{self, SessionInit};
use crate::{paths, store};

const AGENT_ID: &str = "claude-code";

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Re-classify codeburn-style agent turns from `~/.claude/projects/*/*.jsonl`.
    AgentTurns(AgentTurnsArgs),
}

#[derive(Debug, Parser, Default)]
pub struct AgentTurnsArgs {
    /// Run silently (no output). Used by post-auth-login background fork.
    #[arg(long)]
    pub background: bool,
    /// Process every session, even those whose `classifier_version` already
    /// matches the current version. Default skips up-to-date sessions.
    #[arg(long)]
    pub all: bool,
}

pub fn run(args: Args) -> Result<()> {
    match args.action {
        Action::AgentTurns(a) => run_agent_turns(a),
    }
}

pub fn run_agent_turns(args: AgentTurnsArgs) -> Result<()> {
    let conn = store::open(&paths::db_path()?)?;
    let pricing = PricingTable::load(Some(&paths::pricing_path()?))?;
    let now_ms = paths::now_ms();

    let projects = claude_projects_dir();
    let entries = match collect_session_jsonls(&projects) {
        Ok(e) => e,
        Err(e) => {
            if !args.background {
                eprintln!("cc-ledger: skipping backfill — {e}");
            }
            return Ok(());
        }
    };

    if !args.background {
        eprintln!(
            "Backfilling activity categories from {} session(s) in {}…",
            entries.len(),
            projects.display()
        );
    }

    let mut sessions_done = 0usize;
    let mut turns_total = 0usize;
    let mut skipped = 0usize;
    for (session_id, path) in &entries {
        if !args.all && session_already_current(&conn, session_id)? {
            skipped += 1;
            continue;
        }
        // Read the transcript once, do both passes from the same buffer:
        //   1. synthesize a `sessions` row (cwd / model / started_at) so the
        //      stats panels can join on it — backfill-only data otherwise
        //      has no session metadata at all.
        //   2. (re-)classify agent_turns from the same JSONL.
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                if !args.background {
                    eprintln!("  skip {session_id}: {e}");
                }
                continue;
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        if let Some(meta) = SessionMeta::from_jsonl(&text) {
            let _ = queries::upsert_session(
                &conn,
                &SessionInit {
                    session_id,
                    agent_id: AGENT_ID,
                    started_at: meta.started_at_ms,
                    cwd: meta.cwd.as_deref(),
                    model: meta.model.as_deref(),
                    ..SessionInit::default()
                },
            );
        }
        match agent_turns::rebuild_for_session(&conn, session_id, &text, &pricing, now_ms) {
            Ok(n) => {
                sessions_done += 1;
                turns_total += n;
            }
            Err(e) => {
                if !args.background {
                    eprintln!("  skip {session_id}: {e}");
                }
            }
        }
    }

    queries::set_config_i64(&conn, "last_categorize_at_ms", now_ms)?;

    if !args.background {
        let mut out = std::io::stdout();
        writeln!(
            out,
            "Backfill complete: {sessions_done} session(s) classified, {turns_total} agent_turns rows written, {skipped} skipped (already current)."
        )?;
    }
    Ok(())
}

/// Default Claude Code projects directory.
fn claude_projects_dir() -> PathBuf {
    if let Ok(home) = std::env::var("CC_LEDGER_CLAUDE_HOME") {
        return PathBuf::from(home).join("projects");
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
        .join("projects")
}

/// Walk `<projects>/<project>/*.jsonl` and `<projects>/<project>/subagents/*.jsonl`,
/// returning `(session_id, path)` pairs. The session_id is the file stem.
fn collect_session_jsonls(projects: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let proj_iter =
        std::fs::read_dir(projects).with_context(|| format!("read {}", projects.display()))?;
    for proj in proj_iter {
        let proj = match proj {
            Ok(p) => p,
            Err(_) => continue,
        };
        let proj_path = proj.path();
        if !proj_path.is_dir() {
            continue;
        }
        push_jsonls_in(&proj_path, &mut out);
        let subagents = proj_path.join("subagents");
        if subagents.is_dir() {
            push_jsonls_in(&subagents, &mut out);
        }
    }
    Ok(out)
}

fn push_jsonls_in(dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let iter = match std::fs::read_dir(dir) {
        Ok(i) => i,
        Err(_) => return,
    };
    for f in iter.flatten() {
        let path = f.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push((stem.to_string(), path));
    }
}

/// True if every existing `agent_turns` row for this session was written by
/// the current `CLASSIFIER_VERSION`. Empty sessions are NOT current — we
/// haven't tried them yet.
fn session_already_current(conn: &rusqlite::Connection, session_id: &str) -> Result<bool> {
    let (count, max_v): (i64, Option<i64>) = conn.query_row(
        "SELECT COUNT(*), MIN(classifier_version)
           FROM agent_turns WHERE session_id = ?1",
        [session_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(count > 0 && max_v.unwrap_or(0) >= categorize::CLASSIFIER_VERSION)
}

/// Session-level metadata recoverable from a Claude Code JSONL transcript.
/// All fields are best-effort; older transcripts may lack any of them.
#[derive(Debug, Default)]
struct SessionMeta {
    cwd: Option<String>,
    model: Option<String>,
    started_at_ms: Option<i64>,
}

impl SessionMeta {
    /// Walk the transcript line-by-line and pull:
    ///
    /// - `cwd`   — first non-empty `cwd` field (constant within a session).
    /// - `model` — first `message.model` we see (assistant lines carry it).
    /// - `started_at_ms` — earliest ISO-8601 `timestamp`.
    ///
    /// Returns `None` if the transcript is empty / malformed (no fields at all).
    fn from_jsonl(jsonl: &str) -> Option<Self> {
        let mut meta = SessionMeta::default();
        let mut any = false;
        for line in jsonl.lines() {
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            any = true;
            if meta.cwd.is_none() {
                if let Some(s) = v.get("cwd").and_then(|x| x.as_str()) {
                    if !s.is_empty() {
                        meta.cwd = Some(s.to_string());
                    }
                }
            }
            if meta.model.is_none() {
                if let Some(s) = v
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(|x| x.as_str())
                {
                    if !s.is_empty() {
                        meta.model = Some(s.to_string());
                    }
                }
            }
            if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
                if let Ok(t) = chrono::DateTime::parse_from_rfc3339(ts) {
                    let ms = t.timestamp_millis();
                    meta.started_at_ms = Some(meta.started_at_ms.map_or(ms, |s| s.min(ms)));
                }
            }
        }
        any.then_some(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_meta_picks_cwd_model_and_earliest_ts() {
        let jsonl = r#"{"type":"user","cwd":"/repo","timestamp":"2026-05-02T10:00:00Z"}
{"type":"assistant","cwd":"/repo","timestamp":"2026-05-02T10:00:01Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"user","cwd":"/repo","timestamp":"2026-05-02T09:00:00Z"}"#;
        let meta = SessionMeta::from_jsonl(jsonl).unwrap();
        assert_eq!(meta.cwd.as_deref(), Some("/repo"));
        assert_eq!(meta.model.as_deref(), Some("claude-opus-4-7"));
        // Earliest timestamp wins (the third line, 09:00).
        let expect = chrono::DateTime::parse_from_rfc3339("2026-05-02T09:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(meta.started_at_ms, Some(expect));
    }

    #[test]
    fn session_meta_returns_none_on_empty() {
        assert!(SessionMeta::from_jsonl("").is_none());
    }

    #[test]
    fn session_meta_handles_missing_fields() {
        // No cwd, no model — only a timestamp.
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z"}"#;
        let meta = SessionMeta::from_jsonl(jsonl).unwrap();
        assert!(meta.cwd.is_none());
        assert!(meta.model.is_none());
        assert!(meta.started_at_ms.is_some());
    }
}
