use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

use crate::autosync;
use crate::cmd;
use crate::config;
use crate::updater::{self, CmdKind, UpdateOutcome};

#[derive(Debug, Parser)]
#[command(
    name = "cc-ledger",
    version,
    about = "Local ledger of Claude Code edits and per-turn token usage"
)]
pub struct Cli {
    /// Check for and install a new version before running. Works with
    /// any subcommand (or alone). Equivalent to setting
    /// `CC_LEDGER_AUTO_UPDATE=1` for a single invocation.
    #[arg(long, global = true)]
    pub update: bool,

    /// Skip the opportunistic background sync that runs at the start of
    /// most CLI invocations. Equivalent to `CC_LEDGER_NO_AUTOSYNC=1`.
    #[arg(long, global = true)]
    pub no_autosync: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install cc-ledger hooks into ~/.claude/settings.json.
    Install(cmd::install::Args),
    /// Handle a Claude Code hook event. Reads JSON payload from stdin.
    Hook(cmd::hook::Args),
    /// Show usage statistics from the local ledger.
    Stats(cmd::stats::Args),
    /// Show cost-per-PR from the local ledger. No network, no auth.
    PrCost(cmd::pr_cost::Args),
    /// Read or set local config keys (e.g. `git_notes.enabled`).
    Config(cmd::config::Args),
    /// Authenticate with WorkOS via Device Authorization Flow.
    Auth(cmd::auth::Args),
    /// Push local aggregates to ccledger.dev. Auto-runs at SessionEnd if logged in.
    Sync(cmd::sync::Args),
    /// Re-classify codeburn-style activity categories from local Claude Code transcripts.
    Backfill(cmd::backfill::Args),
    /// Print OTel auth headers JSON for Claude Code's otelHeadersHelper.
    #[command(hide = true)]
    OtelHeaders,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let kind = cmd_kind(&cli.command);

    // Best-effort opportunistic sync. Forks a detached `cc-ledger sync
    // --background` if the user is logged in and the last sync watermark
    // is older than 15 min, then continues immediately to dispatch.
    autosync::maybe_spawn(&cli.command, cli.no_autosync);

    // Honor `--update` and (for user-facing commands only) the
    // `CC_LEDGER_AUTO_UPDATE=1` env var. Skip silently for hooks and
    // otel-headers to avoid slowing down machine-driven invocations.
    let auto = std::env::var(config::AUTO_UPDATE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let want_update = cli.update || (auto && kind == CmdKind::UserFacing);

    if want_update {
        match updater::run_update() {
            Ok(UpdateOutcome::AlreadyLatest { version }) => {
                if cli.update {
                    eprintln!("cc-ledger {version} is the latest version");
                }
                // Continue to the regular dispatch below.
            }
            Ok(UpdateOutcome::Updated { from, to }) => {
                eprintln!("✓ updated cc-ledger {from} → {to}");
                if cli.command.is_some() {
                    // Re-exec into the freshly-installed binary with the
                    // original argv (minus --update). Never returns on
                    // success; on failure we surface the error.
                    let _: std::convert::Infallible = updater::reexec_self()?;
                }
                return Ok(());
            }
            Err(e) if cli.update => return Err(e),
            Err(e) => {
                eprintln!("⚠ auto-update failed: {e}; continuing with current binary");
            }
        }
    }

    let result = match cli.command {
        Some(Command::Install(args)) => cmd::install::run(args),
        Some(Command::Hook(args)) => cmd::hook::run(args),
        Some(Command::Stats(args)) => cmd::stats::run(args),
        Some(Command::PrCost(args)) => cmd::pr_cost::run(args),
        Some(Command::Config(args)) => cmd::config::run(args),
        Some(Command::Auth(args)) => cmd::auth::run(args),
        Some(Command::Sync(args)) => cmd::sync::run(args),
        Some(Command::Backfill(args)) => cmd::backfill::run(args),
        Some(Command::OtelHeaders) => cmd::otel_headers::run(),
        None => {
            // No subcommand. If --update was the whole point, we already
            // handled it above. Otherwise show help so the user sees the
            // available commands instead of a silent no-op.
            if !want_update {
                Cli::command().print_help().ok();
                println!();
            }
            Ok(())
        }
    };

    // Post-dispatch banner — soft check, never blocks or fails the
    // command. Suppressed for machine-driven subcommands and when an
    // update was just performed (we don't need to nag if we just
    // updated, and `Updated` already returned above).
    if !want_update && result.is_ok() {
        updater::maybe_show_banner(kind);
    }

    result
}

fn cmd_kind(cmd: &Option<Command>) -> CmdKind {
    match cmd {
        Some(Command::Hook(_)) | Some(Command::OtelHeaders) => CmdKind::MachineDriven,
        // `cc-ledger sync --background` is forked from SessionEnd; treat it
        // as machine-driven so the post-command banner doesn't print to a
        // detached stderr that nobody reads.
        Some(Command::Sync(args)) if args.background => CmdKind::MachineDriven,
        // Same for the post-auth backfill fork.
        Some(Command::Backfill(args)) => match &args.action {
            cmd::backfill::Action::AgentTurns(a) if a.background => CmdKind::MachineDriven,
            _ => CmdKind::UserFacing,
        },
        _ => CmdKind::UserFacing,
    }
}
