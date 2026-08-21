//! PostToolUse output filtering for Claude Code.
//!
//! The tool (Read/Grep/Glob/WebFetch/WebSearch) runs raw; Claude Code sends
//! the event JSON on stdin; this module classifies the output's content
//! format, applies a `core::layers` chain, and returns
//! `hookSpecificOutput.updatedToolOutput`, which replaces what the model
//! reads. Everything that knows the words "claude", "Read", "tool_response"
//! lives here — layers stay agent-agnostic.
//!
//! Protocol invariants:
//! - **Fail-open everywhere**: any error emits nothing and exits 0; Claude
//!   Code keeps the original output. The host also silently ignores
//!   schema-mismatched replacements (verified empirically, Phase 0).
//! - The only stdout write is the final serialized `hookSpecificOutput`.
//! - Never emit an unchanged or larger output — emit nothing instead.
//! - One `Config::load()` per invocation.

use super::constants::POST_TOOL_USE_KEY;
use crate::core::config::{Config, PosthookConfig};
use crate::core::filter::Language;
use crate::core::layers::{self, ContentFormat, LayerCtx};
use crate::core::lens::mirror as lens_mirror;
use crate::core::tee;
use crate::core::tracking::{estimate_tokens, Tracker};
use crate::core::utils::glob_match;
use regex::Regex;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::sync::LazyLock;

/// Raw content below this is not worth the write/track overhead.
const MIN_POSTHOOK_SIZE: usize = 512;

/// Second converted Read of the same path inside this window is served raw:
/// the model's natural "read it again" always reaches ground truth.
const LENS_REREAD_WINDOW_SECS: u64 = 120;

/// Entry point: never panics, never errors, emits at most one stdout line.
pub fn run(event: &Value) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(event)));
    if let Ok(Some(output)) = outcome {
        let _ = writeln!(io::stdout(), "{output}");
    }
}

/// Full pipeline behind the fail-open envelope. `None` = emit nothing.
fn process(event: &Value) -> Option<String> {
    // Kill switches, cheapest first.
    if std::env::var("RTK_DISABLED").ok().as_deref() == Some("1") {
        return None;
    }
    if std::env::var("RTK_POSTHOOK").ok().as_deref() == Some("0") {
        return None;
    }

    let config = Config::load().unwrap_or_default();
    process_with_config(event, &config)
}

/// Testable core: gates → extract → classify → chain → guard → re-wrap.
fn process_with_config(event: &Value, config: &Config) -> Option<String> {
    if !config.posthook.enabled {
        return None;
    }

    let tool_name = event.get("tool_name")?.as_str()?;
    if !tool_enabled(&config.posthook, tool_name) {
        return None;
    }

    let source = source_of(event, tool_name);
    if config
        .posthook
        .exclude_paths
        .iter()
        .any(|p| glob_match(p, &source))
    {
        return None;
    }

    let duration_ms = event
        .get("duration_ms")
        .and_then(|d| d.as_u64())
        .unwrap_or(0);

    if tool_name == "WebSearch" {
        return process_websearch(event, config, &source, duration_ms);
    }
    if tool_name == "Bash" {
        return process_bash(event, config, duration_ms);
    }
    if matches!(tool_name, "Edit" | "Write") {
        return process_mirror_write(event);
    }

    let (raw, format) = extract(event, tool_name, &source)?;
    if raw.len() < MIN_POSTHOOK_SIZE {
        return None;
    }

    let Some(format) = format else {
        // Unsupported in Part 1 (e.g. Read of .rs): passthrough, but tracked
        // so coverage stays visible in `rtk gain`.
        track(tool_name, "raw", &source, &raw, &raw, duration_ms);
        return None;
    };

    match format_setting(&config.posthook, format) {
        FormatSetting::Off => return None,
        FormatSetting::Auto => {}
    }

    let lens_view = tool_name == "Read" && format == ContentFormat::Json;
    if lens_view && tee::posthook_recently_read(config, &source, LENS_REREAD_WINDOW_SECS) {
        track(
            tool_name,
            format_token(format),
            &source,
            &raw,
            &raw,
            duration_ms,
        );
        return None;
    }

    let composed = filter_and_compose(config, tool_name, &source, &raw, format, duration_ms)?;

    let updated = rebuild(event, tool_name, &composed)?;
    let hook_output = json!({
        "hookEventName": POST_TOOL_USE_KEY,
        "updatedToolOutput": updated,
    });
    if lens_view {
        tee::posthook_mark_read(config, &source);
    }

    track(
        tool_name,
        format_token(format),
        &source,
        &raw,
        &composed,
        duration_ms,
    );
    Some(json!({ "hookSpecificOutput": hook_output }).to_string())
}

