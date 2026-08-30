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
fn rendered_chrome_contains_sparse_shaped_glyph_ink() {
    let (out, _) = render_offscreen();
    let decoder = png::Decoder::new(std::fs::File::open(out.path().join("boot-home.png")).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let pixels = &pixels[..info.buffer_size()];
    let stride = info.width as usize * 4;

    for (name, x0, y0, width, height) in [
        ("room strip", 420, 16, 440, 32),
        ("status cluster", 1032, 16, 200, 32),
        ("prompt text", 680, 660, 552, 32),
    ] {
        let surface = &pixels[y0 * stride + x0 * 4..y0 * stride + x0 * 4 + 4];
        let mut ink = 0_usize;
        let mut surface_pixels = 0_usize;
        for y in y0..y0 + height {
            for x in x0..x0 + width {
                let offset = y * stride + x * 4;
                if &pixels[offset..offset + 4] == surface {
                    surface_pixels += 1;
                } else {
                    ink += 1;
                }
            }
        }
        let area = width * height;
        assert!(ink > 20, "{name} must contain visible shaped glyph ink");
        assert!(
            surface_pixels > area * 3 / 4,
            "{name} must remain sparse glyphs on the bar, not a uniform chip"
        );
    }
}

#[test]
fn vertical_slice_frame_hashes_are_stable() {
    let (_out, lines) = render_offscreen();
    for expected in [
        "5ccf59cca151e0b176c841483f70154164851698757100585543e82525efc2c1  ",
        "0c516c3fb29948950fdf1ca31d24db27f72f7ec9c5db5ad88c1ce4278c282862  ",
        "9ca038f67d1dce725041bcb5fd9243233e18f49ced7ae05a3d38b3891c2592d1  ",
        "0c516c3fb29948950fdf1ca31d24db27f72f7ec9c5db5ad88c1ce4278c282862  ",
        "da2c7c1430ef38cf1c5dcabfbadab665ca659e7434deb95a2229f88570404c67  ",
        "df2c08d5ee7a172634f3f0ca80f7620472f06e0900720fbd81c4158cd8dddb41  ",
        "afed935e11e9ea10dd19d594f7a18afff9f1bc48b67b27bb654c7a31e1a5c88e  ",
        "603739b294e684636e2284d414c34b59622bfe0bf407ab849669e880c8c0e2ac  ",
        "3a36ad0a0f3dc2406b31f638fec4c919050d61c3f46206529746181c756f31d3  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("0c516c3"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("0c516c3"),
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
        transcript.contains("21143d6b3c7d3b41e4a547d9eca4fe61c30f5a787b7067157ab918e978b457d6  ")
    );
    assert!(
        transcript.contains("656f8a2983bddbe1b26b60e60b220159e2d31c19478cca0c39db9d36161944bd  ")
    );
    assert!(
        transcript.contains("089450b31a6775abc698ae7d551fee216ca466dd773bf5b1857b9bad9187e55e  ")
    );
    assert!(
        transcript.contains("4ff2774d1a072a601dadc125d95896c62f83e2a1df359f1826d72994ec294fc5  ")
    );
    assert!(
        transcript.contains("833eac731dbb4830284f238e544c8d3178c12ed2fde18d45d8a4c33975330392  ")
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
        transcript.contains("7137b458b9648f289d0fefba82c8937e1059007ae3ca5902551d367f5ea904ef  ")
    );
}
