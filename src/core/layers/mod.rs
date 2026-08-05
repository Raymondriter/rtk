//! Agent-agnostic output-compression layer engine.
//!
//! Layers see only content + [`ContentFormat`] — never agents or tool names
//! (that knowledge lives in `src/hooks/posthook.rs`). [`run_chain`] is pure:
//! panic containment (`catch_unwind`) and the `never_worse` size guard are
//! the caller's job, keeping this module panic-policy-free.
//!
//! Layer names (`ansi`, `toon`, `minify-json`, `web-md`, `truncate`,
//! `grep-group`) are a frozen public vocabulary used in tracking and future
//! config. Reserved for Part 2: `dedup`, `unicode`, `ipynb-strip`,
//! `tree-sitter`.

// Wired by the posthook runtime (src/hooks/posthook.rs, next phase).
#![allow(dead_code)]

pub mod ansi;
pub mod grep_group;
pub mod minify_json;
pub mod toon;
pub mod truncate;
pub mod web_md;

use crate::core::filter::Language;

/// Content format of a tool output, resolved by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentFormat {
    /// JSON documents (e.g. Read of a `.json` file).
    Json,
    /// Web content (WebFetch/WebSearch responses).
    Web,
    /// Grep-style match lines (`path:line:content`); internal format.
    Matches,
}

/// Context passed to every layer in a chain.
pub struct LayerCtx<'a> {
    pub format: ContentFormat,
    /// Source language, for future code-aware layers.
    pub lang: Language,
    /// Path or URL the content came from — diagnostics only.
    pub source: &'a str,
}

pub enum LayerOutcome {
    /// Pass the (possibly transformed) content to the next layer.
    Continue(String),
    /// Stop the chain and use this as the final output.
    ShortCircuit(String),
}

pub trait Layer: Sync {
    /// Stable public identifier.
    fn name(&self) -> &'static str;
    fn apply(&self, input: &str, ctx: &LayerCtx) -> LayerOutcome;
}

/// Run `layers` over `input` in order. Pure: no I/O, no panic handling.
pub fn run_chain(layers: &[&dyn Layer], input: &str, ctx: &LayerCtx) -> String {
    let mut current = input.to_string();
    for layer in layers {
        match layer.apply(&current, ctx) {
            LayerOutcome::Continue(next) => current = next,
            LayerOutcome::ShortCircuit(out) => return out,
        }
    }
    current
}

pub static ANSI: ansi::AnsiLayer = ansi::AnsiLayer;
pub static TOON: toon::ToonLayer = toon::ToonLayer;
pub static MINIFY_JSON: minify_json::MinifyJsonLayer = minify_json::MinifyJsonLayer;
pub static WEB_MD: web_md::WebMdLayer = web_md::WebMdLayer;
pub static TRUNCATE: truncate::TruncateLayer = truncate::TruncateLayer;
pub static GREP_GROUP: grep_group::GrepGroupLayer = grep_group::GrepGroupLayer;

static JSON_CHAIN: [&dyn Layer; 1] = [&TOON];
static WEB_CHAIN: [&dyn Layer; 3] = [&ANSI, &WEB_MD, &TRUNCATE];
static MATCHES_CHAIN: [&dyn Layer; 3] = [&ANSI, &GREP_GROUP, &TRUNCATE];

/// Hardcoded Part 1 chain per content format (RTK-owned; users get on/off
/// per tool + per format + exclude_paths, not chain editing).
pub fn chain_for(format: ContentFormat) -> &'static [&'static dyn Layer] {
    match format {
        ContentFormat::Json => &JSON_CHAIN,
        ContentFormat::Web => &WEB_CHAIN,
        ContentFormat::Matches => &MATCHES_CHAIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chain_layer_names_are_frozen_vocabulary() {
        let names: Vec<&str> = chain_for(ContentFormat::Web)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(names, vec!["ansi", "web-md", "truncate"]);

        let names: Vec<&str> = chain_for(ContentFormat::Matches)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(names, vec!["ansi", "grep-group", "truncate"]);

        let names: Vec<&str> = chain_for(ContentFormat::Json)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(names, vec!["toon"]);
    }

    #[test]
    fn test_run_chain_applies_layers_in_order() {
        let ctx = LayerCtx {
            format: ContentFormat::Web,
            lang: Language::Unknown,
            source: "test",
        };
        let input = "\x1b[31mhello\x1b[0m";
        let out = run_chain(&[&ANSI], input, &ctx);
        assert_eq!(out, "hello");
    }

    #[test]
    fn test_run_chain_empty_input() {
        let ctx = LayerCtx {
            format: ContentFormat::Json,
            lang: Language::Data,
            source: "test.json",
        };
        let out = run_chain(chain_for(ContentFormat::Json), "", &ctx);
        assert_eq!(out, "");
    }
}
