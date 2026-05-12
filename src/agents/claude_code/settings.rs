//! Read, merge, and atomically write `~/.claude/settings.json`.
//!
//! The core algorithm is a port of git-ai's
//! `src/mdm/agents/claude_code.rs:71-238`, generalized to install many event
//! types and kept pure-functional on `serde_json::Value` so it's
//! unit-testable without filesystem.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::events::{ClaudeHookEvent, INSTALL_EVENTS};
use crate::agents::{InstallOptions, InstallOutcome};
use crate::config;

const AGENT_ID: &str = "claude-code";

/// Telemetry env keys we own. When `include_otel` is true, fresh values
/// are written; existing values are preserved per [`OTEL_KEY_POLICY`].
const OTEL_HEADERS_HELPER_KEY: &str = "otelHeadersHelper";

/// Heuristic: an `OTEL_EXPORTER_OTLP_*_ENDPOINT` containing one of these
/// path fragments is one we previously installed (or one targeting this
/// project's URL shape), so we own its value.
const OTEL_ENDPOINT_OWNERSHIP_FRAGMENTS: &[&str] = &["/otel/logs", "/otel/metrics"];

/// Directory that holds Claude Code's user settings. Honors
/// `CLAUDE_CONFIG_DIR` (matches Claude Code's own behavior) and falls back
/// to `~/.claude`.
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs::home_dir()
        .map(|h| h.join(".claude"))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// Top-level install entry point.
pub fn install(opts: &InstallOptions) -> Result<InstallOutcome> {
    let path = settings_path();

    // Read existing settings — missing/empty becomes `{}`; malformed bails.
    let existing = read_settings(&path)?;
    let existing_text = pretty(&existing);

    let merged = merge(existing.clone(), &opts.binary_path, opts.include_otel);
    let merged_text = pretty(&merged);

    if existing_text == merged_text {
        return Ok(InstallOutcome::AlreadyInstalled);
    }

    let diff = make_diff(&existing_text, &merged_text);
    if opts.dry_run {
        return Ok(InstallOutcome::DryRun { diff });
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    write_atomic(&path, merged_text.as_bytes())?;
    Ok(InstallOutcome::Installed { diff })
}

/// Read settings.json. Missing → `{}`. Empty → `{}`. Malformed → error.
fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).with_context(|| format!("malformed JSON in {}", path.display()))
}

fn build_command(binary_path: &Path, event: ClaudeHookEvent) -> String {
    format!(
        "{} hook {} {}",
        binary_path.display(),
        AGENT_ID,
        event.as_kebab_name()
    )
}

/// Recognize any cc-ledger hook entry for this agent, regardless of binary path.
fn is_cc_ledger_command(cmd: &str) -> bool {
    cmd.contains("cc-ledger") && cmd.contains("hook") && cmd.contains(AGENT_ID)
}

/// Recognize a cc-ledger `otel-headers` helper invocation regardless of
/// binary path. Used to decide whether we own a stale `otelHeadersHelper`
/// entry and may overwrite it.
fn is_cc_ledger_otel_helper(cmd: &str) -> bool {
    cmd.contains("cc-ledger") && cmd.contains("otel-headers")
}

