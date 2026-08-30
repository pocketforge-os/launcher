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
        "efd2bdf2e3ebe4befce4e5009bcf1e84822cd31b2bc9dd501b5cdbfbbf6de1f9  ",
        "fcaac191c257db1b6d7dc40d5ca980b497f0ef4a7024390217097fd60348ba40  ",
        "637aad4857e3a5b6e7426c42a82fd086cfa8742e207c92ffd7edd381b9c8720b  ",
        "29bed51e5d9565b2b6084e4c74723a2a8c878f41b71b87bbcf6e4be25f884e08  ",
        "b4b1fc33101c43efe51179beb78ba06b7ca5cb8847c58af937ec0062204cc091  ",
        "9e5af8cfe8681341a8a859ec74b2ca8608367abf4bc4478ec8b44dd4f68d8ad5  ",
        "03b5ed9a5677c9e01e5c83e70b1f1589612c8083881077c029693d453eb15f34  ",
        "e64003101db290871f23a9bc373d97664a72586e1c5060f333d42850d788ce1a  ",
        "26befca17a60ff2cc8f3f14689c784a4b7bfe160e94367b8e5c4366b95bd0e77  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("fcaac19"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("29bed51"),
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
        transcript.contains("04046b7574d88c5761538aec2a3a5c8540d81e98e05c9684bbc672f03bcc29a6  ")
    );
    assert!(
        transcript.contains("d678f4484682b2a7dca24fba0a8e457c282976ec6fae1b72be4c314f8b4f6a09  ")
    );
    assert!(
        transcript.contains("ce8aca7c7d0d2be0c0341b76f2b04416fd97f3542aa35c1b88f6e33b8a1ba917  ")
    );
    assert!(
        transcript.contains("bc79d3fae93c97c431917b53fdcb54760840de9bb0d8b79b1a00b853b7a3259f  ")
    );
    assert!(
        transcript.contains("289f1563c9c41c01c337c1fdb16fe1f71db1a620913a862f73659d248cf7ad01  ")
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
        transcript.contains("54a4bdf53f57f38300095c89d1ec3cf4c5a830b4c9ee04c5f0879e0a3522a336  ")
    );
}
