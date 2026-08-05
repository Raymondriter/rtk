//! `progress` layer: collapses CR-overwrite progress frames.
//!
//! A terminal renders only the final frame of a `\r`-rewritten line; earlier
//! frames are definitionally transient. Per line, the last non-empty CR
//! segment is kept (a bare trailing `\r` erases nothing in a real terminal,
//! so an empty final segment falls back to the previous one).

use super::{Layer, LayerCtx, LayerOutcome};

pub struct ProgressLayer;

impl Layer for ProgressLayer {
    fn name(&self) -> &'static str {
        "progress"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        if !input.contains('\r') {
            return LayerOutcome::Continue(input.to_string());
        }
        let out = input
            .lines()
            .map(|line| {
                if line.contains('\r') {
                    line.rsplit('\r').find(|seg| !seg.is_empty()).unwrap_or("")
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let out = if input.ends_with('\n') {
            out + "\n"
        } else {
            out
        };
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
        match ProgressLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    fn fixture_stdout(raw: &str) -> String {
        let event: serde_json::Value = serde_json::from_str(raw).expect("fixture parses");
        event["tool_response"]["stdout"]
            .as_str()
            .expect("fixture has stdout")
            .to_string()
    }

    #[test]
    fn test_real_progress_frames_collapse_to_final() {
        let input = fixture_stdout(include_str!(
            "../../../tests/fixtures/posthook/bash_progress_cr.json"
        ));
        let out = apply(&input);
        assert!(
            out.contains("Downloading 100% done"),
            "final frame kept: {out}"
        );
        assert!(!out.contains("Downloading 0%"), "intermediate frames gone");
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(
            savings >= 90.0,
            "expected ≥90% on frames, got {savings:.1}%"
        );
    }

    #[test]
    fn test_trailing_cr_keeps_visible_frame() {
        assert_eq!(apply("Working 99%\r"), "Working 99%");
    }

    #[test]
    fn test_no_cr_untouched() {
        let input = "plain line\nanother\n";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_crlf_normalized_to_lf() {
        let input = "line one\r\nline two\r\n";
        assert_eq!(apply(input), "line one\nline two\n");
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(apply(""), "");
    }
}
