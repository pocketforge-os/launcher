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
        } else {
            assert!(
                out.path().join("settings-edit.png").is_file(),
                "the guarded Settings evidence set must include value-edit mode"
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
        // Home intentionally rebaselines for the corrected attention-pill dot geometry.
        // Details routes rebaseline for the quiet-console layout polish, including
        // the active-variant window, collapsed empty-description slot, and the
        // described fixture's two-line body copy.
        // Search intentionally rebaselines for its single focus owner and chip-only prompt paint.
        "6f9c658df4def86762e94e126564d9657a5c27c5fe5bf31264ad93cab747b244  ",
        "320d9f85666bd44045bd285973c08baaad36f8492534c1178ea7e892a3ced7bd  ",
        "c56f6f95c9edcbb63fec128d4c2600eb4c30a82adb1aab70226668a8c43ba1a4  ",
        "5fff0dfbacc295e040573db074474899b225a462cc2ac4efc24793d4c610749c  ",
        // Quick intentionally rebaselines after flowing its notes below the action rows.
        "cbe491df596ca152b0f850e9f241e34958ac2ab207735adf7f4463d1f620cbcb  ",
        "8f4b67846de0c3450531fa7064874e2187bf914f75ac8fd9804bf8222328c836  ",
        "8f4b67846de0c3450531fa7064874e2187bf914f75ac8fd9804bf8222328c836  ",
        "83618428f80c2376d167435dcae5cc11c575a1e09148e2cca650ffe411397c5d  ",
        "1d524919fe690aefe16595c30785394e876b593392b0abe83c41a1383f0d05ca  ",
        "129850b67760cdbe6c9ec9f167e0140188738474348f7fe1eb86fae48f608156  ",
        "eb3e4049e7b52d92370ca9f7824953835211da8938286313086b44537809e5b3  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("320d9f8"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("5fff0df"),
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
    // tsp-op5a.369 intentionally repaints Settings values as individual chips and adds the
    // focused value-edit frame. Round 3 restores semantic focus ownership to that row, so
    // only the edit frame is repinned; the remaining hashes pin the unchanged route sequence.
    assert!(
        transcript.contains("e2c6c13e17ea149e9942e7f4158e750f5d2b3b719d8a33e27da84d61d471f7f4  ")
    );
    assert!(
        transcript.contains("3837f70659d65037ac0b7bd2267b470c45d70f3046359608f8cbefa8d060ce00  ")
    );
    assert!(
        transcript.contains("5df9261925db52c484f7b89ebfa853d38ce68c7dd8eee96c91d3c33ff1ad9f74  ")
    );
    assert!(
        transcript.contains("d12d4d489e74c69613a3b67d237846a6e497b59f2af0d2923f3d956dff959ea4  ")
    );
    assert!(
        transcript.contains("197e9092bba58f5222bc4f4393d4363ce105be23cd1fac2bf8eb6421ce567064  ")
    );
    assert!(
        transcript.contains("090e9f2e75175128affce48766df3be5974abe4f2f209cc8641989e12f8e48fc  ")
    );
    assert!(out.path().join("settings.png").is_file());
    assert!(out.path().join("settings-edit.png").is_file());
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
        transcript.contains("17562411e168caeef61a00511a4e78add6c4ca6ead0ec87b63d965a45f7a1564  ")
    );
}
