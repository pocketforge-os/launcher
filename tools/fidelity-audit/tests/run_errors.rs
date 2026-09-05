//! A REAL error (a missing render artifact) must make `run` fail, so the CI
//! step surfaces it instead of a `continue-on-error` swallowing it. This is the
//! red-first backing for dropping `continue-on-error` on the ci.yml audit step:
//! report mode exits 0 on divergences, but a broken input is a hard error.

use std::path::{Path, PathBuf};

use fidelity_audit::{Config, OutputFormat, run};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A minimal valid semantic snapshot (one parseable record) so scene loading
/// succeeds and the raster-integrity checks are what's under test.
const MINIMAL_SEMANTIC: &str = "  n role=Group label=\"\" bounds=0.0,0.0,1.0,1.0 state=NodeState { focused: false, selected: false } action=None\n";

fn config_for(renders: &Path, out: &Path) -> Config {
    let dir = crate_dir();
    Config {
        crate_dir: dir.clone(),
        repo_root: dir.join("../.."),
        renders_dir: renders.to_path_buf(),
        out_dir: out.to_path_buf(),
        shell_bin: None,
        routes: Some(vec!["quiet-console/home".to_string()]),
        gate: false,
        format: OutputFormat::Table,
    }
}

#[test]
fn run_errors_when_the_scene_snapshot_is_missing() {
    let renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let err = run(&config_for(renders.path(), out.path()))
        .expect_err("a missing render must be a hard error, not a silent pass");
    assert!(
        err.contains("semantic snapshot") || err.contains("read"),
        "unexpected error: {err}"
    );
}

#[test]
fn run_errors_when_a_required_png_is_absent() {
    // Semantic present, but the shell PNG a crop/color comparator needs is absent.
    let renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    std::fs::write(
        renders.path().join("boot-home.semantic.txt"),
        MINIMAL_SEMANTIC,
    )
    .unwrap();
    let err = run(&config_for(renders.path(), out.path()))
        .expect_err("an absent required PNG must be a hard error");
    assert!(
        err.contains("open png") || err.contains("boot-home.png"),
        "unexpected error: {err}"
    );
}

#[test]
fn run_errors_when_a_required_png_is_corrupt() {
    // Semantic present; the shell PNG exists but is not a valid PNG.
    let renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    std::fs::write(
        renders.path().join("boot-home.semantic.txt"),
        MINIMAL_SEMANTIC,
    )
    .unwrap();
    std::fs::write(renders.path().join("boot-home.png"), b"this is not a PNG").unwrap();
    let err = run(&config_for(renders.path(), out.path()))
        .expect_err("a corrupt required PNG must be a hard error");
    assert!(err.contains("png"), "unexpected error: {err}");
}
