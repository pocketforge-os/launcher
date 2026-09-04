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
        // tsp-op5a.391 (Family B focus/selection system fix) rebaselines the five routes
        // whose fixed controls repaint: quick-power (quick rows drop the second, glow-less
        // border ring — focus is the renderer's single state.focused ring+glow), library +
        // library-focused-search (filter chip: the hand-drawn L-frame — state.selected left
        // bar + square 4-node box border + square bottom bar — becomes a rounded strong
        // border with the shared selected_underline_inner bottom underline), search (box +
        // result rows drop the duplicate inset ring), and details / details-unavailable
        // (variant border is strong-when-selected, focus is the ring only). boot-home /
        // focus-moved / launch-dimmed / returned / variant-chooser are unchanged (Home card
        // reverted to baseline; those frames carry none of the repainted control classes).
        // tsp-op5a.390 (Family D iconography system) rebaselines every status-bar
        // route: the wifi glyph shrinks to 9x7 and the battery to a delicate 12x7
        // outline capsule, both centered with the %/clock text on one optical
        // centerline; the Home hero + Details ready cue become a drawn green 8px dot;
        // card favorite pips gain a centered star image; the Library magnifier becomes
        // a real glyph; and the Network/Update badges take the globe / info-i glyphs.
        // search and variant-chooser carry none of those and keep their digests.
        // All raster guards pass with PF_RASTER_INK_GUARD=1.
        // tsp-op5a.395 (Family E heading/label hierarchy wiring) rebaselines the
        // three Details/chooser routes: the Details title drops from the Hero role
        // (52/800) to its Title role (34/800, ~25px cap), the option-card name/sub
        // split onto the Label/Caption roles instead of the shared Body default, and
        // the variant chooser now paints a game-name Eyebrow over a Title question
        // (replacing the tiny small-caps route-heading Eyebrow that stood in for the
        // whole heading). boot-home / focus-moved / launch-dimmed / returned /
        // quick-power / library / library-focused-search / search carry none of the
        // retuned type roles and keep their digests. All raster guards pass with
        // PF_RASTER_INK_GUARD=1.
        // tsp-op5a.394 (evidence-harness chrome parity, ledger sweep 1) rebaselines ALL
        // SIX f10-evidence routes (library, library-focused-search, search, details,
        // details-unavailable, variant-chooser): those cores now load the same
        // device-status / network / time providers and control bindings the live shell
        // loads, so every evidence route renders the top-right status cluster
        // (Wi-Fi/battery/clock) + per-route hint legend the shell draws on all screens
        // (previously blank on Library/Details/Search). The digests below are measured on
        // the MERGED pixels (this bead's chrome atop tsp-op5a.395's Family E type roles).
        // boot-home / focus-moved / launch-dimmed / returned / quick-power use the default
        // core and are UNCHANGED from tsp-op5a.395. The status/legend nodes are passive
        // (no action), so the target-size documented-baseline gate is untouched; all
        // raster guards stay green.
        "ff485292548353dde311ca62d2c52a8cbd7c8c56ca973987424305fb70345fea  ",
        "046f3f78c505d699210a9486e745c592eb50caec3b5acaaf4a81c7cb7a78ff2f  ",
        "7c8a61c2ec46686369fbb3b659439faaa28cfc270d0794ac6d97f8389a78a1a9  ",
        "63bcd820e33b4b0e440dee6c269fab7ac360ec4af77cc00de293c2486ab38298  ",
        "3c545fced30389c4c70b0e57bf388f622cb7f4c32f7405085c36d9a9ff4f5217  ",
        "a7759bdd41f02b2035247f4f0632bcdc8b2d5ccc06f4524629b484faa6131368  ",
        "a7759bdd41f02b2035247f4f0632bcdc8b2d5ccc06f4524629b484faa6131368  ",
        "974780574547cc01aeee8fc0e92db2f71242ff8661d4538d7902f54ab848ba09  ",
        "ee44ff799ec375c1bfd4b5c16d1c3fa136124c8ac288b7b0dc3fb8b5216a5efd  ",
        "4b2f650d8d67552f80590acf5c7471f132a839bb2740c2bd0cf87852644333f6  ",
        "e795fa8c509cbfe022388a2f9e16b62f70d761df85f86917fc4c8194b1cbf43e  ",
    ] {
        assert!(lines.contains(expected), "missing {expected} in {lines}");
    }
    assert!(lines.lines().nth(1).unwrap().starts_with("046f3f78"));
    assert!(
        lines.lines().nth(3).unwrap().starts_with("63bcd820"),
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
    // tsp-op5a.391 (Family B focus/selection system fix) rebaselines all six frames: the
    // settings rail current item now carries the renderer's warm state.selected left bar
    // (the faked near-white ▌ glyph is removed, and selection is no longer suppressed while
    // focused), the text-size segmented control repaints (subtle overlay fill + primary text
    // + shared bottom-underline layer, no state.selected left bar / accent flood), and the
    // focused rows use the single state.focused ring+glow (the duplicate ring-colored border
    // is dropped — including the first-run row + its value chip). Every Settings/first-run
    // frame carries the rail, so all six shift; raster guards pass with PF_RASTER_INK_GUARD=1.
    // tsp-op5a.395 (Family E heading/label hierarchy wiring) rebaselines all six
    // frames: the Settings section title takes the H1 role (22/700) instead of the
    // Body default (it had read at body scale), the row name/sublabel split onto the
    // Label/Caption roles rather than sharing the Body default, and first-run repaints
    // its Title-role heading (34/800, its box grown to keep the descenders contained)
    // over an Eyebrow-role kicker (was a Label title under a Caption eyebrow). Every
    // Settings frame carries the section title, so all six shift; raster guards pass
    // with PF_RASTER_INK_GUARD=1.
    assert!(
        transcript.contains("1bc970c37f00b5fa377d14a26b4fa70b2ca968e8ccda41cc5221a54bb84f1dd9  ")
    );
    assert!(
        transcript.contains("09d5eedd6911ab05a06c32aabec8273a88875ed2382be89b976a3a0e9add0d0f  ")
    );
    assert!(
        transcript.contains("9107e63b0bf5b77f5d77b67f8c65b00bfc5fcc43418c8190b4473fafd2592565  ")
    );
    assert!(
        transcript.contains("0372535b54b2a58befa71bb64053bd531e9949447668633e076282068792e1da  ")
    );
    assert!(
        transcript.contains("ab7d0c6f1a2694d5ba523dae0d2e18ae7f053392f82750bf1b87a03022f303c1  ")
    );
    assert!(
        transcript.contains("d597ec7d46d3bdea8ed92714f4f5d2886481e3d22b8f50f00cff4ea9feea5167  ")
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
        transcript.contains("8edba36cfeb1c7732087ce6c7d486d5a8411e8c4e63f49fd0c47028eb0f05841  ")
    );
}

