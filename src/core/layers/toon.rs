//! `toon` layer: shape-gated TOON encoding for JSON content.
//!
//! Measured reality: TOON wins (−40/−60%) on uniform arrays of flat objects
//! but loses to minified JSON on nested/non-uniform data. The shape gate is
//! therefore mandatory: gate → encode → compare vs minified → keep the
//! smaller. Parse failure → passthrough.

use super::{Layer, LayerCtx, LayerOutcome};
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
        let Ok(minified) = serde_json::to_string(&value) else {
            return LayerOutcome::Continue(input.to_string());
        };

        if is_uniform_flat_array(&value) {
            if let Ok(toon) = toon_format::encode_default(&value) {
                if toon.len() < minified.len() {
                    return LayerOutcome::Continue(toon);
                }
            }
        }
        LayerOutcome::Continue(minified)
    }
}

/// TOON's winning shape: an array (≥2 items) of objects sharing one flat
/// key set (scalar values only).
fn is_uniform_flat_array(value: &Value) -> bool {
    let Value::Array(items) = value else {
        return false;
    };
    if items.len() < 2 {
        return false;
    }
    let Some(Value::Object(first)) = items.first() else {
        return false;
    };
    let keys: Vec<&String> = first.keys().collect();

    items.iter().all(|item| match item {
        Value::Object(obj) => {
            obj.len() == keys.len()
                && obj.keys().zip(keys.iter()).all(|(a, b)| a == *b)
                && obj.values().all(|v| !v.is_array() && !v.is_object())
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{ContentFormat, LayerCtx};
    use super::*;
    use crate::core::filter::Language;

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
    fn test_nested_json_falls_back_to_minify() {
        let input = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/read_json_nested.json"
        ));
        let out = apply(&input);
        // Nested/non-uniform: TOON loses, minified JSON must win.
        assert!(out.trim_start().starts_with('{'), "expected minified JSON");
        let reparsed: serde_json::Value = serde_json::from_str(&out).expect("still valid JSON");
        let original: serde_json::Value = serde_json::from_str(&input).expect("valid input");
        assert_eq!(reparsed, original, "minify must be lossless");
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(
            savings >= 20.0,
            "nested json minify: expected ≥20% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_non_uniform_array_falls_back_to_minify() {
        let input = r#"[{"a": 1}, {"b": {"nested": true}}]"#;
        let out = apply(input);
        assert_eq!(out, r#"[{"a":1},{"b":{"nested":true}}]"#);
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
