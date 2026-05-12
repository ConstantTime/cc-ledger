//! Thin wrapper over the `git` binary.
//!
//! Every call uses `std::process::Command` with stdout captured and a hard
//! per-call timeout. Failures degrade to `None` so the hooks that drive this
//! module never block the agent. Nothing here writes to git — read-only.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Default timeout for any single git subprocess. Anything longer is treated
/// as a failure and discarded.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Timeout for `git ls-remote` — has to make a network round trip, so a bit
/// more headroom.
pub const LSREMOTE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `git -C <cwd> <args>` and return stdout as a string on success.
/// Returns `None` if git fails, isn't installed, or exceeds `timeout`.
pub fn run(cwd: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return None,
    };

    // Wait for the child on a worker thread; main thread enforces the
    // timeout via channel recv. We don't want to pull in tokio for one site.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = child.wait_with_output();
        let _ = tx.send(res);
    });
    let output = match rx.recv_timeout(timeout) {
        Ok(Ok(o)) => o,
        Ok(Err(_)) | Err(_) => return None,
    };
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// `git rev-parse HEAD` — current commit sha. None if not in a repo.
pub fn head_sha(cwd: &Path) -> Option<String> {
    run(cwd, &["rev-parse", "HEAD"], DEFAULT_TIMEOUT).map(|s| s.trim().to_string())
}

/// `git rev-parse --abbrev-ref HEAD` — current branch. None if detached.
pub fn current_branch(cwd: &Path) -> Option<String> {
    let s = run(cwd, &["rev-parse", "--abbrev-ref", "HEAD"], DEFAULT_TIMEOUT)?;
    let s = s.trim();
    if s == "HEAD" {
        return None; // detached
    }
    Some(s.to_string())
}

/// `git rev-parse @{u}` — sha of the upstream tracking branch.
pub fn upstream_sha(cwd: &Path) -> Option<String> {
    run(cwd, &["rev-parse", "@{u}"], DEFAULT_TIMEOUT).map(|s| s.trim().to_string())
}

