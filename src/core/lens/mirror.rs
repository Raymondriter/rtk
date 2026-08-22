//! TOON mirrors: `.toon` working copies of JSON files.
//!
//! JSON stays canonical (committed, schema-validated, what tools read); the
//! `.toon` mirror is a regenerable working copy an agent reads and edits at
//! roughly a third of the size. Both directions regenerate, so a format or
//! encoder change costs nothing: throw the mirrors away and re-extract.
//!
//! Every write is verified first — a mirror is only written when it decodes
//! back to the source value, and a JSON file is only rewritten when the
//! decoded mirror round-trips. A malformed edit can never produce a broken
//! artifact.

use super::options::{decode_view, encode_view, values_equal};
use super::style::Style;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MIRROR_EXT: &str = "toon";

/// `data.json` -> `data.toon`
pub fn mirror_path(json: &Path) -> PathBuf {
    json.with_extension(MIRROR_EXT)
}

/// `data.toon` -> `data.json`
pub fn source_path(mirror: &Path) -> PathBuf {
    mirror.with_extension("json")
}

pub struct Written {
    pub path: PathBuf,
    pub from_bytes: usize,
    pub to_bytes: usize,
    /// Regenerating the source reproduced it byte for byte.
    pub byte_identical: bool,
}

/// JSON -> TOON mirror beside the source (the `rtk toon` CLI shape).
pub fn extract(json_path: &Path) -> Result<Written> {
    extract_to(json_path, &mirror_path(json_path))
}

/// JSON -> TOON mirror at an explicit path. Refuses to write a mirror that
/// does not decode back to the same value.
pub fn extract_to(json_path: &Path, out: &Path) -> Result<Written> {
    let raw = std::fs::read_to_string(json_path)
        .with_context(|| format!("Failed to read {}", json_path.display()))?;
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", json_path.display()))?;

    let toon = encode_view(&value)
        .with_context(|| format!("Failed to encode {} as TOON", json_path.display()))?;
    let decoded = decode_view(&toon)
        .with_context(|| format!("{} does not round-trip through TOON", json_path.display()))?;
    if !values_equal(&value, &decoded) {
        bail!(
            "{} does not round-trip through TOON: values differ",
            json_path.display()
        );
    }

    let regenerated = render_json(&decoded, &Style::detect(&raw), raw.ends_with('\n'))?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    std::fs::write(out, &toon).with_context(|| format!("Failed to write {}", out.display()))?;

    Ok(Written {
        path: out.to_path_buf(),
        from_bytes: raw.len(),
        to_bytes: toon.len(),
        byte_identical: regenerated == raw,
    })
}

/// TOON mirror -> the JSON beside it (the `rtk toon` CLI shape).
pub fn compile(mirror: &Path) -> Result<Written> {
    compile_to(mirror, &source_path(mirror))
}

/// TOON mirror -> an explicit JSON path. Verifies before writing; on any doubt
/// the existing JSON is left untouched.
pub fn compile_to(mirror: &Path, out: &Path) -> Result<Written> {
    let toon = std::fs::read_to_string(mirror)
        .with_context(|| format!("Failed to read {}", mirror.display()))?;
    // Deleting or adding a row leaves the `[N]` header stale, and a strict
    // decode rejects the whole file for it. Reconcile the count with the rows
    // actually present before decoding: an agent editing rows should not have
    // to keep a running total.
    let reconciled = reconcile_counts(&toon);
    let value = decode_view(&reconciled).with_context(|| {
        format!(
            "{} is not valid TOON (check the field list)",
            mirror.display()
        )
    })?;
    // Deliberately not written back. Compiling makes the source newer than the
    // mirror, so the next read re-extracts a fresh one anyway; rewriting here
    // only makes the host announce "hook modified this file after your edit",
    // which invites the agent to re-read and costs a round trip.
    if encode_view(&value).is_none() {
        bail!("{} could not be re-encoded", mirror.display());
    }

    // Match the existing source's formatting so a compile produces a minimal
    // diff, including whether it ends with a newline.
    let existing = std::fs::read_to_string(out).ok();
    let style = existing.as_deref().map(Style::detect).unwrap_or(Style {
        indent: Some("  ".into()),
    });
    let trailing_newline = existing
        .as_deref()
        .map(|e| e.ends_with('\n'))
        .unwrap_or(true);
    let rendered = render_json(&value, &style, trailing_newline)?;
    std::fs::write(out, &rendered).with_context(|| format!("Failed to write {}", out.display()))?;

    Ok(Written {
        path: out.to_path_buf(),
        from_bytes: toon.len(),
        to_bytes: rendered.len(),
        byte_identical: true,
    })
}

