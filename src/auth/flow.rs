//! High-level orchestration for `cc-ledger auth` (login / status / logout).

use std::io::{IsTerminal, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::auth::orgs::{self, Selection};
use crate::auth::storage;
use crate::auth::workos::{self, PollResult};
use crate::paths::{self, now_ms};

const SLOW_DOWN_BUMP_SECS: u64 = 5;

/// Wrap `text` in an OSC 8 terminal hyperlink pointing at `url`. Falls back to
/// the plain URL when stdout is not a TTY (pipes, CI logs).
fn hyperlink(url: &str, text: &str) -> String {
    if std::io::stdout().is_terminal() {
        format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
    } else {
        text.to_string()
    }
}

pub fn login() -> Result<()> {
    let auth = workos::request_device_code()?;

    println!(
        "Visit: {}",
        hyperlink(&auth.verification_uri, &auth.verification_uri)
    );
    println!("Enter code: {}", auth.user_code);
    println!();
    println!(
        "(Or open: {})",
        hyperlink(
            &auth.verification_uri_complete,
            &auth.verification_uri_complete
        ),
    );
    println!();
    let _ = webbrowser::open(&auth.verification_uri_complete);

    print!("Waiting for confirmation");
    let _ = std::io::stdout().flush();

    let deadline = Instant::now() + Duration::from_secs(auth.expires_in);
    let mut interval = Duration::from_secs(auth.interval.max(1));

    loop {
        if Instant::now() >= deadline {
            println!();
            return Err(anyhow!(
                "device code expired before authorization completed"
            ));
        }
        sleep(interval);
        print!(".");
        let _ = std::io::stdout().flush();

        match workos::poll_token(&auth.device_code)? {
            PollResult::Approved(mut tokens) => {
                println!();
                // Save once before org selection so a network blip during
                // `/orgs` doesn't lose the access token. We re-save with
                // the picked org below.
                storage::save(&tokens)?;
                attach_org(&mut tokens)?;
                storage::save(&tokens)?;
                println!(
                    "✓ Logged in. Tokens saved to {}",
                    paths::auth_path()?.display()
                );

                // Kick off a detached background process that classifies
                // existing Claude Code transcripts and pushes them to the
                // backend. So the user sees data on /overview right away
                // instead of waiting for the next SessionEnd.
                if let Ok(bin) = std::env::current_exe() {
                    let _ = crate::sync::spawn_background_full_backfill(&bin);
                    println!("Backfilling activity categories in the background…");
                }
                return Ok(());
            }
            PollResult::Pending => {}
            PollResult::SlowDown => {
                interval += Duration::from_secs(SLOW_DOWN_BUMP_SECS);
            }
            PollResult::Denied => {
                println!();
                return Err(anyhow!("authorization denied by the user"));
            }
            PollResult::Expired => {
                println!();
                return Err(anyhow!(
                    "device code expired before authorization completed"
                ));
            }
            PollResult::Fatal(msg) => {
                println!();
                return Err(anyhow!("WorkOS error: {msg}"));
            }
        }
    }
}

pub fn status() -> Result<()> {
    match storage::load()? {
        Some(t) => {
            let remaining_ms = t.expires_at_ms - now_ms();
            if remaining_ms <= 0 {
                println!(
                    "Logged in, but access token expired (refresh token present). \
                     Run `cc-ledger auth` to re-authenticate."
                );
            } else {
                let mins = remaining_ms / 60_000;
                println!("Logged in. Access token expires in {mins}m (refresh token present).");
            }
            match (&t.org_name, &t.org_id) {
                (Some(name), _) => println!("Organization: {name}"),
                (None, Some(id)) => println!("Organization: {id}"),
                _ => {}
            }
        }
        None => println!("Not logged in."),
    }
    Ok(())
}

pub fn logout() -> Result<()> {
    if storage::clear()? {
        println!("Removed {}.", paths::auth_path()?.display());
    } else {
        println!("Already logged out.");
    }
    Ok(())
}

/// Fetch the user's WorkOS orgs from the backend and prompt for a
/// selection. Mutates `tokens` in place. Soft-fails on network errors
/// (we already have a valid login; the user can re-run `cc-ledger auth`
/// later to pick).
fn attach_org(tokens: &mut storage::Tokens) -> Result<()> {
    let orgs = match orgs::fetch(&tokens.access_token) {
        Ok(o) => o,
        Err(e) => {
            // `{e:#}` includes the full anyhow context chain; without `#`
            // we'd lose every layer except the outermost, which is what
            // hid the real cause earlier.
            eprintln!("⚠ couldn't fetch organizations ({e:#}); skipping selection");
            return Ok(());
        }
    };

    match orgs::select(orgs)? {
        Selection::Picked(o) => {
            tokens.org_id = Some(o.id);
            tokens.org_name = Some(o.name);
        }
        Selection::NonInteractive(o) => {
            eprintln!(
                "⚠ stdin is not a TTY; defaulted to '{}'. Re-run interactively to choose.",
                o.name
            );
            tokens.org_id = Some(o.id);
            tokens.org_name = Some(o.name);
        }
        Selection::NoOrgs => {
            eprintln!("⚠ you don't belong to any organization yet — usage will not be attributed.");
        }
    }
    Ok(())
}