/// Resolve the default branch name. Tries `git symbolic-ref
/// refs/remotes/origin/HEAD` (returns e.g. `refs/remotes/origin/main`); falls
/// back to `main` then `master`. Returns the short name (`main`, `master`).
pub fn default_branch(cwd: &Path) -> Option<String> {
    if let Some(s) = run(
        cwd,
        &["symbolic-ref", "refs/remotes/origin/HEAD"],
        DEFAULT_TIMEOUT,
    ) {
        if let Some(name) = s.trim().strip_prefix("refs/remotes/origin/") {
            return Some(name.to_string());
        }
    }
    for candidate in ["main", "master"] {
        if run(
            cwd,
            &["rev-parse", "--verify", &format!("origin/{candidate}")],
            DEFAULT_TIMEOUT,
        )
        .is_some()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// `git remote get-url origin`. Returns the raw URL (could be ssh or https).
pub fn origin_url(cwd: &Path) -> Option<String> {
    run(cwd, &["remote", "get-url", "origin"], DEFAULT_TIMEOUT).map(|s| s.trim().to_string())
}

/// Best-effort repo basename derived from an `origin` URL. Strips `.git` and
/// returns the trailing path component. None if no remote.
pub fn repo_basename(cwd: &Path) -> Option<String> {
    let url = origin_url(cwd)?;
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.trim_end_matches(".git");
    let last = trimmed.rsplit(['/', ':']).next()?;
    if last.is_empty() {
        return None;
    }
    Some(last.to_string())
}

/// Repo identity parsed from an `origin` URL: where it's hosted and who owns it.
///
/// `host` is plaintext (e.g. `github.com`). It's no more revealing than
/// `repo_basename`, which we already sync. Self-hosted hosts surface in
/// plaintext too; users who need that hashed can opt in later.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoIdent {
    pub host: String,
    pub owner: String,
    pub name: String,
}

/// Parse the three URL forms git emits for `origin`:
/// - `git@github.com:owner/repo.git`            (SSH alias)
/// - `https://github.com/owner/repo[.git]`      (HTTPS)
/// - `ssh://git@github.com/owner/repo[.git]`    (SSH explicit)
///
/// Returns `None` for forms we don't recognize (filesystem paths, gh:// scheme,
/// malformed URLs).
pub fn parse_remote_url(url: &str) -> Option<RepoIdent> {
    let s = url.trim();
    if s.is_empty() {
        return None;
    }

    // ssh://[user@]host[:port]/owner/repo[.git]
    // https://host[:port]/owner/repo[.git]
    if let Some(rest) = s
        .strip_prefix("ssh://")
        .or_else(|| s.strip_prefix("https://"))
        .or_else(|| s.strip_prefix("http://"))
    {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?; // drop user@
        let host = host.split(':').next()?; // drop :port
        return parse_owner_repo(host, path);
    }

    // git@host:owner/repo[.git]
    if let Some(after_user) = s.strip_prefix("git@") {
        let (host, path) = after_user.split_once(':')?;
        return parse_owner_repo(host, path);
    }

    None
}

fn parse_owner_repo(host: &str, path: &str) -> Option<RepoIdent> {
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if host.is_empty() || owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(RepoIdent {
        host: host.to_string(),
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// Convenience: parse `origin` URL of a working directory. None if no remote
/// or the URL doesn't match any known form.
pub fn repo_ident(cwd: &Path) -> Option<RepoIdent> {
    parse_remote_url(&origin_url(cwd)?)
}

/// One commit's diffstat. Mirrors `git log -1 --numstat <sha>`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitDiffstat {
    pub sha: String,
    pub authored_at_unix: Option<i64>,
    pub subject: Option<String>,
    pub additions: i64,
    pub deletions: i64,
    pub files: Vec<CommitFile>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommitFile {
    pub path: String,
    pub additions: i64,
    pub deletions: i64,
}

/// `git log -1 --numstat --format='%H%n%at%n%s' <sha>` — single commit's
/// metadata + per-file numstat. None on any parse error or git failure.
pub fn commit_diffstat(cwd: &Path, sha: &str) -> Option<CommitDiffstat> {
    let out = run(
        cwd,
        &["log", "-1", "--numstat", "--format=%H%n%at%n%s", sha],
        DEFAULT_TIMEOUT,
    )?;
    parse_commit_diffstat(&out)
}

fn parse_commit_diffstat(text: &str) -> Option<CommitDiffstat> {
    let mut lines = text.lines();
    let sha = lines.next()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }
    let authored_at_unix = lines.next().and_then(|s| s.trim().parse::<i64>().ok());
    let subject = lines.next().map(|s| s.to_string());
    // Followed by an empty line, then numstat rows:  "<add>\t<del>\t<path>"
    let mut additions = 0i64;
    let mut deletions = 0i64;
    let mut files = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let add = parts.next()?;
        let del = parts.next()?;
        let path = parts.next()?;
        // Binary files show "-\t-\t<path>" — count zero adds/dels.
        let add: i64 = add.parse().unwrap_or(0);
        let del: i64 = del.parse().unwrap_or(0);
        additions += add;
        deletions += del;
        files.push(CommitFile {
            path: path.to_string(),
            additions: add,
            deletions: del,
        });
    }
    Some(CommitDiffstat {
        sha,
        authored_at_unix,
        subject,
        additions,
        deletions,
        files,
    })
}

/// `git rev-list <branch> ^<base>` — commit shas unique to `branch`. Empty
/// vector if branch == base or anything fails.
pub fn rev_list_branch(cwd: &Path, branch: &str, base: &str) -> Vec<String> {
    let Some(out) = run(
        cwd,
        &["rev-list", branch, &format!("^{base}")],
        DEFAULT_TIMEOUT,
    ) else {
        return Vec::new();
    };
    out.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// `git ls-remote --refs origin 'refs/pull/*/head'` — returns the `(sha, n)`
/// pairs for every PR head ref the remote publishes. Empty when `origin` isn't
/// GitHub or the remote is unreachable.
pub fn list_pr_head_refs(cwd: &Path) -> Vec<(String, i64)> {
    let Some(out) = run(
        cwd,
        &["ls-remote", "--refs", "origin", "refs/pull/*/head"],
        LSREMOTE_TIMEOUT,
    ) else {
        return Vec::new();
    };
    parse_pr_head_refs(&out)
}

fn parse_pr_head_refs(text: &str) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(2, '\t');
        let sha = parts.next().unwrap_or("").trim();
        let refname = parts.next().unwrap_or("").trim();
        let Some(rest) = refname.strip_prefix("refs/pull/") else {
            continue;
        };
        let Some(num_str) = rest.strip_suffix("/head") else {
            continue;
        };
        let Ok(n) = num_str.parse::<i64>() else {
            continue;
        };
        if !sha.is_empty() {
            out.push((sha.to_string(), n));
        }
    }
    out
}

/// Walk `git log --first-parent <base> --format='%H %s'` looking for a commit
/// whose subject ends in `(#<pr_number>)` — the GitHub squash-merge format.
/// Returns `(merge_sha, authored_at_unix)` if found.
pub fn find_squash_merge(cwd: &Path, base: &str, pr_number: i64) -> Option<(String, i64)> {
    let needle = format!("(#{pr_number})");
    let out = run(
        cwd,
        &[
            "log",
            "--first-parent",
            base,
            "--format=%H %at %s",
            "-n",
            "200",
        ],
        DEFAULT_TIMEOUT,
    )?;
    for line in out.lines() {
        let mut parts = line.splitn(3, ' ');
        let sha = parts.next()?;
        let ts = parts.next()?;
        let subject = parts.next().unwrap_or("");
        if subject.contains(&needle) {
            let ts: i64 = ts.parse().ok()?;
            return Some((sha.to_string(), ts));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commit_diffstat_basic() {
        let text = "abc123\n1717000000\nfeat: thing\n\n3\t1\tsrc/a.rs\n10\t0\tsrc/b.rs\n";
        let c = parse_commit_diffstat(text).unwrap();
        assert_eq!(c.sha, "abc123");
        assert_eq!(c.authored_at_unix, Some(1717000000));
        assert_eq!(c.subject.as_deref(), Some("feat: thing"));
        assert_eq!(c.additions, 13);
        assert_eq!(c.deletions, 1);
        assert_eq!(c.files.len(), 2);
        assert_eq!(c.files[0].path, "src/a.rs");
        assert_eq!(c.files[1].additions, 10);
    }

    #[test]
    fn parse_commit_diffstat_handles_binary_files() {
        let text = "abc\n100\nbinary commit\n\n-\t-\tsrc/img.png\n2\t1\tsrc/a.rs\n";
        let c = parse_commit_diffstat(text).unwrap();
        assert_eq!(c.additions, 2);
        assert_eq!(c.deletions, 1);
        assert_eq!(c.files.len(), 2);
    }

    #[test]
    fn parse_pr_head_refs_filters_garbage() {
        let text = "abc\trefs/pull/12/head\nzzz\trefs/heads/main\n\
                    deadbeef\trefs/pull/9/merge\nfoo\trefs/pull/3/head\n";
        let out = parse_pr_head_refs(text);
        assert_eq!(out, vec![("abc".into(), 12), ("foo".into(), 3)]);
    }

    #[test]
    fn parse_pr_head_refs_skips_non_numeric() {
        let text = "abc\trefs/pull/notanumber/head\n";
        assert!(parse_pr_head_refs(text).is_empty());
    }

    #[test]
    fn parse_remote_url_ssh_alias() {
        let r = parse_remote_url("git@github.com:delta-hq/cc-ledger.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "delta-hq");
        assert_eq!(r.name, "cc-ledger");
    }

    #[test]
    fn parse_remote_url_https() {
        let r = parse_remote_url("https://github.com/delta-hq/cc-ledger.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "delta-hq");
        assert_eq!(r.name, "cc-ledger");
    }

    #[test]
    fn parse_remote_url_https_no_dot_git() {
        let r = parse_remote_url("https://gitlab.com/owner/sub-repo").unwrap();
        assert_eq!(r.host, "gitlab.com");
        assert_eq!(r.owner, "owner");
        assert_eq!(r.name, "sub-repo");
    }

    #[test]
    fn parse_remote_url_ssh_explicit() {
        let r = parse_remote_url("ssh://git@github.com/delta-hq/cc-ledger.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "delta-hq");
        assert_eq!(r.name, "cc-ledger");
    }

    #[test]
    fn parse_remote_url_https_with_port() {
        let r = parse_remote_url("https://gitlab.example.com:8443/g/r.git").unwrap();
        assert_eq!(r.host, "gitlab.example.com");
        assert_eq!(r.owner, "g");
        assert_eq!(r.name, "r");
    }

    #[test]
    fn parse_remote_url_self_hosted_ssh() {
        let r = parse_remote_url("git@gitlab.example-corp.com:platform/infra.git").unwrap();
        assert_eq!(r.host, "gitlab.example-corp.com");
        assert_eq!(r.owner, "platform");
        assert_eq!(r.name, "infra");
    }

    #[test]
    fn parse_remote_url_rejects_unknown_forms() {
        assert!(parse_remote_url("").is_none());
        assert!(parse_remote_url("not a url").is_none());
        assert!(parse_remote_url("/local/path").is_none());
        assert!(parse_remote_url("file:///tmp/repo").is_none());
        assert!(parse_remote_url("https://github.com/owner").is_none()); // missing repo
    }

    #[test]
    fn parse_remote_url_trims_trailing_slash() {
        let r = parse_remote_url("https://github.com/owner/repo/").unwrap();
        assert_eq!(r.name, "repo");
    }
}
