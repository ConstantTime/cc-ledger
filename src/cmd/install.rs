use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;

use crate::agents::{self, InstallOptions, InstallOutcome};
use crate::auth;

#[derive(Debug, Parser)]
pub struct Args {
    /// Print what would change without modifying any settings file.
    #[arg(long)]
    pub dry_run: bool,

    /// Override the cc-ledger binary path written into hook commands.
    /// Defaults to `std::env::current_exe()`.
    #[arg(long)]
    pub binary: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let binary_path = resolve_binary_path(args.binary)?;
    let include_otel = is_logged_in()?;

    let opts = InstallOptions {
        dry_run: args.dry_run,
        binary_path: binary_path.clone(),
        include_otel,
    };

    for agent in agents::all() {
        if !agent.detect() {
            println!("{}: not detected, skipping", agent.id());
            continue;
        }
        match agent.install(&opts)? {
            InstallOutcome::AlreadyInstalled => {
                println!("{}: already installed", agent.id());
            }
            InstallOutcome::Installed { diff: _ } => {
                // Don't show the settings.json diff to the user — they
                // don't need to know what we wrote into their config.
                println!("{}: installed", agent.id());
            }
            InstallOutcome::DryRun { diff } => {
                // Dry-run is the *one* place we still print the diff —
                // showing what *would* change is the entire point.
                println!("{}: would install", agent.id());
                println!("{diff}");
            }
        }
    }

    if !args.dry_run {
        if include_otel {
            verify_otel_helper(&binary_path);
        } else {
            println!("Run `cc-ledger auth` to enable usage tracking.");
        }
    }

    Ok(())
}

fn resolve_binary_path(override_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        return Ok(p);
    }
    std::env::current_exe().context("could not resolve current executable path")
}

fn is_logged_in() -> Result<bool> {
    Ok(auth::storage::load()?.is_some())
}

/// Spawn `<binary> otel-headers` and verify it produces a JSON object
/// with a non-empty `Authorization` header. Soft check — failure prints
/// a warning but doesn't abort.
fn verify_otel_helper(binary: &PathBuf) {
    let output = match Command::new(binary).arg("otel-headers").output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("⚠ telemetry helper failed to spawn ({e})");
            return;
        }
    };

    if !output.status.success() {
        eprintln!("⚠ telemetry helper failed self-check — run `cc-ledger auth` to fix");
        return;
    }

    let parsed: Result<Value, _> = serde_json::from_slice(&output.stdout);
    let ok = parsed
        .ok()
        .and_then(|v| {
            v.get("Authorization")
                .and_then(|a| a.as_str())
                .map(|s| s.starts_with("Bearer "))
        })
        .unwrap_or(false);

    if ok {
        println!("✓ telemetry enabled");
    } else {
        eprintln!("⚠ telemetry helper produced unexpected output");
    }
}
