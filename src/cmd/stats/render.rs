//! Shared rendering primitives for every stats panel.
//!
//! - [`section_header`] writes a heavy-rule heading with the question dimmed.
//! - [`new_table`] returns a pre-styled `comfy_table::Table` with the
//!   project's preset (UTF8_FULL borders, bold cyan headers).
//!
//! TTY / `NO_COLOR`: comfy-table auto-detects via `is-terminal`, owo-colors
//! short-circuits when `NO_COLOR` is set. No explicit flag handling needed
//! at this layer.

use std::io::Write;

use anyhow::Result;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;

/// Render a section header with a heavy double rule, the title in bold cyan,
/// and the question dimmed grey on the same line.
pub(super) fn section_header<W: Write>(w: &mut W, title: &str, question: &str) -> Result<()> {
    let rule = "━".repeat(78);
    writeln!(w)?;
    writeln!(w, "{}", rule.dimmed())?;
    writeln!(
        w,
        "  {}    {}",
        title.bold().cyan(),
        question.dimmed().italic()
    )?;
    writeln!(w, "{}", rule.dimmed())?;
    Ok(())
}

/// Build a styled comfy-table with the project's standard look: UTF8_FULL
/// borders, content-aware widths, bold cyan headers. Callers append rows.
pub(super) fn new_table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);
    t.set_header(
        headers
            .iter()
            .map(|h| Cell::new(h).add_attribute(Attribute::Bold).fg(Color::Cyan))
            .collect::<Vec<_>>(),
    );
    t
}

/// Render a table to the writer, or print "(no data)" if it has no rows.
pub(super) fn print_or_no_data<W: Write>(w: &mut W, t: Table, has_rows: bool) -> Result<()> {
    if !has_rows {
        writeln!(w, "(no data)")?;
        return Ok(());
    }
    writeln!(w, "{t}")?;
    Ok(())
}
