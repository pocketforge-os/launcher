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
        "f784ca679181183cef5a368efb2ac16785f39afffcdf8219e34ced2dc6d3475a  ",
        "14f95fdcd4875cf08c1109df11e74f690ab1208eaf6fda9c5ebd3a3afcc196c3  ",
        "8d5491d8be2e776fb817783c1211dd5b87aea4874dec45b985d231ac8081e0c4  ",
        "2d5bf8bfe2ba06eb7967adab05de52de47bfd6a73bfd1ce14f1189496183a374  ",
        "7406be94de19901de71d391fc4e9d0da4f7eaa001ff21df4d586839e467b44cb  ",
        "ed834653df734bce10b27975dd9902fb522cfbb2adbea1b540d2082a36199915  ",
        "d183cf77d9516f39e2dca6f2c86ccb57b2fad5dc29d1c39acc0fdd7e99558a45  ",
        "20930cc253e2f9b91753d764f1f288ebc2ff66092ecb839218080e667ad0ec89  ",
        "c536f4a909a8e2bc41958884baf187e77cf4e6c24c607536c15c98692daf15af  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("14f95fd"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("2d5bf8b"),
        "Returned must restore focused Home with the just-now acknowledgement"
    );
    for route in [
        "library.png",
        "search.png",
        "details.png",
        "variant-chooser.png",
        "quick-power.png",
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
    assert!(
        transcript.contains("ddbcb6a272918daceb3aaa7f7bb9e347a0191bd7ebd229892814399c37ce4d17  ")
    );
    assert!(
        transcript.contains("945fbb288770a59408b10970e2925059a2f17ec5c912e28a6dd4354c4b929e9a  ")
    );
    assert!(out.path().join("settings.png").is_file());
    assert!(out.path().join("network.png").is_file());
    assert!(out.path().join("system.png").is_file());
    assert!(out.path().join("first-run.png").is_file());
}

#[test]
fn degraded_authority_status_indicator_frame_hash_is_stable() {
    let out = tempfile::tempdir().unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_pf-shell"))
        .args([
            "--offscreen",
            "--session-unavailable",
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
        transcript.contains("79693b01d9a8f7b916a062209af509011e62181ea133e842eaf461ea30ceeb34  ")
    );
}
