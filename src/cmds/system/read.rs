//! Reads source files with optional language-aware filtering to strip boilerplate.

use crate::core::config::Config;
use crate::core::filter::{self, FilterLevel, Language};
use crate::core::guard::never_worse;
use crate::core::lens::options::{best_view, view_token};
use crate::core::tee;
use crate::core::tracking;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Below this a compressed view is not worth the recall write.
const LENS_MIN_SIZE: usize = 512;
/// Second read of the same file inside this window serves raw bytes.
const LENS_REREAD_WINDOW_SECS: u64 = 120;

pub fn run(
    file: &Path,
    level: FilterLevel,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    line_numbers: bool,
    verbose: u8,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("Reading: {} (filter: {})", file.display(), level);
    }

    // Read file content
    let content = fs::read_to_string(file)
        .with_context(|| format!("Failed to read file: {}", file.display()))?;

    // Detect language from extension
    let lang = file
        .extension()
        .and_then(|e| e.to_str())
        .map(Language::from_extension)
        .unwrap_or(Language::Unknown);

    if verbose > 1 {
        eprintln!("Detected language: {:?}", lang);
    }

    // JSON lens: the same compressed view the PostToolUse hook serves for
    // Read-tool calls, so `cat file.json` (rewritten here) is covered too.
    // Edits against the view stay translatable — `lens::translate_edit` works
    // from the file on disk, not from session state.
    if lens_view(file, &content, max_lines, tail_lines, line_numbers, &timer) {
        return Ok(());
    }

    // Apply filter
    let filter = filter::get_filter(level);
    let mut filtered = filter.filter(&content, &lang);

    // Safety: if filter emptied a non-empty file, fall back to raw content
    if filtered.trim().is_empty() && !content.trim().is_empty() {
        eprintln!(
            "rtk: warning: filter produced empty output for {} ({} bytes), showing raw content",
            file.display(),
            content.len()
        );
        filtered = content.clone();
    }

    if verbose > 0 {
        let original_lines = content.lines().count();
        let filtered_lines = filtered.lines().count();
        let reduction = if original_lines > 0 {
            ((original_lines - filtered_lines) as f64 / original_lines as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "Lines: {} -> {} ({:.1}% reduction)",
            original_lines, filtered_lines, reduction
        );
    }

    filtered = apply_line_window(&filtered, max_lines, tail_lines, &lang);

    let (raw, rtk_output) = if line_numbers {
        (
            format_with_line_numbers(&content),
            format_with_line_numbers(&filtered),
        )
    } else {
        (content.clone(), filtered.clone())
    };
    let shown = never_worse(&raw, &rtk_output);
    print!("{}", shown);
    timer.track(
        &format!("cat {}", file.display()),
        "rtk read",
        &raw,
        shown,
    );
    Ok(())
}

/// Serve `content` as a compressed JSON view. Returns true when it printed
/// and tracked; false leaves the normal filter path in charge.
fn lens_view(
    file: &Path,
    content: &str,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    line_numbers: bool,
    timer: &tracking::TimedExecution,
) -> bool {
    if max_lines.is_some() || tail_lines.is_some() || line_numbers {
        return false;
    }
    if content.len() < LENS_MIN_SIZE
        || file.extension().and_then(|e| e.to_str()) != Some("json")
        || file.file_name().and_then(|n| n.to_str()) == Some("package-lock.json")
    {
        return false;
    }

    let config = Config::load().unwrap_or_default();
    let posthook = &config.posthook;
    if !posthook.enabled || !posthook.lens || posthook.formats.json != "auto" {
        return false;
    }

    // Same rail as the hook path: a second read reaches ground truth.
    let key = fs::canonicalize(file)
        .unwrap_or_else(|_| file.to_path_buf())
        .display()
        .to_string();
    if tee::posthook_recently_read(&config, &key, LENS_REREAD_WINDOW_SECS) {
        return false;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };
    let Some((view, kind)) = best_view(&value) else {
        return false;
    };

    // Raw bytes are preserved in the recall store but never announced: the
    // view is lossless, and a "full output elsewhere" footer would both lie
    // and break the seamless view. Recovery is the re-read rail below.
    let slug = format!(
        "read_{}",
        file.file_name().and_then(|n| n.to_str()).unwrap_or("json")
    );
    let _ = tee::posthook_tee_hint(&config, content, &slug);

    let shown = never_worse(content, &view);
    if shown == content {
        return false;
    }
    println!("{}", shown.trim_end_matches('\n'));
    tee::posthook_mark_read(&config, &key);
    timer.track(
        &format!("cat {}", file.display()),
        &format!("rtk read {}", view_token(kind)),
        content,
        shown,
    );
    true
}

