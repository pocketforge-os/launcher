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
        "6330a3eb0ce2fdf07957ca2ffd5cdd7dbb2d35808ebede3df5222ed206baade7  ",
        "123cc0f0ab3b0b1d65c141ab3c59d3786b01048c054be87f95aabeab5a96d8ab  ",
        "0739f93a839dfa3b7b84682635658438fe118123a26646a0833829d02cfeed2c  ",
        "a2296a1361d75624e87e4f8695f8720a370654313d0c7c7fdb13c757d5a83ff8  ",
        "6b10a40afa4146e8f7b899003bcd2ebeed0f4366303b6df1a6bccce011033ddb  ",
        "706d400cd157bf7b74e8159d9fd9126f7e912a7ea4a0924a5d04386701499b33  ",
        "77d087b08b6ac7b847ab8432df3d3bd2f4b033c9bd10d583fa2d11afbf80ada6  ",
        "110d95adac519f2907cb2f9679749b02a7867d159f783b541f235840414e5490  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("123cc0f"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("a2296a1"),
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
