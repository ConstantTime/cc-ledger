//! Organization selection during `cc-ledger auth`.
//!
//! After the device-flow login returns valid tokens, we ask the cc-ledger
//! backend for the user's WorkOS org memberships. If the user belongs to
//! more than one we prompt interactively; one auto-selects; zero is a
//! soft warning.

use std::io::IsTerminal;

use anyhow::{anyhow, Context, Result};
use dialoguer::{theme::ColorfulTheme, Select};
use serde::Deserialize;

use crate::config;

/// One row returned by `GET {api_base}/orgs`. Mirrors the backend's
/// [`getWorkOS().organizations.getOrganization`] result trimmed to what
/// we render.
#[derive(Debug, Deserialize, Clone)]
pub struct Org {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// Selection outcome.
pub enum Selection {
    Picked(Org),
    NoOrgs,
    NonInteractive(Org),
}

/// Fetch the user's orgs from the cc-ledger backend.
pub fn fetch(access_token: &str) -> Result<Vec<Org>> {
    let url = format!("{}/orgs", config::api_base());
    let result = ureq::get(&url)
        .set("Authorization", &format!("Bearer {access_token}"))
        .call();

    match result {
        Ok(resp) => resp
            .into_json::<Vec<Org>>()
            .context("decoding orgs response"),
        // Non-2xx: surface status + body so the user can see whether
        // it's a 401 (token/JWKS mismatch), 404 (route missing — old
        // dev server?), 5xx (backend bug), etc.
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp
                .into_string()
                .unwrap_or_else(|_| "<unreadable body>".into());
            Err(anyhow::anyhow!(
                "GET {url} returned HTTP {code}: {}",
                body.trim()
            ))
        }
        // Transport: connection refused, DNS, timeout, etc.
        Err(e) => Err(anyhow::anyhow!(
            "GET {url} failed before reaching the server: {e}"
        )),
    }
}

/// Choose an org. Auto-picks for 0/1 cases, prompts otherwise. Returns
/// `NoOrgs` if the user has no memberships, `NonInteractive(org)` if
/// stdin isn't a TTY (e.g. CI) and we picked the first.
pub fn select(orgs: Vec<Org>) -> Result<Selection> {
    match orgs.len() {
        0 => Ok(Selection::NoOrgs),
        1 => Ok(Selection::Picked(orgs.into_iter().next().unwrap())),
        _ => {
            if !std::io::stdin().is_terminal() {
                // No human to prompt — fall back to first. Caller can
                // emit a warning so the user knows to re-run interactively.
                return Ok(Selection::NonInteractive(orgs.into_iter().next().unwrap()));
            }
            let labels: Vec<String> = orgs
                .iter()
                .map(|o| match &o.role {
                    Some(r) => format!("{} ({})", o.name, r),
                    None => o.name.clone(),
                })
                .collect();
            let idx = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Select organization")
                .items(&labels)
                .default(0)
                .interact()
                .map_err(|e| anyhow!("org selection cancelled: {e}"))?;
            Ok(Selection::Picked(orgs.into_iter().nth(idx).unwrap()))
        }
    }
}
