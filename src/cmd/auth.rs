use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::auth;

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    pub action: Option<Action>,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Show whether the CLI has saved tokens and when the access token expires.
    Status,
    /// Remove saved tokens from `~/.cc-ledger/auth.json`.
    Logout,
}

pub fn run(args: Args) -> Result<()> {
    match args.action {
        None => auth::login(),
        Some(Action::Status) => auth::status(),
        Some(Action::Logout) => auth::logout(),
    }
}
