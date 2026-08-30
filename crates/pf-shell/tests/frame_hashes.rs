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
            .args(&extra_args)
            .args(["--out", out.path().to_str().unwrap()])
            .env("PF_RASTER_INK_GUARD", "1");
        let run = command.output().unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        if extra_args.is_empty() {
            assert!(
                out.path().join("search.png").is_file(),
                "the guarded evidence route set must include Search"
            );
        }
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
        "2b4f696045b121f4c2fcc29b683a8eef61d313a8b93595b3db22be97a58a1e79  ",
        "7787aa4b8400f8368bce424d413754be3c9487f221d9e8b910c178e4a237a906  ",
        "f4a71f46d9b93b9338252a7294a5ea04e2fab2cfe11c63cf48b23181efeefd55  ",
        "3d1e9c32a8fdb2eb8d0d3910f8fa9d9f36be2a082d388614722b97dc0a31ab2d  ",
        "4b6d2ffe193bab49a645ed8978d803b354d11884fe22965010d35d712efbfd51  ",
        "ca93211b27214a9de4ff88159c5da68bd5dbb5b1208c8cb630f06d98b6e4a94f  ",
        "9deb5d54235f106b8e0b314d79dcce0cfbd238f00b8b5b0db915f7fac0b814de  ",
        "ae8bb54e53c8ef6796534eacf0d5a911895836d07701d0e01239d4289a345a67  ",
        "8f87fea4ece796c834f4d1293e83d10eebe961baf1c7624261f213ff8a9a0721  ",
        "8a0f7b4cdbb95c8e043fe41b6189def734b5fe8a2cd44adcf5721919d96edea8  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("7787aa4"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("3d1e9c3"),
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
        transcript.contains("e381bee34567e1bcb1e7e689167e0db575bc589decd034280d7b9cac0b4f1848  ")
    );
    assert!(
        transcript.contains("d3be44730f104b4bf9ead7aef2cb66d4d2e8db272aa1c5e3f2510ea9a2755ad0  ")
    );
    assert!(
        transcript.contains("720a54816daa35f71c59652e0fc8697a07a2de53ea7cc6c85ea2e301b9a597a2  ")
    );
    assert!(
        transcript.contains("dddc642bf719a535cc5b8ace92637bace4f44bdf13f79f6f0bcf786ec0a25e0b  ")
    );
    assert!(
        transcript.contains("f885eede7587428b04680f32bbc0ba9c6cd71204ed2b5d802d31eb2301db576b  ")
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
        transcript.contains("11a403c7bd8183966d916bd4c6762fc5e9fbed6932de223286302aba4cab4cf6  ")
    );
}