// ---- tsp-op5a.390 Family D (iconography system) structural guards ------------------
// Decode a route's PNG from a fresh offscreen render into (pixels, stride).
fn decode_route(dir: &std::path::Path, file: &str) -> (Vec<u8>, usize, usize, usize) {
    let decoder = png::Decoder::new(std::fs::File::open(dir.join(file)).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let stride = info.width as usize * 4;
    (
        pixels[..info.buffer_size()].to_vec(),
        stride,
        info.width as usize,
        info.height as usize,
    )
}

/// Ink bounding box (`min_x`, `max_x`, `min_y`, `max_y`, count) of pixels brighter
/// than `th` on all three channels, within the `[x0,x1)` x `[y0,y1)` window.
#[allow(clippy::similar_names)]
fn ink_bbox(
    pixels: &[u8],
    stride: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    th: u8,
) -> Option<(usize, usize, usize, usize, usize)> {
    let (mut minx, mut miny, mut maxx, mut maxy, mut count) = (usize::MAX, usize::MAX, 0, 0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            let o = y * stride + x * 4;
            if pixels[o] > th && pixels[o + 1] > th && pixels[o + 2] > th {
                count += 1;
                minx = minx.min(x);
                maxx = maxx.max(x);
                miny = miny.min(y);
                maxy = maxy.max(y);
            }
        }
    }
    (count > 0).then_some((minx, maxx, miny, maxy, count))
}

