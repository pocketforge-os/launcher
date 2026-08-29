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
        "4188b2c46754a1299434b8a17fa0b666b532ef37de42752f1de7ec0b7acdf853  ",
        "eb78dc5de69fdaf549628655d0a5add6e327b78eb5cc43f136d4938ff878f6cf  ",
        "2660a0b02b6f76a4c15e14123249329c435093a217d77f5251f24bae3cf77f81  ",
        "ef5b132eeeb0924d6f8d57045cda86bd3dfee524f2d474bbdaa3583d4aa7f546  ",
        "d584050e83bbbd958ac0e973aef16e5ea90312b1185fabf1aaf9a7d9a80ca41f  ",
        "ad8ddb6d6fd29e5c6da41ff3189131690fecec1291eaccfa267b3276888a837e  ",
        "bec81a1ebe5c6e809310297768114173bd3b4222cda61f10c677774508872e6c  ",
        "8fbb8df010458167231169ba0668283a8790ed56ee14fd1aaa513a5afcc19b18  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("eb78dc5"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("ef5b132"),
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
