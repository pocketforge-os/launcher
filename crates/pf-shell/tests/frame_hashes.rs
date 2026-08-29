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
        "4e3e64cdfd2ba452c1835355331fb30428340d4a8ea465effc8f75a41d0cf099  ",
        "4f546b616eaa524b55e4d60520b55790401e8c45f9c579669805d87bf5adfb90  ",
        "5cfd55fa7a4d3090a6b578cf3d16b8a2587d607e9278b4f186db6cb76005c0dd  ",
        "e25ba195b7db3aeccb09c5e3bc64139b49237b5cad28da56877b4e7f96334906  ",
        "6fec954c9e50c6d149bf6636129c3217baf6be1fae5190d2f5f2f4259298d049  ",
        "706d400cd157bf7b74e8159d9fd9126f7e912a7ea4a0924a5d04386701499b33  ",
        "77d087b08b6ac7b847ab8432df3d3bd2f4b033c9bd10d583fa2d11afbf80ada6  ",
        "110d95adac519f2907cb2f9679749b02a7867d159f783b541f235840414e5490  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("4f546b6"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("e25ba19"),
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
