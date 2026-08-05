//! `dedup` layer: collapses runs of identical adjacent lines.
//!
//! Adjacent-only, non-blank lines only, run ≥ 3, explicit `[xN]` marker:
//! content kept once, exact count kept, ordering kept — identical adjacent
//! lines carry no distinguishing information by definition. Cross-distance
//! dedup would destroy structure and is deliberately not done. Blank-line
//! runs are exempt (marker noise would cost more than it saves).

use super::{Layer, LayerCtx, LayerOutcome};

const MIN_RUN: usize = 3;

pub struct DedupLayer;

impl Layer for DedupLayer {
    fn name(&self) -> &'static str {
        "dedup"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        let lines: Vec<&str> = input.lines().collect();
        let mut out: Vec<String> = Vec::with_capacity(lines.len());
        let mut i = 0;
        let mut changed = false;

        while i < lines.len() {
            let line = lines[i];
            let mut run = 1;
            while i + run < lines.len() && lines[i + run] == line {
                run += 1;
            }
            if run >= MIN_RUN && !line.trim().is_empty() {
                out.push(line.to_string());
                out.push(format!("[x{run}]"));
                changed = true;
            } else {
                for _ in 0..run {
                    out.push(line.to_string());
                }
            }
            i += run;
        }

        if !changed {
            return LayerOutcome::Continue(input.to_string());
        }
        let mut result = out.join("\n");
        if input.ends_with('\n') {
            result.push('\n');
        }
        LayerOutcome::Continue(result)
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
        match DedupLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    #[test]
    fn test_real_repetitive_output_collapsed_with_marker() {
        let event: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/posthook/bash_repetitive_lines.json"
        ))
        .expect("fixture parses");
        let input = event["tool_response"]["stdout"].as_str().unwrap();
        let out = apply(input);
        assert_eq!(
            out.trim_end(),
            "warning: unused import in module_x\n[x40]",
            "40 identical lines → line + explicit count"
        );
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(savings >= 90.0, "expected ≥90%, got {savings:.1}%");
    }

    #[test]
    fn test_below_threshold_untouched() {
        let input = "same\nsame\nother\n";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_blank_runs_exempt() {
        let input = "a\n\n\n\n\nb\n";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_order_and_interleaving_preserved() {
        let input = "x\nx\nx\ny\nx\nx\nx\n";
        assert_eq!(apply(input), "x\n[x3]\ny\nx\n[x3]\n");
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(apply(""), "");
    }
}
