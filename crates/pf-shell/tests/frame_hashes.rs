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
        "7277db404512f709bec030d2ae245649ba504398d285b2d6100ae92a3a07917c  ",
        "f7f8c6002224f0d378592601beb240f0c42d13438bf4e38618ac21bb2c976ba5  ",
        "5826ccad7cf184f8de93b72a4a0ce0bbda4ac93e75824f7770222ab179720c59  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("f7f8c600"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("f7f8c600"),
        "Returned must restore the focused Home frame"
    );
}
