//! `cc-ledger otel-headers` — print OTel auth headers as JSON.
//!
//! Invoked by Claude Code's `otelHeadersHelper`. Hidden from `--help`
//! because it's not for direct human use.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::auth;

pub fn run() -> Result<()> {
    let tokens = auth::ensure_fresh_token()?;
    let mut headers = Map::new();
    headers.insert(
        "Authorization".to_string(),
        Value::String(format!("Bearer {}", tokens.access_token)),
    );
    if let Some(org_id) = tokens.org_id {
        headers.insert("X-CC-Ledger-Org-Id".to_string(), Value::String(org_id));
    }
    println!("{}", serde_json::to_string(&Value::Object(headers))?);
    Ok(())
}
