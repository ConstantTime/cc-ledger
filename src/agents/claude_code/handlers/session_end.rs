//! `SessionEnd` — stamp `sessions.ended_at`, then recompute PR rollups for
//! any open PR in the session's `cwd` and reconcile squash-merges on the base
//! branch.
//!
//! Idempotent: if `SessionEnd` fires before any `SessionStart` (e.g. hook
//! misfire), we still create a sessions row so the ended_at isn't lost.

use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::HookContext;
use crate::auth;
use crate::pr;
use crate::store::queries;
use crate::sync;

#[derive(Deserialize, Default)]
struct Payload {
    session_id: Option<String>,
    cwd: Option<String>,
}

pub fn handle(payload: &Value, ctx: &HookContext) -> Result<()> {
    let p: Payload = serde_json::from_value(payload.clone()).unwrap_or_default();
    let Some(session_id) = p.session_id.as_deref() else {
        return Ok(());
    };
    queries::end_session(ctx.conn, session_id, ctx.now_ms)?;

    // PR housekeeping is best-effort. Failures are swallowed so a bad git
    // state never blocks SessionEnd's main job.
    if let Some(cwd) = p.cwd.as_deref() {
        let cwd_path = PathBuf::from(cwd);
        let _ = pr::reconcile_squash_merges(ctx.conn, &cwd_path, ctx.now_ms);
        // Recompute rollups for any open PR in this cwd that the session
        // could have touched. Cheap — at most a handful of PRs per cwd.
        if let Ok(prs) = queries::open_prs(ctx.conn, cwd, None) {
            for (pr_number, _) in prs {
                let _ = pr::recompute_rollup(ctx.conn, cwd, pr_number, ctx.now_ms);
            }
        }
    }

    // Auto-sync to ccledger.dev when authenticated. Forked into a child so
    // SessionEnd returns immediately; the child runs `cc-ledger sync
    // --background` against the same DB. Auth check is by file existence —
    // we don't want to perform a token refresh inside a hook.
    if auth::storage::load().ok().flatten().is_some() {
        if let Ok(self_path) = std::env::current_exe() {
            let _ = sync::spawn_background_sync(&self_path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::handlers::test_support::ContextFixture;
    use crate::agents::claude_code::handlers::{session_start, HookContext};
    use serde_json::json;

    fn ctx(fx: &ContextFixture, now_ms: i64) -> HookContext<'_> {
        fx.ctx(now_ms)
    }

    #[test]
    fn end_after_start_records_ended_at() {
        let fx = ContextFixture::new();
        session_start::handle(&json!({ "session_id": "s1" }), &ctx(&fx, 1000)).unwrap();
        handle(&json!({ "session_id": "s1" }), &ctx(&fx, 5000)).unwrap();
        let (started, ended): (i64, i64) = fx
            .conn
            .query_row(
                "SELECT started_at, ended_at FROM sessions WHERE session_id='s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(started, 1000);
        assert_eq!(ended, 5000);
    }

    #[test]
    fn end_without_prior_start_creates_a_row() {
        let fx = ContextFixture::new();
        handle(&json!({ "session_id": "orphan" }), &ctx(&fx, 5000)).unwrap();
        let ended: i64 = fx
            .conn
            .query_row(
                "SELECT ended_at FROM sessions WHERE session_id='orphan'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ended, 5000);
    }

    #[test]
    fn missing_session_id_is_a_soft_skip() {
        let fx = ContextFixture::new();
        handle(&json!({}), &ctx(&fx, 1000)).unwrap();
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
