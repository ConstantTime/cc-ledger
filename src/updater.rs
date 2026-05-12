//! Self-update over HTTP from the Vercel Blob distribution bucket.
//!
//! Mirrors `install.sh`'s flow: resolve `${BLOB_BASE}/latest`, download
//! `versions/<v>/cc-ledger-<target>.tar.gz`, verify against `SHA256SUMS`,
//! atomically swap into `current_exe()` (kernel keeps the inode alive
//! during a same-fs `rename` mid-execution).
//!
//! Exposes:
//! - [`run_update`] — explicit (force-checks; downloads + swaps).
//! - [`reexec_self`] — `exec()` into the freshly-installed binary with
//!   the original argv, sans `--update` and `CC_LEDGER_AUTO_UPDATE`.
//! - [`maybe_show_banner`] — post-dispatch nag, throttled to once per
//!   24h via `~/.cc-ledger/version-check.json`.

use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config;
use crate::paths;

/// Subcommand category. Decides whether we honor `--update` /
/// `CC_LEDGER_AUTO_UPDATE` and whether the banner is allowed to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
    /// Hooks and `otel-headers` — invoked by Claude Code on every event,
    /// must finish fast and (for `otel-headers`) emit clean JSON.
    MachineDriven,
    /// Everything else (`auth`, `install`, `stats`, no subcommand, …).
    UserFacing,
}

#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    AlreadyLatest { version: String },
    Updated { from: String, to: String },
}

/// Force-check + download + atomic swap. Used by both `--update` and
/// `CC_LEDGER_AUTO_UPDATE=1`.
pub fn run_update() -> Result<UpdateOutcome> {
    let exe = std::env::current_exe().context("resolving current executable")?;
    if exe.to_string_lossy().contains("/target/") {
        bail!("refusing to update a development build (cargo run target/)");
    }

    eprintln!("==> resolving latest version");
    let latest = fetch_latest(Duration::from_secs(10))?;
    let current = env!("CARGO_PKG_VERSION");

    // Bail unless `latest` is strictly newer than what we're running.
    // Equality => already up-to-date. Older `latest` would otherwise be
    // a silent destructive downgrade if the blob ever publishes one.
    if !is_newer_version(&latest, current) {
        let _ = write_cache(&latest);
        return Ok(UpdateOutcome::AlreadyLatest {
            version: current.to_string(),
        });
    }

    eprintln!("==> {current} → {latest}");

    let tarball_name = format!("cc-ledger-{}.tar.gz", config::TARGET_TRIPLE);
    let tarball_url = format!(
        "{}/versions/{}/{}",
        config::blob_base(),
        latest,
        tarball_name
    );
    let sums_url = format!("{}/versions/{}/SHA256SUMS", config::blob_base(), latest);

    eprintln!("==> downloading {tarball_name}");
    let tarball_bytes = http_get_bytes(&tarball_url)?;
    let sums_text = http_get_text(&sums_url)?;

    eprintln!("==> verifying SHA256");
    verify_checksum(&tarball_bytes, &tarball_name, &sums_text)?;

    eprintln!("==> swapping {}", exe.display());
    let staged = extract_binary(&tarball_bytes, &exe)?;
    std::fs::rename(&staged, &exe)
        .with_context(|| format!("rename {} -> {}", staged.display(), exe.display()))?;

    let _ = write_cache(&latest);

    Ok(UpdateOutcome::Updated {
        from: current.to_string(),
        to: latest,
    })
}

/// Replace the current process with the same binary path + the original
/// argv, stripped of `--update`, with `CC_LEDGER_AUTO_UPDATE` cleared
/// (belt-and-suspenders against an infinite re-update loop).
///
/// Returns `Err` only when the `exec` syscall itself fails — on success,
/// it never returns at all.
pub fn reexec_self() -> Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let argv: Vec<OsString> = std::env::args_os()
        .skip(1)
        .filter(|a| a != "--update")
        .collect();
    let err = std::process::Command::new(&exe)
        .args(&argv)
        .env_remove(config::AUTO_UPDATE_ENV_VAR)
        .exec();
    Err(anyhow!("re-exec after update failed: {err}"))
}

/// Print one-line banner if a newer version is known. Silent no-op
/// under any of the documented suppression conditions.
pub fn maybe_show_banner(cmd_kind: CmdKind) {
    if cmd_kind == CmdKind::MachineDriven {
        return;
    }
    if !std::io::stdout().is_terminal() {
        return;
    }
    if env_flag("CI") || env_flag(config::NO_UPDATE_CHECK_ENV_VAR) {
        return;
    }

    let Some(latest) = read_or_refresh_cache() else {
        return;
    };
    let current = env!("CARGO_PKG_VERSION");
    // Only nag for actual upgrades. A stale cache that lags the blob
    // would otherwise tell users they can "update" to an older version.
    if !is_newer_version(&latest, current) {
        return;
    }
    eprintln!("→ cc-ledger {latest} available — run `cc-ledger --update`");
}

/// True iff `candidate` is strictly newer than `base` under semver-ish
/// `MAJOR.MINOR.PATCH` semantics. Anything after `-` or `+` (prerelease
/// or build metadata) is ignored. Falls back to `false` if either string
/// fails to parse — better to suppress a real notification than to nag
/// the user with a downgrade.
fn is_newer_version(candidate: &str, base: &str) -> bool {
    fn parse(s: &str) -> Option<(u64, u64, u64)> {
        let core = s.trim().split(['-', '+']).next()?;
        let mut it = core.split('.');
        let major: u64 = it.next()?.parse().ok()?;
        let minor: u64 = it.next()?.parse().ok()?;
        let patch: u64 = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        Some((major, minor, patch))
    }
    match (parse(candidate), parse(base)) {
        (Some(c), Some(b)) => c > b,
        _ => false,
    }
}

