//! Codeburn-style "agent turns" — one row per (user message + agent's
//! response). Distinct from cc-ledger's per-API-call `turns` table; an
//! `agent_turns` row aggregates all API calls between two consecutive
//! user messages.
//!
//! Pure parsing + grouping in [`parse_jsonl`]; persistence in
//! [`rebuild_for_session`] (delete-and-replace per session, idempotent).

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;

use super::categorize::{self, is_bash_tool, Category, Tier};
use crate::pricing::{compute_cost_usd_api_equiv, PricingTable};
use crate::store::queries::{self, AgentTurnRow, TurnTokens};

/// One assistant API call within a turn — what we extracted from a single
/// `type:"assistant"` JSONL entry.
#[derive(Debug, Default, Clone)]
struct ApiCall {
    timestamp_ms: Option<i64>,
    model: Option<String>,
    service_tier: Option<String>,
    tokens: TurnTokens,
    tools: Vec<String>,
    bash_commands: Vec<String>,
    has_plan_mode: bool,
    has_agent_spawn: bool,
    /// Anthropic streaming uses the same `message.id` across emit steps;
    /// dedupe keeps only the last entry per id (latest `usage` wins).
    message_id: Option<String>,
}

/// Codeburn-style turn extracted from a transcript JSONL.
#[derive(Debug, Clone)]
pub struct AgentTurn {
    pub user_message: String,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub api_call_count: i64,
    pub tokens: TurnTokens,
    pub cost_usd_api_equiv: f64,
    pub category: Category,
    pub tier: Tier,
}

fn as_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn parse_iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Pull all `text` content from a user `message.content` (string or array
/// of blocks). Tool-result-only user messages return empty.
fn extract_user_text(message: &Value) -> String {
    let content = match message.get("content") {
        Some(c) => c,
        None => return String::new(),
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for block in arr {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    parts.push(t);
                }
            }
        }
        return parts.join(" ");
    }
    String::new()
}

fn parse_assistant(entry: &Value, ts_ms: Option<i64>) -> Option<ApiCall> {
    let msg = entry.get("message")?;
    let usage = msg.get("usage")?;
    let model = msg.get("model").and_then(Value::as_str).map(String::from);

    let mut tokens = TurnTokens {
        input: as_i64(usage, "input_tokens"),
        output: as_i64(usage, "output_tokens"),
        cache_read: as_i64(usage, "cache_read_input_tokens"),
        cache_write_5m: 0,
        cache_write_1h: 0,
    };
    if let Some(cc) = usage.get("cache_creation") {
        tokens.cache_write_5m = as_i64(cc, "ephemeral_5m_input_tokens");
        tokens.cache_write_1h = as_i64(cc, "ephemeral_1h_input_tokens");
    } else {
        tokens.cache_write_5m = as_i64(usage, "cache_creation_input_tokens");
    }

    let mut tools: Vec<String> = Vec::new();
    let mut bash_commands: Vec<String> = Vec::new();
    let mut has_plan_mode = false;
    let mut has_agent_spawn = false;

    if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(name) = block.get("name").and_then(Value::as_str) else {
                continue;
            };
            let name_owned = name.to_string();
            if name == "EnterPlanMode" {
                has_plan_mode = true;
            }
            if name == "Agent" {
                has_agent_spawn = true;
            }
            if is_bash_tool(name) {
                if let Some(cmd) = block
                    .get("input")
                    .and_then(|i| i.get("command"))
                    .and_then(Value::as_str)
                {
                    bash_commands.push(cmd.to_string());
                }
            }
            tools.push(name_owned);
        }
    }

    let service_tier = usage
        .get("service_tier")
        .or_else(|| usage.get("speed"))
        .and_then(Value::as_str)
        .map(String::from);

    let message_id = msg.get("id").and_then(Value::as_str).map(String::from);

    Some(ApiCall {
        timestamp_ms: ts_ms,
        model,
        service_tier,
        tokens,
        tools,
        bash_commands,
        has_plan_mode,
        has_agent_spawn,
        message_id,
    })
}

