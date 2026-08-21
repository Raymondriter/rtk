//! Single source of determinism: the encode/decode options every lens path
//! (Read view, Edit translation) must share, plus value equality rules.

use serde_json::Value;
use toon_format::{DecodeOptions, EncodeOptions};

/// Which compressed rendering a view is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Toon,
    Minified,
}

fn encode_options() -> EncodeOptions {
    EncodeOptions::default()
}

fn decode_options() -> DecodeOptions {
    DecodeOptions::default()
}

/// Deterministic TOON rendering of a JSON value.
pub fn encode_view(value: &Value) -> Option<String> {
    toon_format::encode(value, &encode_options()).ok()
}

/// Strict TOON decode: `[N]` and field-count mismatches (model syntax slips)
/// are errors, never silently repaired.
pub fn decode_view(view: &str) -> Option<Value> {
    toon_format::decode::<Value>(view, &decode_options()).ok()
}

/// Render a value in the given view kind.
pub fn render(value: &Value, kind: ViewKind) -> Option<String> {
    match kind {
        ViewKind::Toon => encode_view(value),
        ViewKind::Minified => serde_json::to_string(value).ok(),
    }
}

/// Parse a view of the given kind back to a value.
pub fn parse_view(view: &str, kind: ViewKind) -> Option<Value> {
    match kind {
        ViewKind::Toon => decode_view(view),
        ViewKind::Minified => serde_json::from_str(view).ok(),
    }
}

/// Value equality with numeric tolerance: TOON canonicalizes numbers
/// (`1500.0` → `1500`), so numbers compare as f64, everything else exactly.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(fx), Some(fy)) => fx == fy,
            _ => x == y,
        },
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| values_equal(x, y))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .all(|(k, x)| ys.get(k).is_some_and(|y| values_equal(x, y)))
        }
        _ => a == b,
    }
}

/// Smallest admissible rendering of `value`: TOON when it beats minified
/// JSON, minified otherwise, and only if the chosen view round-trips.
/// Single source for every view RTK serves (Read hook, `rtk read`).
pub fn best_view(value: &Value) -> Option<(String, ViewKind)> {
    let minified = render(value, ViewKind::Minified)?;
    let mut candidates: Vec<(String, ViewKind)> = Vec::with_capacity(2);
    if let Some(toon) = render(value, ViewKind::Toon) {
        if toon.len() < minified.len() {
            candidates.push((toon, ViewKind::Toon));
        }
    }
    candidates.push((minified, ViewKind::Minified));
    candidates
        .into_iter()
        .find(|(view, kind)| round_trips(value, view, *kind))
}

/// Stable identifier of a view kind, for tracking rows.
pub fn view_token(kind: ViewKind) -> &'static str {
    match kind {
        ViewKind::Toon => "toon",
        ViewKind::Minified => "json",
    }
}

/// Admissibility check: the view decodes back to the source value.
pub fn round_trips(value: &Value, view: &str, kind: ViewKind) -> bool {
    parse_view(view, kind).is_some_and(|decoded| values_equal(value, &decoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_content(raw: &str) -> Value {
        let event: Value = serde_json::from_str(raw).expect("fixture parses");
        let content = event["tool_response"]["file"]["content"]
            .as_str()
            .expect("content");
        serde_json::from_str(content).expect("json content")
    }

    #[test]
    fn test_flat_fixture_round_trips_toon() {
        let value = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/read_json_flat.json"
        ));
        let view = encode_view(&value).expect("encodes");
        assert!(round_trips(&value, &view, ViewKind::Toon));
    }

    #[test]
    fn test_nested_fixture_round_trips_toon_and_minified() {
        let value = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/read_json_nested.json"
        ));
        let toon = encode_view(&value).expect("encodes");
        assert!(round_trips(&value, &toon, ViewKind::Toon));
        let min = render(&value, ViewKind::Minified).expect("minifies");
        assert!(round_trips(&value, &min, ViewKind::Minified));
    }

    #[test]
    fn test_best_view_prefers_toon_only_when_smaller() {
        let flat = fixture_content(include_str!(
            "../../../tests/fixtures/posthook/read_json_flat.json"
        ));
        let (view, kind) = best_view(&flat).expect("view");
        assert_eq!(kind, ViewKind::Toon);
        assert!(view.starts_with("[120]{id,name,email,active,score}:"));

        let deep: Value = serde_json::from_str(r#"{"a":{"b":{"c":{"d":{"e":[1,2,3]}}}}}"#).unwrap();
        let (view, kind) = best_view(&deep).expect("view");
        assert!(view.len() <= serde_json::to_string(&deep).unwrap().len());
        assert!(round_trips(&deep, &view, kind));
    }

    #[test]
    fn test_values_equal_numeric_tolerance() {
        let a: Value = serde_json::from_str(r#"{"retry_ms": 1500.0, "v": "1.10"}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"retry_ms": 1500, "v": "1.10"}"#).unwrap();
        assert!(values_equal(&a, &b));
        let c: Value = serde_json::from_str(r#"{"retry_ms": 1501, "v": "1.10"}"#).unwrap();
        assert!(!values_equal(&a, &c));
    }

    #[test]
    fn test_strict_decode_rejects_stale_count() {
        let bad = "[2]{id,name}:\n  1,a\n  2,b\n  3,c";
        assert!(decode_view(bad).is_none());
    }
}