// The status-bar chrome (wifi glyph, battery capsule, %/clock text) must share ONE
// optical centerline within 1px and stay within per-icon size limits. Red before
// tsp-op5a.390: the shipped items sat at ink-midlines wifi y=34 / battery y=30.5 /
// text y=28 (a 6px spread) with a 26x14 near-solid battery block and a 14x11 wifi.
#[test]
#[allow(clippy::cast_precision_loss, clippy::similar_names)]
fn status_cluster_items_share_one_optical_centerline() {
    let (out, _) = render_offscreen();
    let (pixels, stride, _, _) = decode_route(out.path(), "boot-home.png");
    let wifi = ink_bbox(&pixels, stride, 1103, 1120, 20, 44, 55).expect("wifi glyph ink");
    let battery = ink_bbox(&pixels, stride, 1121, 1141, 20, 44, 40).expect("battery ink");
    let text = ink_bbox(&pixels, stride, 1144, 1215, 20, 44, 110).expect("status text ink");
    let center_y = |b: (usize, usize, usize, usize, usize)| (b.2 + b.3) as f32 / 2.0;
    let (wy, by, ty) = (center_y(wifi), center_y(battery), center_y(text));
    assert!(
        (wy - ty).abs() <= 1.5,
        "wifi ink centerline {wy} must sit within 1px of the status text {ty}"
    );
    assert!(
        (by - ty).abs() <= 1.5,
        "battery ink centerline {by} must sit within 1px of the status text {ty}"
    );
    let dims = |b: (usize, usize, usize, usize, usize)| (b.1 - b.0 + 1, b.3 - b.2 + 1);
    let (ww, wh) = dims(wifi);
    assert!(
        ww <= 12 && wh <= 9,
        "wifi glyph {ww}x{wh} exceeds the 12x9 cap"
    );
    let (bw, bh) = dims(battery);
    assert!(
        bw <= 18 && bh <= 10,
        "battery capsule {bw}x{bh} exceeds the 18x10 cap (must be a delicate outline, not a block)"
    );
}

// The Library search magnifier must render as a real glyph (thin ring + handle), not a
// tofu box. Red before tsp-op5a.390: the `⌕` (U+2315) codepoint is in neither bundled
// face, so it painted a near-solid `.notdef` rectangle (fill ratio ~0.97 of its bbox).
// A drawn ring+handle glyph is SPARSE (fill ratio ~0.4). This is ink-SHAPE sanity, not
// merely non-empty ink — a solid slab has plenty of ink and must still fail.
#[test]
#[allow(clippy::cast_precision_loss, clippy::similar_names)]
fn library_search_magnifier_is_a_glyph_not_a_tofu_box() {
    let (out, _) = render_offscreen();
    let (pixels, stride, _, _) = decode_route(out.path(), "library.png");
    let glyph = ink_bbox(&pixels, stride, 62, 84, 86, 110, 45).expect("magnifier glyph ink");
    let (minx, maxx, miny, maxy, count) = glyph;
    let (w, h) = (maxx - minx + 1, maxy - miny + 1);
    assert!(count > 8, "magnifier must produce visible glyph ink");
    assert!(
        w <= 16 && h <= 16,
        "magnifier glyph {w}x{h} is too large — a slab, not a mark"
    );
    let fill = count as f32 / (w * h) as f32;
    assert!(
        fill < 0.55,
        "magnifier fill ratio {fill:.2} of its {w}x{h} box is a solid tofu slab, not a ring+handle glyph"
    );
}

