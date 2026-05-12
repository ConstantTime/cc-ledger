//! Path resolution for the cc-ledger home directory.
//!
//! All filenames and env-var names come from [`crate::config`] — this
//! module owns only the *resolution logic* (env override → home_dir
//! fallback → joins).

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;

/// Root directory: `$CC_LEDGER_HOME` or `~/.cc-ledger`.
pub fn home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(config::HOME_ENV_VAR) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = dirs::home_dir().context("home directory not found")?;
    Ok(home.join(config::HOME_DIR_NAME))
}

pub fn db_path() -> Result<PathBuf> {
    Ok(home()?.join(config::DB_FILE_NAME))
}

pub fn blobs_dir() -> Result<PathBuf> {
    Ok(home()?.join(config::BLOBS_DIR_NAME))
}

pub fn audit_dir() -> Result<PathBuf> {
    Ok(home()?.join(config::AUDIT_DIR_NAME))
}

pub fn auth_path() -> Result<PathBuf> {
    Ok(home()?.join(config::AUTH_FILE_NAME))
}

pub fn auth_lock_path() -> Result<PathBuf> {
    Ok(home()?.join(config::AUTH_LOCK_FILE_NAME))
}

pub fn version_check_path() -> Result<PathBuf> {
    Ok(home()?.join(config::VERSION_CHECK_FILE_NAME))
}

pub fn pricing_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(config::PRICING_PATH_ENV_VAR) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(home()?.join(config::PRICING_FILE_NAME))
}

/// Create the root + `blobs/` if missing. Idempotent.
pub fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(home()?)?;
    std::fs::create_dir_all(blobs_dir()?)?;
    Ok(())
}

/// Current time as Unix ms.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
