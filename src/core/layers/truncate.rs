//! `truncate` layer: per-line char cap (`utils::truncate`) + line windowing
//! (`filter::smart_truncate`, `[N more lines]` trailer).

use super::{Layer, LayerCtx, LayerOutcome};
use crate::core::filter::smart_truncate;
use crate::core::utils::truncate as truncate_line;

/// Line window for posthook payloads. Grep content arrives host-capped at
/// 250 lines; Read/WebFetch payloads beyond this are tails the recall file
/// preserves.
pub const MAX_LINES: usize = 300;
/// Per-line char cap — protects against minified one-liner blobs.
pub const MAX_LINE_CHARS: usize = 500;

pub struct TruncateLayer;

impl Layer for TruncateLayer {
    fn name(&self) -> &'static str {
        "truncate"
    }

    fn apply(&self, input: &str, ctx: &LayerCtx) -> LayerOutcome {
        let needs_line_cap = input.lines().any(|l| l.chars().count() > MAX_LINE_CHARS);
        let needs_window = input.lines().count() > MAX_LINES;

        if !needs_line_cap && !needs_window {
            return LayerOutcome::Continue(input.to_string());
        }

        let capped = if needs_line_cap {
            input
                .lines()
                .map(|l| truncate_line(l, MAX_LINE_CHARS))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            input.to_string()
        };

        if needs_window {
            LayerOutcome::Continue(smart_truncate(&capped, MAX_LINES, &ctx.lang))
        } else {
            LayerOutcome::Continue(capped)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ContentFormat, LayerCtx};
    use super::*;
    use crate::core::filter::Language;

    fn ctx() -> LayerCtx<'static> {
        LayerCtx {
            format: ContentFormat::Web,
            lang: Language::Unknown,
            source: "test",
        }
    }

    fn apply(input: &str) -> String {
        match TruncateLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    #[test]
    fn test_short_content_unchanged() {
        let input = "line one\nline two";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_long_line_capped() {
        let long = "x".repeat(2 * MAX_LINE_CHARS);
        let out = apply(&long);
        assert!(out.chars().count() <= MAX_LINE_CHARS);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn test_many_lines_windowed_with_trailer() {
        let input = (0..2 * MAX_LINES)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = apply(&input);
        assert!(out.lines().count() <= MAX_LINES + 1);
        assert!(out.contains("more lines]"), "trailer must be present");
    }
}
