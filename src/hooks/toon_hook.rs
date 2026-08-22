//! Session-scoped TOON mirrors for Claude Code.
//!
//! A JSON read is served from a `.toon` working copy created on demand, so
//! the agent reads roughly a third of the bytes and edits a file whose text
//! it can anchor on. Whichever side is edited, the other is re-derived, and
//! the mirrors are deleted when the session ends.
//!
//! Fail-open like every other hook here: any error emits nothing and the
//! original tool call proceeds untouched.

use super::constants::{POST_TOOL_USE_KEY, PRE_TOOL_USE_KEY, SESSION_START_KEY};
use crate::core::config::Config;
use crate::core::lens::mirror;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::path::Path;

/// Second read of the same file inside this window is served raw, so an agent
/// that needs exact bytes always has a way to reach them.
const RAW_REREAD_WINDOW_SECS: u64 = 120;

pub fn run_pre_read(event: &Value) {
    emit(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || pre_read(event),
    )));
}

pub fn run_session_start(event: &Value) {
    emit(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || session_start(event),
    )));
}

pub fn run_session_end(event: &Value) {
    emit(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || session_end(event),
    )));
}

fn emit(outcome: std::thread::Result<Option<String>>) {
    if let Ok(Some(output)) = outcome {
        let _ = writeln!(io::stdout(), "{output}");
    }
}

/// Redirect a JSON read to its mirror, creating the mirror if needed.
fn pre_read(event: &Value) -> Option<String> {
    if std::env::var("RTK_DISABLED").ok().as_deref() == Some("1") {
        return None;
    }
    let config = Config::load().unwrap_or_default();
    pre_read_with(&config, event)
}

fn pre_read_with(config: &Config, event: &Value) -> Option<String> {
    let input = event.get("tool_input")?;
    let path = input.get("file_path")?.as_str()?;
    let json = Path::new(path);
    if !mirror::session::eligible(json, &config.toon) {
        return None;
    }
    // A ranged read is a request for exact bytes: serve raw and keep serving
    // raw for this file.
    if input.get("offset").is_some() || input.get("limit").is_some() {
        mirror::session::pin_raw(config, json);
        return None;
    }
    // A second read means the agent wants the real thing: pin it raw for the
    // rest of the session so the escape hatch never expires mid-task. Re-reading
    // a file we just served as a mirror is the expensive failure — a whole round
    // trip spent checking on us — so it counts against the session.
    if crate::core::tee::posthook_recently_read(config, path, RAW_REREAD_WINDOW_SECS) {
        if mirror::session::mirror_for(json).exists() {
            mirror::session::strike(config);
        }
        mirror::session::pin_raw(config, json);
        return None;
    }

    let served = mirror::session::ensure(config, json)?;
    crate::core::tee::posthook_mark_read(config, path);

    let mut updated = input.clone();
    updated.as_object_mut()?.insert(
        "file_path".into(),
        json!(served.mirror.display().to_string()),
    );

    let mut hook_output = json!({
        "hookEventName": PRE_TOOL_USE_KEY,
        "permissionDecision": "allow",
        "permissionDecisionReason": "RTK: TOON working copy",
        "updatedInput": updated,
    });
    // The redirect is invisible: the model asked for the JSON path, so without
    // being told the mirror's name it addresses edits to the JSON and its TOON
    // anchor matches nothing. Name the file to edit, once per file.
    if served.first_time {
        // The full mirror path, because Edit needs one and the store is not
        // beside the source any more.
        if let Some(source) = json.file_name().and_then(|n| n.to_str()) {
            hook_output["additionalContext"] = json!(format!(
                "Edit {} (TOON view of {source}).",
                served.mirror.display()
            ));
        }
    }
    Some(json!({ "hookSpecificOutput": hook_output }).to_string())
}

/// A file was written: keep the mirror and its JSON source in step.
/// `.toon` edited -> compile; `.json` edited -> re-derive the mirror.
pub fn post_write(event: &Value) -> Option<String> {
    let config = Config::load().unwrap_or_default();
    post_write_with(&config, event)
}

