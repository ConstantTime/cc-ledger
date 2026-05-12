//! Thin sync wrapper over the two WorkOS endpoints used by Device Auth.
//!
//! - `POST /user_management/authorize/device`  → start the flow
//! - `POST /user_management/authenticate`      → poll for tokens

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::auth::storage::Tokens;
use crate::config;
use crate::paths::now_ms;

const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const REFRESH_TOKEN_GRANT: &str = "refresh_token";

#[derive(Debug, Deserialize)]
pub struct DeviceAuth {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug)]
pub enum PollResult {
    Approved(Tokens),
    Pending,
    SlowDown,
    Denied,
    Expired,
    Fatal(String),
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default = "default_token_type")]
    token_type: String,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

fn default_expires_in() -> i64 {
    600
}

/// Decode the `exp` claim (seconds since epoch) from a JWT and return it
/// in milliseconds. WorkOS' `expires_in` response field has been observed
/// to disagree with the JWT it actually mints (see incident: 600s response
/// vs 300s `exp - iat`), and the server validates `exp` from the token
/// itself — so the JWT is the only trustworthy source.
fn jwt_exp_ms(access_token: &str) -> Option<i64> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let payload_b64 = access_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp")?.as_i64()?;
    Some(exp.saturating_mul(1000))
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

pub fn request_device_code() -> Result<DeviceAuth> {
    let url = format!(
        "{}/user_management/authorize/device",
        config::workos_api_base()
    );
    let cid = config::workos_client_id();
    let resp = ureq::post(&url)
        .send_form(&[("client_id", &cid)])
        .with_context(|| format!("POST {url}"))?;
    resp.into_json::<DeviceAuth>()
        .context("decoding device authorization response")
}

pub fn poll_token(device_code: &str) -> Result<PollResult> {
    let url = format!("{}/user_management/authenticate", config::workos_api_base());
    let cid = config::workos_client_id();
    let result = ureq::post(&url).send_form(&[
        ("grant_type", DEVICE_CODE_GRANT),
        ("device_code", device_code),
        ("client_id", &cid),
    ]);

    match result {
        Ok(resp) => {
            let tr: TokenResponse = resp.into_json().context("decoding token response")?;
            let exp_ms = jwt_exp_ms(&tr.access_token)
                .unwrap_or_else(|| now_ms().saturating_add(tr.expires_in.saturating_mul(1000)));
            Ok(PollResult::Approved(Tokens {
                access_token: tr.access_token,
                refresh_token: tr.refresh_token,
                token_type: tr.token_type,
                expires_at_ms: exp_ms,
                // Org is selected later by the login flow (see auth::orgs);
                // refresh paths preserve whatever was already on disk.
                org_id: None,
                org_name: None,
            }))
        }
        Err(ureq::Error::Status(_code, resp)) => {
            let body = resp.into_string().context("reading error response body")?;
            let err: OAuthError = serde_json::from_str(&body)
                .map_err(|e| anyhow!("unparseable error from WorkOS: {e} (body: {body})"))?;
            Ok(match err.error.as_str() {
                "authorization_pending" => PollResult::Pending,
                "slow_down" => PollResult::SlowDown,
                "access_denied" => PollResult::Denied,
                "expired_token" => PollResult::Expired,
                _ => PollResult::Fatal(format!(
                    "{}: {}",
                    err.error,
                    err.error_description.unwrap_or_default()
                )),
            })
        }
        Err(e) => Err(anyhow!("network error polling {url}: {e}")),
    }
}

/// Exchange a refresh token for a fresh `(access_token, refresh_token)`
/// pair. WorkOS rotates refresh tokens, so the returned `refresh_token`
/// must replace the old one in storage — the old one is now invalid.
///
/// Returns `Err` with a clear "please re-login" message on
/// `invalid_grant` (token revoked, rotated by another process, or
/// expired).
pub fn refresh(refresh_token: &str) -> Result<Tokens> {
    let url = format!("{}/user_management/authenticate", config::workos_api_base());
    let cid = config::workos_client_id();
    let result = ureq::post(&url).send_form(&[
        ("grant_type", REFRESH_TOKEN_GRANT),
        ("refresh_token", refresh_token),
        ("client_id", &cid),
    ]);

    match result {
        Ok(resp) => {
            let tr: TokenResponse = resp.into_json().context("decoding refresh response")?;
            let exp_ms = jwt_exp_ms(&tr.access_token)
                .unwrap_or_else(|| now_ms().saturating_add(tr.expires_in.saturating_mul(1000)));
            // Org fields left empty here; the caller (ensure_fresh_token)
            // merges them from the stored tokens so the user's selection
            // survives every refresh.
            Ok(Tokens {
                access_token: tr.access_token,
                refresh_token: tr.refresh_token,
                token_type: tr.token_type,
                expires_at_ms: exp_ms,
                org_id: None,
                org_name: None,
            })
        }
        Err(ureq::Error::Status(_code, resp)) => {
            let body = resp.into_string().context("reading error response body")?;
            let err: OAuthError = serde_json::from_str(&body)
                .map_err(|e| anyhow!("unparseable error from WorkOS: {e} (body: {body})"))?;
            if err.error == "invalid_grant" {
                Err(anyhow!(
                    "refresh token rejected by WorkOS — run `cc-ledger auth` to log in again"
                ))
            } else {
                Err(anyhow!(
                    "refresh failed ({}): {}",
                    err.error,
                    err.error_description.unwrap_or_default()
                ))
            }
        }
        Err(e) => Err(anyhow!("network error refreshing at {url}: {e}")),
    }
}
