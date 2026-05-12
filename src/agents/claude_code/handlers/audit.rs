//! Opt-in raw-payload archive.
//!
//! When `CC_LEDGER_AUDIT=1`, `dispatch` writes the raw stdin JSON of every
//! hook event under `~/.cc-ledger/audit/<session_id>/<EventName>-<ts_ms>.json`.
//! Failures are best-effort — never block the agent.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::super::events::ClaudeHookEvent;

/// Is audit mode on for this process?
pub fn enabled() -> bool {
    matches!(std::env::var("CC_LEDGER_AUDIT").as_deref(), Ok("1"))
}

/// Write `payload` to `<audit_dir>/<session>/<Event>-<ts>.json`. Session id
/// is taken from the payload; missing → `"unknown"`.
pub fn write_payload(
    audit_dir: &Path,
    event: ClaudeHookEvent,
    payload: &Value,
    now_ms: i64,
) -> Result<()> {
    let session = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let dir = audit_dir.join(session);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}-{}.json", event.as_wire_name(), now_ms));
    let text = serde_json::to_string_pretty(payload)?;
    std::fs::write(&file, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_under_session_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let payload = json!({ "session_id": "abc", "hook_event_name": "PreToolUse" });
        write_payload(dir.path(), ClaudeHookEvent::PreToolUse, &payload, 12345).unwrap();
        let f = dir.path().join("abc").join("PreToolUse-12345.json");
        assert!(f.exists());
        let content: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(content["session_id"], "abc");
    }

    #[test]
    fn missing_session_id_falls_back_to_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let payload = json!({ "hook_event_name": "Stop" });
        write_payload(dir.path(), ClaudeHookEvent::Stop, &payload, 1).unwrap();
        assert!(dir.path().join("unknown").join("Stop-1.json").exists());
    }
}
