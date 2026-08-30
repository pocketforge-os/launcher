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
fn every_evidence_route_has_rasterized_contrasting_text_ink() {
    for extra_args in [Vec::<&str>::new(), vec!["--settings-evidence"]] {
        let out = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_pf-shell"));
        command
            .arg("--offscreen")
            .args(extra_args)
            .args(["--out", out.path().to_str().unwrap()])
            .env("PF_RASTER_INK_GUARD", "1");
        let run = command.output().unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
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
        "c8b40913465a0ba7138c410ca87870974cc3151c78891f263c0c7a22f767c9bd  ",
        "d16fd816c49c23bbfd50e7d4c600e3afb86f3aeeb6b7bfb771081678502f70b4  ",
        "a58032a14837df70b262c78d34919c353182b24693c4c961860b725b361335dd  ",
        "c4b5fecf7dbd18f9020c8a994f751b0d8f84a8cf0017321fad5f218446672ea0  ",
        "4a71144112f14350c05814ecd05df35c0cd5bb01f8b00bf56ad8704e39ea4cc3  ",
        "c2afae35a7487ff313627f40a8e8909949222bb39b7433ef8c0668a2b426e7d0  ",
        "85b78605aaabb610ce59082dae593be90fd9b44fc2cae38bf14917dc25e29306  ",
        "60e7de23530a5284b05b40e8e4e28f3ed5ab40ef80d617264ae056d548734600  ",
        "5eee4192d6e0b87ec27609badc03905a08e8c426acafea1bc920dbfe7cb58fcb  ",
        "305c0d3ae297b0a91be60bd633b1539ccb003a7bc6ac28a6e7789c92f5b617a1  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("d16fd81"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("c4b5fec"),
        "Returned must restore focused Home with the just-now acknowledgement"
    );
    for route in [
        "library.png",
        "search.png",
        "details.png",
        "details-unavailable.png",
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
        transcript.contains("2817f16c66acb58308c3c06672f6e8a90c566d7a98c1860d5eeaeb6374f1e8e4  ")
    );
    assert!(
        transcript.contains("87d4bc2833def29332b1dc93840ed95ca41318ae2b2ccd77911830cbb0ed2937  ")
    );
    assert!(
        transcript.contains("c836f961b4ee2c57891dbe902f7533600c7c87b658d4c78accfa808e113223b9  ")
    );
    assert!(
        transcript.contains("3675ba020d25cc943f3417a9e69167b7b7b8100ab3f22fe6a1e7ca6bb55eb948  ")
    );
    assert!(
        transcript.contains("bd1849c34b77e2d0fb710f5312057c6613afba5f0688ab49309baf7bc7ba677f  ")
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
        transcript.contains("161400d31083dd9cae9dd3ba61742f4a42d38e1a0ed705dd72abd29b1ba0789c  ")
    );
}