/// Chain + recall store + guard for a single content string.
/// `None` = chain was a no-op or not smaller (tracked as passthrough).
fn filter_and_compose(
    config: &Config,
    tool_name: &str,
    source: &str,
    raw: &str,
    format: ContentFormat,
    duration_ms: u64,
) -> Option<String> {
    let lang = Language::from_extension(extension_of(source));
    let ctx = LayerCtx {
        format,
        lang,
        source,
    };
    let filtered = layers::run_chain(layers::chain_for(format), raw, &ctx);

    if filtered == raw {
        // No-op chain: skip the recall store entirely.
        track(
            tool_name,
            format_token(format),
            source,
            raw,
            raw,
            duration_ms,
        );
        return None;
    }

    // A lens view is lossless: nothing lives "elsewhere", so announcing a
    // recall file would be both untrue and a tell that breaks the seamless
    // view. Lossy chains keep the hint. Recovery for views is the re-read
    // rail, which needs no words. The recall file is still written.
    let slug = format!("{}_{}", tool_name.to_lowercase(), basename_of(source));
    let hint = tee::posthook_tee_hint(config, raw, &slug);
    // Ordering invariant (emit_guarded pattern): compose filtered + hint
    // FIRST, then guard — the hint can never push output above raw.
    let composed = match hint.filter(|_| !is_lens_format(format)) {
        Some(h) => format!("{filtered}\n{h}"),
        None => filtered,
    };

    if estimate_tokens(&composed) > estimate_tokens(raw) {
        track(
            tool_name,
            format_token(format),
            source,
            raw,
            raw,
            duration_ms,
        );
        return None;
    }
    Some(composed)
}

/// WebSearch responses carry text as string elements inside a mixed
/// `results` array (strings + link objects). Each string element runs the
/// web chain; the array is rebuilt in place with link objects untouched.
fn process_websearch(
    event: &Value,
    config: &Config,
    source: &str,
    duration_ms: u64,
) -> Option<String> {
    let results = event.pointer("/tool_response/results")?.as_array()?;

    let raw_concat: String = results
        .iter()
        .filter_map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if raw_concat.len() < MIN_POSTHOOK_SIZE {
        return None;
    }
    match format_setting(&config.posthook, ContentFormat::Web) {
        FormatSetting::Off => return None,
        FormatSetting::Auto => {}
    }

    let ctx = LayerCtx {
        format: ContentFormat::Web,
        lang: Language::Unknown,
        source,
    };
    let chain = layers::chain_for(ContentFormat::Web);
    let mut changed = false;
    let mut new_results: Vec<Value> = Vec::with_capacity(results.len());
    let mut filtered_concat = String::new();
    for r in results {
        match r.as_str() {
            Some(text) => {
                let filtered = layers::run_chain(chain, text, &ctx);
                if filtered != text {
                    changed = true;
                }
                if !filtered_concat.is_empty() {
                    filtered_concat.push_str("\n\n");
                }
                filtered_concat.push_str(&filtered);
                new_results.push(Value::String(filtered));
            }
            None => new_results.push(r.clone()),
        }
    }

    if !changed {
        track(
            "WebSearch",
            "web",
            source,
            &raw_concat,
            &raw_concat,
            duration_ms,
        );
        return None;
    }

    // Compose-then-guard: hint rides in the last string element.
    let hint = tee::posthook_tee_hint(config, &raw_concat, "websearch_results");
    if let (Some(h), Some(last)) = (
        hint,
        new_results.iter_mut().rev().find_map(|v| match v {
            Value::String(s) => Some(s),
            _ => None,
        }),
    ) {
        last.push('\n');
        last.push_str(&h);
        filtered_concat.push('\n');
        filtered_concat.push_str(last);
    }

    if estimate_tokens(&filtered_concat) > estimate_tokens(&raw_concat) {
        track(
            "WebSearch",
            "web",
            source,
            &raw_concat,
            &raw_concat,
            duration_ms,
        );
        return None;
    }

    let mut updated = event.get("tool_response")?.clone();
    updated["results"] = Value::Array(new_results);

    track(
        "WebSearch",
        "web",
        source,
        &raw_concat,
        &filtered_concat,
        duration_ms,
    );
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": POST_TOOL_USE_KEY,
                "updatedToolOutput": updated,
            }
        })
        .to_string(),
    )
}

/// A `.toon` mirror was just edited: regenerate its JSON source. On failure
/// the source is left untouched and the decode error goes back to the agent,
/// which is the only channel that tells it the edit did not land.
fn process_mirror_write(event: &Value) -> Option<String> {
    let path = event.pointer("/tool_input/file_path")?.as_str()?;
    let mirror = std::path::Path::new(path);
    if mirror.extension().and_then(|e| e.to_str()) != Some(lens_mirror::MIRROR_EXT) {
        return None;
    }
    match lens_mirror::compile(mirror) {
        Ok(_) => None,
        Err(e) => Some(
            json!({
                "hookSpecificOutput": {
                    "hookEventName": POST_TOOL_USE_KEY,
                    "additionalContext": format!(
                        "The TOON mirror {} did not compile, so {} was NOT updated: {e:#}",
                        mirror.display(),
                        lens_mirror::source_path(mirror).display()
                    ),
                }
            })
            .to_string(),
        ),
    }
}

