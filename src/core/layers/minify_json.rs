//! `minify-json` layer: lossless JSON compaction via serde_json re-serialization.
//!
//! Not the lossy summarizers from `json_cmd` — those may become named
//! converters in Part 2.

use super::{Layer, LayerCtx, LayerOutcome};

/// Parse and re-serialize compactly. Returns `None` when `input` is not JSON.
pub(super) fn minify(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    serde_json::to_string(&value).ok()
}

pub struct MinifyJsonLayer;

impl Layer for MinifyJsonLayer {
    fn name(&self) -> &'static str {
        "minify-json"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        match minify(input) {
            Some(minified) => LayerOutcome::Continue(minified),
            None => LayerOutcome::Continue(input.to_string()),
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
            format: ContentFormat::Json,
            lang: Language::Data,
            source: "test.json",
        }
    }

    #[test]
    fn test_minifies_pretty_json() {
        let input = "{\n  \"a\": 1,\n  \"b\": [\n    2,\n    3\n  ]\n}";
        let LayerOutcome::Continue(out) = MinifyJsonLayer.apply(input, &ctx()) else {
            panic!("must continue");
        };
        assert_eq!(out, r#"{"a":1,"b":[2,3]}"#);
    }

    #[test]
    fn test_invalid_json_passes_through() {
        let LayerOutcome::Continue(out) = MinifyJsonLayer.apply("not json {", &ctx()) else {
            panic!("must continue");
        };
        assert_eq!(out, "not json {");
    }
}
