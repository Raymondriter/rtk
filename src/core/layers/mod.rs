//! Agent-agnostic output-compression layer engine.
//!
//! Layers see only content + [`ContentFormat`] — never agents or tool names
//! (that knowledge lives in `src/hooks/posthook.rs`). [`run_chain`] is pure:
//! panic containment (`catch_unwind`) and the `never_worse` size guard are
//! the caller's job, keeping this module panic-policy-free.
//!
//! Layer names (`ansi`, `toon`, `minify-json`, `web-md`, `truncate`,
//! `grep-group`, `base64-elide`, `lockfile`, `md-slim`) are a frozen public
//! vocabulary used in tracking and future config. Reserved for Part 2:
//! `dedup`, `unicode`, `ipynb-strip`, `tree-sitter`.

pub mod ansi;
pub mod base64_elide;
pub mod cap;
pub mod dedup;
pub mod grep_group;
pub mod lockfile;
pub mod md_slim;
pub mod minify_json;
pub mod progress;
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
    /// Package lockfiles (package-lock.json, Cargo.lock, yarn.lock, pnpm-lock.yaml).
    Lockfile,
    /// Raw terminal output (Bash tool, commands not rewritten by RTK).
    Term,
}

/// Context passed to every layer in a chain. `format`/`source` are part of
/// the layer contract even where no Part 1 layer reads them yet.
#[allow(dead_code)]
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
    /// Stop the chain and use this as the final output (e.g. `toon` after a
    /// JSON conversion — later truncation would corrupt the encoded data).
    ShortCircuit(String),
}

pub trait Layer: Sync {
    /// Stable public identifier (tracking + future config vocabulary).
    #[allow(dead_code)]
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
pub static BASE64_ELIDE: base64_elide::Base64ElideLayer = base64_elide::Base64ElideLayer;
pub static TOON: toon::ToonLayer = toon::ToonLayer;
#[allow(dead_code)] // frozen layer name; json chain minifies inside `toon`'s shape gate
pub static MINIFY_JSON: minify_json::MinifyJsonLayer = minify_json::MinifyJsonLayer;
pub static WEB_MD: web_md::WebMdLayer = web_md::WebMdLayer;
pub static MD_SLIM: md_slim::MdSlimLayer = md_slim::MdSlimLayer;
#[allow(dead_code)] // reserved layer: no Part 1 chain truncates (conversion-only)
pub static TRUNCATE: truncate::TruncateLayer = truncate::TruncateLayer;
pub static GREP_GROUP: grep_group::GrepGroupLayer = grep_group::GrepGroupLayer;
pub static LOCKFILE: lockfile::LockfileLayer = lockfile::LockfileLayer;
pub static PROGRESS: progress::ProgressLayer = progress::ProgressLayer;
pub static DEDUP: dedup::DedupLayer = dedup::DedupLayer;
pub static CAP: cap::CapLayer = cap::CapLayer;

// Part 1 chains are conversion-only (maintainer decision): no truncation on
// the posthook path — content is reformatted, never dropped. `truncate`
// stays available as a reserved layer.
static JSON_CHAIN: [&dyn Layer; 2] = [&BASE64_ELIDE, &TOON];
// `toon` after `web-md`: fires only when the response body parses as JSON
// (raw JSON API responses) and short-circuits on conversion, so `md-slim`
// only ever sees markdown/plain text.
static WEB_CHAIN: [&dyn Layer; 5] = [&ANSI, &BASE64_ELIDE, &WEB_MD, &TOON, &MD_SLIM];
// No `ansi`/`base64-elide` here: bytes in a Grep match are genuine file
// content (the host never colors Grep output) — rewriting them would
// falsify the file.
static MATCHES_CHAIN: [&dyn Layer; 1] = [&GREP_GROUP];
static LOCKFILE_CHAIN: [&dyn Layer; 1] = [&LOCKFILE];
// Bash generic floor: objective byte-level transforms only, each lossless or
// explicitly marked and recoverable via the recall file. `cap` is tail-biased
// (bash errors live at the end) — the one deliberately lossy step, requested
// for runaway outputs.
static TERM_CHAIN: [&dyn Layer; 5] = [&ANSI, &PROGRESS, &DEDUP, &BASE64_ELIDE, &CAP];

/// Hardcoded Part 1 chain per content format (RTK-owned; users get on/off
/// per tool + per format + exclude_paths, not chain editing).
pub fn chain_for(format: ContentFormat) -> &'static [&'static dyn Layer] {
    match format {
        ContentFormat::Json => &JSON_CHAIN,
        ContentFormat::Web => &WEB_CHAIN,
        ContentFormat::Matches => &MATCHES_CHAIN,
        ContentFormat::Lockfile => &LOCKFILE_CHAIN,
        ContentFormat::Term => &TERM_CHAIN,
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
        assert_eq!(
            names,
            vec!["ansi", "base64-elide", "web-md", "toon", "md-slim"]
        );

        let names: Vec<&str> = chain_for(ContentFormat::Matches)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(names, vec!["grep-group"]);

        let names: Vec<&str> = chain_for(ContentFormat::Json)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(names, vec!["base64-elide", "toon"]);

        let names: Vec<&str> = chain_for(ContentFormat::Lockfile)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(names, vec!["lockfile"]);

        let names: Vec<&str> = chain_for(ContentFormat::Term)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(
            names,
            vec!["ansi", "progress", "dedup", "base64-elide", "cap"]
        );
    }

    #[test]
    fn test_web_chain_json_conversion_never_line_capped() {
        // Nested JSON → toon falls back to minify → ONE long line. toon must
        // ShortCircuit so truncate's 500-char line cap can't chop the data.
        let ctx = LayerCtx {
            format: ContentFormat::Web,
            lang: Language::Unknown,
            source: "https://api.example.com/big",
        };
        let items: Vec<String> = (0..200)
            .map(|i| format!("{{\"k{i}\": {{\"nested\": \"value_{i}\"}}}}"))
            .collect();
        let input = format!("[\n  {}\n]", items.join(",\n  "));
        let out = run_chain(chain_for(ContentFormat::Web), &input, &ctx);
        assert!(out.len() > 2000, "minified one-liner must survive whole");
        assert!(!out.ends_with("..."), "must not be line-capped");
        serde_json::from_str::<serde_json::Value>(&out).expect("still valid JSON");
    }

    #[test]
    fn test_web_chain_toon_encodes_raw_json_response() {
        // A WebFetch/WebSearch body that is raw JSON (API endpoint): web-md
        // skips it (not HTML), toon picks it up.
        let ctx = LayerCtx {
            format: ContentFormat::Web,
            lang: Language::Unknown,
            source: "https://api.example.com/users",
        };
        let input = r#"[
  {"id": 1, "name": "a"},
  {"id": 2, "name": "b"},
  {"id": 3, "name": "c"}
]"#;
        let out = run_chain(chain_for(ContentFormat::Web), input, &ctx);
        assert!(
            out.starts_with("[3]{id,name}:"),
            "raw JSON over web must TOON-encode, got: {out}"
        );
    }

    #[test]
    fn test_web_chain_markdown_untouched_by_toon() {
        let ctx = LayerCtx {
            format: ContentFormat::Web,
            lang: Language::Unknown,
            source: "https://example.com",
        };
        let input = "# Title\n\nSome *markdown* text.\n";
        assert_eq!(run_chain(chain_for(ContentFormat::Web), input, &ctx), input);
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