fn tool_enabled(config: &PosthookConfig, tool_name: &str) -> bool {
    match tool_name {
        "Read" => config.tools.read,
        "Grep" => config.tools.grep,
        "Glob" => config.tools.glob,
        "WebFetch" => config.tools.webfetch,
        "WebSearch" => config.tools.websearch,
        "Bash" => config.tools.bash,
        // Mirror compilation is not a view filter: it must run whenever a
        // `.toon` file is written, independent of the view toggles.
        "Edit" | "Write" => true,
        _ => false,
    }
}

/// `rtk` invoked anywhere in the command (start or after a shell operator):
/// output already went through RTK's own filters — never double-process.
/// False positives (e.g. `echo rtk hook`) just skip filtering, safe direction.
static RTK_IN_COMMAND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[|&;(\s])rtk\s").expect("valid regex"));

/// Kill-switch prefixes need lexical detection: `RTK_POSTHOOK=0 cmd` puts the
/// variable in the CHILD's environment — the hook process never inherits it
/// (live-validated). Same approach as the rewrite registry's lexical
/// `RTK_DISABLED` handling. Only leading VAR=VAL assignments are scanned.
fn command_disables_posthook(command: &str) -> bool {
    for token in command.split_whitespace() {
        match token {
            "RTK_POSTHOOK=0" | "RTK_DISABLED=1" => return true,
            t if t.contains('=') && !t.starts_with('=') => continue,
            _ => return false,
        }
    }
    false
}

/// First token after any leading VAR=VAL assignments — the actual program
/// name, for slugs and tracking rows.
fn first_program_word(command: &str) -> &str {
    command
        .split_whitespace()
        .find(|t| !t.contains('=') || t.starts_with('='))
        .unwrap_or("")
}

/// Generic floor for Bash commands not rewritten by RTK: objective
/// byte-level transforms only (ansi, progress, dedup, base64-elide, cap).
fn process_bash(event: &Value, config: &Config, duration_ms: u64) -> Option<String> {
    let response = event.get("tool_response")?;
    if response
        .get("interrupted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || response
            .get("isImage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }

    let command = event
        .pointer("/tool_input/command")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if RTK_IN_COMMAND.is_match(command) {
        return None;
    }
    if command_disables_posthook(command) {
        return None;
    }
    // Recall-recovery gate: a command reading the recall store must see the
    // raw bytes (else e.g. an elided base64 blob could never be recovered).
    if command.contains("tee/posthook")
        || tee::posthook_recall_dir(config)
            .is_some_and(|d| command.contains(&d.display().to_string()))
    {
        return None;
    }

    match format_setting(&config.posthook, ContentFormat::Term) {
        FormatSetting::Off => return None,
        FormatSetting::Auto => {}
    }

    let stdout = response
        .get("stdout")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let stderr = response
        .get("stderr")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if stdout.len() + stderr.len() < MIN_POSTHOOK_SIZE {
        return None;
    }

    let first_word = first_program_word(command);
    let ctx = LayerCtx {
        format: ContentFormat::Term,
        lang: Language::Unknown,
        source: command,
    };
    let chain = layers::chain_for(ContentFormat::Term);
    let filtered_stdout = layers::run_chain(chain, stdout, &ctx);
    let filtered_stderr = if stderr.is_empty() {
        String::new()
    } else {
        layers::run_chain(chain, stderr, &ctx)
    };

    let raw_combined = if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{stdout}\n--- stderr ---\n{stderr}")
    };

    if filtered_stdout == stdout && filtered_stderr == stderr {
        track(
            "Bash",
            "term",
            first_word,
            &raw_combined,
            &raw_combined,
            duration_ms,
        );
        return None;
    }

    let slug = format!("bash_{first_word}");
    let hint = tee::posthook_tee_hint(config, &raw_combined, &slug);
    let composed_stdout = match hint {
        Some(h) => format!("{filtered_stdout}\n{h}"),
        None => filtered_stdout,
    };

    let emitted_combined = format!("{composed_stdout}{filtered_stderr}");
    if estimate_tokens(&emitted_combined) > estimate_tokens(&raw_combined) {
        track(
            "Bash",
            "term",
            first_word,
            &raw_combined,
            &raw_combined,
            duration_ms,
        );
        return None;
    }

    let mut updated = response.clone();
    let obj = updated.as_object_mut()?;
    obj.insert("stdout".into(), json!(composed_stdout));
    obj.insert("stderr".into(), json!(filtered_stderr));

    track(
        "Bash",
        "term",
        first_word,
        &raw_combined,
        &emitted_combined,
        duration_ms,
    );
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": POST_TOOL_USE_KEY,
                "updatedToolOutput": updated,
            }
        })
        .to_string(),
    )
}

