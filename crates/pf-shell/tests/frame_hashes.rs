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
        "b0d0ed23332350bb578f8df35cd7de44e7537d99ebb3ac67ced3ee0dd51b1740  ",
        "22ea7720fc4d31190edbcc9233b5b066cb857f20f054cb4edd3258fe250ffbde  ",
        "f9f312daa73af1597cdcb6ae17e23b0b0dbed4f053e58046ece48858ce3d877d  ",
        "65842bcadce8f526b28518794d5ac1bbe5cdd1f717f01beb19eae7993474c654  ",
        "35b2375a868970ad1b720b66a71d77b75c5df75c623a529cf6cf4b8f2f39493a  ",
        "2865186161371bfe2f18ca438a7a087faefc1bfb223009f7321b192631b63334  ",
        "071570a0d4db0ea37f85546c780f464d0fc097724cabdca9a3b3fed5818ccbe6  ",
        "110d95adac519f2907cb2f9679749b02a7867d159f783b541f235840414e5490  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("22ea772"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("65842bc"),
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
