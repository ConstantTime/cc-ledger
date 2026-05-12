//! Periodic auto-sync. Called near the top of `cli::run()` for any
//! command that's allowed to opportunistically push fresh data.
//!
//! Decision tree:
//! 1. Honor `--no-autosync` and `CC_LEDGER_NO_AUTOSYNC=1` — bail.
//! 2. Skip commands that shouldn't trigger sync (Sync, Auth, Install, etc.).
//! 3. Skip if not authenticated.
//! 4. Skip if `last_sync_at_ms` is fresher than `AUTOSYNC_THRESHOLD_MS`.
//! 5. Atomically claim the sync slot (`sync_in_progress_since_ms` in
//!    `config`); skip if another process is already syncing.
//! 6. Detached fork of `cc-ledger sync --background`; we return immediately
//!    so the user's actual command runs without delay.
//!
//! All failures swallow silently. Auto-sync is opportunistic — never fail
//! the user's command because a background push couldn't start.

use std::time::Duration;

use rusqlite::Connection;

use crate::auth::storage;
use crate::cli::Command;
use crate::store::queries;
use crate::{paths, store, sync};

/// How fresh the last sync must be to skip an autosync. 15 min keeps the
/// dashboard close-to-live without thrashing the network.
pub const AUTOSYNC_THRESHOLD_MS: i64 = 15 * 60 * 1000;

/// `sync_in_progress_since_ms` older than this is considered stale and
/// reclaimable (a previous sync process must have crashed).
pub const SYNC_IN_PROGRESS_STALE_MS: i64 = 10 * 60 * 1000;

const ENV_DISABLE: &str = "CC_LEDGER_NO_AUTOSYNC";

/// Best-effort top-level entry point. Never returns Err.
pub fn maybe_spawn(cmd: &Option<Command>, no_autosync: bool) {
    let _ = try_maybe_spawn(cmd, no_autosync);
}

fn try_maybe_spawn(cmd: &Option<Command>, no_autosync: bool) -> Result<(), ()> {
    if no_autosync {
        return Err(());
    }
    if std::env::var(ENV_DISABLE)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Err(());
    }
    if !allows_autosync(cmd) {
        return Err(());
    }
    // Cheap check first: is the user logged in?
    match storage::load() {
        Ok(Some(_)) => {}
        _ => return Err(()),
    }

    let db_path = paths::db_path().map_err(|_| ())?;
    let conn = store::open(&db_path).map_err(|_| ())?;
    let now_ms = paths::now_ms();

    let last = queries::get_config_i64(&conn, "last_sync_at_ms")
        .map_err(|_| ())?
        .unwrap_or(0);
    if now_ms - last < AUTOSYNC_THRESHOLD_MS {
        return Err(());
    }

    if !try_claim_sync_slot(&conn, now_ms).map_err(|_| ())? {
        return Err(());
    }

    drop(conn);

    let bin = std::env::current_exe().map_err(|_| ())?;
    let _ = sync::spawn_background_sync(&bin);
    Ok(())
}

/// Commands that should fire opportunistic autosync. Keep this list small —
/// the goal is fresh data on the dashboard with zero cost to the user's
/// flow, not maximum sync frequency.
fn allows_autosync(cmd: &Option<Command>) -> bool {
    matches!(
        cmd,
        Some(Command::Hook(_)) | Some(Command::Stats(_)) | Some(Command::PrCost(_))
    )
}

/// Atomic compare-and-set on the `sync_in_progress_since_ms` config key.
/// Returns true if we won the race. Stale entries (older than
/// `SYNC_IN_PROGRESS_STALE_MS`) are reclaimed.
pub fn try_claim_sync_slot(conn: &Connection, now_ms: i64) -> anyhow::Result<bool> {
    // BEGIN IMMEDIATE serializes writers. Cheaper than a separate file lock
    // and survives across our background fork (the child re-opens the DB).
    conn.busy_timeout(Duration::from_secs(2))?;
    let tx = conn.unchecked_transaction()?;
    let cur = queries::get_config_i64(&tx, "sync_in_progress_since_ms")?;
    let stale = cur
        .map(|v| now_ms - v > SYNC_IN_PROGRESS_STALE_MS)
        .unwrap_or(true);
    if !stale {
        // Someone else holds the slot recently — back off.
        tx.rollback()?;
        return Ok(false);
    }
    queries::set_config_i64(&tx, "sync_in_progress_since_ms", now_ms)?;
    tx.commit()?;
    Ok(true)
}

pub fn release_sync_slot(conn: &Connection) -> anyhow::Result<()> {
    queries::delete_config(conn, "sync_in_progress_since_ms")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        std::mem::forget(dir);
        conn
    }

    #[test]
    fn first_claim_wins_second_loses() {
        let conn = fresh();
        let now = 1_000_000;
        assert!(try_claim_sync_slot(&conn, now).unwrap());
        // No release yet — second attempt sees a fresh slot and bails.
        assert!(!try_claim_sync_slot(&conn, now + 1000).unwrap());
    }

    #[test]
    fn stale_slot_is_reclaimable() {
        let conn = fresh();
        let now = 1_000_000;
        assert!(try_claim_sync_slot(&conn, now).unwrap());
        // Roll forward past the stale threshold; the slot becomes claimable.
        let later = now + SYNC_IN_PROGRESS_STALE_MS + 1;
        assert!(try_claim_sync_slot(&conn, later).unwrap());
    }

    #[test]
    fn release_clears_slot() {
        let conn = fresh();
        let now = 1_000_000;
        assert!(try_claim_sync_slot(&conn, now).unwrap());
        release_sync_slot(&conn).unwrap();
        // Slot is gone — fresh claim wins.
        assert!(try_claim_sync_slot(&conn, now + 100).unwrap());
    }
}
