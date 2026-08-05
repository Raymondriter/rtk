//! `grep-group` layer: groups `path:line:content` match lines by file.
//!
//! Seeded from `pipe_cmd::grep_wrapper` (colon-heuristic parsing, cap-aware);
//! convergence with `search.rs::group_matches` is a Part 2 refactor.
//!
//! Faithfulness guard: if any non-empty line does not parse as
//! `path:line:content` (e.g. `-C` context lines using `-` separators), the
//! whole input passes through unchanged — grouping must never drop lines.
//! Lossless by design: every match is emitted (no per-file cap) — the
//! posthook path converts, it never truncates.

use super::{Layer, LayerCtx, LayerOutcome};
use std::collections::HashMap;

pub struct GrepGroupLayer;

impl Layer for GrepGroupLayer {
    fn name(&self) -> &'static str {
        "grep-group"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        let mut by_file: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
        let mut order: Vec<&str> = Vec::new();
        let mut total = 0;

        for line in input.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.splitn(3, ':').collect();
            let parsed = parts.len() == 3 && parts[1].parse::<usize>().is_ok();
            if !parsed {
                // Context lines or unexpected format: stay faithful, pass through.
                return LayerOutcome::Continue(input.to_string());
            }
            total += 1;
            let entry = by_file.entry(parts[0]).or_default();
            if entry.is_empty() {
                order.push(parts[0]);
            }
            entry.push((parts[1], parts[2]));
        }

        if total == 0 {
            return LayerOutcome::Continue(input.to_string());
        }

        let mut out = format!("{} matches in {}F:\n\n", total, by_file.len());
        for file in order {
            let matches = &by_file[file];
            out.push_str(&format!("[file] {} ({}):\n", file, matches.len()));
            for (line_num, content) in matches {
                out.push_str(&format!("  {:>4}: {}\n", line_num, content.trim()));
            }
            out.push('\n');
        }

        LayerOutcome::Continue(out.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ContentFormat, LayerCtx};
    use super::*;
    use crate::core::filter::Language;

    fn ctx() -> LayerCtx<'static> {
        LayerCtx {
            format: ContentFormat::Matches,
            lang: Language::Unknown,
            source: "grep",
        }
    }

    fn apply(input: &str) -> String {
        match GrepGroupLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    fn fixture_content(raw: &str) -> String {
        let event: serde_json::Value = serde_json::from_str(raw).expect("fixture parses");
        event["tool_response"]["content"]
            .as_str()
            .expect("fixture has content")
            .to_string()
    }

    #[test]
    fn test_groups_real_grep_content_by_file() {
        let input = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/grep_content_small.json"
        ));
        let out = apply(&input);
        assert!(out.starts_with("43 matches in 1F:"), "header: {out}");
        assert_eq!(
            out.matches("haystack.rs").count(),
            1,
            "filename must appear once, not per line"
        );
    }

    #[test]
    fn test_large_truncated_fixture_savings_at_least_20_percent() {
        let input = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/grep_content_large_truncated.json"
        ));
        let out = apply(&input);
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(
            savings >= 20.0,
            "grep-group: expected ≥20% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_all_matches_emitted_no_cap() {
        let input = (1..=40)
            .map(|i| format!("src/big.rs:{i}:let x = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = apply(&input);
        assert!(out.starts_with("40 matches in 1F:"));
        assert!(
            out.contains("  40: let x = 40;"),
            "last match present: {out}"
        );
        assert!(
            !out.contains('+'),
            "no overflow marker, grouping is lossless"
        );
    }

    #[test]
    fn test_context_lines_pass_through_unchanged() {
        // `grep -C` context lines use `-` separators; grouping must not drop them.
        let input = "src/a.rs:10:match line\nsrc/a.rs-11-context line";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_non_grep_content_passes_through() {
        let input = "just some text\nwithout match format";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(apply(""), "");
    }
}