/// Path or URL the exclude globs and tracking rows key on.
fn source_of(event: &Value, tool_name: &str) -> String {
    let pointer = match tool_name {
        "Read" | "Grep" | "Glob" => "/tool_input/file_path",
        "WebFetch" => "/tool_input/url",
        _ => return String::new(),
    };
    event
        .pointer(pointer)
        .or_else(|| event.pointer("/tool_input/path"))
        .and_then(|p| p.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Content string the model would read + its Part 1 format
/// (`None` format = unsupported → tracked passthrough).
fn extract(
    event: &Value,
    tool_name: &str,
    source: &str,
) -> Option<(String, Option<ContentFormat>)> {
    match tool_name {
        "Read" => {
            let content = event.pointer("/tool_response/file/content")?.as_str()?;
            let file = event.pointer("/tool_response/file")?;
            // Partial reads (offset/limit, or host-side token-cap truncation)
            // must never be converted: a lockfile summary of a partial file
            // would present wrong counts as truth (live-validated), and raw
            // ranged reads are the documented path for building Edit anchors.
            let start_line = file.get("startLine").and_then(|v| v.as_u64()).unwrap_or(1);
            let num_lines = file.get("numLines").and_then(|v| v.as_u64()).unwrap_or(0);
            let total_lines = file
                .get("totalLines")
                .and_then(|v| v.as_u64())
                .unwrap_or(num_lines);
            let partial = start_line > 1 || num_lines < total_lines;

            // Lockfile check first: package-lock.json would otherwise
            // classify as plain json by extension.
            let format = if partial {
                None
            } else if is_lockfile_name(&basename_of(source)) {
                Some(ContentFormat::Lockfile)
            } else if extension_of(source) == "json" {
                Some(ContentFormat::Json)
            } else {
                None
            };
            Some((content.to_string(), format))
        }
        "Grep" => {
            let response = event.get("tool_response")?;
            let mode = response.get("mode").and_then(|m| m.as_str())?;
            let content = response.get("content").and_then(|c| c.as_str());
            match (mode, content) {
                ("content", Some(text)) => Some((text.to_string(), Some(ContentFormat::Matches))),
                // count / files_with_matches are already compact.
                (_, Some(text)) => Some((text.to_string(), None)),
                _ => None,
            }
        }
        "WebFetch" => {
            let result = event.pointer("/tool_response/result")?.as_str()?;
            Some((result.to_string(), Some(ContentFormat::Web)))
        }
        // Glob output (filenames array) has no Part 1 format.
        "Glob" => {
            let filenames = event.pointer("/tool_response/filenames")?.as_array()?;
            let joined = filenames
                .iter()
                .filter_map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            Some((joined, None))
        }
        _ => None,
    }
}

enum FormatSetting {
    Auto,
    Off,
}

/// Resolve the per-format converter setting. `matches` is an internal format,
/// not user-configurable. Unknown strings degrade to "auto" with one stderr
/// warning (fail-open).
fn format_setting(config: &PosthookConfig, format: ContentFormat) -> FormatSetting {
    let value = match format {
        ContentFormat::Json => config.formats.json.as_str(),
        ContentFormat::Web => config.formats.web.as_str(),
        ContentFormat::Lockfile => config.formats.lockfile.as_str(),
        ContentFormat::Term => config.formats.term.as_str(),
        ContentFormat::Matches => return FormatSetting::Auto,
    };
    match value {
        "off" => FormatSetting::Off,
        "auto" => FormatSetting::Auto,
        other => {
            let _ = writeln!(
                io::stderr(),
                "[rtk posthook] unknown format value {other:?}, treating as \"auto\""
            );
            FormatSetting::Auto
        }
    }
}

/// Formats whose view is a lossless rendering of the source (the lens).
fn is_lens_format(format: ContentFormat) -> bool {
    matches!(format, ContentFormat::Json)
}

fn format_token(format: ContentFormat) -> &'static str {
    match format {
        ContentFormat::Json => "json",
        ContentFormat::Web => "web",
        ContentFormat::Matches => "matches",
        ContentFormat::Lockfile => "lockfile",
        ContentFormat::Term => "term",
    }
}

/// Lockfile basenames served by the `lockfile` format (type is then
/// content-sniffed inside the layer).
fn is_lockfile_name(basename: &str) -> bool {
    matches!(
        basename,
        "package-lock.json" | "Cargo.lock" | "yarn.lock" | "pnpm-lock.yaml"
    )
}

/// Re-wrap the filtered text in the same output shape the tool produced.
/// `None` when the shape can't be reproduced confidently → emit nothing.
fn rebuild(event: &Value, tool_name: &str, composed: &str) -> Option<Value> {
    let mut response = event.get("tool_response")?.clone();
    match tool_name {
        "Read" => {
            let file = response.get_mut("file")?.as_object_mut()?;
            file.insert("content".into(), json!(composed));
            // Host renders line numbers from raw content (Phase 0): keep
            // metadata coherent with the replacement.
            let lines = composed.lines().count();
            file.insert("numLines".into(), json!(lines));
            file.insert("totalLines".into(), json!(lines));
            Some(response)
        }
        "Grep" => {
            let obj = response.as_object_mut()?;
            obj.insert("content".into(), json!(composed));
            obj.insert("numLines".into(), json!(composed.lines().count()));
            Some(response)
        }
        "WebFetch" => {
            let obj = response.as_object_mut()?;
            obj.insert("result".into(), json!(composed));
            Some(response)
        }
        _ => None,
    }
}

fn extension_of(source: &str) -> &str {
    std::path::Path::new(source)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}

fn basename_of(source: &str) -> String {
    if let Some(host) = source
        .strip_prefix("https://")
        .or_else(|| source.strip_prefix("http://"))
    {
        return host.split('/').next().unwrap_or(host).to_string();
    }
    std::path::Path::new(source)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("output")
        .to_string()
}

/// Record a tracking row without ever crashing (mirrors
/// `record_parse_failure_silent`). Every posthook code path tracks, including
/// passthroughs — otherwise gains silently fail (pipe_cmd precedent).
fn track(
    tool_name: &str,
    format_token: &str,
    source: &str,
    raw: &str,
    emitted: &str,
    duration_ms: u64,
) {
    let rtk_cmd = format!("rtk posthook {} {}", tool_name.to_lowercase(), format_token);
    let original_cmd = if source.is_empty() {
        tool_name.to_string()
    } else {
        format!("{tool_name} {source}")
    };
    if let Ok(tracker) = Tracker::new() {
        let _ = tracker.record(
            &original_cmd,
            &rtk_cmd,
            estimate_tokens(raw),
            estimate_tokens(emitted),
            duration_ms,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        let mut config = Config::default();
        // Tests never touch the real tee dir or write recall files.
        config.tee.enabled = false;
        config
    }

    fn fixture(name: &str) -> Value {
        let raw = match name {
            "read_flat" => include_str!("../../tests/fixtures/posthook/read_json_flat.json"),
            "read_nested" => include_str!("../../tests/fixtures/posthook/read_json_nested.json"),
            "grep_small" => include_str!("../../tests/fixtures/posthook/grep_content_small.json"),
            "grep_large" => {
                include_str!("../../tests/fixtures/posthook/grep_content_large_truncated.json")
            }
            "grep_count" => include_str!("../../tests/fixtures/posthook/grep_count.json"),
            "webfetch_md" => {
                include_str!("../../tests/fixtures/posthook/webfetch_markdown_url.json")
            }
            "websearch" => include_str!("../../tests/fixtures/posthook/websearch_results.json"),
            "glob" => include_str!("../../tests/fixtures/posthook/glob_json.json"),
            _ => panic!("unknown fixture {name}"),
        };
        serde_json::from_str(raw).expect("fixture parses")
    }

    fn run_fixture(name: &str) -> Option<Value> {
        let output = process_with_config(&fixture(name), &test_config())?;
        Some(serde_json::from_str(&output).expect("emitted output is valid JSON"))
    }

    #[test]
    fn test_read_json_flat_rewrites_to_toon() {
        let out = run_fixture("read_flat").expect("flat json must be rewritten");
        let hook = &out["hookSpecificOutput"];
        assert_eq!(hook["hookEventName"], "PostToolUse");

        let file = &hook["updatedToolOutput"]["file"];
        let content = file["content"].as_str().unwrap();
        assert!(
            content.starts_with("[120]{id,name,email,active,score}:"),
            "TOON content"
        );
        // Shape round-trip: metadata fields survive, counts match replacement.
        assert!(file["filePath"]
            .as_str()
            .unwrap()
            .ends_with("data_flat.json"));
        assert_eq!(
            file["numLines"].as_u64().unwrap() as usize,
            content.lines().count()
        );
        assert_eq!(hook["updatedToolOutput"]["type"], "text");
        assert!(
            hook.get("additionalContext").is_none(),
            "lens system: no per-event note"
        );
    }

    #[test]
    fn test_read_nested_below_min_size_passes_through() {
        // Nested fixture content is ~1KB... actually 822 bytes: above 512.
        // It converts to minified JSON, so it must be rewritten.
        let out = run_fixture("read_nested");
        if let Some(out) = out {
            let content = out["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
                .as_str()
                .unwrap();
            assert!(content.starts_with('{'), "minified JSON expected");
        }
    }

    #[test]
    fn test_grep_content_groups_by_file() {
        let out = run_fixture("grep_large").expect("large grep content must be rewritten");
        let hook = &out["hookSpecificOutput"];
        let response = &hook["updatedToolOutput"];
        let content = response["content"].as_str().unwrap();
        assert!(content.contains("matches in"), "grouped header");
        // Metadata preserved / updated coherently.
        assert_eq!(response["mode"], "content");
        assert_eq!(
            response["numLines"].as_u64().unwrap() as usize,
            content.lines().count()
        );
        // No conversion note for grep-group chains.
        assert!(hook.get("additionalContext").is_none());
    }

    #[test]
    fn test_never_worse_guard_hint_overhead_emits_nothing() {
        let tee_dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.tee.directory = Some(tee_dir.path().to_path_buf());

        let pairs: Vec<String> = (0..40).map(|i| format!("\"k{i}\":{i}")).collect();
        let almost_min = format!("{{\n{}}}", pairs.join(",\n"));
        let minified_len = serde_json::to_string(
            &serde_json::from_str::<serde_json::Value>(&almost_min).expect("valid"),
        )
        .expect("serialize")
        .len();
        assert!(
            almost_min.len() - minified_len < 60,
            "premise: savings smaller than hint length"
        );

        let event = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/tmp/almost.json"},
            "tool_response": {"type": "text", "file": {
                "filePath": "/tmp/almost.json",
                "content": almost_min,
                "numLines": 42, "startLine": 1, "totalLines": 42
            }},
            "duration_ms": 1
        });
        assert!(
            process_with_config(&event, &config).is_none(),
            "composed (filtered+hint) > raw must emit nothing"
        );
    }

    #[test]
    fn test_grep_count_mode_not_rewritten() {
        assert!(
            run_fixture("grep_count").is_none(),
            "count mode is compact already"
        );
    }

    #[test]
    fn test_webfetch_markdown_passes_through() {
        // Real WebFetch results are already-markdown AI summaries: the web
        // chain is a no-op and nothing must be emitted.
        assert!(run_fixture("webfetch_md").is_none());
    }

    #[test]
    fn test_websearch_passes_through_when_chain_noop() {
        assert!(run_fixture("websearch").is_none());
    }

    #[test]
    fn test_glob_unsupported_format_passes_through() {
        assert!(run_fixture("glob").is_none());
    }

    #[test]
    fn test_malformed_event_emits_nothing() {
        assert!(process_with_config(&serde_json::json!({}), &test_config()).is_none());
        assert!(
            process_with_config(&serde_json::json!({"tool_name": 42}), &test_config()).is_none()
        );
        assert!(process_with_config(
            &serde_json::json!({"tool_name": "Read", "tool_response": "not an object"}),
            &test_config()
        )
        .is_none());
    }

    #[test]
    fn test_unknown_tool_emits_nothing() {
        let event = serde_json::json!({
            "tool_name": "Bash",
            "tool_response": {"output": "x".repeat(2000)}
        });
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_gate_posthook_disabled() {
        let mut config = test_config();
        config.posthook.enabled = false;
        assert!(process_with_config(&fixture("read_flat"), &config).is_none());
    }

    #[test]
    fn test_gate_tool_toggle_off() {
        let mut config = test_config();
        config.posthook.tools.read = false;
        assert!(process_with_config(&fixture("read_flat"), &config).is_none());
    }

    #[test]
    fn test_gate_exclude_paths() {
        let mut config = test_config();
        config.posthook.exclude_paths = vec!["*data_flat*".into()];
        assert!(process_with_config(&fixture("read_flat"), &config).is_none());
    }

    #[test]
    fn test_gate_format_off() {
        let mut config = test_config();
        config.posthook.formats.json = "off".into();
        assert!(process_with_config(&fixture("read_flat"), &config).is_none());
    }

    #[test]
    fn test_gate_unknown_format_value_treated_as_auto() {
        let mut config = test_config();
        config.posthook.formats.json = "toon".into(); // Part 2 value, unknown today
        assert!(
            process_with_config(&fixture("read_flat"), &config).is_some(),
            "unknown format value must degrade to auto, not disable"
        );
    }

    #[test]
    fn test_gate_min_size() {
        let event = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/tmp/small.json"},
            "tool_response": {"type": "text", "file": {
                "filePath": "/tmp/small.json",
                "content": "{\"a\": 1}",
                "numLines": 1, "startLine": 1, "totalLines": 1
            }}
        });
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_read_non_json_tracked_passthrough_no_output() {
        let event = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/tmp/code.rs"},
            "tool_response": {"type": "text", "file": {
                "filePath": "/tmp/code.rs",
                "content": "fn main() {}\n".repeat(100),
                "numLines": 100, "startLine": 1, "totalLines": 100
            }}
        });
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_read_cargo_lock_summarized() {
        let content = include_str!("../../Cargo.lock");
        let event = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/repo/Cargo.lock"},
            "tool_response": {"type": "text", "file": {
                "filePath": "/repo/Cargo.lock",
                "content": content,
                "numLines": content.lines().count(),
                "startLine": 1,
                "totalLines": content.lines().count()
            }},
            "duration_ms": 3
        });
        let out = process_with_config(&event, &test_config()).expect("lockfile must summarize");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let hook = &v["hookSpecificOutput"];
        let new_content = hook["updatedToolOutput"]["file"]["content"]
            .as_str()
            .unwrap();
        assert!(new_content.starts_with("Cargo.lock: "), "summary header");
        assert!(new_content.contains("serde@"));
        assert!(hook.get("additionalContext").is_none());
    }

    fn bash_fixture(name: &str) -> Value {
        let raw = match name {
            "ansi" => include_str!("../../tests/fixtures/posthook/bash_ansi_output.json"),
            "progress" => include_str!("../../tests/fixtures/posthook/bash_progress_cr.json"),
            "repetitive" => {
                include_str!("../../tests/fixtures/posthook/bash_repetitive_lines.json")
            }
            _ => panic!("unknown fixture {name}"),
        };
        serde_json::from_str(raw).expect("fixture parses")
    }

    #[test]
    fn test_bash_ansi_output_stripped_and_rewrapped() {
        let out = process_with_config(&bash_fixture("ansi"), &test_config())
            .expect("ansi output must be filtered");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let response = &v["hookSpecificOutput"]["updatedToolOutput"];
        let stdout = response["stdout"].as_str().unwrap();
        assert!(!stdout.contains('\x1b'), "ANSI gone");
        assert!(stdout.contains("PASS test_0"), "content kept");
        assert_eq!(response["interrupted"], false, "metadata preserved");
        assert_eq!(response["isImage"], false);
        assert!(
            v["hookSpecificOutput"].get("additionalContext").is_none(),
            "no conversion note for the generic floor"
        );
    }

    #[test]
    fn test_bash_progress_frames_collapsed() {
        let out = process_with_config(&bash_fixture("progress"), &test_config())
            .expect("progress output must be filtered");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let stdout = v["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap();
        assert!(stdout.contains("Downloading 100% done"));
        assert!(!stdout.contains("Downloading 0%"));
    }

    #[test]
    fn test_bash_repetitive_lines_deduped() {
        let out = process_with_config(&bash_fixture("repetitive"), &test_config())
            .expect("repetitive output must be filtered");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let stdout = v["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap();
        assert!(stdout.contains("[x40]"), "explicit dedup marker: {stdout}");
    }

    #[test]
    fn test_bash_rtk_rewritten_command_skipped() {
        let mut event = bash_fixture("repetitive");
        event["tool_input"]["command"] = json!("rtk cargo test");
        assert!(process_with_config(&event, &test_config()).is_none());

        event["tool_input"]["command"] = json!("git add . && rtk cargo test");
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_bash_recall_recovery_command_skipped() {
        let mut event = bash_fixture("repetitive");
        event["tool_input"]["command"] =
            json!("cat \"$HOME/.local/share/rtk/tee/posthook/123_1-0_bash_x.log\"");
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_bash_interrupted_skipped() {
        let mut event = bash_fixture("repetitive");
        event["tool_response"]["interrupted"] = json!(true);
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_bash_tool_toggle_off() {
        let mut config = test_config();
        config.posthook.tools.bash = false;
        assert!(process_with_config(&bash_fixture("repetitive"), &config).is_none());
    }

    #[test]
    fn test_bash_stderr_filtered_independently() {
        let mut event = bash_fixture("ansi");
        event["tool_response"]["stderr"] = json!("\x1b[31merror line\x1b[0m\n".repeat(10));
        let out = process_with_config(&event, &test_config()).expect("stderr must be filtered too");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let stderr = v["hookSpecificOutput"]["updatedToolOutput"]["stderr"]
            .as_str()
            .unwrap();
        assert!(!stderr.contains('\x1b'));
        assert!(stderr.contains("[x10]"));
    }

    #[test]
    fn test_bash_env_prefix_kill_switch() {
        let mut event = bash_fixture("repetitive");
        event["tool_input"]["command"] = json!("RTK_POSTHOOK=0 python3 -c \"print('x')\"");
        assert!(process_with_config(&event, &test_config()).is_none());

        event["tool_input"]["command"] = json!("RTK_DISABLED=1 python3 -c \"print('x')\"");
        assert!(process_with_config(&event, &test_config()).is_none());

        event["tool_input"]["command"] = json!("FOO=bar python3 -c \"print('x')\"");
        assert!(
            process_with_config(&event, &test_config()).is_some(),
            "unrelated env assignment must not disable filtering"
        );

        event["tool_input"]["command"] = json!("echo RTK_POSTHOOK=0");
        assert!(
            process_with_config(&event, &test_config()).is_some(),
            "non-leading mention is not a kill switch"
        );
    }

    #[test]
    fn test_first_program_word_skips_assignments() {
        assert_eq!(
            first_program_word("RTK_POSTHOOK=0 python3 -c 'x'"),
            "python3"
        );
        assert_eq!(first_program_word("FOO=1 BAR=2 make all"), "make");
        assert_eq!(first_program_word("git status"), "git");
        assert_eq!(first_program_word(""), "");
    }

    #[test]
    fn test_lens_view_carries_no_recall_hint() {
        let tee_dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.tee.directory = Some(tee_dir.path().to_path_buf());

        let out = process_with_config(&fixture("read_flat"), &config).expect("converts");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let content = v["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
            .as_str()
            .unwrap();
        assert!(
            !content.contains("full output:"),
            "a lossless view must not announce a recall file: {}",
            &content[content.len().saturating_sub(120)..]
        );
        // The recall file is still written, just not advertised.
        let stored = std::fs::read_dir(tee_dir.path().join(tee::POSTHOOK_SUBDIR))
            .expect("recall dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
            .count();
        assert_eq!(stored, 1, "raw bytes preserved on disk");
    }

    #[test]
    fn test_lossy_chain_keeps_recall_hint() {
        let tee_dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.tee.directory = Some(tee_dir.path().to_path_buf());

        let out = process_with_config(&bash_fixture("repetitive"), &config).expect("converts");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let stdout = v["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap();
        assert!(
            stdout.contains("full output:"),
            "dedup/cap drop lines: hint required"
        );
    }

    #[test]
    fn test_reread_within_window_serves_raw() {
        let tee_dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.tee.directory = Some(tee_dir.path().to_path_buf());

        assert!(
            process_with_config(&fixture("read_flat"), &config).is_some(),
            "first read converts"
        );
        assert!(
            process_with_config(&fixture("read_flat"), &config).is_none(),
            "second read within the window passes through raw"
        );
        assert!(
            process_with_config(&fixture("read_nested"), &config).is_some(),
            "other paths unaffected"
        );
    }

    #[test]
    fn test_read_partial_never_converted() {
        let mut event = fixture("read_flat");
        event["tool_response"]["file"]["numLines"] = json!(500);
        assert!(
            process_with_config(&event, &test_config()).is_none(),
            "host-truncated read (numLines < totalLines) must pass through"
        );

        let mut event = fixture("read_flat");
        event["tool_response"]["file"]["startLine"] = json!(10);
        assert!(
            process_with_config(&event, &test_config()).is_none(),
            "ranged read (offset) must pass through"
        );
    }

    #[test]
    fn test_toon_mirror_edit_compiles_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let json = dir.path().join("data.json");
        let raw = "[\n  {\n    \"id\": 1,\n    \"name\": \"a\"\n  },\n  {\n    \"id\": 2,\n    \"name\": \"b\"\n  }\n]\n";
        std::fs::write(&json, raw).expect("write");
        let mirror = crate::core::lens::mirror::extract(&json)
            .expect("extract")
            .path;

        // Agent edits the mirror; the hook must regenerate the JSON source.
        let toon = std::fs::read_to_string(&mirror).expect("read");
        std::fs::write(&mirror, toon.replace("1,a", "1,renamed")).expect("write");

        let event = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": mirror.to_string_lossy()},
            "tool_response": {"filePath": mirror.to_string_lossy(), "success": true}
        });
        assert!(
            process_with_config(&event, &test_config()).is_none(),
            "a successful compile is silent"
        );
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(&json).expect("read")).expect("json");
        assert_eq!(value[0]["name"], "renamed");
        assert_eq!(value[1]["name"], "b");
    }

    #[test]
    fn test_toon_mirror_bad_edit_reports_and_keeps_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let json = dir.path().join("data.json");
        let raw = "[\n  {\n    \"id\": 1,\n    \"name\": \"a\"\n  },\n  {\n    \"id\": 2,\n    \"name\": \"b\"\n  }\n]\n";
        std::fs::write(&json, raw).expect("write");
        let mirror = crate::core::lens::mirror::extract(&json)
            .expect("extract")
            .path;

        // Stale row count: strict decode rejects it.
        let toon = std::fs::read_to_string(&mirror).expect("read");
        std::fs::write(&mirror, toon.replace("[2]", "[9]")).expect("write");

        let event = serde_json::json!({
            "tool_name": "Write",
            "tool_input": {"file_path": mirror.to_string_lossy()},
            "tool_response": {"filePath": mirror.to_string_lossy(), "success": true}
        });
        let out = process_with_config(&event, &test_config()).expect("reports the failure");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let note = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(note.contains("did not compile"), "agent is told: {note}");
        assert_eq!(
            std::fs::read_to_string(&json).expect("read"),
            raw,
            "source untouched"
        );
    }

    #[test]
    fn test_non_mirror_edit_ignored() {
        let event = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": "/tmp/whatever.rs"},
            "tool_response": {"filePath": "/tmp/whatever.rs", "success": true}
        });
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_run_never_panics_on_garbage() {
        run(&serde_json::json!(null));
        run(&serde_json::json!("string event"));
        run(&serde_json::json!({"tool_name": "Read"}));
    }

    #[test]
    fn test_recall_hint_ordering_never_pushes_above_raw() {
        // filter_and_compose guards AFTER composing hint: craft content where
        // filtered+hint would exceed raw and assert nothing is emitted.
        // A JSON file that's already minified: toon layer output == minify == raw
        // (no-op chain) → None without any store.
        let raw = format!(
            "[{}]",
            (0..200)
                .map(|i| format!("{i}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let event = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/tmp/nums.json"},
            "tool_response": {"type": "text", "file": {
                "filePath": "/tmp/nums.json",
                "content": raw,
                "numLines": 1, "startLine": 1, "totalLines": 1
            }}
        });
        assert!(process_with_config(&event, &test_config()).is_none());
    }

    #[test]
    fn test_source_and_helpers() {
        assert_eq!(extension_of("/a/b/data.json"), "json");
        assert_eq!(extension_of("https://docs.rs/toon"), "");
        assert_eq!(basename_of("/a/b/data.json"), "data.json");
        assert_eq!(basename_of("https://docs.rs/toon-format"), "docs.rs");
    }
}