/// Pure merge function: take existing settings + the cc-ledger binary path,
/// return new settings. Each event slot gets its own command of the form
/// `<binary> hook <event-kebab-name> --agent claude-code`. Idempotent —
/// feeding output back in produces the same Value.
///
/// When `include_otel` is true, also writes the `otelHeadersHelper` and
/// `env.OTEL_*` block routing Claude Code's telemetry to the cc-ledger
/// backend. When false, those keys are not touched (pre-existing ones
/// stay — we don't strip on logout).
pub fn merge(mut settings: Value, binary_path: &Path, include_otel: bool) -> Value {
    // Ensure the top-level `hooks` object exists.
    let hooks = settings
        .as_object_mut()
        .expect("settings must be a JSON object")
        .entry("hooks")
        .or_insert_with(|| json!({}));

    let hooks_obj = hooks
        .as_object_mut()
        .expect("settings.hooks must be a JSON object");

    for &event in INSTALL_EVENTS {
        let key = event.as_wire_name();
        let matcher = event.catch_all_matcher();
        let desired_cmd = build_command(binary_path, event);
        let blocks = hooks_obj
            .entry(key.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("settings.hooks.<event> must be an array");

        merge_event(blocks, matcher, &desired_cmd);
    }

    // Cleanup pass: strip cc-ledger entries from any event keys we no longer
    // install (e.g. UserPromptSubmit was dropped when we stopped duplicating
    // prompt text that already lives in `~/.claude/projects/*.jsonl`).
    // Users' own non-cc-ledger hooks under those keys are preserved verbatim;
    // an event key that has no blocks left after the strip is removed.
    let install_keys: std::collections::HashSet<&str> =
        INSTALL_EVENTS.iter().map(|e| e.as_wire_name()).collect();
    let stale_keys: Vec<String> = hooks_obj
        .keys()
        .filter(|k| !install_keys.contains(k.as_str()))
        .cloned()
        .collect();
    for key in stale_keys {
        let drop_key = if let Some(blocks) = hooks_obj.get_mut(&key).and_then(|v| v.as_array_mut())
        {
            strip_event(blocks);
            blocks.is_empty()
        } else {
            false
        };
        if drop_key {
            hooks_obj.remove(&key);
        }
    }

    if include_otel {
        merge_otel(&mut settings, binary_path);
    }

    settings
}

/// Merge in cc-ledger's OTel routing. Idempotent and conservative:
/// - `otelHeadersHelper`: replaced if it currently points at any cc-ledger
///   binary (matches `is_cc_ledger_command`); otherwise left alone (the
///   user has their own helper).
/// - `env.OTEL_EXPORTER_OTLP_*_ENDPOINT`: replaced if the existing value
///   contains `/otel/logs` or `/otel/metrics` (heuristic for "ours");
///   otherwise left alone. This means a user override of
///   `CC_LEDGER_API_BASE` flows through on every install.
/// - All other keys: only set if absent. Respects user opt-outs (e.g. a
///   pre-existing `CLAUDE_CODE_ENABLE_TELEMETRY=0`).
fn merge_otel(settings: &mut Value, binary_path: &Path) {
    let root = settings
        .as_object_mut()
        .expect("settings must be a JSON object");

    // 1. otelHeadersHelper
    let desired_helper = format!("{} otel-headers", binary_path.display());
    let helper_owned = root
        .get(OTEL_HEADERS_HELPER_KEY)
        .and_then(|v| v.as_str())
        .map(is_cc_ledger_otel_helper)
        .unwrap_or(true); // absent → we own it
    if helper_owned {
        root.insert(
            OTEL_HEADERS_HELPER_KEY.to_string(),
            Value::String(desired_helper),
        );
    }

    // 2. env.* keys
    let env = root
        .entry("env".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("settings.env must be a JSON object");

    set_if_absent(env, "CLAUDE_CODE_ENABLE_TELEMETRY", "1");
    set_if_absent(env, "OTEL_LOGS_EXPORTER", "otlp");
    set_if_absent(env, "OTEL_METRICS_EXPORTER", "otlp");
    set_if_absent(env, "OTEL_EXPORTER_OTLP_PROTOCOL", "http/json");
    set_if_absent(
        env,
        "CLAUDE_CODE_OTEL_HEADERS_HELPER_DEBOUNCE_MS",
        config::OTEL_HEADERS_DEBOUNCE_MS,
    );

    // Endpoints: always recompute from current config (so a CC_LEDGER_API_BASE
    // override flows through), and replace if the existing value looks like
    // ours.
    set_or_replace_if_owned(
        env,
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        &config::otel_logs_endpoint(),
    );
    set_or_replace_if_owned(
        env,
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        &config::otel_metrics_endpoint(),
    );
}

fn set_if_absent(env: &mut serde_json::Map<String, Value>, key: &str, value: &str) {
    if !env.contains_key(key) {
        env.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn set_or_replace_if_owned(env: &mut serde_json::Map<String, Value>, key: &str, desired: &str) {
    let owned = env
        .get(key)
        .and_then(|v| v.as_str())
        .map(|existing| {
            OTEL_ENDPOINT_OWNERSHIP_FRAGMENTS
                .iter()
                .any(|f| existing.contains(f))
        })
        .unwrap_or(true); // absent → we own it
    if owned {
        env.insert(key.to_string(), Value::String(desired.to_string()));
    }
}

/// Strip cc-ledger entries from every matcher block in `blocks`. Drop blocks
/// emptied by the strip; leave foreign blocks alone (the user owns them).
/// Used by the cleanup pass to unhook events no longer in `INSTALL_EVENTS`.
fn strip_event(blocks: &mut Vec<Value>) {
    let mut emptied = vec![false; blocks.len()];
    for (i, block) in blocks.iter_mut().enumerate() {
        if let Some(arr) = block.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            let before = arr.len();
            arr.retain(|h| !is_cc_ledger_hook(h));
            if before > 0 && arr.is_empty() {
                emptied[i] = true;
            }
        }
    }
    let mut idx = 0;
    blocks.retain(|_| {
        let drop = emptied[idx];
        idx += 1;
        !drop
    });
}

/// In-place merge for one event's array of matcher blocks.
fn merge_event(blocks: &mut Vec<Value>, catch_all: &str, desired_cmd: &str) {
    // Step 1: strip cc-ledger entries from any non-catch-all blocks.
    // Track which we emptied so we can drop them; leave pre-existing empty
    // blocks alone (the user may be staging their own config).
    let mut emptied = vec![false; blocks.len()];
    for (i, block) in blocks.iter_mut().enumerate() {
        if block_matcher(block) == catch_all {
            continue;
        }
        if let Some(arr) = block.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            let before = arr.len();
            arr.retain(|h| !is_cc_ledger_hook(h));
            if before > 0 && arr.is_empty() {
                emptied[i] = true;
            }
        }
    }
    let mut idx = 0;
    blocks.retain(|_| {
        let drop = emptied[idx];
        idx += 1;
        !drop
    });

    // Step 2: find or create the catch-all block.
    let catch_all_idx = blocks
        .iter()
        .position(|b| block_matcher(b) == catch_all)
        .unwrap_or_else(|| {
            blocks.push(json!({ "matcher": catch_all, "hooks": [] }));
            blocks.len() - 1
        });

    // Step 3: ensure exactly one cc-ledger hook in the catch-all block.
    let arr = blocks[catch_all_idx]
        .get_mut("hooks")
        .and_then(|h| h.as_array_mut())
        .expect("catch-all block has a hooks array");

    let mut keep_idx: Option<usize> = None;
    for (i, hook) in arr.iter_mut().enumerate() {
        if !is_cc_ledger_hook(hook) {
            continue;
        }
        if keep_idx.is_none() {
            // First cc-ledger entry: update command in place if stale.
            let needs_update = hook.get("command").and_then(|c| c.as_str()) != Some(desired_cmd);
            if needs_update {
                *hook = cc_ledger_hook(desired_cmd);
            }
            keep_idx = Some(i);
        }
    }

    if keep_idx.is_none() {
        arr.push(cc_ledger_hook(desired_cmd));
    } else {
        // Drop duplicates that come after the kept entry.
        let kept = keep_idx.unwrap();
        let mut i = 0;
        arr.retain(|h| {
            let keep = i == kept || !is_cc_ledger_hook(h);
            i += 1;
            keep
        });
    }
}

fn block_matcher(block: &Value) -> &str {
    block.get("matcher").and_then(|m| m.as_str()).unwrap_or("")
}

fn is_cc_ledger_hook(hook: &Value) -> bool {
    hook.get("command")
        .and_then(|c| c.as_str())
        .map(is_cc_ledger_command)
        .unwrap_or(false)
}

fn cc_ledger_hook(cmd: &str) -> Value {
    json!({ "type": "command", "command": cmd })
}

/// Pretty-print canonically. Used both for diffing and writing.
fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("Value serializes");
    s.push('\n');
    s
}

fn make_diff(old: &str, new: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            similar::ChangeTag::Delete => "-",
            similar::ChangeTag::Insert => "+",
            similar::ChangeTag::Equal => " ",
        };
        out.push_str(sign);
        out.push_str(change.value());
    }
    out
}

