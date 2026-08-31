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
fn every_presented_evidence_route_has_action_labels_and_rasterized_text_ink() {
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
    let mut ink = 0_usize;
    for y in 144..216 {
        for x in 48..1232 {
            let offset = y * stride + x * 4;
            if pixels[offset] > 180 && pixels[offset + 1] > 180 && pixels[offset + 2] > 180 {
                ink += 1;
            }
        }
    }
    let area = 72 * 1184;
    assert!(ink > 100, "hero title must produce visible glyph ink");
    assert!(
        ink < area / 4,
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
        let mut ink = 0_usize;
        for y in y0..y0 + height {
            for x in x0..x0 + width {
                let offset = y * stride + x * 4;
                if pixels[offset] > 175 && pixels[offset + 1] > 175 && pixels[offset + 2] > 175 {
                    ink += 1;
                }
            }
        }
        let area = width * height;
        assert!(ink > 20, "{name} must contain visible shaped glyph ink");
        assert!(
            ink < area / 4,
            "{name} must remain sparse glyphs on the bar, not a uniform chip"
        );
    }
}

#[test]
fn vertical_slice_frame_hashes_are_stable() {
    let (_out, lines) = render_offscreen();
    for expected in [
        "06b496499746aa41fabe68d03785b1f4289cd7e2fbad5678febb71705062a706  ",
        "cccd3930e99cb94770be5388185ae97bc4c2c9a231d6236efbe90541b2749c6e  ",
        "5bff134d8f1d049178cc736a03441e1e9785eb9c8b97c14af80cd4130fa73182  ",
        "25c8003bde1227ddbe269f2a7b5ecdcbf49c45d16475cae4d630922519a2647d  ",
        "af7350a01923eabb9a682f78270df688bde5148284edb5a97bdea3b56d16b831  ",
        "a2ed5d1ce581f1d99428b771265ad0481c899c035dece83db6395419db0f6b63  ",
        "08ccbb0bbe77a723aeae5e1bddc2c9067f011bea87a98dc975ff7a3f26695478  ",
        "0042aef93a2d63dc349992f82719f40b35753a734afd7f4f9e3cc5596dfe3a38  ",
        "fd602138f3d263994e9c1b3efafebed89f5dcbe4639d83ac968a93e4d146833b  ",
        "898b2fc02063ac869e87de689f00e46bf7ad61e6489caed837aa9295cb8af103  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("cccd393"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("25c8003"),
        "Returned must restore focused Home with the just-now acknowledgement"
    );
    for route in [
        "library.png",
        "library-focused-search.png",
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
        transcript.contains("773f429710f9e0b0c18df25dbbdcaefcf92acbf1e3c3c5f9663c3ef9f09be022  ")
    );
    assert!(
        transcript.contains("af05965a0e9e94913af8917d1e28cbcf93bfe5bd0367d2468a46a1e00aa134b7  ")
    );
    assert!(
        transcript.contains("bf67859336cc7fd212dec8dd9fec72db6ca52be4c78b8c1ec7afae014170bd45  ")
    );
    assert!(
        transcript.contains("25bee8d7f3f8aa1e8ee0c676911e15be34caf75da681193f3984fe555893ab18  ")
    );
    assert!(
        transcript.contains("e47b76a58371db516db5c2d6ce54702aeb7e04000899f5c715c7c1e244ab6652  ")
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
        transcript.contains("5f7d4e71576af4efa6f02a8decc6053bf3eb10425442528734b43af3458247c2  ")
    );
}
