//! Claude Code adapter for the JSON lens: PreToolUse `Edit` events whose
//! `old_string` was taken from a compressed view (TOON / minified) are
//! rewritten to the equivalent raw-byte edit via `updatedInput`.
//!
//! Thin by design — marshals the tool payload into
//! `lens::translate_edit(raw, view_old, view_new)` and back. Every failure
//! emits nothing (exit 0): the host then runs the original Edit, which fails
//! closed on exact-match. Raw anchors never reach the lens at all.

use super::constants::PRE_TOOL_USE_KEY;
use crate::core::config::Config;
use crate::core::lens::translate::translate_edit;
use crate::core::tracking::{estimate_tokens, Tracker};
use serde_json::{json, Value};
use std::io::{self, Write};

/// Files above this are never served as views, so never translated.
const MAX_FILE_BYTES: u64 = 262_144;

pub fn run(event: &Value) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(event)));
    if let Ok(Some(output)) = outcome {
        let _ = writeln!(io::stdout(), "{output}");
    }
}

fn process(event: &Value) -> Option<String> {
    if std::env::var("RTK_DISABLED").ok().as_deref() == Some("1") {
        return None;
    }
    let config = Config::load().unwrap_or_default();
    let input = event.get("tool_input")?;
    let file_path = input.get("file_path")?.as_str()?;
    let raw = read_file(&config, file_path)?;
    process_with(&config, input, &raw)
}

/// Testable core: gates → lens → `updatedInput`.
fn process_with(config: &Config, input: &Value, raw: &str) -> Option<String> {
    if !config.posthook.enabled || !config.posthook.lens || !config.posthook.tools.read {
        return None;
    }
    let file_path = input.get("file_path")?.as_str()?;
    if !eligible_path(file_path) {
        return None;
    }
    if input
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let old_string = input.get("old_string")?.as_str()?;
    let new_string = input.get("new_string")?.as_str()?;

    let translation = translate_edit(raw, old_string, new_string)?;

    let mut updated = input.clone();
    let obj = updated.as_object_mut()?;
    obj.insert("old_string".into(), json!(translation.raw_old));
    obj.insert("new_string".into(), json!(translation.raw_new));

    track(file_path, &translation.raw_old);
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": PRE_TOOL_USE_KEY,
                "permissionDecisionReason": "RTK lens: edit translated from compressed view",
                "updatedInput": updated,
            }
        })
        .to_string(),
    )
}

/// Same eligibility as the Read view: `.json` files that are not lockfiles.
fn eligible_path(file_path: &str) -> bool {
    let path = std::path::Path::new(file_path);
    let is_json = path.extension().and_then(|e| e.to_str()) == Some("json");
    let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    is_json && basename != "package-lock.json"
}

fn read_file(config: &Config, file_path: &str) -> Option<String> {
    let _ = config;
    let meta = std::fs::metadata(file_path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(file_path).ok()
}

fn track(file_path: &str, raw_old: &str) {
    if let Ok(tracker) = Tracker::new() {
        let tokens = estimate_tokens(raw_old);
        let _ = tracker.record(
            &format!("Edit {file_path}"),
            "rtk lens edit json",
            tokens,
            tokens,
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lens::options::encode_view;

    fn fixture_event() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/posthook/edit_json_event.json"
        ))
        .expect("fixture parses")
    }

    fn flat_raw() -> String {
        let event: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/posthook/read_json_flat.json"
        ))
        .unwrap();
        event["tool_response"]["file"]["content"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn config() -> Config {
        let mut config = Config::default();
        config.tee.enabled = false;
        config
    }

    #[test]
    fn test_raw_anchor_passes_through_untouched() {
        // The captured event: old_string is a raw JSON line from the file.
        let event = fixture_event();
        let raw = "{\n  \"name\": \"demo\",\n  \"port\": 8080\n}\n";
        assert!(process_with(&config(), &event["tool_input"], raw).is_none());
    }

    #[test]
    fn test_toon_anchor_translated_to_raw_updated_input() {
        let raw = flat_raw();
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = view
            .lines()
            .find(|l| l.contains("1,user_1,user1@example.com"))
            .unwrap()
            .to_string();
        let new = old.replace("user_1", "renamed_1");

        let mut event = fixture_event();
        event["tool_input"]["file_path"] = json!("/home/user/project/data_flat.json");
        event["tool_input"]["old_string"] = json!(old);
        event["tool_input"]["new_string"] = json!(new);

        let out = process_with(&config(), &event["tool_input"], &raw).expect("translated");
        let v: Value = serde_json::from_str(&out).unwrap();
        let hook = &v["hookSpecificOutput"];
        assert_eq!(hook["hookEventName"], "PreToolUse");
        assert!(
            hook.get("permissionDecision").is_none(),
            "host permission flow untouched"
        );
        let updated = &hook["updatedInput"];
        let raw_old = updated["old_string"].as_str().unwrap();
        let raw_new = updated["new_string"].as_str().unwrap();
        assert_eq!(
            raw.matches(raw_old).count(),
            1,
            "raw anchor exact-matches the file once"
        );
        assert!(raw_old.contains("\"user_1\""));
        assert!(raw_new.contains("\"renamed_1\""));
        assert_eq!(updated["file_path"], "/home/user/project/data_flat.json");
        assert_eq!(updated["replace_all"], false);

        let applied: Value = serde_json::from_str(&raw.replacen(raw_old, raw_new, 1)).unwrap();
        assert_eq!(applied[1]["name"], "renamed_1");
    }

    #[test]
    fn test_gates_lens_off_tools_read_off_replace_all_non_json() {
        let raw = flat_raw();
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = view.lines().nth(1).unwrap().to_string();
        let mut event = fixture_event();
        event["tool_input"]["file_path"] = json!("/home/user/project/data_flat.json");
        event["tool_input"]["old_string"] = json!(old);
        event["tool_input"]["new_string"] = json!(old.replace("user_0", "x"));

        let mut off = config();
        off.posthook.lens = false;
        assert!(process_with(&off, &event["tool_input"], &raw).is_none());

        let mut no_read = config();
        no_read.posthook.tools.read = false;
        assert!(process_with(&no_read, &event["tool_input"], &raw).is_none());

        let mut replace_all = event.clone();
        replace_all["tool_input"]["replace_all"] = json!(true);
        assert!(process_with(&config(), &replace_all["tool_input"], &raw).is_none());

        let mut not_json = event.clone();
        not_json["tool_input"]["file_path"] = json!("/home/user/project/data.yaml");
        assert!(process_with(&config(), &not_json["tool_input"], &raw).is_none());

        let mut lockfile = event.clone();
        lockfile["tool_input"]["file_path"] = json!("/home/user/project/package-lock.json");
        assert!(process_with(&config(), &lockfile["tool_input"], &raw).is_none());
    }

    #[test]
    fn test_unknown_anchor_and_malformed_emit_nothing() {
        let raw = flat_raw();
        let mut event = fixture_event();
        event["tool_input"]["old_string"] = json!("nothing like this in any view");
        assert!(process_with(&config(), &event["tool_input"], &raw).is_none());
        assert!(process_with(&config(), &json!({}), &raw).is_none());
        assert!(process_with(&config(), &json!({"file_path": 42}), &raw).is_none());
        run(&json!(null));
        run(&json!({"tool_name": "Edit"}));
    }
}
