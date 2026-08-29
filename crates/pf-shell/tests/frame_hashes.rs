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
        "c2c6f818bfd01342343a1148950d7806ad041e444a8ff31272d1a1cefc0a92ba  ",
        "93114ecbafa64b9295fb1addb6b3114b8951ecc31eae66bbe2735a716d1649aa  ",
        "075dcb04c0f528628d2787b6125fb87589ed070e97fc18ba00898e8f675afb8f  ",
        "b271ec1254b3f94f4a67b9a664e3ee2d25fce834648c914c74de4d4e15925ba1  ",
        "706d400cd157bf7b74e8159d9fd9126f7e912a7ea4a0924a5d04386701499b33  ",
        "ad807586d64f93559fdd39038daa86f06efed3476013cffbeefd11bbbc29c4e0  ",
        "77d087b08b6ac7b847ab8432df3d3bd2f4b033c9bd10d583fa2d11afbf80ada6  ",
        "110d95adac519f2907cb2f9679749b02a7867d159f783b541f235840414e5490  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("93114ec"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("b271ec1"),
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
