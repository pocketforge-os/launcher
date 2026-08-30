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
        "bbb8e3a168a9ae9de298c9b7fcb5cf2e22a174496b3918970106fb561ca18bd2  ",
        "aa9b4b532b90d62fe0b09f1a3023140afd32b59c86004e21ec8c080fca4b4723  ",
        "f8509018858de165d23b84ef0f21b4d1c52ed87bb992dfe2b27c0169703cee48  ",
        "1a83f55c28c5865da52bcc8f65031f4581bba735607f842f1e142404d1df7568  ",
        "4b6d2ffe193bab49a645ed8978d803b354d11884fe22965010d35d712efbfd51  ",
        "3594646a49212ff0a2516bd5838a6c032edf08f2b08749ec5ffeb01d578f98c4  ",
        "9deb5d54235f106b8e0b314d79dcce0cfbd238f00b8b5b0db915f7fac0b814de  ",
        "f48303ac97853d9c9676faba34a49a8ddf949a44359b2b36aa945acb5b4ac750  ",
        "0c9e2dba2301532c174a2c9738bb8975ce1a6de6dd24fc0a3d946b6b79f86961  ",
        "8a0f7b4cdbb95c8e043fe41b6189def734b5fe8a2cd44adcf5721919d96edea8  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("aa9b4b5"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("1a83f55"),
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
        transcript.contains("c913508b5a2bbc543bc6d034d5a0e875865013c695ba2537c6c1fad307ed3e67  ")
    );
    assert!(
        transcript.contains("9d32b1bca0ffefbbc9e199ea3facabfa4eb48fb65ba61e4b7eb5f96dfe111e2c  ")
    );
    assert!(
        transcript.contains("07d52133220c7b543d7203dd683f25d86f45715b34507a0d60effb279339a023  ")
    );
    assert!(
        transcript.contains("2b7d01de6e8d0ee49bf148b772d08562a9c08524de9ca72c1317819fefe9fa70  ")
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
        transcript.contains("f4cb7e68ad64a347e34409e8be6247be1ffc059bd4f0d52e55283b84b966b3a7  ")
    );
}
