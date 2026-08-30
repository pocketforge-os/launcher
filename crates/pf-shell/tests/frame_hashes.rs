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
        "25e81439ea858d1a863fd7640c165b4ffa78ffdee47af2e2d6c2de38324d6cb5  ",
        "53887481b035321bbde2d902c7d56461674b856e0d44442e99882824fb13dd49  ",
        "08d497c16f3511f7abc6b7664a520daf7c990017b3c7ee7e1de6b4e03bf72a2a  ",
        "53887481b035321bbde2d902c7d56461674b856e0d44442e99882824fb13dd49  ",
        "e918cb5bdef79ff59ff74dc80816a097a5ddea2f72c3d0150dfdb6129e6aa5a0  ",
        "accc43e0b8f6cc52b6501d2376f30404e5946109e75335ed28b7c439a792ff5f  ",
        "84fdeb3db0c9dd4143732c39251da322cc3616afe38702e147de8e95495c0c93  ",
        "2074069911e03f056d02cf323bac74888d96f6e9b9f39fefc9be55d4862dd1f8  ",
        "9e06259a9514ed99c08777d3b11be2b350828b5846f596a44e866a996548e9bb  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("5388748"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("5388748"),
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
        transcript.contains("40004562ab402d3ec8caa9a0929b0379ce011435ae36e8d528b93fec965dfda4  ")
    );
    assert!(
        transcript.contains("c7d54ee2de89e3f467af1ee47d5051336b9dc7dbc2d773d44fd31e06144d4418  ")
    );
    assert!(
        transcript.contains("fdaf390ba94bc4e39d66f42eb06b2dc97d54263fa448260600b237719e8d67bf  ")
    );
    assert!(
        transcript.contains("3aa69c288d3761ab161c2c1b90ce8d51c335dc1753b072385ddc47df0176555b  ")
    );
    assert!(
        transcript.contains("677fe72dd1a209e63cf00dba25d04436d4f7a80e93bb415a67926f1f02ed5db2  ")
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
        transcript.contains("8fc57858be546f509a87350f50155592a2369dd9de671bba8fa1d6e6f1eeb5f5  ")
    );
}
