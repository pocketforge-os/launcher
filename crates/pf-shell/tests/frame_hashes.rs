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
        "2c7f7a484c52e2ef7680d1f8a9c2a176e50bb405134b9b1938c84cc0c3494631  ",
        "cfa7c2a3e3b34b701a68e9ef152cb89f873b0f57da9dac7d6a76c96745695968  ",
        "5d7bce838f40ce4896e7875c4633569c56221899d84646f95ed7683262d842bd  ",
        "55f2d018d14c2ce0c8dfe81b55d3debc2dcfbb3e33347fd50d8b2af6e7a505e9  ",
        "c79da4ebc7c4c567e1d8a4e30085dbb3cdc1738b4c9da363cdf5faa496b45017  ",
        "2865186161371bfe2f18ca438a7a087faefc1bfb223009f7321b192631b63334  ",
        "071570a0d4db0ea37f85546c780f464d0fc097724cabdca9a3b3fed5818ccbe6  ",
        "110d95adac519f2907cb2f9679749b02a7867d159f783b541f235840414e5490  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("cfa7c2a"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("55f2d01"),
        "Returned must restore focused Home with the just-now acknowledgement"
    );
    for route in [
        "library.png",
        "search.png",
        "details.png",
        "variant-chooser.png",
    ] {
        assert!(lines.contains(route), "missing F10 evidence route {route}");
    }
}

#[test]
fn settings_and_first_run_frame_hashes_are_stable() {
    let out = tempfile::tempdir().unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_pf-shell"))
        .args([
            "--offscreen",
            "--settings-evidence",
            "--out",
            out.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let transcript = String::from_utf8(run.stdout).unwrap();
    assert!(
        transcript.contains("0c93216b0126acff1a6097034338defc86c73b242eaf54be79417fee8e2f3736  ")
    );
    assert!(
        transcript.contains("adcc55c717383ace233142c224a884d76e38cdb0dc461dd29dae72df3354d919  ")
    );
    assert!(out.path().join("settings.png").is_file());
    assert!(out.path().join("first-run.png").is_file());
}
