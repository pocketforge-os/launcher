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
        "0e7d64e079601cb90102968d3262ac9f4d2a126867fe8948ad5e4a475b56c451  ",
        "ed834653df734bce10b27975dd9902fb522cfbb2adbea1b540d2082a36199915  ",
        "d183cf77d9516f39e2dca6f2c86ccb57b2fad5dc29d1c39acc0fdd7e99558a45  ",
        "f86789258a9934905e37842352710f863fb40ffacf15e34dfb04f836053d0d66  ",
        "63e8481d64f1603e3eb0bb465b1c267711bba582d4658d9c136e2c791fc8b3d3  ",
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
        transcript.contains("8c219ffa1a5d1fecb5a04b2ffa12ce3f6ab5a10d12942cc3a39b665549e906b4  ")
    );
    assert!(
        transcript.contains("89aff19d00e5c2de5c5e7a3d8c83bc0b5341df963e1b55c839a334db809db6b0  ")
    );
    assert!(
        transcript.contains("ddbcb6a272918daceb3aaa7f7bb9e347a0191bd7ebd229892814399c37ce4d17  ")
    );
    assert!(
        transcript.contains("386de425da34d23814ef155b20adc33198b9170d14865298be102ca7fb0a56c9  ")
    );
    assert!(
        transcript.contains("bc5c6597d77cd4fd749b11329b82110530f4e672cd5f8db05e9520a71ad3af4b  ")
    );
    assert!(out.path().join("settings.png").is_file());
    assert!(out.path().join("controls.png").is_file());
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
