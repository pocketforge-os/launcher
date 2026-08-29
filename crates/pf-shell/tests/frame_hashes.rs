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
        "36f2a9fc452dd81aa1398eef89ca9d0354e3ab38af86d72f0dc64bdf3f1a4ad1  ",
        "f3b4393dbf774b994575170e27365bf0dc0a1e05743105a3fff2d4aec44a1d12  ",
        "55faddfc1dbdc0128c04bb9d04b1c32d0be877d23decc4438ad015370d8a1f23  ",
        "d175c3d9c508cdba62185f81d159ef6c5bc4243619cb451f5600b44972393a46  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("f3b4393"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("d175c3d"),
        "Returned must restore focused Home with the just-now acknowledgement"
    );
}