/// Walk JSONL, group into codeburn turns, classify each, return.
///
/// Streaming dedupe: if multiple assistant entries share a `message.id` we
/// keep only the last (latest usage snapshot).
pub fn parse_jsonl(jsonl: &str, pricing: &PricingTable) -> Vec<AgentTurn> {
    // First pass: collect all entries we care about (and find last index per
    // message_id for dedupe).
    enum Entry {
        UserText { text: String, ts_ms: Option<i64> },
        Assistant(ApiCall),
    }

    let mut raw: Vec<Entry> = Vec::new();
    let mut last_idx_by_id: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v.get("type").and_then(Value::as_str).unwrap_or("");
        let ts_ms = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms);
        match typ {
            "user" => {
                let msg = v.get("message").cloned().unwrap_or(Value::Null);
                let text = extract_user_text(&msg);
                if !text.trim().is_empty() {
                    raw.push(Entry::UserText { text, ts_ms });
                }
            }
            "assistant" => {
                if let Some(call) = parse_assistant(&v, ts_ms) {
                    if let Some(id) = call.message_id.as_deref() {
                        last_idx_by_id.insert(id.to_string(), raw.len());
                    }
                    raw.push(Entry::Assistant(call));
                }
            }
            _ => {}
        }
    }

    // Filter: drop assistant entries that aren't the last occurrence of their
    // message_id (they're streaming-step duplicates).
    let kept: Vec<Entry> = raw
        .into_iter()
        .enumerate()
        .filter_map(|(idx, e)| match &e {
            Entry::Assistant(c) => match c.message_id.as_deref() {
                Some(id) => match last_idx_by_id.get(id) {
                    Some(last) if *last == idx => Some(e),
                    Some(_) => None, // not the last — drop
                    None => Some(e),
                },
                None => Some(e),
            },
            _ => Some(e),
        })
        .collect();

    // Group: each non-empty user text starts a new turn; subsequent assistant
    // calls accumulate until the next user text.
    let mut turns: Vec<(String, Option<i64>, Vec<ApiCall>)> = Vec::new();
    let mut cur_user = String::new();
    let mut cur_ts: Option<i64> = None;
    let mut cur_calls: Vec<ApiCall> = Vec::new();
    let mut started = false;

    for e in kept {
        match e {
            Entry::UserText { text, ts_ms } => {
                if started {
                    turns.push((
                        std::mem::take(&mut cur_user),
                        cur_ts,
                        std::mem::take(&mut cur_calls),
                    ));
                }
                cur_user = text;
                cur_ts = ts_ms;
                cur_calls = Vec::new();
                started = true;
            }
            Entry::Assistant(call) => {
                if started {
                    cur_calls.push(call);
                }
                // Assistant turns before any user text are dropped (system /
                // pre-amble).
            }
        }
    }
    if started {
        turns.push((cur_user, cur_ts, cur_calls));
    }

    // Classify + materialize. Drop turns with zero assistant calls — those
    // are user messages with no agent response yet (shouldn't be charged).
    turns
        .into_iter()
        .filter(|(_, _, calls)| !calls.is_empty())
        .map(|(user_message, started_at_ms, calls)| {
            // Sum tokens; pick the model of the call with the largest output
            // for cost (codeburn uses per-call models — we approximate by
            // computing per-call cost and summing).
            let mut tokens = TurnTokens::default();
            let mut cost = 0.0f64;
            let mut ended_at_ms: Option<i64> = None;

            for c in &calls {
                tokens.input += c.tokens.input;
                tokens.output += c.tokens.output;
                tokens.cache_read += c.tokens.cache_read;
                tokens.cache_write_5m += c.tokens.cache_write_5m;
                tokens.cache_write_1h += c.tokens.cache_write_1h;
                if let Some(model) = c.model.as_deref() {
                    if let Some(per_call) = compute_cost_usd_api_equiv(
                        model,
                        &c.tokens,
                        c.service_tier.as_deref(),
                        pricing,
                    ) {
                        cost += per_call;
                    }
                }
                ended_at_ms = ended_at_ms.max(c.timestamp_ms).or(c.timestamp_ms);
            }

            // Build classifier facts from the union of all calls.
            let tools: Vec<&str> = calls
                .iter()
                .flat_map(|c| c.tools.iter().map(|s| s.as_str()))
                .collect();
            let bash_commands: Vec<&str> = calls
                .iter()
                .flat_map(|c| c.bash_commands.iter().map(|s| s.as_str()))
                .collect();
            let has_plan_mode = calls.iter().any(|c| c.has_plan_mode);
            let has_agent_spawn = calls.iter().any(|c| c.has_agent_spawn);

            let facts = categorize::TurnFacts {
                user_message: &user_message,
                tools,
                bash_commands,
                has_plan_mode,
                has_agent_spawn,
            };
            let (category, tier) = categorize::classify_turn(&facts);

            AgentTurn {
                user_message,
                started_at_ms,
                ended_at_ms,
                api_call_count: calls.len() as i64,
                tokens,
                cost_usd_api_equiv: cost,
                category,
                tier,
            }
        })
        .collect()
}

