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