fn post_write_with(config: &Config, event: &Value) -> Option<String> {
    let path = event.pointer("/tool_input/file_path")?.as_str()?;
    let file = Path::new(path);

    match file.extension().and_then(|e| e.to_str()) {
        Some(mirror::MIRROR_EXT) => {
            let source = mirror::session::source_of(config, file);
            match mirror::compile_to(file, &source) {
                Ok(_) => None,
                Err(e) => {
                    let gave_up = mirror::session::record_failure(config, file);
                    let context = if gave_up {
                        format!(
                            "{} still does not compile, so {} is unchanged: {e:#}\n\
                         Edit {} directly from here; the .toon is no longer in use.",
                            file.display(),
                            source.display(),
                            source.display()
                        )
                    } else {
                        format!(
                            "{} did not compile, so {} was NOT updated: {e:#}",
                            file.display(),
                            source.display()
                        )
                    };
                    Some(
                        json!({
                            "hookSpecificOutput": {
                                "hookEventName": POST_TOOL_USE_KEY,
                                "additionalContext": context,
                            }
                        })
                        .to_string(),
                    )
                }
            }
        }
        Some("json") => {
            // The agent edited the source directly, so it is working against
            // real bytes: stop serving a mirror for this file.
            mirror::session::pin_raw(config, file);
            None
        }
        _ => None,
    }
}

/// Compile anything pending and remove the session's mirrors.
/// A mirror the model was not warned about costs a verification round trip,
/// and one round trip replays the whole prefix — far more than a mirror ever
/// saves. Warning once, before any tool call, is what makes the trade work.
const MIRROR_NOTICE: &str = "System may serve a large .json as a .toon working copy. \
Edit the .toon; it will automatically edit the json source.";

fn session_start(event: &Value) -> Option<String> {
    if std::env::var("RTK_DISABLED").ok().as_deref() == Some("1") {
        return None;
    }
    let config = Config::load().unwrap_or_default();
    if !config.posthook.enabled || !config.toon.mirrors {
        return None;
    }
    // A crashed session leaves its mirrors behind. Sweep them on a real start
    // only: resume and compact fire here too, and their mirrors are live.
    if matches!(
        event.get("source").and_then(|s| s.as_str()),
        Some("startup") | Some("clear") | None
    ) {
        mirror::session::retire(&config);
    }
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": SESSION_START_KEY,
                "additionalContext": MIRROR_NOTICE,
            }
        })
        .to_string(),
    )
}