// ---- tsp-op5a.394 evidence-harness chrome parity (ledger sweep 1) ------------------
// The offscreen evidence harness must render the SAME live-parity chrome the shell
// draws on EVERY screen: the top-right status cluster (Wi-Fi glyph, battery capsule,
// battery%/clock text) and the per-route hint-legend prompt bar. Before this bead the
// f10-evidence cores were built by a bare `fixture_core` with no device-status /
// network / time providers and no control bindings, so Library / Details / Search lost
// the status cluster and Library / Details lost the hint legend — the vision-review
// gate reviewed understated frames on those routes while the live shell (rig-proven on
// fid-{home,library,settings}) drew both. This asserts the SCENE structure so the
// parity cannot silently regress. It reads the per-route `.semantic.txt` node-tree the
// harness writes beside each PNG (structure is guard-independent, so no ink guard is
// needed here).
fn render_evidence_set(out: &std::path::Path, extra: &[&str]) {
    let run = Command::new(env!("CARGO_BIN_EXE_pf-shell"))
        .arg("--offscreen")
        .args(extra)
        .args(["--out", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

fn semantic_has_node(semantic: &str, id: &str) -> bool {
    let needle = format!("{id} ");
    semantic
        .lines()
        .any(|line| line.trim_start().starts_with(&needle))
}

fn status_cluster_present(semantic: &str) -> bool {
    // The full "wifi/battery/82/clock" cluster: the labeled text node plus the Wi-Fi
    // glyph and the delicate battery outline capsule.
    semantic_has_node(semantic, "status-cluster")
        && semantic_has_node(semantic, "wifi-glyph")
        && semantic_has_node(semantic, "battery-outline")
}

fn hint_legend_present(semantic: &str) -> bool {
    // Home / Library / Details / Search render the legend as keycap + verb child nodes;
    // Settings renders it as a single labeled text prompt. Either form is live-parity;
    // an empty `prompts` group with no children (the pre-fix Library / Details bug) is
    // not.
    semantic_has_node(semantic, "home-prompt-keycap-0")
        || semantic.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("prompts ") && !line.contains("label=\"\"")
        })
}

#[test]
fn every_evidence_screen_route_renders_live_parity_chrome() {
    let default_dir = tempfile::tempdir().unwrap();
    render_evidence_set(default_dir.path(), &[]);
    let settings_dir = tempfile::tempdir().unwrap();
    render_evidence_set(settings_dir.path(), &["--settings-evidence"]);

    // (render dir, route semantic file) — every evidence screen whose live shell draws
    // full chrome. Home is the always-green control; Library / Details / Search are red
    // on pre-tsp-op5a.394 main (status cluster missing on all three, hint legend missing
    // on Library / Details); Settings already carried both. ALL SIX chrome-rebaselined
    // f10 routes are covered, including the three off the top-level path — details-
    // unavailable and variant-chooser render from SEPARATE cores (the `unavailable` and
    // `chooser` cores in emit_f10_evidence), so dropping apply_evidence_chrome from
    // either path would otherwise slip past a guard named "every evidence route"
    // (tsp-op5a.394 r2 review note, coordinator-adopted).
    let cases: [(&std::path::Path, &str); 9] = [
        (default_dir.path(), "boot-home"),
        (default_dir.path(), "library"),
        (default_dir.path(), "library-focused-search"),
        (default_dir.path(), "details"),
        (default_dir.path(), "details-unavailable"),
        (default_dir.path(), "search"),
        (default_dir.path(), "variant-chooser"),
        (settings_dir.path(), "settings"),
        (settings_dir.path(), "settings-edit"),
    ];

    let mut failures = Vec::new();
    for (dir, route) in cases {
        let semantic = std::fs::read_to_string(dir.join(format!("{route}.semantic.txt"))).unwrap();
        if !status_cluster_present(&semantic) {
            failures.push(format!(
                "{route}: evidence scene is missing the top-right status cluster (Wi-Fi / battery / clock)"
            ));
        }
        if !hint_legend_present(&semantic) {
            failures.push(format!(
                "{route}: evidence scene is missing the bottom hint-legend prompt bar"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "evidence routes omit live-parity chrome the shell draws on every screen:\n{}",
        failures.join("\n")
    );
}
