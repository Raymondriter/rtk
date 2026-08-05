//! `lockfile` layer: summarizes package lockfiles to `name@version` lists.
//!
//! Lockfiles are read-for-consumption content — no agent Edits one via
//! `old_string` (tooling regenerates them), so the fidelity trap doesn't
//! apply. The list keeps the answer agents actually Read them for ("which
//! version is installed") and drops integrity hashes, resolved URLs and
//! graph edges (the recall file preserves them).
//!
//! Type detection is content-based (`source` stays diagnostics-only).
//! Unknown or unparseable content passes through unchanged.

use super::{Layer, LayerCtx, LayerOutcome};
use regex::Regex;
use std::sync::LazyLock;

pub struct LockfileLayer;

impl Layer for LockfileLayer {
    fn name(&self) -> &'static str {
        "lockfile"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        let summary = detect_and_summarize(input);
        match summary {
            Some(s) if s.len() < input.len() => LayerOutcome::ShortCircuit(s),
            _ => LayerOutcome::Continue(input.to_string()),
        }
    }
}

fn detect_and_summarize(input: &str) -> Option<String> {
    let head = &input[..input.len().min(512)];
    if head.trim_start().starts_with('{') && input.contains("\"lockfileVersion\"") {
        return summarize_npm(input);
    }
    if input.contains("[[package]]") {
        return Some(render("Cargo.lock", parse_cargo(input)));
    }
    if head.contains("lockfileVersion:") {
        return Some(render("pnpm-lock.yaml", parse_pnpm(input)));
    }
    if head.contains("yarn lockfile v1") {
        return Some(render("yarn.lock", parse_yarn(input)));
    }
    None
}

fn render(kind: &str, mut packages: Vec<String>) -> String {
    packages.sort();
    packages.dedup();
    let mut out = format!("{}: {} packages\n", kind, packages.len());
    for p in &packages {
        out.push_str(p);
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn summarize_npm(input: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(input).ok()?;
    let mut packages = Vec::new();

    // v2/v3: "packages" map keyed by "node_modules/<name>".
    if let Some(map) = root.get("packages").and_then(|p| p.as_object()) {
        for (key, entry) in map {
            if key.is_empty() {
                continue; // root project entry
            }
            let name = key.rsplit("node_modules/").next().unwrap_or(key);
            if let Some(version) = entry.get("version").and_then(|v| v.as_str()) {
                packages.push(format!("{name}@{version}"));
            }
        }
    } else if let Some(map) = root.get("dependencies").and_then(|p| p.as_object()) {
        // v1 top-level fallback.
        for (name, entry) in map {
            if let Some(version) = entry.get("version").and_then(|v| v.as_str()) {
                packages.push(format!("{name}@{version}"));
            }
        }
    }

    if packages.is_empty() {
        return None;
    }
    Some(render("package-lock.json", packages))
}

fn parse_cargo(input: &str) -> Vec<String> {
    let mut packages = Vec::new();
    let mut name: Option<&str> = None;
    for line in input.lines() {
        if line.starts_with("[[package]]") {
            name = None;
        } else if let Some(rest) = line.strip_prefix("name = \"") {
            name = rest.strip_suffix('"');
        } else if let Some(rest) = line.strip_prefix("version = \"") {
            if let (Some(n), Some(v)) = (name.take(), rest.strip_suffix('"')) {
                packages.push(format!("{n}@{v}"));
            }
        }
    }
    packages
}

// Optional quotes handled without backreferences (unsupported by the regex
// crate): the version charset already excludes the closing quote.
static PNPM_PKG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s{2}/?'?(@?[A-Za-z0-9._/-]+)@([^:('\s]+)'?[:(]").expect("valid regex")
});

fn parse_pnpm(input: &str) -> Vec<String> {
    let mut in_packages = false;
    let mut packages = Vec::new();
    for line in input.lines() {
        if line.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if in_packages {
            if !line.starts_with(' ') && !line.trim().is_empty() {
                break; // left the packages: section
            }
            if let Some(caps) = PNPM_PKG.captures(line) {
                packages.push(format!("{}@{}", &caps[1], &caps[2]));
            }
        }
    }
    packages
}

