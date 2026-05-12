//! `SessionStart` — stamp the `sessions` row.
//!
//! Captures session lifecycle metadata: id, agent, started_at, cwd, model,
//! OS, hostname, and billing mode (env-driven). Idempotent: re-running with
//! the same `session_id` updates fields without overwriting `started_at`.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::HookContext;
use crate::store::queries::{self, SessionInit};

const AGENT_ID: &str = "claude-code";

/// Just the fields this handler reads. Other payload fields are ignored.
#[derive(Deserialize, Default)]
struct Payload {
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    #[allow(dead_code)]
    source: Option<String>,
}

pub fn handle(payload: &Value, ctx: &HookContext) -> Result<()> {
    let p: Payload = serde_json::from_value(payload.clone()).unwrap_or_default();
    let Some(session_id) = p.session_id.as_deref() else {
        return Ok(()); // missing required field — soft skip
    };

    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .ok();
    let billing_mode = std::env::var("CC_LEDGER_BILLING").ok();

    queries::upsert_session(
        ctx.conn,
        &SessionInit {
            session_id,
            agent_id: AGENT_ID,
            started_at: Some(ctx.now_ms),
            cwd: p.cwd.as_deref(),
            model: p.model.as_deref(),
            os: Some(std::env::consts::OS),
            hostname: hostname.as_deref(),
            cc_version: None,
            billing_mode: billing_mode.as_deref(),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::handlers::test_support::ContextFixture;
    use serde_json::json;

    #[test]
    fn inserts_row_with_supplied_fields() {
        let fx = ContextFixture::new();
        let payload = json!({
            "session_id": "s1",
            "cwd": "/repo",
            "model": "claude-opus-4-7-20260301",
            "source": "startup",
        });
        handle(&payload, &fx.ctx(1000)).unwrap();

        let (sid, agent, started, cwd, model): (String, String, i64, String, String) = fx
            .conn
            .query_row(
                "SELECT session_id, agent_id, started_at, cwd, model
                 FROM sessions WHERE session_id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(sid, "s1");
        assert_eq!(agent, AGENT_ID);
        assert_eq!(started, 1000);
        assert_eq!(cwd, "/repo");
        assert_eq!(model, "claude-opus-4-7-20260301");
    }

    #[test]
    fn missing_session_id_is_a_soft_skip() {
        let fx = ContextFixture::new();
        let payload = json!({ "cwd": "/repo" });
        handle(&payload, &fx.ctx(1000)).unwrap();
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn second_call_does_not_overwrite_started_at() {
        let fx = ContextFixture::new();
        handle(&json!({ "session_id": "s1" }), &fx.ctx(1000)).unwrap();
        // Re-fire with a later "started_at"; should be ignored.
        handle(
            &json!({ "session_id": "s1", "model": "claude-haiku-4-5" }),
            &fx.ctx(2000),
        )
        .unwrap();
        let (started, model): (i64, String) = fx
            .conn
            .query_row(
                "SELECT started_at, model FROM sessions WHERE session_id='s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(started, 1000);
        assert_eq!(model, "claude-haiku-4-5");
    }

    #[test]
    fn os_field_is_populated() {
        let fx = ContextFixture::new();
        handle(&json!({ "session_id": "s1" }), &fx.ctx(1000)).unwrap();
        let os: String = fx
            .conn
            .query_row("SELECT os FROM sessions WHERE session_id='s1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(matches!(os.as_str(), "macos" | "linux" | "windows"));
    }
}