/// Rewrite every tabular `[N]` header to the number of rows that follow it.
///
/// A row is a line indented deeper than its header; the block ends at the first
/// line that is not. Headers whose count already matches are left untouched, so
/// a file needing no repair comes back byte-identical.
fn reconcile_counts(toon: &str) -> String {
    static HEADER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^(?<indent>[ \t]*)(?<key>[^\[\]{}:]*)\[(?<count>\d+)\](?<fields>\{[^{}]*\}):[ \t]*$",
        )
        .expect("valid header pattern")
    });

    let lines: Vec<&str> = toon.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let Some(caps) = HEADER.captures(line) else {
            out.push((*line).to_string());
            continue;
        };
        let indent = caps.name("indent").map(|m| m.as_str()).unwrap_or("").len();
        let rows = lines[i + 1..]
            .iter()
            .take_while(|row| !row.trim().is_empty() && row.len() - row.trim_start().len() > indent)
            .count();
        let declared: usize = caps["count"].parse().unwrap_or(rows);
        if declared == rows {
            out.push((*line).to_string());
        } else {
            out.push(format!(
                "{}{}[{}]{}:",
                &caps["indent"], &caps["key"], rows, &caps["fields"]
            ));
        }
    }
    let mut joined = out.join("\n");
    if toon.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Mirror and source agree (both directions decode to the same value).
pub fn in_sync(mirror: &Path) -> Result<bool> {
    let toon = std::fs::read_to_string(mirror)?;
    let json = std::fs::read_to_string(source_path(mirror))?;
    let from_mirror = decode_view(&toon).context("mirror is not valid TOON")?;
    let from_source: Value = serde_json::from_str(&json).context("source is not valid JSON")?;
    Ok(values_equal(&from_mirror, &from_source))
}

