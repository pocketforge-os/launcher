//! A REAL error (a missing render artifact) must make `run` fail, so the CI
//! step surfaces it instead of a `continue-on-error` swallowing it. This is the
//! red-first backing for dropping `continue-on-error` on the ci.yml audit step:
//! report mode exits 0 on divergences, but a broken input is a hard error.

use std::path::PathBuf;

use fidelity_audit::{Config, OutputFormat, run};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn run_errors_when_a_render_artifact_is_missing() {
    let empty_renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let dir = crate_dir();
    let config = Config {
        crate_dir: dir.clone(),
        repo_root: dir.join("../.."),
        renders_dir: empty_renders.path().to_path_buf(),
        out_dir: out.path().to_path_buf(),
        shell_bin: None, // no render step -> the semantic snapshot is absent
        routes: Some(vec!["quiet-console/home".to_string()]),
        gate: false,
        format: OutputFormat::Table,
    };
    let err = run(&config).expect_err("a missing render must be a hard error, not a silent pass");
    assert!(
        err.contains("semantic snapshot") || err.contains("read"),
        "unexpected error: {err}"
    );
}
