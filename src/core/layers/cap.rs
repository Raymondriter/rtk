//! `cap` layer: bounds runaway terminal outputs, tail-biased.
//!
//! Bash errors live at the END of output, so the window keeps a small head
//! and a large tail with an explicit elision marker in between — unlike
//! `truncate`/`smart_truncate`, which are head-biased and code-oriented.
//! The full copy always lands in the recall file before this layer runs.

use super::{Layer, LayerCtx, LayerOutcome};
use crate::core::tracking::estimate_tokens;

pub const HEAD_LINES: usize = 50;
pub const TAIL_LINES: usize = 200;
pub const MAX_LINES: usize = HEAD_LINES + TAIL_LINES + 50;
pub const MAX_LINE_CHARS: usize = 2000;

pub struct CapLayer;

impl Layer for CapLayer {
    fn name(&self) -> &'static str {
        "cap"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        let needs_line_cap = input.lines().any(|l| l.chars().count() > MAX_LINE_CHARS);
        let total = input.lines().count();
        let needs_window = total > MAX_LINES;

        if !needs_line_cap && !needs_window {
            return LayerOutcome::Continue(input.to_string());
        }

        let cap_line = |l: &str| -> String {
            let count = l.chars().count();
            if count > MAX_LINE_CHARS {
                let kept: String = l.chars().take(MAX_LINE_CHARS).collect();
                format!("{} [+{} chars]", kept, count - MAX_LINE_CHARS)
            } else {
                l.to_string()
            }
        };

        let out = if needs_window {
            let lines: Vec<&str> = input.lines().collect();
            let elided = total - HEAD_LINES - TAIL_LINES;
            let mut result: Vec<String> = Vec::with_capacity(HEAD_LINES + TAIL_LINES + 1);
            result.extend(lines[..HEAD_LINES].iter().map(|l| cap_line(l)));
            result.push(format!(
                "[{elided} lines elided — full output in recall file]"
            ));
            result.extend(lines[total - TAIL_LINES..].iter().map(|l| cap_line(l)));
            result.join("\n")
        } else {
            input.lines().map(cap_line).collect::<Vec<_>>().join("\n")
        };

        let out = if input.ends_with('\n') {
            out + "\n"
        } else {
            out
        };
        // Degenerate inputs (many huge lines) could grow via markers.
        if estimate_tokens(&out) > estimate_tokens(input) {
            return LayerOutcome::Continue(input.to_string());
        }
        LayerOutcome::Continue(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ContentFormat, LayerCtx};
    use super::*;
    use crate::core::filter::Language;

    fn ctx() -> LayerCtx<'static> {
        LayerCtx {
            format: ContentFormat::Term,
            lang: Language::Unknown,
            source: "bash",
        }
    }

    fn apply(input: &str) -> String {
        match CapLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    #[test]
    fn test_under_caps_untouched() {
        let input = "short\nlines\n";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_window_keeps_tail_where_errors_live() {
        let input = (0..1000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\nerror: the thing that matters";
        let out = apply(&input);
        assert!(out.contains("error: the thing that matters"), "tail kept");
        assert!(out.contains("line 0"), "head kept");
        assert!(out.contains("lines elided — full output in recall file]"));
    }

    #[test]
    fn test_marker_arithmetic() {
        let total = 2 * MAX_LINES;
        let input = (0..total)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = apply(&input);
        let elided = total - HEAD_LINES - TAIL_LINES;
        assert!(out.contains(&format!("[{elided} lines elided")));
        assert_eq!(out.lines().count(), HEAD_LINES + TAIL_LINES + 1);
    }

    #[test]
    fn test_long_line_suffix_marker() {
        let input = "x".repeat(MAX_LINE_CHARS + 500);
        let out = apply(&input);
        assert!(
            out.ends_with("[+500 chars]"),
            "explicit char count: {}",
            &out[out.len() - 30..]
        );
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(apply(""), "");
    }
}