fn render_json(value: &Value, style: &Style, trailing_newline: bool) -> Result<String> {
    let mut out = style
        .serialize(value, 0)
        .context("Failed to serialize JSON")?;
    if trailing_newline {
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> String {
        let event: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/posthook/read_json_flat.json"
        ))
        .expect("fixture");
        event["tool_response"]["file"]["content"]
            .as_str()
            .expect("content")
            .to_string()
    }

    fn setup(dir: &TempDir, body: &str) -> PathBuf {
        let p = dir.path().join("data.json");
        std::fs::write(&p, body).expect("write");
        p
    }

    #[test]
    fn test_extract_then_compile_round_trips() {
        let dir = TempDir::new().expect("tempdir");
        let raw = fixture();
        let json = setup(&dir, &raw);

        let w = extract(&json).expect("extract");
        assert_eq!(w.path, dir.path().join("data.toon"));
        assert!(w.to_bytes < w.from_bytes / 2, "mirror is much smaller");
        assert!(in_sync(&w.path).expect("in_sync"));
        // This fixture holds `64.0`, which TOON canonicalizes to `64`: the
        // value survives, the bytes do not. `extract` reports that so the
        // user sees the one-time reformat before adopting.
        assert!(!w.byte_identical);

        let compiled = compile(&w.path).expect("compile");
        assert_eq!(compiled.path, json);
        let after: Value =
            serde_json::from_str(&std::fs::read_to_string(&json).expect("read")).expect("json");
        let before: Value = serde_json::from_str(&raw).expect("json");
        assert!(values_equal(&before, &after), "compile is value-lossless");
    }

    #[test]
    fn test_edited_mirror_updates_source() {
        let dir = TempDir::new().expect("tempdir");
        let json = setup(&dir, &fixture());
        let mirror = extract(&json).expect("extract").path;

        let toon = std::fs::read_to_string(&mirror).expect("read");
        let edited = toon.replace(
            "1,user_1,user1@example.com,false,2.5",
            "1,renamed,user1@example.com,true,2.5",
        );
        assert_ne!(edited, toon, "test edit applies");
        std::fs::write(&mirror, &edited).expect("write");

        compile(&mirror).expect("compile");
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(&json).expect("read")).expect("json");
        assert_eq!(value[1]["name"], "renamed");
        assert_eq!(value[1]["active"], true);
        assert_eq!(value[0]["name"], "user_0", "neighbours untouched");
        assert_eq!(value.as_array().expect("array").len(), 120);
    }

    #[test]
    fn test_deleting_a_row_does_not_require_fixing_the_count() {
        let dir = TempDir::new().expect("tempdir");
        let json = setup(&dir, &fixture());
        let mirror = extract(&json).expect("extract").path;

        let toon = std::fs::read_to_string(&mirror).expect("read");
        let kept: Vec<&str> = toon
            .lines()
            .filter(|l| !l.contains("1,user_1,user1@example.com"))
            .collect();
        std::fs::write(&mirror, kept.join("\n") + "\n").expect("write");

        compile(&mirror).expect("a stale [N] is repaired, not rejected");
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(&json).expect("read")).expect("json");
        assert_eq!(value.as_array().expect("array").len(), 119);
        assert_eq!(value[1]["name"], "user_2", "the right row went");
        assert!(
            std::fs::read_to_string(&mirror)
                .expect("read")
                .starts_with("[120]"),
            "the stale header is reconciled in memory, never rewritten under the agent"
        );
    }

    #[test]
    fn test_non_canonical_spelling_is_accepted_not_rejected() {
        let dir = TempDir::new().expect("tempdir");
        let json = setup(&dir, &fixture());
        let mirror = extract(&json).expect("extract").path;

        let toon = std::fs::read_to_string(&mirror).expect("read");
        // Same value, quoted where the encoder would not quote.
        let edited = toon.replace(
            "1,user_1,user1@example.com,false,2.5",
            "1,\"user_1\",user1@example.com,false,2.5",
        );
        assert_ne!(edited, toon, "test edit applies");
        std::fs::write(&mirror, &edited).expect("write");

        compile(&mirror).expect("decodes, so it is accepted");
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(&json).expect("read")).expect("json");
        assert_eq!(value[1]["name"], "user_1");
        assert_eq!(
            std::fs::read_to_string(&mirror).expect("read"),
            edited,
            "the mirror is left exactly as the agent wrote it"
        );
    }

    #[test]
    fn test_invalid_mirror_never_touches_source() {
        let dir = TempDir::new().expect("tempdir");
        let raw = fixture();
        let json = setup(&dir, &raw);
        let mirror = extract(&json).expect("extract").path;

        // A row missing fields: the shape is broken, not just the tally.
        let toon = std::fs::read_to_string(&mirror).expect("read");
        std::fs::write(
            &mirror,
            toon.replace("1,user_1,user1@example.com,false,2.5", "1,user_1"),
        )
        .expect("write");

        assert!(compile(&mirror).is_err(), "malformed mirror is refused");
        assert_eq!(
            std::fs::read_to_string(&json).expect("read"),
            raw,
            "source untouched"
        );
        assert!(
            !in_sync(&mirror).unwrap_or(false),
            "an unreadable mirror counts as out of sync"
        );
    }

    #[test]
    fn test_extract_refuses_non_json() {
        let dir = TempDir::new().expect("tempdir");
        let p = dir.path().join("data.json");
        std::fs::write(&p, "not json").expect("write");
        assert!(extract(&p).is_err());
        assert!(!dir.path().join("data.toon").exists());
    }

    #[test]
    fn test_byte_identical_when_no_canonicalization() {
        let dir = TempDir::new().expect("tempdir");
        let raw = "[\n  {\n    \"id\": 1,\n    \"name\": \"a\"\n  },\n  {\n    \"id\": 2,\n    \"name\": \"b\"\n  }\n]\n";
        let json = setup(&dir, raw);
        let w = extract(&json).expect("extract");
        assert!(w.byte_identical, "no float/escape canonicalization here");
        compile(&w.path).expect("compile");
        assert_eq!(std::fs::read_to_string(&json).expect("read"), raw);
    }

    #[test]
    fn test_paths() {
        assert_eq!(mirror_path(Path::new("/a/b.json")), Path::new("/a/b.toon"));
        assert_eq!(source_path(Path::new("/a/b.toon")), Path::new("/a/b.json"));
    }
}

