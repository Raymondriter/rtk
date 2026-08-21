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

/// JSON -> TOON mirror. Refuses to write a mirror that does not decode back
/// to the same value.
pub fn extract(json_path: &Path) -> Result<Written> {
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
    let out = mirror_path(json_path);
    std::fs::write(&out, &toon).with_context(|| format!("Failed to write {}", out.display()))?;

    Ok(Written {
        path: out,
        from_bytes: raw.len(),
        to_bytes: toon.len(),
        byte_identical: regenerated == raw,
    })
}

/// TOON mirror -> JSON. Verifies before writing; on any doubt the existing
/// JSON is left untouched.
pub fn compile(mirror: &Path) -> Result<Written> {
    let toon = std::fs::read_to_string(mirror)
        .with_context(|| format!("Failed to read {}", mirror.display()))?;
    let value = decode_view(&toon).with_context(|| {
        format!(
            "{} is not valid TOON (check row count and field list)",
            mirror.display()
        )
    })?;
    // Verify: re-encoding the decoded value must reproduce the mirror.
    match encode_view(&value) {
        Some(re) if re.trim_end() == toon.trim_end() => {}
        Some(_) => bail!(
            "{} decodes but does not re-encode identically; refusing to write",
            mirror.display()
        ),
        None => bail!("{} could not be re-encoded", mirror.display()),
    }

    // Match the existing source's formatting so a compile produces a minimal
    // diff, including whether it ends with a newline.
    let out = source_path(mirror);
    let existing = std::fs::read_to_string(&out).ok();
    let style = existing.as_deref().map(Style::detect).unwrap_or(Style {
        indent: Some("  ".into()),
    });
    let trailing_newline = existing
        .as_deref()
        .map(|e| e.ends_with('\n'))
        .unwrap_or(true);
    let rendered = render_json(&value, &style, trailing_newline)?;
    std::fs::write(&out, &rendered)
        .with_context(|| format!("Failed to write {}", out.display()))?;

    Ok(Written {
        path: out,
        from_bytes: toon.len(),
        to_bytes: rendered.len(),
        byte_identical: true,
    })
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
    fn test_invalid_mirror_never_touches_source() {
        let dir = TempDir::new().expect("tempdir");
        let raw = fixture();
        let json = setup(&dir, &raw);
        let mirror = extract(&json).expect("extract").path;

        // Stale row count: strict decode must reject it.
        let toon = std::fs::read_to_string(&mirror).expect("read");
        std::fs::write(&mirror, toon.replace("[120]", "[999]")).expect("write");

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
    use std::collections::BTreeSet;

    /// A mirror must be this much smaller than its source to be worth serving.
    const MAX_MIRROR_RATIO: f64 = 0.8;
    /// ...and must save at least this many bytes, several times the cost of
    /// the one-line note that announces it.
    const MIN_SAVED_BYTES: usize = 200;

    #[derive(Debug, Default, Serialize, Deserialize)]
    struct Index {
        /// Mirrors this session created.
        mirrors: BTreeSet<String>,
        /// Sources that failed the mirror policy; never retried this session.
        skipped: BTreeSet<String>,
        /// Sources pinned to raw bytes for the rest of the session.
        raw: BTreeSet<String>,
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
        if index.raw.contains(&key) || index.skipped.contains(&key) {
            return None;
        }

        let mirror = mirror_path(json);
        if !mirror.exists() || newer(json, &mirror) {
            let Ok(written) = extract(json) else {
                index.skipped.insert(key);
                save(config, &index);
                return None;
            };
            if !written.byte_identical || !worth_it(written.from_bytes, written.to_bytes) {
                let _ = std::fs::remove_file(&mirror);
                index.skipped.insert(key);
                save(config, &index);
                return None;
            }
        }

        let first_time = index.mirrors.insert(mirror.display().to_string());
        if first_time {
            save(config, &index);
        }
        Some(Served { mirror, first_time })
    }

    fn worth_it(source: usize, mirror: usize) -> bool {
        let saved = source.saturating_sub(mirror);
        saved >= MIN_SAVED_BYTES && (mirror as f64) <= (source as f64) * MAX_MIRROR_RATIO
    }

    /// Pin a source to raw bytes for the rest of the session. Used when the
    /// agent asks for a line range, re-reads, or edits the JSON directly —
    /// all signals that it is working against real bytes.
    pub fn pin_raw(config: &Config, json: &Path) {
        let mut index = load(config);
        if !index.raw.insert(json.display().to_string()) {
            return;
        }
        let mirror = mirror_path(json);
        if index.mirrors.remove(&mirror.display().to_string()) {
            let _ = std::fs::remove_file(&mirror);
        }
        save(config, &index);
    }

    /// Compile anything still pending, then delete every mirror this session
    /// created.
    pub fn retire(config: &Config) -> usize {
        let index = load(config);
        let mut removed = 0;
        for entry in &index.mirrors {
            let mirror = PathBuf::from(entry);
            if !mirror.exists() {
                continue;
            }
            if newer(&mirror, &source_path(&mirror)) {
                let _ = compile(&mirror);
            }
            if std::fs::remove_file(&mirror).is_ok() {
                removed += 1;
            }
        }
        save(config, &Index::default());
        removed
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
                !worth_it(source, toon.len()),
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
            assert!(worth_it(10_000, 4_000), "big structural win");
            assert!(!worth_it(10_000, 9_000), "10 percent is not worth a note");
            assert!(!worth_it(600, 500), "ratio fine, absolute saving too small");
            assert!(!worth_it(1_000, 1_200), "mirror larger than source");
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
