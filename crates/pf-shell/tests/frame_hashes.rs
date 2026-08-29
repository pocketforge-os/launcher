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
        "53ea31646acb0395ad7a5acfca5c49ca9c686c678411a5626329487ac3451fbf  ",
        "8d7e15b513cf12dfe3a799a452377de4282ca474eb10c15100e97055a58ce472  ",
        "13745e6a2297a1f40945db9ff8a0a33e4197baf9c291ee6cd562b1a31ab63eff  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("8d7e15b"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("8d7e15b"),
        "Returned must restore the focused Home frame"
    );
}