fn session_end(_event: &Value) -> Option<String> {
    let config = Config::load().unwrap_or_default();
    mirror::session::retire(&config);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config_for(dir: &TempDir) -> Config {
        let mut config = Config::default();
        config.tee.directory = Some(dir.path().join("tee"));
        config.toon.min_bytes = 256;
        config.toon.min_saved_bytes = 200;
        config
    }

    fn rows(n: usize) -> String {
        let body: Vec<String> = (0..n)
            .map(|i| format!("  {{\n    \"id\": {i},\n    \"name\": \"row_{i}\"\n  }}"))
            .collect();
        format!("[\n{}\n]\n", body.join(",\n"))
    }

    fn read_event(path: &Path) -> Value {
        json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": path.display().to_string()}
        })
    }

    #[test]
    fn test_json_read_is_redirected_to_a_fresh_mirror() {
        let dir = TempDir::new().expect("tempdir");
        let json = dir.path().join("records.json");
        std::fs::write(&json, rows(40)).expect("write");

        let out = pre_read_with(&config_for(&dir), &read_event(&json)).expect("redirected");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let served = v["hookSpecificOutput"]["updatedInput"]["file_path"]
            .as_str()
            .unwrap();
        assert!(served.ends_with(".toon"));
        assert!(Path::new(served).exists(), "mirror created on demand");
        assert!(
            std::fs::metadata(served).unwrap().len() < std::fs::metadata(&json).unwrap().len() / 2
        );
    }

    #[test]
    fn test_session_start_warns_before_any_tool_runs() {
        let out = session_start(&json!({"hook_event_name": "SessionStart", "source": "startup"}))
            .expect("notice emitted");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let notice = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("context");
        assert!(notice.contains(".toon"), "names the format: {notice}");
        assert!(
            notice.len() < 200,
            "rides in the cached prefix, so it stays small: {} bytes",
            notice.len()
        );
    }

    #[test]
    fn test_note_names_the_file_to_edit() {
        let dir = TempDir::new().expect("tempdir");
        let json = dir.path().join("records.json");
        std::fs::write(&json, rows(40)).expect("write");
        let config = config_for(&dir);

        let out = pre_read_with(&config, &read_event(&json)).expect("redirected");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        // The model asked for the JSON path and cannot see the swap, so the
        // note has to spell out which file its anchors belong to.
        let note = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("note");
        assert!(note.starts_with("Edit "), "{note}");
        assert!(note.contains(".rtk"), "names the store path: {note}");
        assert!(
            note.ends_with("(TOON view of records.json)."),
            "names the source: {note}"
        );

        // A second read is the raw escape hatch, so the note cannot repeat
        // here; the once-per-file flag itself is covered in mirror::session.
        assert!(
            pre_read_with(&config, &read_event(&json)).is_none(),
            "second read serves raw"
        );
    }

    #[test]
    fn test_small_and_excluded_files_are_untouched() {
        let dir = TempDir::new().expect("tempdir");
        let small = dir.path().join("small.json");
        std::fs::write(&small, "{\"a\": 1}").expect("write");
        let config = config_for(&dir);
        assert!(pre_read_with(&config, &read_event(&small)).is_none());

        let pkg = dir.path().join("package.json");
        std::fs::write(&pkg, rows(40)).expect("write");
        assert!(pre_read_with(&config, &read_event(&pkg)).is_none());
        assert!(!dir.path().join("package.toon").exists());
    }

    #[test]
    fn test_ranged_read_stays_raw() {
        let dir = TempDir::new().expect("tempdir");
        let json = dir.path().join("records.json");
        std::fs::write(&json, rows(40)).expect("write");
        let mut ev = read_event(&json);
        ev["tool_input"]["offset"] = json!(10);
        assert!(pre_read_with(&config_for(&dir), &ev).is_none());
    }

    #[test]
    fn test_json_write_pins_the_file_raw() {
        let dir = TempDir::new().expect("tempdir");
        let json = dir.path().join("records.json");
        std::fs::write(&json, rows(40)).expect("write");
        let config = config_for(&dir);
        pre_read_with(&config, &read_event(&json)).expect("redirected");
        let mirror = mirror::session::mirror_for(&json);

        let raw = std::fs::read_to_string(&json).expect("read");
        std::fs::write(&json, raw.replace("row_0", "renamed")).expect("write");
        let ev = json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": json.display().to_string()}
        });
        assert!(post_write_with(&config, &ev).is_none());
        assert!(
            !mirror.exists(),
            "stale mirror removed, not left in the repo"
        );
        assert!(
            pre_read_with(&config, &read_event(&json)).is_none(),
            "the agent now holds raw anchors: keep serving raw"
        );
    }

    #[test]
    fn test_bad_mirror_edit_is_reported() {
        let dir = TempDir::new().expect("tempdir");
        let json = dir.path().join("records.json");
        std::fs::write(&json, rows(40)).expect("write");
        let config = config_for(&dir);
        pre_read_with(&config, &read_event(&json)).expect("redirected");
        let mirror = mirror::session::mirror_for(&json);

        let toon = std::fs::read_to_string(&mirror).expect("read");
        let broken = toon
            .lines()
            .map(|l| {
                if l.starts_with("  1,") {
                    "  1,a,b,c,d,e,f,g,h"
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&mirror, broken).expect("write");
        let ev = json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": mirror.display().to_string()}
        });
        let out = post_write_with(&config, &ev).expect("failure reported");
        assert!(out.contains("did not compile"));
        assert_eq!(
            std::fs::read_to_string(&json).expect("read"),
            rows(40),
            "source untouched"
        );
    }
}
