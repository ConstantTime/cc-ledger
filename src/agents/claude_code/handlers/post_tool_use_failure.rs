//! `PostToolUseFailure` — flag the tool call as failed; no attribution.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::HookContext;
use crate::store::queries;

#[derive(Deserialize, Default)]
struct Payload {
    session_id: Option<String>,
    tool_use_id: Option<String>,
    error: Option<String>,
}

pub fn handle(payload: &Value, ctx: &HookContext) -> Result<()> {
    let p: Payload = serde_json::from_value(payload.clone()).unwrap_or_default();
    let (Some(session_id), Some(tool_use_id)) = (p.session_id.as_deref(), p.tool_use_id.as_deref())
    else {
        return Ok(());
    };
    queries::mark_tool_failure(
        ctx.conn,
        session_id,
        tool_use_id,
        ctx.now_ms,
        p.error.as_deref(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::handlers::pre_tool_use;
    use crate::agents::claude_code::handlers::test_support::ContextFixture;
    use serde_json::json;
    use std::fs;

    #[test]
    fn marks_existing_call_as_failure() {
        let fx = ContextFixture::new();
        let file = fx.blobs_dir.parent().unwrap().join("a.rs");
        fs::write(&file, "x\n").unwrap();
        let pre_payload = json!({
            "session_id": "s",
            "tool_name": "Edit",
            "tool_use_id": "tu",
            "tool_input": { "file_path": file.to_str().unwrap() },
        });
        pre_tool_use::handle(&pre_payload, &fx.ctx(100)).unwrap();
        let fail = json!({
            "session_id": "s",
            "tool_use_id": "tu",
            "error": "permission denied",
        });
        handle(&fail, &fx.ctx(200)).unwrap();

        let (status, err, end_ms): (String, String, i64) = fx
            .conn
            .query_row(
                "SELECT status, error_message, end_ms FROM tool_calls",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "failure");
        assert_eq!(err, "permission denied");
        assert_eq!(end_ms, 200);
    }

    #[test]
    fn missing_session_id_is_a_soft_skip() {
        let fx = ContextFixture::new();
        handle(&json!({ "tool_use_id": "tu" }), &fx.ctx(100)).unwrap();
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
