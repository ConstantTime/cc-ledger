//! End-to-end tests for `cc-ledger install` and `cc-ledger hook`.
//!
//! Each test points `CLAUDE_CONFIG_DIR` at a `tempfile::TempDir` so we never
//! touch the real `~/.claude`. The binary is invoked via `assert_cmd`, which
//! recompiles and locates `target/debug/cc-ledger`.

use std::fs;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const FAKE_BINARY: &str = "/usr/local/bin/cc-ledger";

fn install_cmd(claude_dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("cc-ledger").unwrap();
    cmd.env("CLAUDE_CONFIG_DIR", claude_dir.path())
        .arg("install")
        .arg("--binary")
        .arg(FAKE_BINARY);
    cmd
}

fn settings_path(claude_dir: &TempDir) -> std::path::PathBuf {
    claude_dir.path().join("settings.json")
}

#[test]
fn install_not_detected_skips_cleanly() {
    // Point CLAUDE_CONFIG_DIR at a path that doesn't exist.
    let mut cmd = Command::cargo_bin("cc-ledger").unwrap();
    cmd.env("CLAUDE_CONFIG_DIR", "/nonexistent/.claude")
        .arg("install");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("not detected"));
}

#[test]
fn fresh_install_writes_settings_file() {
    let dir = tempfile::tempdir().unwrap();
    install_cmd(&dir).assert().success();

    let path = settings_path(&dir);
    let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let hooks = v.get("hooks").and_then(|h| h.as_object()).unwrap();
    // 7 events from INSTALL_EVENTS.
    assert_eq!(hooks.len(), 7);
    assert!(
        !hooks.contains_key("UserPromptSubmit"),
        "UserPromptSubmit was dropped in favor of reading prompts from \
         ~/.claude/projects/.../*.jsonl directly",
    );
    let pre = hooks.get("PreToolUse").and_then(|v| v.as_array()).unwrap();
    assert_eq!(pre[0].get("matcher").unwrap(), "*");
    assert_eq!(
        pre[0]["hooks"][0]["command"],
        format!("{FAKE_BINARY} hook claude-code pre-tool-use")
    );
    let stop = hooks.get("Stop").and_then(|v| v.as_array()).unwrap();
    assert_eq!(
        stop[0]["hooks"][0]["command"],
        format!("{FAKE_BINARY} hook claude-code stop")
    );
    let session_start = hooks
        .get("SessionStart")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(
        session_start[0]["hooks"][0]["command"],
        format!("{FAKE_BINARY} hook claude-code session-start")
    );
}

#[test]
fn second_install_is_idempotent_no_op() {
    let dir = tempfile::tempdir().unwrap();
    install_cmd(&dir).assert().success();
    let after_first = fs::read_to_string(settings_path(&dir)).unwrap();

    install_cmd(&dir)
        .assert()
        .success()
        .stdout(predicates::str::contains("already installed"));
    let after_second = fs::read_to_string(settings_path(&dir)).unwrap();
    assert_eq!(after_first, after_second);
}

#[test]
fn dry_run_does_not_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = install_cmd(&dir);
    cmd.arg("--dry-run");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("would install"));
    assert!(!settings_path(&dir).exists());
}

#[test]
fn stale_user_prompt_submit_hook_is_removed_on_reinstall() {
    // Existing users who installed a previous cc-ledger version still have a
    // `UserPromptSubmit` cc-ledger hook in their settings.json. The cleanup
    // pass in `merge` must strip it without touching their own hooks.
    let dir = tempfile::tempdir().unwrap();
    let pre = r#"{
  "hooks": {
    "UserPromptSubmit": [
      { "matcher": "",
        "hooks": [
          { "type": "command", "command": "/old/path/cc-ledger hook claude-code user-prompt-submit" },
          { "type": "command", "command": "/usr/bin/notify.sh" }
        ] }
    ]
  }
}"#;
    fs::write(settings_path(&dir), pre).unwrap();

    install_cmd(&dir).assert().success();

    let v: Value = serde_json::from_str(&fs::read_to_string(settings_path(&dir)).unwrap()).unwrap();
    let blocks = v["hooks"]["UserPromptSubmit"].as_array().unwrap();
    // The cc-ledger entry is gone but the user's notify.sh hook is preserved.
    let hooks: Vec<&str> = blocks
        .iter()
        .flat_map(|b| b["hooks"].as_array().unwrap().iter())
        .map(|h| h["command"].as_str().unwrap())
        .collect();
    assert_eq!(hooks, vec!["/usr/bin/notify.sh"]);
}

#[test]
fn stale_event_with_only_cc_ledger_hooks_is_removed_entirely() {
    // If the only thing under a removed event was cc-ledger's own hook, the
    // whole event key should disappear from settings.json.
    let dir = tempfile::tempdir().unwrap();
    let pre = r#"{
  "hooks": {
    "UserPromptSubmit": [
      { "matcher": "",
        "hooks": [
          { "type": "command", "command": "/old/cc-ledger hook claude-code user-prompt-submit" }
        ] }
    ]
  }
}"#;
    fs::write(settings_path(&dir), pre).unwrap();

    install_cmd(&dir).assert().success();

    let v: Value = serde_json::from_str(&fs::read_to_string(settings_path(&dir)).unwrap()).unwrap();
    assert!(
        v["hooks"].get("UserPromptSubmit").is_none(),
        "UserPromptSubmit should have been removed entirely",
    );
}

#[test]
fn user_hook_in_unrelated_block_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    // Pre-seed settings.json with the user's own format-on-edit hook.
    let pre = r#"{
  "hooks": {
    "PostToolUse": [
      { "matcher": "Edit|Write",
        "hooks": [{ "type": "command", "command": "/usr/bin/format.sh" }] }
    ]
  }
}"#;
    fs::write(settings_path(&dir), pre).unwrap();

    install_cmd(&dir).assert().success();

    let v: Value = serde_json::from_str(&fs::read_to_string(settings_path(&dir)).unwrap()).unwrap();
    let blocks = v["hooks"]["PostToolUse"].as_array().unwrap();
    let user = blocks
        .iter()
        .find(|b| b.get("matcher") == Some(&Value::String("Edit|Write".into())))
        .expect("user's Edit|Write block survives");
    assert_eq!(user["hooks"][0]["command"], "/usr/bin/format.sh");
}

#[test]
fn hook_with_unknown_agent_errors_but_exits_zero() {
    // Hooks must never block the agent — even on error, exit 0 is the contract.
    let mut cmd = Command::cargo_bin("cc-ledger").unwrap();
    cmd.arg("hook")
        .arg("not-a-real-agent")
        .arg("stop")
        .write_stdin("{}");
    cmd.assert()
        .success()
        .stderr(predicates::str::contains("unknown agent"));
}

#[test]
fn hook_accepts_kebab_positional() {
    // Form: cc-ledger hook <agent> <event>
    // The CLI positional supplies the event; stdin can omit hook_event_name.
    let mut cmd = Command::cargo_bin("cc-ledger").unwrap();
    cmd.arg("hook")
        .arg("claude-code")
        .arg("pre-tool-use")
        .write_stdin(r#"{"session_id":"abc"}"#);
    cmd.assert().success();
}

#[test]
fn hook_positional_overrides_stdin_event_name() {
    // If stdin disagrees with the CLI positional, the CLI wins.
    let mut cmd = Command::cargo_bin("cc-ledger").unwrap();
    cmd.arg("hook")
        .arg("claude-code")
        .arg("session-start")
        .write_stdin(r#"{"hook_event_name":"PreToolUse","session_id":"abc"}"#);
    cmd.assert().success();
}
