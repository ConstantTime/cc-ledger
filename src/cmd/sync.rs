//! `cc-ledger sync` — push local aggregates to ccledger.dev.
//!
//! Auto-triggered after every `SessionEnd` when the user is logged in.
//! Manual invocation prints the per-table count summary; `--background`
//! suppresses output (used by the auto-trigger fork).

use std::io::Write;

use anyhow::Result;
use clap::Parser;

use crate::{paths, store, sync};

#[derive(Debug, Parser)]
pub struct Args {
    /// Don't actually upload — just count rows that would go.
    #[arg(long)]
    pub dry_run: bool,

    /// Run silently (used by the SessionEnd background fork).
    #[arg(long)]
    pub background: bool,

    /// Re-upload everything from scratch by clearing local cursors.
    /// Server-side dedup means this is safe — every batch is upserted on a
    /// natural pk — but it can produce a lot of traffic.
    #[arg(long)]
    pub reset_cursor: bool,

    /// Run the codeburn-style activity-category backfill before syncing.
    /// Used by the post-auth-login background fork so the dashboard has
    /// data on first sign-in.
    #[arg(long)]
    pub full_backfill: bool,

    /// Skip the "About to upload …" prompt.
    #[arg(long)]
    pub yes: bool,
}

pub fn run(args: Args) -> Result<()> {
    let conn = store::open(&paths::db_path()?)?;

    if args.background {
        return sync::run_background(args.full_backfill);
    }

    if args.full_backfill {
        // Foreground full backfill — run categorize first so the sync below
        // picks up the new rows in this single invocation.
        crate::cmd::backfill::run_agent_turns(crate::cmd::backfill::AgentTurnsArgs {
            background: false,
            all: false,
        })?;
    }

    let opts = sync::SyncOptions {
        interactive: !args.yes,
        dry_run: args.dry_run,
        reset_cursor: args.reset_cursor,
    };

    let mut stderr = std::io::stderr();
    if !args.yes && !args.dry_run {
        writeln!(stderr, "About to upload to {}:", crate::config::api_base())?;
    }
    let report = sync::run(&conn, opts)?;

    let total = report.sessions
        + report.tool_aggregates
        + report.attributions
        + report.commits
        + report.pull_requests
        + report.pr_snapshots
        + report.agent_turns;

    let mut stdout = std::io::stdout();
    if args.dry_run {
        writeln!(stdout, "(dry-run) {total} rows total:")?;
    } else if total == 0 {
        writeln!(stdout, "Nothing to sync.")?;
    } else {
        writeln!(stdout, "Synced {total} rows:")?;
    }
    writeln!(stdout, "  sessions:        {}", report.sessions)?;
    writeln!(stdout, "  tool aggregates: {}", report.tool_aggregates)?;
    writeln!(stdout, "  attributions:    {}", report.attributions)?;
    writeln!(stdout, "  commits:         {}", report.commits)?;
    writeln!(stdout, "  pull requests:   {}", report.pull_requests)?;
    writeln!(stdout, "  pr snapshots:    {}", report.pr_snapshots)?;
    writeln!(stdout, "  agent turns:     {}", report.agent_turns)?;
    Ok(())
}
