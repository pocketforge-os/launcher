//! `--shell-bin ... --format json` must emit VALID JSON on stdout. The renderer
//! (`pf-shell --offscreen`) prints one hash line per artifact; if the audit lets
//! it inherit stdout, those lines land before `main`'s ledger JSON and corrupt
//! it for any machine consumer. This is the red-first backing for capturing the
//! child's stdout in `render_slugs` (a subprocess test, so it observes the real
//! process-level stdout the reviewer's flow hits).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Marker the fake renderer prints to stdout — stands in for `pf-shell`'s
/// per-artifact hash lines.
const HASH_MARKER: &str = "HASH deadbeef0000 settings.png";

#[test]
fn shell_bin_stdout_does_not_corrupt_json_output() {
    let renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // A valid `settings` scene (root at the audit space + one mapped node) so the
    // audit runs to the point of printing its ledger. `settings` needs no
    // rasters, so no PNGs are required.
    let semantic = "root role=Group label=\"\" bounds=0.0,0.0,1280.0,720.0 state=NodeState { focused: false, selected: false } action=None\n  status-cluster role=Text label=\"82 9:41\" bounds=1144.0,16.0,152.0,32.0 state=NodeState { focused: false, selected: false } action=None\n";
    std::fs::write(renders.path().join("settings.semantic.txt"), semantic).unwrap();

    // A fake `--shell-bin` that only prints a hash line to stdout, exactly as the
    // real renderer does. It writes nothing (the scene is pre-staged above).
    let fake = renders.path().join("fake-shell.sh");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\necho \"{HASH_MARKER}\"\nexit 0\n"),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let bin = env!("CARGO_BIN_EXE_fidelity-audit");
    let dir = crate_dir();
    let output = Command::new(bin)
        .args([
            "--crate-dir",
            dir.to_str().unwrap(),
            "--repo-root",
            dir.join("../..").to_str().unwrap(),
            "--renders-dir",
            renders.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
            "--route",
            "quiet-console/settings",
            "--shell-bin",
            fake.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("audit binary runs");

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf8");
    assert!(
        !stdout.contains(HASH_MARKER),
        "the renderer's stdout leaked into the audit's stdout:\n{stdout}"
    );
    serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap_or_else(|e| {
        panic!("--format json must emit valid JSON on stdout (parse error: {e}):\n{stdout}")
    });
}
