//! End-to-end stdout-purity coverage for the `pf-input-feed` binary.
//!
//! When the stream sink is stdout — the default (no `--out`) and the explicit
//! `--out -` form — the COMPLETE captured stdout must be exactly the
//! `input_event` records of the focus walk: the `fed=N wall_ms=M` status line
//! rides stderr, never the binary stream (review finding, tsp-ihne1.56.5).
//! A consumer reading stdout to EOF must not see a trailing partial record.

use pf_input_feed::{INPUT_EVENT_SIZE, KEY_LEFT, KEY_RIGHT, encode_press_frame};
use std::process::Command;

fn captured_output(extra_args: &[&str]) -> (Vec<u8>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_pf-input-feed"))
        .args(["--count", "2", "--interval-ms", "0"])
        .args(extra_args)
        .output()
        .expect("pf-input-feed must spawn");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "exit failed; stderr: {stderr}");
    (output.stdout, stderr)
}

fn assert_pure_focus_walk_stream(stdout: &[u8], stderr: &str) {
    // focus_walk(2) = RIGHT then LEFT, one 4-record press frame each.
    let mut expected = Vec::with_capacity(2 * 4 * INPUT_EVENT_SIZE);
    expected.extend_from_slice(&encode_press_frame(KEY_RIGHT));
    expected.extend_from_slice(&encode_press_frame(KEY_LEFT));
    assert_eq!(
        stdout.len(),
        expected.len(),
        "stdout must be exactly two press frames with no status text appended \
         (got {} bytes, expected {})",
        stdout.len(),
        expected.len()
    );
    assert_eq!(
        stdout,
        expected.as_slice(),
        "stdout is not the pure event stream"
    );
    assert!(
        stderr.contains("fed=2"),
        "the status line belongs on stderr; stderr was {stderr:?}"
    );
}

#[test]
fn default_stdout_sink_carries_only_event_records() {
    let (stdout, stderr) = captured_output(&[]);
    assert_pure_focus_walk_stream(&stdout, &stderr);
}

#[test]
fn explicit_dash_stdout_sink_carries_only_event_records() {
    let (stdout, stderr) = captured_output(&["--out", "-"]);
    assert_pure_focus_walk_stream(&stdout, &stderr);
}
