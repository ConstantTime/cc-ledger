//! Coding-agent abstraction.
//!
//! Each supported coding agent (Claude Code, Cursor, Codex, …) implements
//! [`Agent`] in its own submodule. The internal `REGISTRY` is the single
//! source of truth for which agents `cc-ledger` knows about; the CLI
//! iterates it via [`all`] for `install` and looks up by id via [`by_id`]
//! for `hook`.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

pub mod claude_code;

/// What every coding agent must do for `cc-ledger`.
pub trait Agent: Send + Sync {
    /// Stable kebab-case id used in CLI flags, logs, and the command line we
    /// write into the agent's settings file.
    fn id(&self) -> &'static str;

    /// Cheap probe: is this agent present on the current machine? No side
    /// effects. The default heuristic is "the agent's config directory
    /// exists" — implementations may override with something stricter.
    fn detect(&self) -> bool;

    /// Install or update this agent's hooks. Idempotent.
    fn install(&self, opts: &InstallOptions) -> Result<InstallOutcome>;

    /// Handle a single hook event. `payload` is the raw JSON the agent sent
    /// on stdin.
    fn handle_hook(&self, payload: &Value) -> Result<()>;
}

/// Options shared by every agent's install path.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Show what would change but don't write.
    pub dry_run: bool,
    /// Absolute path of the `cc-ledger` binary to embed in the hook command.
    pub binary_path: PathBuf,
    /// When true, agents that support telemetry (Claude Code via OTel)
    /// also write the OTLP export config + headers helper. Set by the
    /// caller based on `auth::storage::load()`'s result — telemetry is
    /// not configured for logged-out users.
    pub include_otel: bool,
}

/// Result of an install attempt.
#[derive(Debug, Clone)]
pub enum InstallOutcome {
    /// Already installed and up to date — nothing written.
    AlreadyInstalled,
    /// Settings file changed and written.
    Installed { diff: String },
    /// `dry_run` was true; this is what would have been written.
    DryRun { diff: String },
}

/// All agents known to `cc-ledger`. Adding a new agent = one line here.
static REGISTRY: &[&dyn Agent] = &[&claude_code::ClaudeCode];

/// Every registered agent, in registry order.
pub fn all() -> &'static [&'static dyn Agent] {
    REGISTRY
}

/// Look up an agent by its [`Agent::id`].
pub fn by_id(id: &str) -> Option<&'static dyn Agent> {
    REGISTRY.iter().copied().find(|a| a.id() == id)
}
