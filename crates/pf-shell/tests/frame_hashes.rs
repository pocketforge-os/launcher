use std::process::Command;

fn render_offscreen() -> (tempfile::TempDir, String) {
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
    (out, String::from_utf8(run.stdout).unwrap())
}

#[test]
fn rendered_home_uses_shaped_hero_glyphs_without_a_solid_text_slab() {
    let (out, _) = render_offscreen();
    let decoder = png::Decoder::new(std::fs::File::open(out.path().join("boot-home.png")).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let pixels = &pixels[..info.buffer_size()];
    assert_eq!((info.width, info.height), (1280, 720));
    assert_eq!(info.color_type, png::ColorType::Rgba);

    let stride = info.width as usize * 4;
    let canvas = &pixels[100 * stride + 10 * 4..100 * stride + 10 * 4 + 4];
    let mut ink = 0_usize;
    let mut canvas_pixels = 0_usize;
    for y in 144..216 {
        for x in 48..1232 {
            let offset = y * stride + x * 4;
            if &pixels[offset..offset + 4] == canvas {
                canvas_pixels += 1;
            } else {
                ink += 1;
            }
        }
    }
    let area = 72 * 1184;
    assert!(ink > 100, "hero title must produce visible glyph ink");
    assert!(
        canvas_pixels > area * 3 / 4,
        "hero title must be sparse shaped glyphs, not a solid extent rectangle"
    );
}

#[test]
fn vertical_slice_frame_hashes_are_stable() {
    let (_out, lines) = render_offscreen();
    for expected in [
        "d5adc836135159caf9ce62aad45f3a283e0a14ec82e95f4d48eeba26c6298d9c  ",
        "9105ec8978cfd89948fc380cbf5128e0520aab13426cd881abbee54284f7417e  ",
        "2e07811a3b7f98becdb757c9dc0cea2b33d27690f7f032a87a95832dba1ccfcf  ",
        "81914dc5e21c977dbe00c98fc3baa348e01fb433b4ca77a0ea797d4c0f720218  ",
        "28490456a795300b745cd9e01b6a26e2b92dbe40d6409c7090945a84b0efdb6e  ",
        "94c50c67b5627e9f7fa4716dc7ce933d8054ba3e7c74a4f460ed2dcff0ea8dee  ",
        "cae2da9b528958a28475468afd08c0fe0ef0d996e02ef4e5f0aa8820e7e9d486  ",
        "86facf30fed289e73ad1b6c5753fbdb6c8358731d8ed2c3f2f5157ed2cad1295  ",
        "3bc2c66fb218042edb7660a04aa70e5ff047fb34ae294f6d1f98dc4d51b09c55  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("9105ec8"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("81914dc"),
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
        transcript.contains("55df39487a518c5e70cf885ee36774d8bae3ed94fbc5a6e8f695d2e02c82b90d  ")
    );
    assert!(
        transcript.contains("839c9ceb1aa07ed6417659685a9de4e92f40af8a2f190919475a6c0b7f8a8a12  ")
    );
    assert!(
        transcript.contains("94232ec020a3a0ff5e341987dff67cf214ad6aabbc2c1a08424b6d944b7956c7  ")
    );
    assert!(
        transcript.contains("0187488f1031c473a78a83b0a850785c80cfa93bf39f1ad4337671b654dccfa1  ")
    );
    assert!(
        transcript.contains("9463bbbc9578fe8c9f208e6c6d380aee1d14d8796d8e1b339109e12bca3fae22  ")
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
        transcript.contains("1ca57d38a0a57afa633d87522877f9a48dea22f2e67cb58f7fe9c412597f4261  ")
    );
}
