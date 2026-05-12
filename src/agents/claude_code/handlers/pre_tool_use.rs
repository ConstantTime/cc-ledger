//! `PreToolUse` — snapshot the baseline file blob before Claude edits.
//!
//! For file-editing tools (Edit / Write / MultiEdit / NotebookEdit) we read
//! the current file contents into a content-addressed blob and write a
//! `tool_calls` row with `status='pending'`. PostToolUse closes it.
//!
//! Non-file tools (Bash, Read, Glob, ...) are soft-skipped.

use std::path::Path;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::super::tool_input;
use super::HookContext;
use crate::store::{blobs, queries};

#[derive(Deserialize, Default)]
struct Payload {
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input: Option<Value>,
}

pub fn handle(payload: &Value, ctx: &HookContext) -> Result<()> {
    let p: Payload = serde_json::from_value(payload.clone()).unwrap_or_default();
    let (Some(session_id), Some(tool_name), Some(tool_use_id)) = (
        p.session_id.as_deref(),
        p.tool_name.as_deref(),
        p.tool_use_id.as_deref(),
    ) else {
        return Ok(());
    };

    let tool_input = p.tool_input.unwrap_or(Value::Null);
    let Some(file_path) = tool_input::affected_file(tool_name, &tool_input) else {
        return Ok(());
    };

    let pre_sha = blobs::put_file_as_blob(ctx.blobs_dir, Path::new(&file_path))?;
    queries::record_pre_tool(
        ctx.conn,
        session_id,
        tool_use_id,
        tool_name,
        Some(&file_path),
        ctx.now_ms,
        Some(&pre_sha),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::handlers::test_support::ContextFixture;
    use serde_json::json;
    use std::fs;

    #[test]
    fn edit_records_pending_tool_call_with_pre_blob() {
        let fx = ContextFixture::new();
        let file = fx.blobs_dir.parent().unwrap().join("a.rs");
        fs::write(&file, "line1\nline2\n").unwrap();

        let payload = json!({
            "session_id": "s1",
            "tool_name": "Edit",
            "tool_use_id": "tu1",
            "tool_input": { "file_path": file.to_str().unwrap() },
        });
        handle(&payload, &fx.ctx(100)).unwrap();

        let (status, pre_sha, file_path): (String, String, String) = fx
            .conn
            .query_row(
                "SELECT status, pre_blob_sha, file_path FROM tool_calls
                 WHERE session_id='s1' AND tool_use_id='tu1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "pending");
        assert!(!pre_sha.is_empty());
        assert_eq!(file_path, file.to_str().unwrap());
    }

    #[test]
    fn write_to_nonexistent_file_uses_empty_blob() {
        let fx = ContextFixture::new();
        let payload = json!({
            "session_id": "s1",
            "tool_name": "Write",
            "tool_use_id": "tu1",
            "tool_input": { "file_path": "/nonexistent/x.rs" },
        });
        handle(&payload, &fx.ctx(100)).unwrap();
        let pre_sha: String = fx
            .conn
            .query_row(
                "SELECT pre_blob_sha FROM tool_calls WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Empty blob's sha256
        assert_eq!(
            pre_sha,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn bash_is_soft_skipped() {
        let fx = ContextFixture::new();
        let payload = json!({
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_use_id": "tu1",
            "tool_input": { "command": "ls" },
        });
        handle(&payload, &fx.ctx(100)).unwrap();
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn missing_required_field_is_a_soft_skip() {
        let fx = ContextFixture::new();
        // No tool_use_id
        let payload = json!({
            "session_id": "s1",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/x" },
        });
        handle(&payload, &fx.ctx(100)).unwrap();
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
