//! `web-md` layer: HTML → Markdown conversion via `htmd`.
//!
//! Skipped when content is already markdown-ish (Claude Code WebFetch
//! responses are usually pre-processed markdown summaries): HTML tag
//! density sniff on the first 1 KB decides.

use super::{Layer, LayerCtx, LayerOutcome};
use regex::Regex;
use std::sync::LazyLock;

static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</?[a-zA-Z][a-zA-Z0-9-]*(\s|>|/>)").unwrap());

/// Minimum opening/closing tags in the first 1 KB to call it HTML.
const TAG_DENSITY_THRESHOLD: usize = 4;

fn looks_like_html(content: &str) -> bool {
    let mut end = content.len().min(1024);
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let head = &content[..end];

    let lower = head.to_lowercase();
    if lower.contains("<!doctype html") || lower.contains("<html") {
        return true;
    }
    TAG_RE.find_iter(head).take(TAG_DENSITY_THRESHOLD).count() >= TAG_DENSITY_THRESHOLD
}

pub struct WebMdLayer;

impl Layer for WebMdLayer {
    fn name(&self) -> &'static str {
        "web-md"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        if !looks_like_html(input) {
            return LayerOutcome::Continue(input.to_string());
        }
        match htmd::convert(input) {
            Ok(md) => LayerOutcome::Continue(md),
            Err(_) => LayerOutcome::Continue(input.to_string()),
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
            source: "https://example.com",
        }
    }

    fn apply(input: &str) -> String {
        match WebMdLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    #[test]
    fn test_real_html_page_converts_to_markdown() {
        let input = include_str!("../../../tests/fixtures/posthook/webpage_example_raw.html");
        let out = apply(input);
        assert!(!out.contains("<div"), "tags must be gone");
        assert!(!out.contains("<style"), "style must be gone");
        assert!(out.contains("Example Domain"), "text content preserved");
    }

    #[test]
    fn test_real_html_savings_at_least_20_percent() {
        let input = include_str!("../../../tests/fixtures/posthook/webpage_example_raw.html");
        let out = apply(input);
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(
            savings >= 20.0,
            "web-md: expected ≥20% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_already_markdown_is_skipped() {
        // Real WebFetch result content (AI-processed markdown summary).
        let event: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/posthook/webfetch_markdown_url.json"
        ))
        .expect("fixture parses");
        let input = event["tool_response"]["result"]
            .as_str()
            .expect("fixture has result");
        let out = apply(input);
        assert_eq!(out, input, "markdown input must pass through untouched");
    }

    #[test]
    fn test_markdown_with_occasional_inline_tag_still_skipped() {
        let input = "# Title\n\nSome *markdown* with one <br> tag.\n\n- a\n- b\n";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(apply(""), "");
    }
}
