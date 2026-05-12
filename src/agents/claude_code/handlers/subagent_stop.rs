//! `SubagentStop` — same payload shape as `Stop`, scoped to subagent turns.
//!
//! For now this delegates to [`super::stop::handle`]. Any future divergence
//! (e.g. tagging subagent turns) goes here.

use anyhow::Result;
use serde_json::Value;

use super::HookContext;

pub fn handle(payload: &Value, ctx: &HookContext) -> Result<()> {
    super::stop::handle(payload, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::handlers::test_support::ContextFixture;
    use serde_json::json;
    use std::fs;

    #[test]
    fn delegates_to_stop_and_records_turn() {
        let fx = ContextFixture::new();
        let dir = fx.blobs_dir.parent().unwrap();
        let path = dir.join("sub.jsonl");
        fs::write(
            &path,
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":1000,"output_tokens":500}}}"#,
        )
        .unwrap();
        handle(
            &json!({
                "session_id": "sub",
                "transcript_path": path.to_str().unwrap(),
            }),
            &fx.ctx(1000),
        )
        .unwrap();
        let n: i64 = fx
            .conn
            .query_row(
                "SELECT COUNT(*) FROM turns WHERE session_id='sub'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }
}
