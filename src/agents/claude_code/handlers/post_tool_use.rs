//! `PostToolUse` — snapshot the post-edit blob, diff against pre, persist
//! per-line attributions, close the `tool_calls` row.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::super::tool_input;
use super::HookContext;
use crate::diff;
use crate::git;
use crate::pr;
use crate::store::{
    blobs,
    queries::{self, AttributionRow},
};

const AGENT_ID: &str = "claude-code";

#[derive(Deserialize, Default)]
struct Payload {
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input: Option<Value>,
    cwd: Option<String>,
}

#[derive(Deserialize, Default)]
struct BashInput {
    command: Option<String>,
}

pub fn handle(payload: &Value, ctx: &HookContext) -> Result<()> {
    let p: Payload = serde_json::from_value(payload.clone()).unwrap_or_default();
    let (Some(session_id), Some(tool_name), Some(tool_use_id)) = (
        p.session_id.as_deref(),
        p.tool_name.as_deref(),
        p.tool_use_id.as_deref(),
    ) else {
        return Ok(());
    };
    let tool_input = p.tool_input.unwrap_or(Value::Null);

    // Bash branch — capture commits / detect PRs / detect squash-merges. The
    // file-edit logic below doesn't apply to Bash so we return after.
    if tool_name == "Bash" {
        if let Some(cwd) = p.cwd.as_deref() {
            handle_bash(cwd, &tool_input, ctx)?;
        }
        return Ok(());
    }

    let Some(file_path) = tool_input::affected_file(tool_name, &tool_input) else {
        return Ok(());
    };

    let post_sha = blobs::put_file_as_blob(ctx.blobs_dir, Path::new(&file_path))?;
    let pre_sha_opt = queries::get_pre_blob_sha(ctx.conn, session_id, tool_use_id)?;

    let Some(pre_sha) = pre_sha_opt else {
        // Post fired without Pre — record what we know, but no attributions.
        queries::record_pre_tool(
            ctx.conn,
            session_id,
            tool_use_id,
            tool_name,
            Some(&file_path),
            ctx.now_ms,
            None,
        )?;
        queries::record_post_tool(
            ctx.conn,
            session_id,
            tool_use_id,
            ctx.now_ms,
            &post_sha,
            0,
            0,
        )?;
        return Ok(());
    };

    let pre_bytes = blobs::read_blob(ctx.blobs_dir, &pre_sha)?;
    let post_bytes = blobs::read_blob(ctx.blobs_dir, &post_sha)?;
    let pre = String::from_utf8_lossy(&pre_bytes);
    let post = String::from_utf8_lossy(&post_bytes);
    let summary = diff::diff_summary(&pre, &post);

    queries::record_post_tool(
        ctx.conn,
        session_id,
        tool_use_id,
        ctx.now_ms,
        &post_sha,
        clamp_to_i64(summary.lines_added),
        clamp_to_i64(summary.lines_removed),
    )?;

    let cwd = p.cwd.as_deref().unwrap_or("");
    let author_id = format!("ai:{}:{}", AGENT_ID, session_id);
    for (start, end) in summary.post_ranges {
        queries::insert_attribution(
            ctx.conn,
            &AttributionRow {
                cwd,
                file_path: &file_path,
                line_start: i64::from(start),
                line_end: i64::from(end),
                session_id,
                tool_use_id,
                author_id: &author_id,
            },
        )?;
    }
    Ok(())
}

fn clamp_to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// React to `Bash` post-tool-use:
/// - `git commit*` → capture the new HEAD commit, backfill attributions,
///   recompute rollups for any open PR on this branch.
/// - `git push*` → run PR detection (single source of truth for "did we just
///   open a PR?").
/// - `git fetch*` → reconcile squash-merges on `origin/<base>`.
///
/// All steps are best-effort — missing git, no remote, not in a repo all
/// degrade silently. We never want hooks to fail the agent's turn.
fn handle_bash(cwd: &str, tool_input: &Value, ctx: &HookContext) -> Result<()> {
    let bi: BashInput = serde_json::from_value(tool_input.clone()).unwrap_or_default();
    let Some(cmd) = bi.command.as_deref() else {
        return Ok(());
    };
    let trimmed = cmd.trim_start();
    let cwd_path = PathBuf::from(cwd);

    if is_git_subcommand(trimmed, "commit") {
        on_git_commit(&cwd_path, ctx)?;
    } else if is_git_subcommand(trimmed, "push") {
        on_git_push(&cwd_path, ctx)?;
    } else if is_git_subcommand(trimmed, "fetch") {
        let _ = pr::reconcile_squash_merges(ctx.conn, &cwd_path, ctx.now_ms);
    }
    Ok(())
}

/// Loose match for "the user invoked `git <sub>` somewhere in this command
/// line". Catches `git commit -m …`, `git -C path commit`, etc.
fn is_git_subcommand(cmd: &str, sub: &str) -> bool {
    let mut tokens = cmd.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok != "git" {
            continue;
        }
        // Now scan for the first non-flag token after `git`.
        while let Some(next) = tokens.next() {
            if next == "-C" {
                let _ = tokens.next();
                continue;
            }
            if next.starts_with('-') {
                continue;
            }
            return next == sub;
        }
        return false;
    }
    false
}

