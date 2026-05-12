//! Claude Code agent.

use anyhow::{Context, Result};
use serde_json::Value;

use super::{Agent, InstallOptions, InstallOutcome};
use crate::{paths, pricing::PricingTable, store};

pub mod agent_turns;
pub mod categorize;
pub mod events;
pub mod handlers;
pub mod settings;
pub mod tool_input;
pub mod transcript;

pub struct ClaudeCode;

impl Agent for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self) -> bool {
        // Claude Code creates `~/.claude/` on first run. Treat its existence
        // as the "Claude is installed" signal — cheap and reliable.
        settings::config_dir().exists()
    }

    fn install(&self, opts: &InstallOptions) -> Result<InstallOutcome> {
        settings::install(opts)
    }

    fn handle_hook(&self, payload: &Value) -> Result<()> {
        let event_name = payload
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .context("hook payload missing `hook_event_name`")?;
        let event = match events::ClaudeHookEvent::from_wire_name(event_name) {
            Some(e) => e,
            // Unknown events are no-ops: Claude Code may add new ones we
            // don't know about yet, and we don't want to fail the agent's
            // turn.
            None => return Ok(()),
        };

        // Resolve once per fire so handlers all see the same paths and the
        // same wall-clock instant.
        paths::ensure_dirs()?;
        let db_path = paths::db_path()?;
        let blobs_dir = paths::blobs_dir()?;
        let audit_dir = paths::audit_dir()?;
        let pricing_path = paths::pricing_path()?;
        let now_ms = paths::now_ms();

        let conn = store::open(&db_path)?;
        let pricing = PricingTable::load(Some(&pricing_path))?;

        let ctx = handlers::HookContext {
            conn: &conn,
            blobs_dir: &blobs_dir,
            audit_dir: &audit_dir,
            pricing: &pricing,
            now_ms,
        };
        handlers::dispatch(event, payload, &ctx)
    }
}
