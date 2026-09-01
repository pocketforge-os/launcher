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
fn two_fresh_process_offscreen_runs_are_byte_identical() {
    let (first, _) = render_offscreen();
    let (second, _) = render_offscreen();
    for entry in std::fs::read_dir(first.path()).unwrap() {
        let entry = entry.unwrap();
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "png")
        {
            let name = entry.file_name();
            assert_eq!(
                std::fs::read(entry.path()).unwrap(),
                std::fs::read(second.path().join(name)).unwrap()
            );
        }
    }
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
fn rendered_attention_pill_keeps_text_on_one_row_with_horizontal_padding() {
    let out = tempfile::tempdir().unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_pf-shell"))
        .args([
            "--offscreen",
            "--device-fixtures",
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

    let decoder = png::Decoder::new(std::fs::File::open(out.path().join("boot-home.png")).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let pixels = &pixels[..info.buffer_size()];
    let stride = info.width as usize * 4;

    // Dusk secondary text is a light, nearly neutral tone. Restricting the scan to
    // the text half of the pill excludes its amber status dot and surrounding wash.
    let mut ink_rows = Vec::new();
    let mut min_x = usize::MAX;
    let mut max_x = 0;
    for y in 77..110 {
        let mut row_has_ink = false;
        for x in 1070..1217 {
            let offset = y * stride + x * 4;
            let [red, green, blue] = [pixels[offset], pixels[offset + 1], pixels[offset + 2]];
            if red > 120
                && green > 120
                && blue > 120
                && red.abs_diff(green) < 35
                && green.abs_diff(blue) < 35
            {
                row_has_ink = true;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
            }
        }
        if row_has_ink {
            ink_rows.push(y);
        }
    }

    assert!(
        !ink_rows.is_empty(),
        "attention message must produce raster ink"
    );
    assert!(
        ink_rows.last().unwrap() - ink_rows[0] < 15,
        "attention text must occupy one caption line-height band, got ink rows {ink_rows:?}"
    );
    assert!(
        min_x >= 1070 && 1232 - max_x >= 15,
        "capsule must enclose text ink with about 16px right padding (ink x={min_x}..={max_x})"
    );
}

#[test]
fn vertical_slice_frame_hashes_are_stable() {
    let (_out, lines) = render_offscreen();
    for expected in [
        // Home prompt rows intentionally rebaseline to the design's outline-only
        // keycaps and plain labels. The footer background now remains visible through
        // every chip; routes which do not use the shared builder remain byte-identical.
        "52cca2ace632091c8ff4efa005baf271807803f92cecb923d1bb20e74dc28329  ",
        "a7df90196a6dc180038920c0f5a05e6299938968ea7ccf9354cd71c88a534086  ",
        "11d43c63fcffb43dbbbc79321eb33816c2243b024aa254bc0996abcab0651ea2  ",
        "d5825aab35d0ab5c9a3eb40fd9c84604571b3a9b250c9942ae3539c19da3b2d1  ",
        "95bdb92d1d35a55f305f24ba0262ba6d9e246023fe905bccedbcf9b1e958ed71  ",
        "8f4b67846de0c3450531fa7064874e2187bf914f75ac8fd9804bf8222328c836  ",
        "8f4b67846de0c3450531fa7064874e2187bf914f75ac8fd9804bf8222328c836  ",
        "b47565735bcca17ad020e3f92bcfa7c305f1291aa717aaa1ba7df102bc62287d  ",
        "04e8e637e69f97837eadb942c44c46fa2e0157816c04e1e71cb7f3149ce51c69  ",
        "98b40ebeb9b7dfb31701daeb0978a2205e2a2eda3c3e35b3e0ec632a1609238d  ",
        "eb3e4049e7b52d92370ca9f7824953835211da8938286313086b44537809e5b3  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("a7df901"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("d5825aa"),
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
        transcript.contains("793a495db1a8ffee63764e92d8ad10a959c13225bdb9df2ee9f1d8698e3f6b33  ")
    );
    assert!(
        transcript.contains("da09b3c191dc879ff6960cded0dc97cd5ad0686fd8ab0d237d68eb506175b365  ")
    );
    assert!(
        transcript.contains("c1e4695389de93a178b51468e78b53e47740704eea704558270933bffdcc1cbd  ")
    );
    assert!(
        transcript.contains("e0282ed7cc23199b2acfdf7a7b48ad77f80d05b6fb51378f54fc607167ed0132  ")
    );
    assert!(
        transcript.contains("090e9f2e75175128affce48766df3be5974abe4f2f209cc8641989e12f8e48fc  ")
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
        transcript.contains("9b06d1ece637410188613c1cd80eb127ab0299626ea91d7d40fe1588ed9d9217  ")
    );
}
