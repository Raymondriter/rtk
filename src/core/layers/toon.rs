//! `toon` layer: lens-governed compressed view of JSON content.
//!
//! Renders TOON and minified JSON with the shared lens options, keeps the
//! smaller one, and serves it only if it round-trips back to the source
//! value (the lens admissibility law — every view shown must be one the
//! lens can translate edits against). Parse failure → passthrough.

use super::{Layer, LayerCtx, LayerOutcome};
use crate::core::lens::options::best_view;
use serde_json::Value;

pub struct ToonLayer;

impl Layer for ToonLayer {
    fn name(&self) -> &'static str {
        "toon"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        let Ok(value) = serde_json::from_str::<Value>(input) else {
            return LayerOutcome::Continue(input.to_string());
        };
        match best_view(&value) {
            // ShortCircuit: encoded output is data the model may need whole —
            // nothing downstream may reshape it.
            Some((view, _)) => LayerOutcome::ShortCircuit(view),
            None => LayerOutcome::Continue(input.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ContentFormat, LayerCtx};
    use super::*;
    use crate::core::filter::Language;
    use crate::core::lens::options::{parse_view, values_equal};

    fn ctx() -> LayerCtx<'static> {
        LayerCtx {
            format: ContentFormat::Json,
            lang: Language::Data,
            source: "test.json",
        }
    }

    fn apply(input: &str) -> String {
        match ToonLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    fn fixture_content(raw: &str) -> String {
        let event: serde_json::Value = serde_json::from_str(raw).expect("fixture parses");
        event["tool_response"]["file"]["content"]
            .as_str()
            .expect("fixture has file content")
            .to_string()
    }

    #[test]
    fn test_flat_uniform_array_encodes_to_toon() {
        let input = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/read_json_flat.json"
        ));
        let out = apply(&input);
        assert!(
            out.starts_with("[120]{id,name,email,active,score}:"),
            "expected TOON tabular header, got: {}",
            &out[..out.len().min(80)]
        );
    }

    #[test]
    fn test_flat_uniform_savings_at_least_20_percent() {
        let input = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/read_json_flat.json"
        ));
        let out = apply(&input);
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(
            savings >= 20.0,
            "json chain: expected ≥20% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_nested_json_view_is_smallest_admissible() {
        let input = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/read_json_nested.json"
        ));
        let value: Value = serde_json::from_str(&input).unwrap();
        let out = apply(&input);
        let (expected, kind) = best_view(&value).expect("admissible view");
        assert_eq!(out, expected);
        let decoded = parse_view(&out, kind).expect("view decodes");
        assert!(values_equal(&value, &decoded), "lossless");
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(savings >= 20.0, "expected ≥20% savings, got {savings:.1}%");
    }

    #[test]
    fn test_view_never_larger_than_minified() {
        let input = r#"[{"a": 1}, {"b": {"nested": true}}]"#;
        let out = apply(input);
        let value: Value = serde_json::from_str(input).unwrap();
        let minified = serde_json::to_string(&value).unwrap();
        assert!(out.len() <= minified.len());
    }

    #[test]
    fn test_invalid_json_passes_through() {
        assert_eq!(apply("not json at all"), "not json at all");
    }

    #[test]
    fn test_empty_input_passes_through() {
        assert_eq!(apply(""), "");
    }
}
