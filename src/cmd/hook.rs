use std::io;

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;

use crate::agents;

#[derive(Debug, Parser)]
pub struct Args {
    /// Agent id this hook fired from (e.g. "claude-code"). Written into
    /// settings.json by `install`.
    pub agent: String,

    /// Hook event name in kebab-case (e.g. "session-start", "user-prompt-submit").
    /// Source of truth for which event fired — overrides any `hook_event_name`
    /// field in the stdin payload. `install` writes commands of the form
    /// `cc-ledger hook <agent> <event>`, so production invocations always
    /// supply this.
    pub hook: String,
}

pub fn run(args: Args) -> Result<()> {
    let agent =
        agents::by_id(&args.agent).with_context(|| format!("unknown agent `{}`", args.agent))?;
    let mut payload: Value =
        serde_json::from_reader(io::stdin()).context("failed to parse hook payload from stdin")?;

    let wire = kebab_to_pascal(&args.hook);
    if let Value::Object(map) = &mut payload {
        map.insert("hook_event_name".to_string(), Value::String(wire));
    }

    agent.handle_hook(&payload)
}

/// Convert kebab-case (e.g. "user-prompt-submit") to PascalCase
/// ("UserPromptSubmit") so it matches Claude's wire names.
fn kebab_to_pascal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if c == '-' {
            capitalize_next = true;
            continue;
        }
        if capitalize_next {
            out.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_to_pascal_basic() {
        assert_eq!(kebab_to_pascal("session-start"), "SessionStart");
        assert_eq!(kebab_to_pascal("user-prompt-submit"), "UserPromptSubmit");
        assert_eq!(kebab_to_pascal("stop"), "Stop");
        assert_eq!(
            kebab_to_pascal("post-tool-use-failure"),
            "PostToolUseFailure"
        );
    }
}
