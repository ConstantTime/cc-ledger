//! `Stop` — read the transcript file, parse per-turn usage, compute USD-API
//! equivalent cost, persist to `turns`.
//!
//! This handler is the entire token + cost capture path. It's idempotent
//! via the (session_id, turn_idx) primary key on `turns`: re-firing the
//! handler updates rows in place rather than duplicating.

use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::super::{agent_turns, transcript};
use super::HookContext;
use crate::pr;
use crate::pricing::compute_cost_usd_api_equiv;
use crate::store::queries::{self, TurnTokens};

#[derive(Deserialize, Default)]
struct Payload {
    session_id: Option<String>,
    transcript_path: Option<String>,
    cwd: Option<String>,
}

pub fn handle(payload: &Value, ctx: &HookContext) -> Result<()> {
    let p: Payload = serde_json::from_value(payload.clone()).unwrap_or_default();
    let (Some(session_id), Some(transcript_path)) =
        (p.session_id.as_deref(), p.transcript_path.as_deref())
    else {
        return Ok(());
    };

    let bytes = match std::fs::read(transcript_path) {
        Ok(b) => b,
        // Transcript not yet on disk: nothing to do.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let text = String::from_utf8_lossy(&bytes);
    let usages = transcript::parse_usages(&text);

    for u in usages {
        let tokens = TurnTokens {
            input: u.input_tokens,
            output: u.output_tokens,
            cache_read: u.cache_read,
            cache_write_5m: u.cache_write_5m,
            cache_write_1h: u.cache_write_1h,
        };
        let cost = u.model.as_deref().and_then(|m| {
            compute_cost_usd_api_equiv(m, &tokens, u.service_tier.as_deref(), ctx.pricing)
        });
        let pricing_version = if cost.is_some() {
            Some(ctx.pricing.pricing_version)
        } else {
            None
        };
        queries::insert_turn(
            ctx.conn,
            session_id,
            u.turn_idx,
            u.started_at_ms,
            u.ended_at_ms.or(Some(ctx.now_ms)),
            u.model.as_deref(),
            &tokens,
            cost,
            pricing_version,
            u.service_tier.as_deref(),
            u.web_search_count,
        )?;
    }

    // Re-classify codeburn-style "agent turns" for this session. Best-effort —
    // failure must not block the agent's session-end hook. Idempotent
    // (delete-and-replace per session).
    let _ = agent_turns::rebuild_for_session(ctx.conn, session_id, &text, ctx.pricing, ctx.now_ms);

    // TTL-gated PR poll. Cheap when cached, hits the network at most once
    // per ~5 min per branch. Best-effort — never bubbles up.
    if let Some(cwd) = p.cwd.as_deref() {
        let _ = pr::poll_if_stale(ctx.conn, &PathBuf::from(cwd), ctx.now_ms);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::handlers::test_support::ContextFixture;
    use serde_json::json;
    use std::fs;

    fn write_transcript(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parses_transcript_and_records_cost() {
        let fx = ContextFixture::new();
        let dir = fx.blobs_dir.parent().unwrap();
        // 1M input + 1M output on Opus 4.7 → exactly $30.
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z"}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"model":"claude-opus-4-7-20260301","usage":{"input_tokens":1000000,"output_tokens":1000000,"service_tier":"standard"}}}"#;
        let path = write_transcript(dir, "t.jsonl", jsonl);
        let payload = json!({
            "session_id": "s",
            "transcript_path": path.to_str().unwrap(),
        });
        handle(&payload, &fx.ctx(9_000_000)).unwrap();

        let (model, input, output, cost, pv): (String, i64, i64, f64, i64) = fx
            .conn
            .query_row(
                "SELECT model, input_tokens, output_tokens, cost_usd_api_equiv, pricing_version
                 FROM turns WHERE session_id='s' AND turn_idx=0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(model, "claude-opus-4-7-20260301");
        assert_eq!(input, 1_000_000);
        assert_eq!(output, 1_000_000);
        assert!((cost - 30.0).abs() < 1e-9);
        assert_eq!(pv, 1);
    }

    #[test]
    fn unknown_model_records_null_cost_and_keeps_tokens() {
        let fx = ContextFixture::new();
        let dir = fx.blobs_dir.parent().unwrap();
        let jsonl = r#"{"type":"assistant","message":{"model":"gpt-4-turbo","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let path = write_transcript(dir, "t.jsonl", jsonl);
        handle(
            &json!({"session_id":"s","transcript_path":path.to_str().unwrap()}),
            &fx.ctx(1000),
        )
        .unwrap();
        let (input, cost): (i64, Option<f64>) = fx
            .conn
            .query_row(
                "SELECT input_tokens, cost_usd_api_equiv FROM turns WHERE session_id='s'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(input, 100);
        assert!(cost.is_none());
    }

    #[test]
    fn missing_transcript_file_is_a_soft_skip() {
        let fx = ContextFixture::new();
        handle(
            &json!({
                "session_id": "s",
                "transcript_path": "/nonexistent/path/does-not-exist.jsonl",
            }),
            &fx.ctx(1000),
        )
        .unwrap();
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn batch_tier_halves_recorded_cost() {
        let fx = ContextFixture::new();
        let dir = fx.blobs_dir.parent().unwrap();
        let jsonl = r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":1000000,"output_tokens":0,"service_tier":"batch"}}}"#;
        let path = write_transcript(dir, "t.jsonl", jsonl);
        handle(
            &json!({"session_id":"s","transcript_path":path.to_str().unwrap()}),
            &fx.ctx(1000),
        )
        .unwrap();
        let cost: f64 = fx
            .conn
            .query_row(
                "SELECT cost_usd_api_equiv FROM turns WHERE session_id='s'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Haiku 4.5 input @ $1/Mtok, batch halves → $0.50.
        assert!((cost - 0.5).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn re_firing_updates_turns_in_place() {
        let fx = ContextFixture::new();
        let dir = fx.blobs_dir.parent().unwrap();
        let jsonl_v1 = r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":100,"output_tokens":0}}}"#;
        let path = write_transcript(dir, "t.jsonl", jsonl_v1);
        let pl = json!({"session_id":"s","transcript_path":path.to_str().unwrap()});
        handle(&pl, &fx.ctx(1000)).unwrap();

        // Transcript appended to in-place; re-fire to refresh the row.
        let jsonl_v2 = format!(
            "{jsonl_v1}\n{}",
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":200,"output_tokens":0}}}"#
        );
        fs::write(&path, jsonl_v2).unwrap();
        handle(&pl, &fx.ctx(2000)).unwrap();

        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM turns WHERE session_id='s'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 2);
    }
}
