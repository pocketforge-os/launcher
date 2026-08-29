use std::process::Command;

#[test]
fn vertical_slice_frame_hashes_are_stable() {
    let out = tempfile::tempdir().unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_pf-shell"))
        .args(["--offscreen", "--out", out.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let lines = String::from_utf8(run.stdout).unwrap();
    for expected in [
        "f7050f463f8ccf7cfb94ba7d7ae2cd34fe71f2a02489879912aa6af0a673451d  ",
        "7e270fe3fc68360edd0f779b05a96a6ac4bb2d28a7e6bf607b25764c63ce7640  ",
        "0ef9bae3ddfd085b765b75027cd1b5369d719cf8b5a4c71a48e24d619c8657fe  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("7e270fe3"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("7e270fe3"),
        "Returned must restore the focused Home frame"
    );
}
