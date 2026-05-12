//! Shared formatters and color rules.
//!
//! Pure functions, easily unit-tested. We do *not* assert on raw ANSI bytes
//! anywhere — those are owo-colors' problem and would make tests brittle.
//! Instead, the gradient functions return a `ColorTier` enum and we test
//! threshold boundaries on that.

use comfy_table::{Cell, Color};

/// USD with sensible precision: 6 decimals when the value is below a cent and
/// non-zero (so $0.000123 doesn't render as $0.0001), 4 decimals otherwise.
pub(super) fn fmt_usd(n: f64) -> String {
    if n.abs() < 0.01 && n != 0.0 {
        format!("${n:.6}")
    } else {
        format!("${n:.4}")
    }
}

/// Compact token count: 1_234 → "1.2K", 1_234_567 → "1.2M", 1B+ → "1.2B".
/// Plain integer below 1K so we don't lose precision for tiny rows.
pub(super) fn fmt_tokens(n: i64) -> String {
    let n = n.max(0);
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n < 1_000_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    }
}

/// Parse a `--since` window into a Unix-ms cutoff (lower bound, inclusive).
/// `"all"` → `Ok(None)` (no time filter). Otherwise expects `<N>d` or `<N>h`,
/// e.g. `30d`, `7d`, `12h`. Anything else is a hard error from `clap` parse
/// time so we never silently disagree with the user.
pub(super) fn parse_since(s: &str, now_ms: i64) -> anyhow::Result<Option<i64>> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let last = s.chars().last().ok_or_else(|| {
        anyhow::anyhow!("empty --since value; use a duration like `30d` or `all`")
    })?;
    let (num_str, unit_ms) = match last {
        'd' | 'D' => (&s[..s.len() - 1], 86_400_000_i64),
        'h' | 'H' => (&s[..s.len() - 1], 3_600_000_i64),
        _ => anyhow::bail!(
            "unsupported --since `{s}` — use `<N>d`, `<N>h`, or `all` (e.g. 30d, 7d, all)"
        ),
    };
    let n: i64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("--since `{s}`: expected number before unit"))?;
    if n <= 0 {
        anyhow::bail!("--since `{s}`: number must be positive");
    }
    Ok(Some(now_ms.saturating_sub(n.saturating_mul(unit_ms))))
}

/// Three-tier color band. Mapped to comfy-table colors at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ColorTier {
    Hot,   // top quartile / over-threshold
    Warm,  // middle
    Cool,  // bottom quartile / safe
    Muted, // not enough data to tier
}

impl ColorTier {
    pub(super) fn comfy(self) -> Option<Color> {
        match self {
            ColorTier::Hot => Some(Color::Red),
            ColorTier::Warm => Some(Color::Yellow),
            ColorTier::Cool => Some(Color::Green),
            ColorTier::Muted => None,
        }
    }
}

/// Cost gradient relative to the *table maximum*: top quartile red, middle
/// yellow, bottom green. With ≤ 2 rows everything is muted (no useful
/// gradient on tiny tables).
pub(super) fn cost_tier(value: f64, max: f64, n_rows: usize) -> ColorTier {
    if n_rows < 3 || max <= 0.0 {
        return ColorTier::Muted;
    }
    let ratio = value / max;
    if ratio >= 0.75 {
        ColorTier::Hot
    } else if ratio >= 0.25 {
        ColorTier::Warm
    } else {
        ColorTier::Cool
    }
}

/// Build a comfy-table cell, applying the tier's color if any.
pub(super) fn cell(text: impl Into<String>, tier: ColorTier) -> Cell {
    let mut c = Cell::new(text.into());
    if let Some(color) = tier.comfy() {
        c = c.fg(color);
    }
    c
}

/// Sparkline-style horizontal bar. `pct` in [0.0, 1.0] → `width` cells of
/// `█` followed by `░`. Used by the activity-category panel's SHARE column.
pub(super) fn bar(pct: f64, width: usize) -> String {
    let filled = (pct.clamp(0.0, 1.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_usd_uses_six_decimals_below_a_cent() {
        assert_eq!(fmt_usd(0.001234), "$0.001234");
        assert_eq!(fmt_usd(0.0), "$0.0000");
        assert_eq!(fmt_usd(1.23456), "$1.2346");
    }

    #[test]
    fn cost_tier_buckets_at_quartile_boundaries() {
        // max = 1.0, 4 rows → meaningful gradient.
        assert_eq!(cost_tier(0.10, 1.0, 4), ColorTier::Cool);
        assert_eq!(cost_tier(0.50, 1.0, 4), ColorTier::Warm);
        assert_eq!(cost_tier(0.80, 1.0, 4), ColorTier::Hot);
        assert_eq!(cost_tier(1.00, 1.0, 4), ColorTier::Hot);
    }

    #[test]
    fn cost_tier_mutes_tiny_tables() {
        assert_eq!(cost_tier(1.0, 1.0, 2), ColorTier::Muted);
        assert_eq!(cost_tier(0.5, 0.0, 5), ColorTier::Muted);
    }

    #[test]
    fn bar_renders_correct_filled_proportion() {
        assert_eq!(bar(0.0, 4), "░░░░");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(1.5, 4), "████"); // clamps
    }

    #[test]
    fn fmt_tokens_uses_si_suffixes() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_234), "1.2K");
        assert_eq!(fmt_tokens(1_234_567), "1.2M");
        assert_eq!(fmt_tokens(1_234_567_890), "1.2B");
    }

    #[test]
    fn parse_since_understands_d_h_and_all() {
        let now: i64 = 10_000_000_000;
        assert_eq!(parse_since("all", now).unwrap(), None);
        assert_eq!(parse_since("ALL", now).unwrap(), None);
        assert_eq!(
            parse_since("30d", now).unwrap(),
            Some(now - 30 * 86_400_000)
        );
        assert_eq!(parse_since("12h", now).unwrap(), Some(now - 12 * 3_600_000));
    }

    #[test]
    fn parse_since_rejects_garbage() {
        let now: i64 = 0;
        assert!(parse_since("", now).is_err());
        assert!(parse_since("30", now).is_err());
        assert!(parse_since("30x", now).is_err());
        assert!(parse_since("0d", now).is_err());
        assert!(parse_since("-7d", now).is_err());
    }
}