// ───── HTTP ─────

fn fetch_latest(timeout: Duration) -> Result<String> {
    let url = format!("{}/latest", config::blob_base());
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let resp = agent
        .get(&url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let body = resp.into_string().context("reading /latest body")?;
    let v = body.trim().to_string();
    if v.is_empty() {
        bail!("got empty version string from {url}");
    }
    Ok(v)
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .with_context(|| format!("reading body of {url}"))?;
    Ok(buf)
}

fn http_get_text(url: &str) -> Result<String> {
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    resp.into_string()
        .with_context(|| format!("reading body of {url}"))
}

// ───── Checksum + extract ─────

fn verify_checksum(tarball_bytes: &[u8], tarball_name: &str, sums_text: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(tarball_bytes));
    let expected = sums_text
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // SHA256SUMS line: "<hex>  <filename>" (two spaces or any whitespace).
            let mut parts = line.splitn(2, char::is_whitespace);
            let hash = parts.next()?;
            let name = parts.next()?.trim_start();
            (name == tarball_name).then(|| hash.to_string())
        })
        .ok_or_else(|| anyhow!("no checksum entry for {tarball_name} in SHA256SUMS"))?;

    if expected != actual {
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Extract the `cc-ledger` binary from a gzipped tarball into a tempfile
/// next to `target` (same filesystem so rename is atomic). Caller is
/// responsible for the rename.
fn extract_binary(tarball_bytes: &[u8], target: &Path) -> Result<PathBuf> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let staged = target.with_file_name(format!(".cc-ledger.update.{}.tmp", std::process::id()));
    if staged.exists() {
        let _ = std::fs::remove_file(&staged);
    }

    let gz = GzDecoder::new(tarball_bytes);
    let mut ar = Archive::new(gz);

    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == "cc-ledger" {
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)
                .with_context(|| format!("create {}", staged.display()))?;
            std::io::copy(&mut entry, &mut out)
                .with_context(|| format!("write {}", staged.display()))?;
            out.sync_all().ok();

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
                    .with_context(|| format!("chmod 0755 {}", staged.display()))?;
            }
            return Ok(staged);
        }
    }
    bail!("tarball did not contain `cc-ledger` binary")
}

// ───── Cache (~/.cc-ledger/version-check.json) ─────

#[derive(Debug, Serialize, Deserialize)]
struct VersionCache {
    checked_at_ms: i64,
    latest: String,
}

fn read_or_refresh_cache() -> Option<String> {
    let path = paths::version_check_path().ok()?;
    let cached = read_cache(&path);
    let now = paths::now_ms();

    let stale = match &cached {
        Some(c) => ((now - c.checked_at_ms) / 1000) > config::VERSION_CHECK_INTERVAL_SECS as i64,
        None => true,
    };

    if stale {
        // Hard timeout so the user's command doesn't hang on a flaky network.
        if let Ok(fresh) = fetch_latest(Duration::from_secs(2)) {
            let _ = write_cache_to(&path, &fresh);
            return Some(fresh);
        }
    }

    cached.map(|c| c.latest)
}

fn read_cache(path: &Path) -> Option<VersionCache> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_cache(latest: &str) -> Result<()> {
    let path = paths::version_check_path()?;
    write_cache_to(&path, latest)
}

fn write_cache_to(path: &Path, latest: &str) -> Result<()> {
    paths::ensure_dirs()?;
    let body = serde_json::to_vec(&VersionCache {
        checked_at_ms: paths::now_ms(),
        latest: latest.to_string(),
    })?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ───── Misc ─────

fn env_flag(key: &str) -> bool {
    std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::is_newer_version;

    #[test]
    fn equal_is_not_newer() {
        assert!(!is_newer_version("0.0.4", "0.0.4"));
        assert!(!is_newer_version("1.2.3", "1.2.3"));
    }

    #[test]
    fn older_candidate_is_not_newer() {
        // Reproduces the bug we just fixed: stale cache `0.0.3` while binary
        // is `0.0.4` would have told the user to "update" to a downgrade.
        assert!(!is_newer_version("0.0.3", "0.0.4"));
        assert!(!is_newer_version("0.99.99", "1.0.0"));
        assert!(!is_newer_version("1.2.3", "1.2.4"));
    }

    #[test]
    fn strictly_newer_is_newer() {
        assert!(is_newer_version("0.0.5", "0.0.4"));
        assert!(is_newer_version("1.0.0", "0.99.99"));
        assert!(is_newer_version("0.1.0", "0.0.99"));
        assert!(is_newer_version("2.0.0", "1.99.99"));
    }

    #[test]
    fn lexicographic_pitfalls_avoided() {
        // String compare would say "10" < "9". Numeric compare must win.
        assert!(is_newer_version("0.10.0", "0.9.0"));
        assert!(!is_newer_version("0.9.0", "0.10.0"));
    }

    #[test]
    fn prerelease_metadata_stripped() {
        // We treat the core triple only — prerelease tags don't bump.
        assert!(!is_newer_version("0.0.4-rc1", "0.0.4"));
        assert!(is_newer_version("0.0.5-rc1", "0.0.4"));
    }

    #[test]
    fn unparseable_falls_back_to_false() {
        // Better to suppress a real notification than to nag with garbage.
        assert!(!is_newer_version("not a version", "0.0.4"));
        assert!(!is_newer_version("0.0.4", "garbage"));
        assert!(!is_newer_version("0.0", "0.0.0"));
        assert!(!is_newer_version("0.0.0.1", "0.0.0"));
    }
}