/// Session-scoped mirrors: created on demand when an agent reads a JSON file,
/// kept in step with whichever side was edited, and removed when the session
/// ends. The repo is only ever left holding JSON.
pub mod session {
    use super::*;
    use crate::core::config::{Config, ToonConfig};
    use serde::{Deserialize, Serialize};
    use std::collections::{BTreeMap, BTreeSet};

    /// Mirrors live in a workspace-local store rather than beside the source.
    /// Inside the project the agent still sees a project file — which is what
    /// made it adopt the mirror at all — but nothing lands next to the JSON,
    /// and one directory holds everything to clean up.
    const STORE_DIR: &str = ".rtk";

    /// A mirror must be this much smaller than its source to be worth serving.
    const MAX_MIRROR_RATIO: f64 = 0.8;
    /// Strikes — a check we provoked, or an edit we could not compile — before
    /// mirrors stop for the session.
    ///
    /// One, because a single detour costs ~17,500 input-equivalent tokens and a
    /// mirrored file returns ~1,400. A session would need a dozen good mirrors
    /// to pay for one bad turn, and typical sessions touch two or three large
    /// JSON files. Once an agent has shown it does not trust the view, the rest
    /// of the session is not where that gets recovered.
    const MAX_STRIKES: u32 = 1;

    #[derive(Debug, Default, Serialize, Deserialize)]
    struct Index {
        /// Mirrors this session created, each mapped to its source.
        mirrors: BTreeMap<String, String>,
        /// Sources that failed the mirror policy; never retried this session.
        skipped: BTreeSet<String>,
        /// Sources pinned to raw bytes for the rest of the session.
        raw: BTreeSet<String>,
        /// Checks provoked and edits refused, session-wide.
        #[serde(default)]
        strikes: u32,
        /// Mirrors that failed to compile, and how often.
        #[serde(default)]
        failures: std::collections::BTreeMap<String, u32>,
    }

    /// Where a source's mirror lives: `<project>/.rtk/<path/to/file>.toon`.
    ///
    /// The project is the nearest ancestor holding a `.git`, so every mirror in
    /// a repo lands in one store; outside a repo the source's own directory
    /// stands in. Keeping the relative path means two `services.json` in
    /// different folders never collide, and the basename an agent sees is the
    /// one it asked for.
    pub fn mirror_for(json: &Path) -> PathBuf {
        let root = project_root(json);
        let relative = json
            .strip_prefix(&root)
            .unwrap_or_else(|_| Path::new(json.file_name().unwrap_or(json.as_os_str())));
        root.join(STORE_DIR)
            .join(relative)
            .with_extension(MIRROR_EXT)
    }

