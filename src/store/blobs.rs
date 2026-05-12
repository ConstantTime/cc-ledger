//! Content-addressed blob store with two-level fan-out.
//!
//! Layout: `<blobs_dir>/<aa>/<bbcc...>` where the full sha256 is `aabbcc...`.
//! Every function takes `blobs_dir` explicitly so tests don't have to mutate
//! environment state.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Hash and store `bytes` under `blobs_dir`. Returns the lowercase-hex sha256 id.
/// Idempotent: re-storing the same content is a no-op.
pub fn put_blob(blobs_dir: &Path, bytes: &[u8]) -> Result<String> {
    let sha = sha256_hex(bytes);
    let dest = blob_path(blobs_dir, &sha);
    if !dest.exists() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("tmp");
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, &dest)
            .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
    }
    Ok(sha)
}

/// Read a file from disk and store it as a blob. A missing file is stored as
/// the empty blob — that matches `Write` of a path that doesn't exist yet at
/// PreToolUse time.
pub fn put_file_as_blob(blobs_dir: &Path, file: &Path) -> Result<String> {
    let bytes = match fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    put_blob(blobs_dir, &bytes)
}

pub fn read_blob(blobs_dir: &Path, sha: &str) -> Result<Vec<u8>> {
    fs::read(blob_path(blobs_dir, sha)).with_context(|| format!("read blob {sha}"))
}

fn blob_path(blobs_dir: &Path, sha: &str) -> PathBuf {
    if sha.len() < 3 {
        return blobs_dir.join(sha);
    }
    let (a, rest) = sha.split_at(2);
    blobs_dir.join(a).join(rest)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn put_then_read_round_trips() {
        let d = dir();
        let sha = put_blob(d.path(), b"hello world").unwrap();
        assert_eq!(read_blob(d.path(), &sha).unwrap(), b"hello world");
    }

    #[test]
    fn put_is_idempotent() {
        let d = dir();
        let a = put_blob(d.path(), b"same").unwrap();
        let b = put_blob(d.path(), b"same").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn missing_file_stores_empty_blob() {
        let d = dir();
        let sha = put_file_as_blob(d.path(), Path::new("/nonexistent/here")).unwrap();
        assert_eq!(
            sha,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn fan_out_layout_uses_two_chars() {
        let d = dir();
        let sha = put_blob(d.path(), b"content").unwrap();
        let p = blob_path(d.path(), &sha);
        assert_eq!(
            p.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn known_sha_is_stable() {
        let d = dir();
        let sha = put_blob(d.path(), b"abc").unwrap();
        // Standard sha256("abc")
        assert_eq!(
            sha,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
