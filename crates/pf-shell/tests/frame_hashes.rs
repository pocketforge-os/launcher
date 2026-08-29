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
        "89a6e4556c8ccb62b0ac7229afd8549e36af44d07303f9fe65bf13d565fb3ad4  ",
        "de488ecb1cf8556f38754e5b90db5c7ec1bca3643e141b429a30e8129571e286  ",
        "846fb94fe9b9d170cf6c74f284eea2b36154adc8e59d2d1052883ecbadab7441  ",
        "f4023267f79cbe4be8d7fcbd1f2013495334155f385441160662248dfc2dbc12  ",
        "65582e5306cfecc2774e5c5635072b66f659cf9c037e8505bcfd5fcaf298088b  ",
        "706d400cd157bf7b74e8159d9fd9126f7e912a7ea4a0924a5d04386701499b33  ",
        "77d087b08b6ac7b847ab8432df3d3bd2f4b033c9bd10d583fa2d11afbf80ada6  ",
        "110d95adac519f2907cb2f9679749b02a7867d159f783b541f235840414e5490  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("de488ec"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("f402326"),
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
