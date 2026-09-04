use std::process::Command;

/// Build the offscreen command that produces frame-hash evidence. EVERY frame-hash
/// test must go through this helper so the structural raster guard runs on the exact
/// scenes whose digests we baseline — an expected-digest update can no longer land
/// without containment / occlusion / target-size firing on the same command.
fn frame_hash_command(out_dir: &std::path::Path, extra_args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pf-shell"));
    command
        .arg("--offscreen")
        .args(extra_args)
        .args(["--out", out_dir.to_str().unwrap()])
        .env("PF_RASTER_INK_GUARD", "1");
    command
}

#[test]
fn frame_hash_commands_always_set_the_raster_ink_guard() {
    let out = tempfile::tempdir().unwrap();
    for extra_args in [
        Vec::<&str>::new(),
        vec!["--settings-evidence"],
        vec!["--session-unavailable"],
    ] {
        let command = frame_hash_command(out.path(), &extra_args);
        let guard = command.get_envs().find_map(|(key, value)| {
            (key == std::ffi::OsStr::new("PF_RASTER_INK_GUARD")).then_some(value)
        });
        assert_eq!(
            guard,
            Some(Some(std::ffi::OsStr::new("1"))),
            "frame-hash command for {extra_args:?} must set PF_RASTER_INK_GUARD=1"
        );
    }
}

fn render_offscreen() -> (tempfile::TempDir, String) {
    let out = tempfile::tempdir().unwrap();
    let run = frame_hash_command(out.path(), &[]).output().unwrap();
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

// Split per evidence route-set so the two subprocess renders schedule as separate
// tests (was one fn looping both arg-sets, spawning them back-to-back). Routing
// through `frame_hash_command` keeps the exact command the old inline build produced
// AND centralizes the PF_RASTER_INK_GUARD=1 invariant every frame-hash test must set.
fn assert_presented_evidence_route(extra_args: &[&str], expected_file: &str) {
    let out = tempfile::tempdir().unwrap();
    let run = frame_hash_command(out.path(), extra_args).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        out.path().join(expected_file).is_file(),
        "the guarded evidence set for {extra_args:?} must include {expected_file}"
    );
}

#[test]
fn presented_default_evidence_route_set_has_action_labels_and_rasterized_text_ink() {
    assert_presented_evidence_route(&[], "search.png");
}

#[test]
fn presented_settings_evidence_route_set_has_action_labels_and_rasterized_text_ink() {
    assert_presented_evidence_route(&["--settings-evidence"], "settings-edit.png");
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
        // Search intentionally rebaselines for tsp-op5a.371's shorter hint after
        // removing the redundant SEARCH prefix.
        // tsp-op5a.393 (Family A tonal wiring, ledger sweep 1) rebaselines every
        // route: muted/secondary text roles that previously fell back to primary now
        // carry explicit ink tokens (nav strip, shelf label, hero meta split, card
        // titles/badges/reasons, chip label/count, detail facts/ways/variant sub).
        // All raster guards (containment/occlusion/target-size/legibility) pass on the
        // new pixels — the offscreen render is invoked with PF_RASTER_INK_GUARD=1.
        "92aa60a88fe1226955ed8034d9ecbab8ad8585051daf5fd8fea154849d9a9a59  ",
        "2d4218d45cc36f6dfafa4bca3951dc72f60fc400b456c86cf5c2739634c408bd  ",
        "cb83f164daaf3c3fea79c40c135835aa24992df6feb939c31d41bdf367a88bec  ",
        "0df8b0a7f0826f1be5df8a3a5f70fb09b23e378f709fda0ba1c040b62e9927e7  ",
        "ddfea48ae7d4768612470e8c21aaa14a4b2f0665580760042fb2b9aae684dbad  ",
        "7dbddbd69f43a4dcb447f333181782c2dbed6bc8a01390cfbb0dfc7eda1289c6  ",
        "7dbddbd69f43a4dcb447f333181782c2dbed6bc8a01390cfbb0dfc7eda1289c6  ",
        "dead6fa3b99b58b72b981731aa34228ca4e4984aabaf36c464aa4039596413d5  ",
        "cb20654572fbf3c77523e0a19b31429f08aee6572891206637bfc72a151f0e6f  ",
        "ed097e22022b546f0a07f8c16f190549ce1d238cd2208dd33068291023f76047  ",
        "5a57875e834d80bceaca128e5a17b64f31d8fd3035bd99d0dede4a188755ffa8  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("2d4218d"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("0df8b0a"),
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
    let run = frame_hash_command(out.path(), &["--settings-evidence"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let transcript = String::from_utf8(run.stdout).unwrap();
    // tsp-op5a.369 intentionally repaints Settings values as individual chips and adds the
    // focused value-edit frame. Round 3 restores semantic focus ownership to that row.
    // tsp-op5a.370 repins first-run for its raised sheet, dim backdrop, value chips,
    // suppressed Home footer, and filled Continue action.
    // tsp-op5a.374 repins all six Settings evidence frames after read-only rows moved to
    // the raised surface with a disabled border/ink treatment and structured value slot:
    // settings/settings-edit include the unavailable remap and diagnostic rows; controls
    // includes remap, Safe Return, and source; network includes SSID and signal/address;
    // system includes About, device/storage, and licenses; first-run retains the dimmed
    // Settings scene beneath its sheet.
    // tsp-op5a.393 (Family A tonal wiring) rebaselines all six Settings/first-run
    // evidence frames: row sublabels muted, segmented-control unselected values muted,
    // toggle OFF knob muted + outlined-transparent track + muted OFF caption, and the
    // first-run value chips de-CTA'd (quiet surface, not the accent fill) while Continue
    // takes the lamplight accent. Raster guards pass with PF_RASTER_INK_GUARD=1.
    assert!(
        transcript.contains("c2e069153b2b509ca6e5fcbf27665ac97858b5d301c5d14e25bf35018a986a92  ")
    );
    assert!(
        transcript.contains("17ab79ae77a37f7066b75e49cfae179273d4654f3cd8928d6ea5c0587a9b9e8f  ")
    );
    assert!(
        transcript.contains("8315c81a32f5ee8a5d634b603234f5d797db9b3b5219397ba4551cd37df353a9  ")
    );
    assert!(
        transcript.contains("be8fe621fd535ca06c209b43de77ad8629431480fa9b47e9250d53dbd7a9ef39  ")
    );
    assert!(
        transcript.contains("6d8d401586ab0063bccbf46d38a55a6804f46b1ed016fde450f78d60760317a6  ")
    );
    assert!(
        transcript.contains("dbfab3c751831e3824ade8f6233a5b7f587596eea0ae448495fe2114b709336d  ")
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
    let run = frame_hash_command(out.path(), &["--session-unavailable"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let transcript = String::from_utf8(run.stdout).unwrap();
    // tsp-op5a.393 (Family A tonal wiring) rebaselines the degraded-session Home frame
    // for the nav strip / shelf label / hero meta tonal fix.
    assert!(
        transcript.contains("f682da19a94f80f0d3353062b742d1c6824d5e2a616a7f81134778dda412bfd6  ")
    );
}
