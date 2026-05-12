//! `cc-ledger config` — read/write the local `config` table.
//!
//! Pure local; never touches the network. Today the only configurable key is
//! `git_notes.enabled`. Sync state is implied by `auth.json` presence per the
//! master plan.

use std::io::Write;

use anyhow::Result;
use clap::Parser;
use rusqlite::Connection;

use crate::store::queries;
use crate::{paths, store};

#[derive(Debug, Parser)]
pub struct Args {
    /// Config key (e.g. `git_notes.enabled`). Omit to print all keys.
    pub key: Option<String>,
    /// New value. When provided alongside `key`, the value is set.
    pub value: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let conn = store::open(&paths::db_path()?)?;
    let mut out = std::io::stdout().lock();
    dispatch(&mut out, &conn, args)
}

fn dispatch<W: Write>(out: &mut W, conn: &Connection, args: Args) -> Result<()> {
    match (args.key, args.value) {
        (None, _) => print_all(out, conn),
        (Some(k), None) => print_one(out, conn, &k),
        (Some(k), Some(v)) => set_one(out, conn, &k, &v),
    }
}

fn print_all<W: Write>(out: &mut W, conn: &Connection) -> Result<()> {
    let entries = queries::list_config(conn)?;
    if entries.is_empty() {
        writeln!(out, "(no config keys set)")?;
        return Ok(());
    }
    let kw = entries.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in entries {
        writeln!(out, "{:<kw$}  {}", k, v, kw = kw)?;
    }
    Ok(())
}

fn print_one<W: Write>(out: &mut W, conn: &Connection, key: &str) -> Result<()> {
    match queries::get_config(conn, key)? {
        Some(v) => writeln!(out, "{v}")?,
        None => writeln!(out, "(unset)")?,
    }
    Ok(())
}

fn set_one<W: Write>(out: &mut W, conn: &Connection, key: &str, value: &str) -> Result<()> {
    queries::set_config(conn, key, value)?;
    writeln!(out, "{key} = {value}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("ledger.db")).unwrap();
        std::mem::forget(dir);
        conn
    }

    #[test]
    fn list_includes_seeded_default() {
        let conn = db();
        let mut buf: Vec<u8> = Vec::new();
        dispatch(
            &mut buf,
            &conn,
            Args {
                key: None,
                value: None,
            },
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("git_notes.enabled"));
        assert!(s.contains("false"));
    }

    #[test]
    fn read_then_write_then_read() {
        let conn = db();

        let mut buf: Vec<u8> = Vec::new();
        dispatch(
            &mut buf,
            &conn,
            Args {
                key: Some("git_notes.enabled".into()),
                value: None,
            },
        )
        .unwrap();
        assert_eq!(String::from_utf8(buf).unwrap().trim(), "false");

        let mut buf: Vec<u8> = Vec::new();
        dispatch(
            &mut buf,
            &conn,
            Args {
                key: Some("git_notes.enabled".into()),
                value: Some("true".into()),
            },
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("git_notes.enabled = true"));

        let mut buf: Vec<u8> = Vec::new();
        dispatch(
            &mut buf,
            &conn,
            Args {
                key: Some("git_notes.enabled".into()),
                value: None,
            },
        )
        .unwrap();
        assert_eq!(String::from_utf8(buf).unwrap().trim(), "true");
    }

    #[test]
    fn unknown_key_prints_unset() {
        let conn = db();
        let mut buf: Vec<u8> = Vec::new();
        dispatch(
            &mut buf,
            &conn,
            Args {
                key: Some("nope".into()),
                value: None,
            },
        )
        .unwrap();
        assert_eq!(String::from_utf8(buf).unwrap().trim(), "(unset)");
    }
}
