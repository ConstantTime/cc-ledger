//! Compute line ranges that changed between a pre and post file image.
//!
//! For attribution we want the **post-image** line numbers of inserted or
//! replaced lines: those are the lines the agent now "owns". Pure deletions
//! produce no ranges (no post-image line to attribute), but neighbouring
//! lines stay un-attributed because the agent didn't modify them.
//!
//! Output is 1-indexed inclusive `(start, end)` to match how editors and
//! `git blame` present line numbers.

use std::ops::Range;

use imara_diff::intern::InternedInput;
use imara_diff::{diff, Algorithm, Sink};

/// Returns post-image `(line_start, line_end)` ranges for every changed
/// hunk. Pure deletions are skipped. Both inputs equal → empty Vec.
pub fn changed_post_ranges(pre: &str, post: &str) -> Vec<(u32, u32)> {
    let input = InternedInput::new(pre, post);
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    diff(
        Algorithm::Histogram,
        &input,
        RangeSink {
            ranges: &mut ranges,
        },
    );
    ranges
}

/// Same as [`changed_post_ranges`] but also returns total lines added /
/// removed. Useful for `tool_calls.lines_added/lines_removed` columns.
pub fn diff_summary(pre: &str, post: &str) -> DiffSummary {
    let input = InternedInput::new(pre, post);
    let mut s = SummarySink {
        ranges: Vec::new(),
        added: 0,
        removed: 0,
    };
    diff(Algorithm::Histogram, &input, &mut s);
    DiffSummary {
        post_ranges: s.ranges,
        lines_added: s.added,
        lines_removed: s.removed,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffSummary {
    pub post_ranges: Vec<(u32, u32)>,
    pub lines_added: u64,
    pub lines_removed: u64,
}

struct RangeSink<'a> {
    ranges: &'a mut Vec<(u32, u32)>,
}

impl Sink for RangeSink<'_> {
    type Out = ();
    fn process_change(&mut self, _before: Range<u32>, after: Range<u32>) {
        if after.start == after.end {
            return; // pure deletion — no post-image line to attribute
        }
        self.ranges.push((after.start + 1, after.end));
    }
    fn finish(self) -> Self::Out {}
}

struct SummarySink {
    ranges: Vec<(u32, u32)>,
    added: u64,
    removed: u64,
}

impl Sink for &mut SummarySink {
    type Out = ();
    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        let before_len = (before.end - before.start) as u64;
        let after_len = (after.end - after.start) as u64;
        self.removed += before_len;
        self.added += after_len;
        if after_len > 0 {
            self.ranges.push((after.start + 1, after.end));
        }
    }
    fn finish(self) -> Self::Out {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_insertion_attributes_added_lines() {
        let pre = "a\nb\nc\n";
        let post = "a\nNEW1\nNEW2\nb\nc\n";
        assert_eq!(changed_post_ranges(pre, post), vec![(2, 3)]);
    }

    #[test]
    fn replacement_attributes_replacement_lines() {
        let pre = "a\nb\nc\n";
        let post = "a\nB\nc\n";
        assert_eq!(changed_post_ranges(pre, post), vec![(2, 2)]);
    }

    #[test]
    fn pure_deletion_yields_no_ranges() {
        let pre = "a\nb\nc\n";
        let post = "a\nc\n";
        assert!(changed_post_ranges(pre, post).is_empty());
    }

    #[test]
    fn no_change_yields_no_ranges() {
        let pre = "a\nb\nc\n";
        let post = "a\nb\nc\n";
        assert!(changed_post_ranges(pre, post).is_empty());
    }

    #[test]
    fn append_at_end() {
        let pre = "a\nb\n";
        let post = "a\nb\nNEW\n";
        assert_eq!(changed_post_ranges(pre, post), vec![(3, 3)]);
    }

    #[test]
    fn create_from_empty() {
        let pre = "";
        let post = "a\nb\nc\n";
        assert_eq!(changed_post_ranges(pre, post), vec![(1, 3)]);
    }

    #[test]
    fn multiple_hunks_in_one_file() {
        let pre = "a\nb\nc\nd\ne\n";
        let post = "a\nB\nc\nD\ne\n";
        assert_eq!(changed_post_ranges(pre, post), vec![(2, 2), (4, 4)]);
    }

    #[test]
    fn summary_counts_added_and_removed() {
        let pre = "a\nb\nc\n";
        let post = "a\nB\nC\nD\n";
        let s = diff_summary(pre, post);
        // b, c removed → 2; B, C, D added → 3.
        assert_eq!(s.lines_removed, 2);
        assert_eq!(s.lines_added, 3);
        assert_eq!(s.post_ranges, vec![(2, 4)]);
    }

    #[test]
    fn summary_pure_deletion_has_no_post_range() {
        let pre = "a\nb\nc\n";
        let post = "a\nc\n";
        let s = diff_summary(pre, post);
        assert_eq!(s.lines_added, 0);
        assert_eq!(s.lines_removed, 1);
        assert!(s.post_ranges.is_empty());
    }
}
