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
        "ff4937d23307cbd20fe438f33eaacd545d899cbb503a32cc80787ee6f6b0a7c7  ",
        "e3d1cce4b375d1c0ccde3df6cb2bb76f7f71f612907a07514dfd89d9670aa8ae  ",
        "f9f4e1a90ac2092ba013078f9e160e400aac247e35e4a97800770a4669c0a8f2  ",
        "4862da723def81878c1ca491b37cd6b20361de9d2cac5c234807d3c241587a6f  ",
        "c79da4ebc7c4c567e1d8a4e30085dbb3cdc1738b4c9da363cdf5faa496b45017  ",
        "2865186161371bfe2f18ca438a7a087faefc1bfb223009f7321b192631b63334  ",
        "071570a0d4db0ea37f85546c780f464d0fc097724cabdca9a3b3fed5818ccbe6  ",
        "110d95adac519f2907cb2f9679749b02a7867d159f783b541f235840414e5490  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("e3d1cce"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("4862da7"),
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
