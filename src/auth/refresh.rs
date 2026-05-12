//! Single-flight access-token refresh.
//!
//! WorkOS rotates refresh tokens — every successful refresh invalidates
//! the prior `refresh_token`. If two cc-ledger processes run concurrently
//! (e.g. an interactive command and Claude Code's `otelHeadersHelper`),
//! they would race: one wins, the other's `refresh_token` is stale and
//! the next refresh fails with `invalid_grant`, kicking the user out.
//!
//! [`ensure_fresh_token`] takes a file lock on `~/.cc-ledger/auth.lock`
//! around the load → check → refresh → save sequence to serialize them.
//! Inside the lock we re-read the file: another process that just
//! refreshed will have bumped `expires_at_ms` past our skew window, so
//! the second process simply uses the freshly-saved tokens.

use std::fs::OpenOptions;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;

use crate::auth::storage::{self, Tokens};
use crate::auth::workos;
use crate::paths;

/// Skew window: refresh if the access token has less than this many ms
/// of life remaining. Big enough that a slow refresh round-trip won't
/// hand back a token that expires mid-flight.
const SKEW_MS: i64 = 60_000;

pub fn ensure_fresh_token() -> Result<Tokens> {
    paths::ensure_dirs()?;
    let lock_path = paths::auth_lock_path()?;
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening lock file {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("acquiring lock on {}", lock_path.display()))?;

    let result = (|| {
        let stored = storage::load()?
            .ok_or_else(|| anyhow!("not logged in — run `cc-ledger auth` first"))?;

        // Inside the lock: another process may have just refreshed.
        if stored.expires_at_ms - paths::now_ms() > SKEW_MS {
            return Ok(stored);
        }

        let mut fresh = workos::refresh(&stored.refresh_token)?;
        // Refresh from WorkOS doesn't carry org context; reattach the
        // user's selection so it survives every refresh cycle.
        fresh.org_id = stored.org_id.clone();
        fresh.org_name = stored.org_name.clone();
        storage::save(&fresh)?;
        Ok(fresh)
    })();

    let _ = FileExt::unlock(&lock_file);
    result
}
