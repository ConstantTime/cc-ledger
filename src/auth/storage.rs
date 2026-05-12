//! Persist WorkOS tokens to `~/.cc-ledger/auth.json` (mode 0600 on Unix).

use std::fs;
use std::io::Write;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_at_ms: i64,
    /// WorkOS organization id selected at login. `None` if the user has
    /// no orgs or declined selection. Sent on every authenticated request
    /// as `X-CC-Ledger-Org-Id` so the backend can stamp usage rows.
    #[serde(default)]
    pub org_id: Option<String>,
    /// Display name of the selected org. Stored alongside id so the
    /// CLI can show "Logged in as Acme Corp" in `auth status` without a
    /// network round-trip.
    #[serde(default)]
    pub org_name: Option<String>,
}

pub fn load() -> Result<Option<Tokens>> {
    let path = paths::auth_path()?;
    match fs::read_to_string(&path) {
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(tokens: &Tokens) -> Result<()> {
    paths::ensure_dirs()?;
    let path = paths::auth_path()?;
    let tmp = path.with_extension("json.tmp");

    let body = serde_json::to_vec_pretty(tokens).context("serializing tokens")?;
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .with_context(|| format!("opening {}", tmp.display()))?;
        f.write_all(&body)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all().ok();
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
    }

    fs::rename(&tmp, &path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

pub fn clear() -> Result<bool> {
    let path = paths::auth_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}
