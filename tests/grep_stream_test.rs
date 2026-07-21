//! Streaming stdin behavior of `rtk grep` (#2962 phase 2): matches must be
//! emitted as they arrive, capped with a recovery hint at cap time, and the
//! engine's exit codes preserved.

use std::io::Write;
use std::process::{Command, Stdio};

fn rtk() -> Command {
    let tmp = std::env::temp_dir().join("rtk_stream_test");
    let mut c = Command::new(env!("CARGO_BIN_EXE_rtk"));
    c.env("RTK_TEE_DIR", tmp.join("tee"));
    c.env("RTK_DB_PATH", tmp.join("test.db"));
    c
}

fn run_with_stdin_bytes(args: &[&str], input: &[u8]) -> (String, i32) {
    let mut child = rtk()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rtk");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait rtk");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn run_with_stdin(args: &[&str], input: &str) -> (String, i32) {
    run_with_stdin_bytes(args, input.as_bytes())
}

#[test]
fn stdin_grep_caps_with_hint_at_cap_time() {
    let input: String = (1..=60).map(|i| format!("match line {}\n", i)).collect();
    let (stdout, code) = run_with_stdin(&["grep", "line"], &input);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 26, "25 matches + hint, got:\n{}", stdout);
    assert!(lines[0].starts_with("1:"));
    assert!(lines[25].contains("+more in (standard input)"));
    assert!(lines[25].contains("tail -n +26"));
}

#[test]
fn stdin_grep_small_stream_uncapped_no_hint() {
    let input = "a match\nno hit\nanother match\n";
    let (stdout, code) = run_with_stdin(&["grep", "match"], input);
    assert_eq!(code, 0);
    assert_eq!(stdout.lines().count(), 2);
    assert!(!stdout.contains("+more"));
}

#[test]
fn stdin_grep_non_utf8_line_does_not_abort_stream() {
    // A single invalid byte used to end the stream: 1 of 3 lines delivered
    // with exit 0.
    let (stdout, code) = run_with_stdin_bytes(
        &["grep", "-a", "match"],
        b"match A\nmatch B \xff\nmatch C\n",
    );
    assert_eq!(code, 0);
    assert_eq!(stdout.lines().count(), 3, "got:\n{}", stdout);
    assert!(stdout.contains("match C"));
}

#[test]
fn stdin_grep_binary_stream_keeps_engine_exit_code() {
    // Forcing -I flipped this to silence + exit 1; real grep reports the
    // match and exits 0.
    let (_, code) = run_with_stdin_bytes(&["grep", "match"], b"some match\x00data\n");
    assert_eq!(code, 0);
}

#[test]
fn stdin_grep_exit_codes_preserved() {
    let (stdout, code) = run_with_stdin(&["grep", "absent"], "nothing here\n");
    assert_eq!(code, 1);
    assert!(stdout.is_empty());

    let (_, code) = run_with_stdin(&["grep", "here"], "something here\n");
    assert_eq!(code, 0);
}

/// A killed pipeline must still have delivered the capped head + hint, and
/// the write-through tee must hold the overflow (buffer-to-EOF delivered
/// nothing, #2962).
#[cfg(unix)]
#[test]
#[ignore] // timing-sensitive; run with: cargo test --ignored
fn stdin_grep_delivers_before_eof_and_tee_survives_kill() {
    let tee_dir = std::env::temp_dir().join(format!("rtk_stream_kill_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tee_dir);
    // Page the binary in so a cold start cannot consume the kill window.
    let _ = Command::new(env!("CARGO_BIN_EXE_rtk"))
        .arg("--version")
        .output();
    let pipeline = format!(
        "(for i in $(seq 1 4000); do echo \"tick $i ERROR padpadpadpadpadpad\"; sleep 0.005; done) | '{}' grep ERROR",
        env!("CARGO_BIN_EXE_rtk")
    );
    let out = Command::new("timeout")
        .args(["8", "bash", "-c", &pipeline])
        .env("RTK_TEE_DIR", &tee_dir)
        .env(
            "RTK_DB_PATH",
            std::env::temp_dir().join("rtk_stream_test").join("test.db"),
        )
        .output()
        .expect("run pipeline");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().count() >= 26,
        "head + hint must be delivered before EOF, got:\n{}",
        stdout
    );
    assert!(stdout.contains("+more in (standard input)"));

    let tee_files: Vec<_> = std::fs::read_dir(&tee_dir)
        .expect("tee dir exists")
        .flatten()
        .collect();
    assert_eq!(tee_files.len(), 1, "one write-through tee file expected");
    let tee = std::fs::read_to_string(tee_files[0].path()).expect("tee content");
    // More lines than the emitted cap proves write-through of the overflow.
    assert!(
        tee.lines().count() > 25,
        "tee must capture overflow while streaming, got {} lines",
        tee.lines().count()
    );
    let _ = std::fs::remove_dir_all(&tee_dir);
}
