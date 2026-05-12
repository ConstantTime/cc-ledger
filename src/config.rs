//! Single source of truth for CLI constants.
//!
//! Every default lives next to its `*_ENV_VAR` const; an accessor function
//! reads the env var (treating empty as unset) and falls back. Inspired by
//! openai/codex's pattern of pairing each default with its override
//! variable, collapsed here into one file because cc-ledger is small enough
//! that a central index is clearer than per-feature spread.

// ─── WorkOS auth ─────────────────────────────────────────────────────────

pub const WORKOS_CLIENT_ID: &str = "client_01KQWQW7DJXTFE0EC8K5S6JX8G";
pub const WORKOS_CLIENT_ID_ENV_VAR: &str = "CC_LEDGER_WORKOS_CLIENT_ID";

pub const WORKOS_API_BASE: &str = "https://api.workos.com";
pub const WORKOS_API_BASE_ENV_VAR: &str = "CC_LEDGER_WORKOS_API_BASE";

pub fn workos_client_id() -> String {
    env_override(WORKOS_CLIENT_ID_ENV_VAR).unwrap_or_else(|| WORKOS_CLIENT_ID.to_string())
}

pub fn workos_api_base() -> String {
    env_override(WORKOS_API_BASE_ENV_VAR)
        .unwrap_or_else(|| WORKOS_API_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}

// ─── cc-ledger backend (Elysia in cc-ledger-frontend) ────────────────────

pub const API_BASE: &str = "https://ccledger.dev/api/v1";
pub const API_BASE_ENV_VAR: &str = "CC_LEDGER_API_BASE";

pub const OTEL_LOGS_SLUG: &str = "/otel/logs";
pub const OTEL_METRICS_SLUG: &str = "/otel/metrics";

/// otelHeadersHelper rerun cadence. Must be < access-token lifetime (~10
/// min) so that every cycle Claude Code asks for fresh headers we deliver
/// a non-expired bearer.
pub const OTEL_HEADERS_DEBOUNCE_MS: &str = "480000"; // 8 min

pub fn api_base() -> String {
    env_override(API_BASE_ENV_VAR)
        .unwrap_or_else(|| API_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}

pub fn otel_logs_endpoint() -> String {
    format!("{}{}", api_base(), OTEL_LOGS_SLUG)
}

pub fn otel_metrics_endpoint() -> String {
    format!("{}{}", api_base(), OTEL_METRICS_SLUG)
}

// ─── Self-update (Vercel Blob distribution) ──────────────────────────────

/// Public Vercel Blob bucket holding `latest`, `versions/<v>/<tarball>`,
/// `SHA256SUMS`, and `install.sh`. Mirrors `BLOB_BASE` in install.sh.
pub const BLOB_BASE: &str = "https://taea7hakf9g56hd8.public.blob.vercel-storage.com";
pub const BLOB_BASE_ENV_VAR: &str = "CC_LEDGER_BLOB_BASE";

/// File the throttled background banner caches its last fetch in.
pub const VERSION_CHECK_FILE_NAME: &str = "version-check.json";
/// Min seconds between background `/latest` fetches. The banner reads the
/// cache; refreshes happen lazily when older than this.
pub const VERSION_CHECK_INTERVAL_SECS: u64 = 86_400; // 24h

/// `=1` makes every user-facing invocation behave as if `--update` was
/// passed. Skipped on machine-driven `hook` / `otel-headers` regardless.
pub const AUTO_UPDATE_ENV_VAR: &str = "CC_LEDGER_AUTO_UPDATE";
/// `=1` suppresses the post-command "new version available" banner and
/// the underlying `/latest` fetch. Useful in CI scripts that already pin
/// a version explicitly.
pub const NO_UPDATE_CHECK_ENV_VAR: &str = "CC_LEDGER_NO_UPDATE_CHECK";

/// Compile-time target triple — must match exactly one of the four
/// entries published by .github/workflows/release.yml.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const TARGET_TRIPLE: &str = "x86_64-apple-darwin";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const TARGET_TRIPLE: &str = "aarch64-apple-darwin";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-musl";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const TARGET_TRIPLE: &str = "aarch64-unknown-linux-musl";

pub fn blob_base() -> String {
    env_override(BLOB_BASE_ENV_VAR)
        .unwrap_or_else(|| BLOB_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}

// ─── Filesystem layout ───────────────────────────────────────────────────

pub const HOME_DIR_NAME: &str = ".cc-ledger";
pub const HOME_ENV_VAR: &str = "CC_LEDGER_HOME";

pub const AUTH_FILE_NAME: &str = "auth.json";
pub const AUTH_LOCK_FILE_NAME: &str = "auth.lock";
pub const DB_FILE_NAME: &str = "ledger.db";
pub const BLOBS_DIR_NAME: &str = "blobs";
pub const AUDIT_DIR_NAME: &str = "audit";

pub const PRICING_FILE_NAME: &str = "pricing.toml";
pub const PRICING_PATH_ENV_VAR: &str = "CC_LEDGER_PRICING";

// ─── Helpers ─────────────────────────────────────────────────────────────

fn env_override(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
