//! A REAL error (a missing render artifact) must make `run` fail, so the CI
//! step surfaces it instead of a `continue-on-error` swallowing it. This is the
//! red-first backing for dropping `continue-on-error` on the ci.yml audit step:
//! report mode exits 0 on divergences, but a broken input is a hard error.

use std::path::{Path, PathBuf};

use fidelity_audit::{Config, OutputFormat, run};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A minimal valid semantic snapshot: the scene root at the documented audit
/// coordinate space (so `require_coordinate_space` passes and the raster /
/// node-integrity checks are what each test actually exercises), plus one child.
const MINIMAL_SEMANTIC: &str = "root role=Group label=\"\" bounds=0.0,0.0,1280.0,720.0 state=NodeState { focused: false, selected: false } action=None\n  n role=Group label=\"\" bounds=0.0,0.0,1.0,1.0 state=NodeState { focused: false, selected: false } action=None\n";

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

#[test]
fn run_errors_when_the_scene_is_at_the_wrong_coordinate_space() {
    // A scene whose root is not the documented audit space would produce a
    // plausible-but-wrong geometry ledger; run() must reject it as a hard input
    // error (proves require_coordinate_space is wired in, before any comparison).
    let renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let wrong_space = "root role=Group label=\"\" bounds=0.0,0.0,1024.0,768.0 state=NodeState { focused: false, selected: false } action=None\n";
    std::fs::write(renders.path().join("boot-home.semantic.txt"), wrong_space).unwrap();
    let err = run(&config_for(renders.path(), out.path()))
        .expect_err("a wrong-viewport scene must be a hard input error");
    assert!(err.contains("coordinate space"), "unexpected error: {err}");
}

#[test]
fn run_errors_on_an_unknown_route_id() {
    // A typo'd --route must be a hard error, not a zero-route empty-ledger exit 0.
    let renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let dir = crate_dir();
    let config = Config {
        crate_dir: dir.clone(),
        repo_root: dir.join("../.."),
        renders_dir: renders.path().to_path_buf(),
        out_dir: out.path().to_path_buf(),
        shell_bin: None,
        routes: Some(vec!["quiet-console/does-not-exist".to_string()]),
        gate: false,
        format: OutputFormat::Table,
    };
    let err = run(&config).expect_err("an unknown route id must be rejected");
    assert!(err.contains("unknown --route"), "unexpected error: {err}");
}

#[test]
fn a_missing_required_scene_node_is_a_gating_divergence() {
    // Route `settings` needs no rasters (no crop/color), so a hand-written
    // semantic dump omitting required nodes exercises the node-absence path.
    // The dump has only `status-cluster`, so `.sys` resolves but every other
    // required settings node is absent -> gating divergences (--gate would fail).
    let renders = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let only_status = "root role=Group label=\"\" bounds=0.0,0.0,1280.0,720.0 state=NodeState { focused: false, selected: false } action=None\n  status-cluster role=Text label=\"82 9:41\" bounds=1144.0,16.0,152.0,32.0 state=NodeState { focused: false, selected: false } action=None\n";
    std::fs::write(renders.path().join("settings.semantic.txt"), only_status).unwrap();
    let dir = crate_dir();
    let config = Config {
        crate_dir: dir.clone(),
        repo_root: dir.join("../.."),
        renders_dir: renders.path().to_path_buf(),
        out_dir: out.path().to_path_buf(),
        shell_bin: None,
        routes: Some(vec!["quiet-console/settings".to_string()]),
        gate: false,
        format: OutputFormat::Table,
    };
    let result = run(&config).expect("settings audits with no rasters");
    assert!(
        result.ledger.gating() > 0,
        "a missing required scene node must produce a gating divergence; ledger: {:?}",
        result.ledger.findings
    );
    assert!(
        result
            .ledger
            .findings
            .iter()
            .any(|f| f.fact_class == "mapping" && f.is_gating()),
        "expected a gating mapping divergence"
    );
}
