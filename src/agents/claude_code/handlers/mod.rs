//! One file per Claude Code hook event, all behind a single dispatch table.
//!
//! Every handler module exposes the same signature:
//!
//! ```ignore
//! pub fn handle(payload: &serde_json::Value, ctx: &HookContext) -> anyhow::Result<()>
//! ```
//!
//! The cross-cutting "log every event to disk" concern lives once in
//! [`dispatch`] (gated by `CC_LEDGER_AUDIT=1`); per-event logic lives in the
//! file named for the event.
//!
//! Handler logic is filled in by Steps 5 / 6 / 7 of the foundation plan;
//! today every handler is a no-op stub.

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;

use super::events::ClaudeHookEvent;
use crate::pricing::PricingTable;

pub mod audit;
pub mod post_tool_use;
pub mod post_tool_use_failure;
pub mod pre_tool_use;
pub mod session_end;
pub mod session_start;
pub mod stop;
pub mod subagent_stop;

/// What every handler receives. Built once per hook fire by
/// `ClaudeCode::handle_hook` and threaded through.
pub struct HookContext<'a> {
    pub conn: &'a Connection,
    pub blobs_dir: &'a Path,
    pub audit_dir: &'a Path,
    pub pricing: &'a PricingTable,
    /// Captured once at the start of the fire so concurrent reads of
    /// `now_ms` within a single hook see the same value (deterministic
    /// in tests).
    pub now_ms: i64,
}

/// Route to the per-event handler. Unknown events no-op so Claude Code
/// can introduce new events without breaking us.
pub fn dispatch(event: ClaudeHookEvent, payload: &Value, ctx: &HookContext) -> Result<()> {
    if audit::enabled() {
        // Best-effort archive — failure here must not block the agent.
        let _ = audit::write_payload(ctx.audit_dir, event, payload, ctx.now_ms);
    }
    use ClaudeHookEvent::*;
    match event {
        SessionStart => session_start::handle(payload, ctx),
        SessionEnd => session_end::handle(payload, ctx),
        PreToolUse => pre_tool_use::handle(payload, ctx),
        PostToolUse => post_tool_use::handle(payload, ctx),
        PostToolUseFailure => post_tool_use_failure::handle(payload, ctx),
        Stop => stop::handle(payload, ctx),
        SubagentStop => subagent_stop::handle(payload, ctx),
        // Events we know about but don't act on (e.g. UserPromptSubmit —
        // prompt text already lives in `~/.claude/projects/.../*.jsonl`),
        // plus events Claude Code may add later: silently ignore.
        _ => Ok(()),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared fixture for handler unit tests. Owns a tempdir + sqlite
    //! connection + pricing table; lends a borrowed [`HookContext`] via
    //! [`ContextFixture::ctx`].

    use std::path::PathBuf;

    use rusqlite::Connection;

    use super::HookContext;
    use crate::pricing::PricingTable;

    pub struct ContextFixture {
        _dir: tempfile::TempDir,
        pub conn: Connection,
        pub pricing: PricingTable,
        pub blobs_dir: PathBuf,
        pub audit_dir: PathBuf,
    }

    impl ContextFixture {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let conn = crate::store::open(&dir.path().join("ledger.db")).unwrap();
            let blobs_dir = dir.path().join("blobs");
            let audit_dir = dir.path().join("audit");
            std::fs::create_dir_all(&blobs_dir).unwrap();
            std::fs::create_dir_all(&audit_dir).unwrap();
            Self {
                _dir: dir,
                conn,
                pricing: PricingTable::embedded().unwrap(),
                blobs_dir,
                audit_dir,
            }
        }

        pub fn ctx(&self, now_ms: i64) -> HookContext<'_> {
            HookContext {
                conn: &self.conn,
                blobs_dir: &self.blobs_dir,
                audit_dir: &self.audit_dir,
                pricing: &self.pricing,
                now_ms,
            }
        }
    }
}