/// Re-classify and persist all agent turns for a session, given the JSONL
/// transcript text. Atomically: deletes existing rows for the session,
/// inserts the freshly-classified set.
pub fn rebuild_for_session(
    conn: &Connection,
    session_id: &str,
    jsonl: &str,
    pricing: &PricingTable,
    now_ms: i64,
) -> Result<usize> {
    let turns = parse_jsonl(jsonl, pricing);
    let tx = conn.unchecked_transaction()?;
    queries::delete_agent_turns_for_session(&tx, session_id)?;
    for (idx, t) in turns.iter().enumerate() {
        queries::upsert_agent_turn(
            &tx,
            &AgentTurnRow {
                session_id,
                user_turn_idx: idx as i64,
                started_at_ms: t.started_at_ms,
                ended_at_ms: t.ended_at_ms,
                api_call_count: t.api_call_count,
                cost_usd_api_equiv: t.cost_usd_api_equiv,
                tokens: t.tokens,
                category: t.category.as_str(),
                classifier_version: categorize::CLASSIFIER_VERSION,
                classifier_tier: t.tier.as_str(),
                computed_at_ms: now_ms,
            },
        )?;
    }
    tx.commit()?;
    Ok(turns.len())
}

/// Read the transcript at `path` and rebuild for `session_id`. NotFound is
/// soft — returns 0 and Ok(()).
pub fn rebuild_from_path(
    conn: &Connection,
    session_id: &str,
    transcript_path: &std::path::Path,
    pricing: &PricingTable,
    now_ms: i64,
) -> Result<usize> {
    let bytes = match std::fs::read(transcript_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(e).with_context(|| format!("read {}", transcript_path.display()));
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    rebuild_for_session(conn, session_id, &text, pricing, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    fn fresh() -> Connection {
        store::open(&std::path::PathBuf::from(":memory:")).unwrap()
    }

    fn pricing() -> PricingTable {
        PricingTable::embedded().unwrap()
    }

    #[test]
    fn empty_transcript_yields_zero_turns() {
        assert!(parse_jsonl("", &pricing()).is_empty());
    }

    #[test]
    fn one_user_one_assistant_yields_one_turn() {
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z","message":{"role":"user","content":"fix the bug"}}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"id":"m1","model":"claude-opus-4-7-20260301","content":[{"type":"tool_use","name":"Edit","input":{}}],"usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let turns = parse_jsonl(jsonl, &pricing());
        assert_eq!(turns.len(), 1);
        let t = &turns[0];
        assert_eq!(t.user_message, "fix the bug");
        assert_eq!(t.api_call_count, 1);
        assert_eq!(t.tokens.input, 100);
        assert_eq!(t.category, Category::Debugging); // "fix" + Edit → debugging
    }

    #[test]
    fn multiple_calls_in_one_turn_sum_tokens() {
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z","message":{"role":"user","content":"add a feature"}}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[{"type":"tool_use","name":"Read","input":{}}],"usage":{"input_tokens":100,"output_tokens":10}}}
{"type":"assistant","timestamp":"2026-05-02T10:00:02Z","message":{"id":"m2","model":"claude-opus-4-7","content":[{"type":"tool_use","name":"Edit","input":{}}],"usage":{"input_tokens":200,"output_tokens":20}}}"#;
        let turns = parse_jsonl(jsonl, &pricing());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].api_call_count, 2);
        assert_eq!(turns[0].tokens.input, 300);
        assert_eq!(turns[0].tokens.output, 30);
        // "add a feature" + Edit → feature
        assert_eq!(turns[0].category, Category::Feature);
    }

    #[test]
    fn user_messages_with_tool_result_only_are_skipped() {
        // Claude Code's internal "user" entries that carry tool_result blocks
        // (no text) shouldn't start a new turn.
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z","message":{"role":"user","content":"do it"}}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[{"type":"tool_use","name":"Edit","input":{}}],"usage":{"input_tokens":100,"output_tokens":10}}}
{"type":"user","timestamp":"2026-05-02T10:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}}
{"type":"assistant","timestamp":"2026-05-02T10:00:03Z","message":{"id":"m2","model":"claude-opus-4-7","content":[{"type":"tool_use","name":"Edit","input":{}}],"usage":{"input_tokens":50,"output_tokens":5}}}"#;
        let turns = parse_jsonl(jsonl, &pricing());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].api_call_count, 2);
    }

    #[test]
    fn streaming_message_id_dedup_keeps_last() {
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z","message":{"role":"user","content":"hi"}}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[],"usage":{"input_tokens":100,"output_tokens":10}}}
{"type":"assistant","timestamp":"2026-05-02T10:00:02Z","message":{"id":"m1","model":"claude-opus-4-7","content":[],"usage":{"input_tokens":150,"output_tokens":15}}}"#;
        let turns = parse_jsonl(jsonl, &pricing());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].api_call_count, 1);
        assert_eq!(turns[0].tokens.input, 150);
    }

    #[test]
    fn rebuild_for_session_is_idempotent() {
        let conn = fresh();
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z","message":{"role":"user","content":"fix this"}}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[{"type":"tool_use","name":"Edit","input":{}}],"usage":{"input_tokens":100,"output_tokens":10}}}"#;
        let n1 = rebuild_for_session(&conn, "s1", jsonl, &pricing(), 1000).unwrap();
        let n2 = rebuild_for_session(&conn, "s1", jsonl, &pricing(), 2000).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 1);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_turns WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