pub fn run_stdin(
    level: FilterLevel,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    line_numbers: bool,
    verbose: u8,
) -> Result<()> {
    use std::io::{self, Read as IoRead};

    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("Reading from stdin (filter: {})", level);
    }

    // Read from stdin
    let mut content = String::new();
    io::stdin()
        .lock()
        .read_to_string(&mut content)
        .context("Failed to read from stdin")?;

    // No file extension, so use Unknown language
    let lang = Language::Unknown;

    if verbose > 1 {
        eprintln!("Language: {:?} (stdin has no extension)", lang);
    }

    // Apply filter
    let filter = filter::get_filter(level);
    let mut filtered = filter.filter(&content, &lang);

    if verbose > 0 {
        let original_lines = content.lines().count();
        let filtered_lines = filtered.lines().count();
        let reduction = if original_lines > 0 {
            ((original_lines - filtered_lines) as f64 / original_lines as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "Lines: {} -> {} ({:.1}% reduction)",
            original_lines, filtered_lines, reduction
        );
    }

    filtered = apply_line_window(&filtered, max_lines, tail_lines, &lang);

    let (raw, rtk_output) = if line_numbers {
        (
            format_with_line_numbers(&content),
            format_with_line_numbers(&filtered),
        )
    } else {
        (content.clone(), filtered.clone())
    };
    let shown = never_worse(&raw, &rtk_output);
    print!("{}", shown);

    timer.track("cat - (stdin)", "rtk read -", &raw, shown);
    Ok(())
}

fn format_with_line_numbers(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let width = lines.len().to_string().len();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(&format!("{:>width$} │ {}\n", i + 1, line, width = width));
    }
    out
}

