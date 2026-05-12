pub mod agents;
pub mod auth;
pub mod autosync;
pub mod cli;
pub mod cmd;
pub mod config;
pub mod diff;
pub mod git;
pub mod paths;
pub mod pr;
pub mod pricing;
pub mod store;
pub mod sync;
pub mod updater;

/// Process entry point. Parses args, dispatches, returns the process exit code.
///
/// We never propagate errors as non-zero exits because Claude Code hooks must
/// not block the agent — any failure is logged to stderr and we return 0.
pub fn run() -> i32 {
    match cli::run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cc-ledger: {e:#}");
            0
        }
    }
}
