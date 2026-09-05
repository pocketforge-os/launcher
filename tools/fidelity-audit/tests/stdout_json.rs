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

    // A fake `--shell-bin` that behaves like the real renderer: it prints a hash
    // line to stdout AND emits the `settings` scene into its `--out` dir (root at
    // the audit space + one mapped node). It must emit the artifact because the
    // audit clears stale outputs before rendering (the provenance guard), so a
    // renderer that wrote nothing would (correctly) fail with a missing artifact
    // rather than exercising the stdout path this test is about. `settings` needs
    // no rasters, so no PNG is required.
    let fake = renders.path().join("fake-shell.sh");
    let scene_body = "root role=Group label=\"\" bounds=0.0,0.0,1280.0,720.0 state=NodeState { focused: false, selected: false } action=None\n  status-cluster role=Text label=\"82 9:41\" bounds=1144.0,16.0,152.0,32.0 state=NodeState { focused: false, selected: false } action=None\n";
    // Built by concatenation (not `format!`) so the scene's `NodeState { }` braces
    // need no escaping. The `--out` scan mirrors how the audit invokes the shell.
    let script = String::from("#!/bin/sh\n")
        + "out=\"\"\n"
        + "while [ $# -gt 0 ]; do\n"
        + "  if [ \"$1\" = \"--out\" ]; then out=\"$2\"; shift 2; else shift; fi\n"
        + "done\n"
        + "echo \""
        + HASH_MARKER
        + "\"\n"
        + "cat > \"$out/settings.semantic.txt\" <<'SCENE'\n"
        + scene_body
        + "SCENE\n"
        + "exit 0\n";
    std::fs::write(&fake, script).unwrap();
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