fn on_git_commit(cwd: &Path, ctx: &HookContext) -> Result<()> {
    let Some(sha) = git::head_sha(cwd) else {
        return Ok(());
    };
    let branch = git::current_branch(cwd);
    let cwd_str = cwd.to_string_lossy();
    pr::capture_commit(ctx.conn, cwd, &sha, branch.as_deref(), ctx.now_ms)?;

    // Invalidate poll state so the next push/Stop poll sees the new tip.
    if let Some(b) = branch.as_deref() {
        let _ = queries::invalidate_poll_state(ctx.conn, &cwd_str, b);

        // Walk any open PR on this branch and refresh its rollup. The new
        // commit isn't linked to the PR yet (push hasn't fired), but its
        // attributions are now anchored, so future recomputes will pick it up.
        let prs = queries::open_prs(ctx.conn, &cwd_str, Some(b)).unwrap_or_default();
        for (pr_number, _) in prs {
            queries::upsert_pr_commit(ctx.conn, &cwd_str, pr_number, &sha)?;
            pr::recompute_rollup(ctx.conn, &cwd_str, pr_number, ctx.now_ms)?;
        }
    }
    Ok(())
}

fn on_git_push(cwd: &Path, ctx: &HookContext) -> Result<()> {
    let _ = pr::detect_and_record_pr(ctx.conn, cwd, ctx.now_ms);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::handlers::pre_tool_use;
    use crate::agents::claude_code::handlers::test_support::ContextFixture;
    use serde_json::json;
    use std::fs;

    fn payload(session: &str, tu: &str, file: &str, cwd: &str) -> Value {
        json!({
            "session_id": session,
            "tool_name": "Edit",
            "tool_use_id": tu,
            "cwd": cwd,
            "tool_input": { "file_path": file },
        })
    }

    #[test]
    fn pre_then_post_records_attributions_for_inserted_lines() {
        let fx = ContextFixture::new();
        let file = fx.blobs_dir.parent().unwrap().join("a.rs");
        fs::write(&file, "a\nb\nc\n").unwrap();
        let p = payload("s", "tu", file.to_str().unwrap(), "/repo");

        pre_tool_use::handle(&p, &fx.ctx(100)).unwrap();
        // Claude inserts a new line between a and b.
        fs::write(&file, "a\nNEW\nb\nc\n").unwrap();
        super::handle(&p, &fx.ctx(200)).unwrap();

        let (status, lines_added, lines_removed): (String, i64, i64) = fx
            .conn
            .query_row(
                "SELECT status, lines_added, lines_removed FROM tool_calls",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "success");
        assert_eq!(lines_added, 1);
        assert_eq!(lines_removed, 0);

        let attribs: Vec<(i64, i64, String)> = fx
            .conn
            .prepare("SELECT line_start, line_end, author_id FROM attributions ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(attribs.len(), 1);
        assert_eq!(attribs[0], (2, 2, "ai:claude-code:s".to_string()));
    }

    #[test]
    fn post_without_pre_records_call_but_no_attributions() {
        let fx = ContextFixture::new();
        let file = fx.blobs_dir.parent().unwrap().join("a.rs");
        fs::write(&file, "x\n").unwrap();
        let p = payload("s", "tu", file.to_str().unwrap(), "/repo");
        super::handle(&p, &fx.ctx(200)).unwrap();

        let (status, has_pre): (String, bool) = fx
            .conn
            .query_row(
                "SELECT status, pre_blob_sha IS NOT NULL FROM tool_calls",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "success");
        assert!(!has_pre, "no pre blob recorded");
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM attributions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn pure_deletion_records_no_attributions_but_marks_success() {
        let fx = ContextFixture::new();
        let file = fx.blobs_dir.parent().unwrap().join("a.rs");
        fs::write(&file, "a\nb\nc\n").unwrap();
        let p = payload("s", "tu", file.to_str().unwrap(), "/repo");
        pre_tool_use::handle(&p, &fx.ctx(100)).unwrap();
        fs::write(&file, "a\nc\n").unwrap();
        super::handle(&p, &fx.ctx(200)).unwrap();

        let (lines_added, lines_removed): (i64, i64) = fx
            .conn
            .query_row(
                "SELECT lines_added, lines_removed FROM tool_calls",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(lines_added, 0);
        assert_eq!(lines_removed, 1);
        let n: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM attributions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn replacement_attributes_replacement_lines() {
        let fx = ContextFixture::new();
        let file = fx.blobs_dir.parent().unwrap().join("a.rs");
        fs::write(&file, "a\nb\nc\n").unwrap();
        let p = payload("s", "tu", file.to_str().unwrap(), "/repo");
        pre_tool_use::handle(&p, &fx.ctx(100)).unwrap();
        fs::write(&file, "a\nB\nc\n").unwrap();
        super::handle(&p, &fx.ctx(200)).unwrap();

        let attribs: Vec<(i64, i64)> = fx
            .conn
            .prepare("SELECT line_start, line_end FROM attributions")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(attribs, vec![(2, 2)]);
    }

    #[test]
    fn cwd_is_persisted_on_attribution() {
        let fx = ContextFixture::new();
        let file = fx.blobs_dir.parent().unwrap().join("a.rs");
        fs::write(&file, "a\n").unwrap();
        let p = payload("s", "tu", file.to_str().unwrap(), "/my/repo");
        pre_tool_use::handle(&p, &fx.ctx(100)).unwrap();
        fs::write(&file, "a\nb\n").unwrap();
        super::handle(&p, &fx.ctx(200)).unwrap();
        let cwd: String = fx
            .conn
            .query_row("SELECT cwd FROM attributions LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cwd, "/my/repo");
    }
}