/// Write `data` to `path` atomically: write to a sibling tmp file, fsync,
/// then rename. Symlink-aware: if `path` is a symlink, write to its target.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let target = if path.is_symlink() {
        fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))?
    } else {
        path.to_path_buf()
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).ok();
    }
    let tmp = target.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_merge(existing: Value, binary: &str) -> Value {
        merge(existing, Path::new(binary), false)
    }

    fn run_merge_with_otel(existing: Value, binary: &str) -> Value {
        merge(existing, Path::new(binary), true)
    }

    fn cc_hook(cmd: &str) -> Value {
        json!({ "type": "command", "command": cmd })
    }

    fn expected_cmd(binary: &str, event: ClaudeHookEvent) -> String {
        format!("{} hook claude-code {}", binary, event.as_kebab_name())
    }

    #[test]
    fn fresh_install_creates_one_block_per_event() {
        let binary = "/bin/cc-ledger";
        let out = run_merge(json!({}), binary);
        let hooks = out.get("hooks").and_then(|h| h.as_object()).unwrap();
        for ev in INSTALL_EVENTS {
            let arr = hooks
                .get(ev.as_wire_name())
                .and_then(|v| v.as_array())
                .unwrap();
            assert_eq!(arr.len(), 1);
            assert_eq!(block_matcher(&arr[0]), ev.catch_all_matcher());
            let inner = arr[0].get("hooks").and_then(|h| h.as_array()).unwrap();
            assert_eq!(inner.len(), 1);
            assert_eq!(
                inner[0].get("command").unwrap().as_str().unwrap(),
                expected_cmd(binary, *ev)
            );
        }
    }

    #[test]
    fn already_installed_is_idempotent() {
        let binary = "/bin/cc-ledger";
        let first = run_merge(json!({}), binary);
        let second = run_merge(first.clone(), binary);
        assert_eq!(first, second);
    }

    #[test]
    fn user_hook_in_other_matcher_is_preserved() {
        let binary = "/bin/cc-ledger";
        let existing = json!({
            "hooks": {
                "PostToolUse": [
                    { "matcher": "Edit|Write",
                      "hooks": [cc_hook("/usr/local/bin/prettier-hook.sh")] }
                ]
            }
        });
        let out = run_merge(existing, binary);
        let blocks = out["hooks"]["PostToolUse"].as_array().unwrap();
        // User block + our catch-all "*" block.
        assert_eq!(blocks.len(), 2);
        let user = blocks
            .iter()
            .find(|b| block_matcher(b) == "Edit|Write")
            .unwrap();
        assert_eq!(
            user["hooks"][0]["command"],
            "/usr/local/bin/prettier-hook.sh"
        );
    }

    #[test]
    fn stale_binary_path_is_updated_in_place() {
        let new_binary = "/new/cc-ledger";
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "*",
                      "hooks": [cc_hook("/old/cc-ledger hook claude-code pre-tool-use")] }
                ]
            }
        });
        let out = run_merge(existing, new_binary);
        let blocks = out["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0]["hooks"][0]["command"],
            expected_cmd(new_binary, ClaudeHookEvent::PreToolUse)
        );
    }

    #[test]
    fn duplicate_cc_ledger_entries_are_consolidated() {
        let binary = "/bin/cc-ledger";
        let stale = "/bin/cc-ledger hook claude-code pre-tool-use";
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "*",
                      "hooks": [cc_hook(stale), cc_hook(stale), cc_hook(stale)] }
                ]
            }
        });
        let out = run_merge(existing, binary);
        let inner = out["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(
            inner[0]["command"],
            expected_cmd(binary, ClaudeHookEvent::PreToolUse)
        );
    }

    #[test]
    fn cc_ledger_in_old_matcher_is_migrated_to_catch_all() {
        let binary = "/bin/cc-ledger";
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Edit",
                      "hooks": [cc_hook("/old/cc-ledger hook claude-code pre-tool-use")] }
                ]
            }
        });
        let out = run_merge(existing, binary);
        let blocks = out["hooks"]["PreToolUse"].as_array().unwrap();
        // Old matcher block was emptied (its sole hook was ours) and removed.
        // Only the catch-all "*" block remains.
        assert_eq!(blocks.len(), 1);
        assert_eq!(block_matcher(&blocks[0]), "*");
        assert_eq!(
            blocks[0]["hooks"][0]["command"],
            expected_cmd(binary, ClaudeHookEvent::PreToolUse)
        );
    }

    #[test]
    fn cc_ledger_in_old_matcher_with_user_hook_preserves_user_hook() {
        let binary = "/bin/cc-ledger";
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Edit",
                      "hooks": [
                          cc_hook("/old/cc-ledger hook claude-code pre-tool-use"),
                          cc_hook("/usr/bin/format.sh")
                      ] }
                ]
            }
        });
        let out = run_merge(existing, binary);
        let blocks = out["hooks"]["PreToolUse"].as_array().unwrap();
        // User's Edit-matcher block survives (with cc-ledger stripped); we
        // also have a catch-all "*" block.
        assert_eq!(blocks.len(), 2);
        let user = blocks.iter().find(|b| block_matcher(b) == "Edit").unwrap();
        let user_hooks = user["hooks"].as_array().unwrap();
        assert_eq!(user_hooks.len(), 1);
        assert_eq!(user_hooks[0]["command"], "/usr/bin/format.sh");
    }

    #[test]
    fn malformed_json_in_read_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{not json").unwrap();
        let err = read_settings(&path).unwrap_err();
        assert!(format!("{err}").contains("malformed"));
    }

    #[test]
    fn write_atomic_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_atomic(&path, b"hello\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
    }

    // ─── OTel merge tests ──────────────────────────────────────────────

    #[test]
    fn merge_with_otel_false_writes_only_hooks() {
        let out = run_merge(json!({}), "/bin/cc-ledger");
        assert!(out.get("otelHeadersHelper").is_none());
        assert!(out.get("env").is_none());
        // hooks block still installed
        assert!(out.get("hooks").is_some());
    }

    #[test]
    fn merge_with_otel_true_writes_env_and_helper() {
        let out = run_merge_with_otel(json!({}), "/bin/cc-ledger");
        assert_eq!(
            out["otelHeadersHelper"].as_str().unwrap(),
            "/bin/cc-ledger otel-headers"
        );
        let env = out["env"].as_object().unwrap();
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(env["OTEL_LOGS_EXPORTER"], "otlp");
        assert_eq!(env["OTEL_METRICS_EXPORTER"], "otlp");
        assert_eq!(env["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/json");
        assert_eq!(env["CLAUDE_CODE_OTEL_HEADERS_HELPER_DEBOUNCE_MS"], "480000");
        assert!(env["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"]
            .as_str()
            .unwrap()
            .ends_with("/otel/logs"));
        assert!(env["OTEL_EXPORTER_OTLP_METRICS_ENDPOINT"]
            .as_str()
            .unwrap()
            .ends_with("/otel/metrics"));
    }

    #[test]
    fn existing_user_otel_endpoint_preserved() {
        // User has a custom endpoint that doesn't match our shape — leave it.
        let existing = json!({
            "env": {
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "https://my-custom-collector.example.com",
            }
        });
        let out = run_merge_with_otel(existing, "/bin/cc-ledger");
        assert_eq!(
            out["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            "https://my-custom-collector.example.com"
        );
    }

    #[test]
    fn stale_helper_path_updated() {
        let existing = json!({
            "otelHeadersHelper": "/old/cc-ledger otel-headers",
        });
        let out = run_merge_with_otel(existing, "/new/cc-ledger");
        assert_eq!(
            out["otelHeadersHelper"].as_str().unwrap(),
            "/new/cc-ledger otel-headers"
        );
    }

    #[test]
    fn non_cc_ledger_helper_left_alone() {
        let existing = json!({
            "otelHeadersHelper": "/usr/local/bin/my-org-headers.sh",
        });
        let out = run_merge_with_otel(existing, "/bin/cc-ledger");
        assert_eq!(
            out["otelHeadersHelper"].as_str().unwrap(),
            "/usr/local/bin/my-org-headers.sh"
        );
    }

    #[test]
    fn running_install_twice_is_idempotent_for_otel_env() {
        let binary = "/bin/cc-ledger";
        let first = run_merge_with_otel(json!({}), binary);
        let second = run_merge_with_otel(first.clone(), binary);
        assert_eq!(first, second);
    }

    #[test]
    fn user_telemetry_optout_preserved() {
        let existing = json!({
            "env": { "CLAUDE_CODE_ENABLE_TELEMETRY": "0" },
        });
        let out = run_merge_with_otel(existing, "/bin/cc-ledger");
        assert_eq!(out["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
    }

    #[test]
    fn existing_otel_keys_kept_when_install_runs_logged_out() {
        // Prior authenticated install wrote OTel keys. Now user runs `install`
        // logged out (include_otel=false) — we must not strip them.
        let existing = json!({
            "otelHeadersHelper": "/bin/cc-ledger otel-headers",
            "env": {
                "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "https://ccledger.dev/api/v1/otel/logs",
            },
        });
        let out = run_merge(existing.clone(), "/bin/cc-ledger");
        assert_eq!(out["otelHeadersHelper"], existing["otelHeadersHelper"]);
        assert_eq!(out["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(
            out["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            "https://ccledger.dev/api/v1/otel/logs"
        );
    }
}