fn parse_yarn(input: &str) -> Vec<String> {
    let mut current_name: Option<String> = None;
    let mut packages = Vec::new();
    for line in input.lines() {
        if !line.starts_with(' ') && line.ends_with(':') && line.contains('@') {
            // `lodash@^4.17.20, lodash@^4.17.21:` — name from first selector.
            let first = line.trim_end_matches(':').split(',').next().unwrap_or("");
            let first = first.trim().trim_matches('"');
            current_name = first
                .rfind('@')
                .filter(|&i| i > 0)
                .map(|i| first[..i].to_string());
        } else if let Some(rest) = line.trim_start().strip_prefix("version \"") {
            if let (Some(name), Some(version)) = (current_name.take(), rest.strip_suffix('"')) {
                packages.push(format!("{name}@{version}"));
            }
        }
    }
    packages
}

#[cfg(test)]
mod tests {
    use super::super::{ContentFormat, LayerCtx};
    use super::*;
    use crate::core::filter::Language;

    fn ctx() -> LayerCtx<'static> {
        LayerCtx {
            format: ContentFormat::Lockfile,
            lang: Language::Data,
            source: "Cargo.lock",
        }
    }

    fn apply(input: &str) -> String {
        match LockfileLayer.apply(input, &ctx()) {
            LayerOutcome::Continue(out) | LayerOutcome::ShortCircuit(out) => out,
        }
    }

    #[test]
    fn test_real_cargo_lock_summarized() {
        // RTK's own lockfile: real captured content by definition.
        let input = include_str!("../../../Cargo.lock");
        let out = apply(input);
        assert!(out.starts_with("Cargo.lock: "), "header: {}", &out[..40]);
        assert!(out.contains("serde@"), "known dep present");
        assert!(out.contains("regex@"), "known dep present");
    }

    #[test]
    fn test_real_cargo_lock_savings_at_least_60_percent() {
        let input = include_str!("../../../Cargo.lock");
        let out = apply(input);
        let savings = 100.0 - (out.len() as f64 / input.len() as f64 * 100.0);
        assert!(savings >= 60.0, "expected ≥60%, got {savings:.1}%");
    }

    #[test]
    fn test_npm_v3_packages_map() {
        let input = r#"{
  "name": "app", "lockfileVersion": 3,
  "packages": {
    "": { "name": "app", "version": "1.0.0" },
    "node_modules/lodash": { "version": "4.17.21", "integrity": "sha512-xxx" },
    "node_modules/@scope/pkg": { "version": "2.0.0" },
    "node_modules/a/node_modules/b": { "version": "0.1.0" }
  }
}"#;
        let out = apply(input);
        assert!(out.starts_with("package-lock.json: 3 packages"));
        assert!(out.contains("lodash@4.17.21"));
        assert!(out.contains("@scope/pkg@2.0.0"));
        assert!(out.contains("b@0.1.0"), "nested name resolved: {out}");
        assert!(!out.contains("integrity"), "hashes dropped");
    }

    #[test]
    fn test_yarn_v1() {
        let input = "# THIS IS AN AUTOGENERATED FILE.\n# yarn lockfile v1\n\n\nlodash@^4.17.20, lodash@^4.17.21:\n  version \"4.17.21\"\n  resolved \"https://registry.yarnpkg.com/lodash\"\n\n\"@scope/pkg@^1.0.0\":\n  version \"1.2.3\"\n";
        let out = apply(input);
        assert!(out.contains("yarn.lock: 2 packages"), "{out}");
        assert!(out.contains("lodash@4.17.21"));
        assert!(out.contains("@scope/pkg@1.2.3"));
    }

    #[test]
    fn test_pnpm_lock() {
        let input = "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n\npackages:\n\n  lodash@4.17.21:\n    resolution: {integrity: sha512-xxx}\n\n  '@scope/pkg@2.0.0':\n    resolution: {integrity: sha512-yyy}\n";
        let out = apply(input);
        assert!(out.contains("pnpm-lock.yaml: 2 packages"), "{out}");
        assert!(out.contains("lodash@4.17.21"));
        assert!(out.contains("@scope/pkg@2.0.0"));
    }

    #[test]
    fn test_unknown_content_passes_through() {
        let input = "just a plain file\nwith some lines\n";
        assert_eq!(apply(input), input);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(apply(""), "");
    }
}