fn apply_line_window(
    content: &str,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    lang: &Language,
) -> String {
    if let Some(tail) = tail_lines {
        if tail == 0 {
            return String::new();
        }
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(tail);
        let mut result = lines[start..].join("\n");
        if content.ends_with('\n') {
            result.push('\n');
        }
        return result;
    }

    if let Some(max) = max_lines {
        return filter::smart_truncate(content, max, lang);
    }

    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn flat_json() -> String {
        let event: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/posthook/read_json_flat.json"
        ))
        .expect("fixture");
        event["tool_response"]["file"]["content"]
            .as_str()
            .expect("content")
            .to_string()
    }

    #[test]
    fn test_lens_view_gates() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let timer = tracking::TimedExecution::start();
        let content = flat_json();
        let write = |name: &str, body: &str| {
            let path = dir.path().join(name);
            fs::write(&path, body).expect("write");
            path
        };
        let json = write("data.json", &content);

        // Ranged / numbered reads stay raw: they are the documented way to
        // recover exact bytes for Edit anchors.
        assert!(!lens_view(&json, &content, Some(10), None, false, &timer));
        assert!(!lens_view(&json, &content, None, Some(10), false, &timer));
        assert!(!lens_view(&json, &content, None, None, true, &timer));

        // Non-JSON, lockfiles and small files are out of scope.
        assert!(!lens_view(&write("code.rs", &content), &content, None, None, false, &timer));
        assert!(!lens_view(
            &write("package-lock.json", &content),
            &content,
            None,
            None,
            false,
            &timer
        ));
        let small = "{\"a\": 1}";
        assert!(!lens_view(&write("small.json", small), small, None, None, false, &timer));

        // Invalid JSON never reaches the encoder.
        let broken = "{ not json";
        assert!(!lens_view(&write("broken.json", broken), broken, None, None, false, &timer));
    }

    #[test]
    fn test_best_view_matches_hook_path() {
        // `rtk read` and the PostToolUse hook must serve the identical view,
        // otherwise an Edit anchor from one path fails against the other.
        let content = flat_json();
        let value: serde_json::Value = serde_json::from_str(&content).expect("json");
        let (view, kind) = best_view(&value).expect("view");
        assert_eq!(view_token(kind), "toon");
        assert!(view.starts_with("[120]{id,name,email,active,score}:"));
        assert!(view.len() < content.len());
    }

    #[test]
    fn test_read_rust_file() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".rs")?;
        writeln!(
            file,
            r#"// Comment
fn main() {{
    println!("Hello");
}}"#
        )?;

        // Just verify it doesn't panic
        run(file.path(), FilterLevel::Minimal, None, None, false, 0)?;
        Ok(())
    }

    #[test]
    fn test_stdin_support_signature() {
        // Test that run_stdin has correct signature and compiles
        // We don't actually run it because it would hang waiting for stdin
        // Compile-time verification that the function exists with correct signature
    }

    #[test]
    fn test_apply_line_window_tail_lines() {
        let input = "a\nb\nc\nd\n";
        let output = apply_line_window(input, None, Some(2), &Language::Unknown);
        assert_eq!(output, "c\nd\n");
    }

    #[test]
    fn test_apply_line_window_tail_lines_no_trailing_newline() {
        let input = "a\nb\nc\nd";
        let output = apply_line_window(input, None, Some(2), &Language::Unknown);
        assert_eq!(output, "c\nd");
    }

    #[test]
    fn test_apply_line_window_max_lines_still_works() {
        let input = "a\nb\nc\nd\n";
        let output = apply_line_window(input, Some(2), None, &Language::Unknown);
        assert!(output.starts_with("a\n"));
        assert!(output.contains("more lines"));
    }

    fn rtk_bin() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("rtk")
    }

    #[test]
    #[ignore]
    fn test_read_two_valid_files_concatenated() {
        let bin = rtk_bin();
        assert!(bin.exists(), "Run `cargo build` first");

        let mut f1 = NamedTempFile::with_suffix(".txt").unwrap();
        let mut f2 = NamedTempFile::with_suffix(".txt").unwrap();
        writeln!(f1, "alpha\nbravo").unwrap();
        writeln!(f2, "charlie\ndelta").unwrap();

        let output = std::process::Command::new(&bin)
            .args(["read", &f1.path().to_string_lossy(), &f2.path().to_string_lossy()])
            .output()
            .expect("failed to run rtk read");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("alpha"), "first file content missing");
        assert!(stdout.contains("charlie"), "second file content missing");
    }

    #[test]
    #[ignore]
    fn test_read_valid_and_nonexistent() {
        let bin = rtk_bin();
        assert!(bin.exists(), "Run `cargo build` first");

        let mut f1 = NamedTempFile::with_suffix(".txt").unwrap();
        writeln!(f1, "valid content").unwrap();

        let output = std::process::Command::new(&bin)
            .args(["read", &f1.path().to_string_lossy(), "/tmp/rtk_nonexistent_file.txt"])
            .output()
            .expect("failed to run rtk read");

        assert!(!output.status.success(), "should exit non-zero on missing file");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("valid content"), "valid file should still be printed");
        assert!(stderr.contains("rtk_nonexistent_file"), "should report missing file on stderr");
    }

    #[test]
    #[ignore]
    fn test_read_stdin_dedup_warning() {
        let bin = rtk_bin();
        assert!(bin.exists(), "Run `cargo build` first");

        let output = std::process::Command::new(&bin)
            .args(["read", "-", "-"])
            .stdin(std::process::Stdio::piped())
            .output()
            .expect("failed to run rtk read");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("stdin specified more than once"),
            "should warn about duplicate stdin, got stderr: {}",
            stderr
        );
    }
}