    fn project_root(json: &Path) -> PathBuf {
        json.ancestors()
            .find(|dir| dir.join(".git").is_dir())
            .map(Path::to_path_buf)
            .or_else(|| json.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The store ignores itself, so nothing RTK writes can be committed and no
    /// file of the user's is touched to arrange that.
    fn seal_store(mirror: &Path) {
        let Some(store) = mirror.ancestors().find(|d| d.ends_with(STORE_DIR)) else {
            return;
        };
        let ignore = store.join(".gitignore");
        if ignore.exists() {
            return;
        }
        if std::fs::create_dir_all(store).is_ok() {
            let _ = std::fs::write(ignore, "*\n");
        }
    }

    pub struct Served {
        pub mirror: PathBuf,
        /// First time this session that this file is served as a mirror.
        pub first_time: bool,
    }

    fn index_path(config: &Config) -> Option<PathBuf> {
        Some(crate::core::tee::posthook_recall_dir(config)?.join("mirrors.json"))
    }

    fn load(config: &Config) -> Index {
        index_path(config)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    }

    fn save(config: &Config, index: &Index) {
        let Some(path) = index_path(config) else {
            return;
        };
        if let Some(dir) = path.parent() {
            if crate::core::utils::create_private_dir(dir).is_err() {
                return;
            }
        }
        if let Ok(json) = serde_json::to_string(index) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Cheap pre-check: extension, exclusions, size floor.
    pub fn eligible(json: &Path, toon: &ToonConfig) -> bool {
        if !toon.mirrors || json.extension().and_then(|e| e.to_str()) != Some("json") {
            return false;
        }
        let name = json.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if toon.exclude.iter().any(|e| e == name) {
            return false;
        }
        std::fs::metadata(json).is_ok_and(|m| m.is_file() && m.len() as usize >= toon.min_bytes)
    }

    /// Ensure a fresh mirror exists and is worth serving.
    ///
    /// A mirror is only kept when regenerating the source reproduces it byte
    /// for byte — otherwise compiling an edit would silently reformat numbers
    /// or escapes — and when it saves materially more than the note that
    /// announces it costs.
    pub fn ensure(config: &Config, json: &Path) -> Option<Served> {
        if !eligible(json, &config.toon) {
            return None;
        }
        let key = json.display().to_string();
        let mut index = load(config);
        if index.strikes >= MAX_STRIKES || index.raw.contains(&key) || index.skipped.contains(&key)
        {
            return None;
        }
        let skip_key = key.clone();

        let mirror = mirror_for(json);
        if !mirror.exists() || newer(json, &mirror) {
            seal_store(&mirror);
            let Ok(written) = extract_to(json, &mirror) else {
                index.skipped.insert(skip_key);
                save(config, &index);
                return None;
            };
            if !written.byte_identical
                || !worth_it(written.from_bytes, written.to_bytes, &config.toon)
            {
                let _ = std::fs::remove_file(&mirror);
                index.skipped.insert(skip_key);
                save(config, &index);
                return None;
            }
        }

        let first_time = index
            .mirrors
            .insert(mirror.display().to_string(), key)
            .is_none();
        if first_time {
            save(config, &index);
        }
        Some(Served { mirror, first_time })
    }

    /// The floor is set by round trips, not by payload. A measured session
    /// replays ~50k tokens of cached prefix per round trip, so one check an
    /// agent runs because it distrusts the view costs ~17,500 input-equivalent
    /// tokens, while saved payload is worth ~1.85x its token count over the
    /// rest of a session. The default 3000 bytes saved is ~750 tokens, ~1,390
    /// input-equivalents — ahead as long as fewer than ~7% of mirrored reads
    /// provoke a check.
    fn worth_it(source: usize, mirror: usize, toon: &ToonConfig) -> bool {
        let saved = source.saturating_sub(mirror);
        saved >= toon.min_saved_bytes && (mirror as f64) <= (source as f64) * MAX_MIRROR_RATIO
    }

    /// Pin a source to raw bytes for the rest of the session. Used when the
    /// agent asks for a line range, re-reads, or edits the JSON directly —
    /// all signals that it is working against real bytes.
    /// The JSON a mirror belongs to. Session mirrors are looked up in the
    /// index; a mirror made by the `rtk toon` CLI sits beside its source.
    pub fn source_of(config: &Config, mirror: &Path) -> PathBuf {
        load(config)
            .mirrors
            .get(&mirror.display().to_string())
            .map(PathBuf::from)
            .unwrap_or_else(|| source_path(mirror))
    }

    /// Record something that cost a round trip. Past the limit the session
    /// stops mirroring: a bounded loss beats an open-ended one.
    pub fn strike(config: &Config) {
        let mut index = load(config);
        index.strikes = index.strikes.saturating_add(1);
        save(config, &index);
    }

    /// A mirror that would not compile. The first failure is the agent's to
    /// fix; a second on the same file means we are the problem, so the source
    /// goes back to raw bytes and the caller says so.
    pub fn record_failure(config: &Config, mirror: &Path) -> bool {
        let mut index = load(config);
        let count = index
            .failures
            .entry(mirror.display().to_string())
            .or_insert(0);
        *count += 1;
        let give_up = *count >= 2;
        index.strikes = index.strikes.saturating_add(1);
        save(config, &index);
        if give_up {
            let source = source_of(config, mirror);
            pin_raw(config, &source);
        }
        give_up
    }

    pub fn pin_raw(config: &Config, json: &Path) {
        let mut index = load(config);
        if !index.raw.insert(json.display().to_string()) {
            return;
        }
        let mirror = mirror_for(json);
        let key = mirror.display().to_string();
        let unmergeable = index.failures.contains_key(&key);
        if index.mirrors.remove(&key).is_some() && !unmergeable {
            let _ = std::fs::remove_file(&mirror);
            prune_store(&mirror);
        }
        save(config, &index);
    }

    /// Compile anything still pending, then delete every mirror this session
    /// created.
    pub fn retire(config: &Config) -> usize {
        let index = load(config);
        let mut removed = 0;
        for (entry, source) in &index.mirrors {
            let mirror = PathBuf::from(entry);
            if !mirror.exists() {
                continue;
            }
            // Never delete work that was never merged: a mirror that fails to
            // compile is left on disk for the user to inspect.
            let source = PathBuf::from(source);
            if newer(&mirror, &source) && compile_to(&mirror, &source).is_err() {
                continue;
            }
            if std::fs::remove_file(&mirror).is_ok() {
                removed += 1;
                prune_store(&mirror);
            }
        }
        save(config, &Index::default());
        removed
    }

    /// Remove empty directories up to and including the store, taking the
    /// self-ignore file with it once nothing else is left.
    fn prune_store(mirror: &Path) {
        let Some(store) = mirror
            .ancestors()
            .find(|d| d.ends_with(STORE_DIR))
            .map(Path::to_path_buf)
        else {
            return;
        };
        for dir in mirror.ancestors().take_while(|d| *d != store) {
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
        }
        let empty_but_sealed = std::fs::read_dir(&store)
            .into_iter()
            .flatten()
            .all(|entry| {
                entry
                    .as_ref()
                    .map(|e| e.file_name() == ".gitignore")
                    .unwrap_or(false)
            });
        if empty_but_sealed {
            let _ = std::fs::remove_file(store.join(".gitignore"));
            let _ = std::fs::remove_dir(&store);
        }
    }

    fn newer(a: &Path, b: &Path) -> bool {
        match (
            a.metadata().and_then(|m| m.modified()),
            b.metadata().and_then(|m| m.modified()),
        ) {
            (Ok(x), Ok(y)) => x > y,
            _ => false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tempfile::TempDir;

        fn rows(n: usize) -> String {
            let body: Vec<String> = (0..n)
                .map(|i| format!("  {{\n    \"id\": {i},\n    \"name\": \"row_{i}\"\n  }}"))
                .collect();
            format!("[\n{}\n]\n", body.join(",\n"))
        }

        fn env() -> (TempDir, Config, PathBuf) {
            let dir = TempDir::new().expect("tempdir");
            let mut config = Config::default();
            config.tee.directory = Some(dir.path().join("tee"));
            config.toon.min_bytes = 256;
            config.toon.min_saved_bytes = 200;
            let json = dir.path().join("records.json");
            std::fs::write(&json, rows(40)).expect("write");
            (dir, config, json)
        }

        #[test]
        fn test_first_serve_is_flagged_once() {
            let (_dir, config, json) = env();
            let first = ensure(&config, &json).expect("mirror");
            assert!(first.first_time, "announce on the first serve");
            let second = ensure(&config, &json).expect("mirror");
            assert!(!second.first_time, "and never again for this file");
        }

        #[test]
        fn test_retire_removes_and_compiles() {
            let (_dir, config, json) = env();
            let served = ensure(&config, &json).expect("mirror");
            let toon = std::fs::read_to_string(&served.mirror).expect("read");
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&served.mirror, toon.replace("row_1", "renamed")).expect("write");

            assert_eq!(retire(&config), 1);
            assert!(!served.mirror.exists(), "repo left with JSON only");
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(&json).expect("read")).expect("json");
            assert_eq!(value[1]["name"], "renamed", "pending work compiled first");
        }

        #[test]
        fn test_lossy_round_trip_is_never_mirrored() {
            let (dir, config, _json) = env();
            // A \u escape does not survive re-encoding byte for byte.
            let escaped = dir.path().join("escaped.json");
            let body = format!(
                "[\n{}\n]\n",
                (0..40)
                    .map(|i| format!(
                        "  {{\n    \"id\": {i},\n    \"note\": \"dash \\u2014 {i}\"\n  }}"
                    ))
                    .collect::<Vec<_>>()
                    .join(",\n")
            );
            std::fs::write(&escaped, &body).expect("write");

            assert!(
                ensure(&config, &escaped).is_none(),
                "refused: would reformat"
            );
            assert!(!mirror_path(&escaped).exists(), "no mirror left behind");
            assert_eq!(std::fs::read_to_string(&escaped).expect("read"), body);
        }

        #[test]
        fn test_mirror_must_pay_for_itself() {
            let (dir, config, _json) = env();
            // Almost all payload, no structure to strip: TOON cannot win here.
            let poor = dir.path().join("prose.json");
            let prose = "lorem ipsum, dolor sit amet, consectetur adipiscing elit, ".repeat(12);
            let value = serde_json::json!({ "a": prose, "b": prose });
            std::fs::write(&poor, serde_json::to_string_pretty(&value).expect("ser"))
                .expect("write");
            let source = std::fs::metadata(&poor).expect("stat").len() as usize;
            let toon = encode_view(&serde_json::json!({ "a": prose, "b": prose })).expect("toon");
            assert!(
                !worth_it(source, toon.len(), &config.toon),
                "fixture must be a genuine non-win: {source} to {}",
                toon.len()
            );
            assert!(
                ensure(&config, &poor).is_none(),
                "no material saving: not worth a mirror or its note"
            );
            assert!(!mirror_path(&poor).exists());
        }

        #[test]
        fn test_worth_it_thresholds() {
            let toon = ToonConfig::default();
            assert!(worth_it(40_000, 10_000, &toon), "big structural win");
            assert!(
                !worth_it(40_000, 37_000, &toon),
                "small ratio never repays a round trip"
            );
            assert!(
                !worth_it(3_400, 1_000, &toon),
                "2400 bytes saved is under the round-trip floor"
            );
            assert!(!worth_it(1_000, 1_200, &toon), "mirror larger than source");
        }

        #[test]
        fn test_strikes_stop_the_session() {
            let (dir, config, json) = env();
            let other = dir.path().join("other.json");
            std::fs::write(&other, rows(60)).expect("write");

            assert!(ensure(&config, &json).is_some());
            strike(&config);
            assert!(
                ensure(&config, &other).is_none(),
                "one detour costs more than the rest of the session can return"
            );
        }

        #[test]
        fn test_second_compile_failure_hands_the_file_back() {
            let (_dir, config, json) = env();
            let served = ensure(&config, &json).expect("mirror");

            assert!(
                !record_failure(&config, &served.mirror),
                "first failure is the agent's to fix"
            );
            assert!(
                record_failure(&config, &served.mirror),
                "a second means we are the problem"
            );
            assert!(
                ensure(&config, &json).is_none(),
                "source handed back to raw bytes"
            );
            assert!(
                served.mirror.exists(),
                "unmergeable edits are never deleted out from under the agent"
            );
        }

        #[test]
        fn test_retire_keeps_a_mirror_it_cannot_merge() {
            let (_dir, config, json) = env();
            let served = ensure(&config, &json).expect("mirror");
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&served.mirror, "[3]{a,b}:\n  1,2,3,4,5\n").expect("write");

            assert_eq!(retire(&config), 0, "nothing safely removable");
            assert!(served.mirror.exists(), "work the agent did is still there");
        }

        #[test]
        fn test_store_is_workspace_local_and_uncommittable() {
            let (dir, config, json) = env();
            let served = ensure(&config, &json).expect("mirror");

            let store = dir.path().join(".rtk");
            assert!(
                served.mirror.starts_with(&store),
                "mirror lives in the store, not beside the source: {}",
                served.mirror.display()
            );
            assert_eq!(
                std::fs::read_to_string(store.join(".gitignore")).expect("seal"),
                "*\n",
                "the store ignores itself, so nothing here can be committed"
            );
            assert!(
                !json.with_extension("toon").exists(),
                "nothing lands next to the JSON"
            );

            retire(&config);
            assert!(!store.exists(), "store removed once it holds nothing");
        }

        #[test]
        fn test_pin_raw_stops_serving_mirrors() {
            let (_dir, config, json) = env();
            assert!(ensure(&config, &json).is_some());
            pin_raw(&config, &json);
            assert!(
                ensure(&config, &json).is_none(),
                "raw for the rest of the session"
            );
        }

        #[test]
        fn test_eligibility_rules() {
            let (dir, config, json) = env();
            assert!(eligible(&json, &config.toon));
            let small = dir.path().join("small.json");
            std::fs::write(&small, "{\"a\":1}").expect("write");
            assert!(!eligible(&small, &config.toon));
            let pkg = dir.path().join("package.json");
            std::fs::write(&pkg, rows(40)).expect("write");
            assert!(!eligible(&pkg, &config.toon));
        }
    }
}
