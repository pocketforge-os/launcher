//! Red-first comparator fixtures: each comparator class must catch a SEEDED
//! divergence (a shifted box, a changed font size, a swapped decoration, a
//! swapped color, a treatment crop) and stay silent when the fact matches.

use fidelity_audit::compare::{
    self, COLOR_TOL, CROP_MAE_THRESHOLD, FONT_TOL_PX, GEOMETRY_TOL_PX, parse_css_rgb,
};
use fidelity_audit::facts::{BBox, Facts};
use fidelity_audit::mapping::Component;
use fidelity_audit::raster::Raster;
use fidelity_audit::scene::{Bounds, SceneNode};

fn component(json: &str) -> Component {
    serde_json::from_str(json).expect("component fixture")
}

fn facts(json: &str) -> Facts {
    serde_json::from_str(json).expect("facts fixture")
}

fn scene_node(json: &str) -> SceneNode {
    serde_json::from_str(json).expect("scene-node fixture")
}

fn bounds(x: f64, y: f64, w: f64, h: f64) -> Bounds {
    serde_json::from_str(&format!(r#"{{"x":{x},"y":{y},"width":{w},"height":{h}}}"#))
        .expect("bounds fixture")
}

// ---------------------------------------------------------------- geometry

#[test]
fn geometry_catches_a_seeded_icon_shift() {
    let comp = component(r#"{"selector":".wifi","node":"wifi-glyph","classes":["geometry"]}"#);
    let mockup = BBox {
        x: 100.0,
        y: 22.0,
        w: 20.0,
        h: 20.0,
    };
    // Seed a 2px horizontal shift — beyond the +-1px tolerance.
    let shifted = bounds(102.0, 22.0, 20.0, 20.0);
    let finding = compare::geometry("r", &comp, &mockup, &shifted).expect("shift must be caught");
    assert_eq!(finding.fact_class, "geometry");
    assert!(finding.delta.contains("dx=+2.0"), "delta={}", finding.delta);
}

#[test]
fn geometry_ignores_a_subpixel_shift_within_tolerance() {
    let comp = component(r#"{"selector":".wifi","node":"wifi-glyph","classes":["geometry"]}"#);
    let mockup = BBox {
        x: 100.0,
        y: 22.0,
        w: 20.0,
        h: 20.0,
    };
    let nudged = bounds(100.0 + GEOMETRY_TOL_PX, 22.0, 20.0, 20.0);
    assert!(compare::geometry("r", &comp, &mockup, &nudged).is_none());
}

// ---------------------------------------------------------------- font-size

#[test]
fn font_size_catches_a_seeded_size_change() {
    let comp =
        component(r#"{"selector":".hero-meta","node":"hero-status","classes":["font-size"]}"#);
    // Shell node is body (15px @100%); mockup expects 14px -> 1px divergence.
    let node = scene_node(
        r#"{"id":"hero-status","bounds":{"x":0,"y":0,"width":10,"height":10},"type_role":"body"}"#,
    );
    let finding =
        compare::font_size("r", &comp, 14.0, &node, 1.0).expect("size change must be caught");
    assert_eq!(finding.fact_class, "font-size");
    assert!(finding.delta.contains("+1.00px"), "delta={}", finding.delta);
}

#[test]
fn font_size_matches_when_role_size_equals_mockup() {
    let comp =
        component(r#"{"selector":".hero-meta","node":"hero-status","classes":["font-size"]}"#);
    let node = scene_node(
        r#"{"id":"hero-status","bounds":{"x":0,"y":0,"width":10,"height":10},"type_role":"body"}"#,
    );
    assert!(compare::font_size("r", &comp, 15.0, &node, 1.0).is_none());
    // And within the epsilon.
    assert!(compare::font_size("r", &comp, 15.0 + FONT_TOL_PX, &node, 1.0).is_none());
}

#[test]
fn font_size_scales_with_text_scale() {
    let comp = component(r#"{"selector":".t","node":"n","classes":["font-size"]}"#);
    let node =
        scene_node(r#"{"id":"n","bounds":{"x":0,"y":0,"width":1,"height":1},"type_role":"body"}"#);
    // 15px body at 200% -> 30px; mockup still 15px -> divergence.
    let finding = compare::font_size("r", &comp, 15.0, &node, 2.0).expect("scaled size diverges");
    assert!(
        finding.shipped.contains("30"),
        "shipped={}",
        finding.shipped
    );
}

#[test]
fn font_size_reports_info_for_unknown_role() {
    let comp = component(r#"{"selector":".t","node":"n","classes":["font-size"]}"#);
    let node =
        scene_node(r#"{"id":"n","bounds":{"x":0,"y":0,"width":1,"height":1},"type_role":"plate"}"#);
    let finding = compare::font_size("r", &comp, 15.0, &node, 1.0).expect("info emitted");
    assert!(finding.delta.contains("no base-size"));
}

// ---------------------------------------------------------------- decoration

#[test]
fn decoration_catches_a_seeded_underline_swap() {
    let comp = component(
        r#"{"selector":".room","index":0,"node":"room-home","classes":["decoration"],"underline_node":"room-home-underline"}"#,
    );
    // Mockup shows NO underline decoration, but the shell paints an underline node.
    let mockup = facts(
        r#"{"bbox":{"x":0,"y":0,"w":10,"h":10},"font":{"size":"14px"},"color":"rgb(0,0,0)","border":{},"textDecoration":{"line":"none"}}"#,
    );
    let finding =
        compare::decoration("r", &comp, &mockup, true).expect("treatment mismatch must be caught");
    assert_eq!(finding.fact_class, "decoration");
    assert_eq!(finding.expected, "no-underline");
    assert!(finding.shipped.contains("room-home-underline"));
}

#[test]
fn decoration_matches_when_both_underline() {
    let comp = component(r#"{"selector":".x","node":"n","classes":["decoration"]}"#);
    let mockup = facts(
        r#"{"bbox":{"x":0,"y":0,"w":10,"h":10},"font":{"size":"14px"},"color":"rgb(0,0,0)","border":{},"textDecoration":{"line":"underline"}}"#,
    );
    assert!(compare::decoration("r", &comp, &mockup, true).is_none());
}

#[test]
fn decoration_reads_bottom_border_as_underline() {
    let comp = component(r#"{"selector":".x","node":"n","classes":["decoration"]}"#);
    // Underline expressed as a bottom border; shell has no underline node -> mismatch.
    let mockup = facts(
        r#"{"bbox":{"x":0,"y":0,"w":10,"h":10},"font":{"size":"14px"},"color":"rgb(0,0,0)","border":{"bottomWidth":"2px","bottomStyle":"solid"},"textDecoration":{"line":"none"}}"#,
    );
    let finding = compare::decoration("r", &comp, &mockup, false).expect("border underline caught");
    assert_eq!(finding.expected, "underline");
}

// ---------------------------------------------------------------- color

fn solid_raster(w: u32, h: u32, rgb: [u8; 3]) -> Raster {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    Raster {
        width: w,
        height: h,
        rgba,
    }
}

fn text_on_bg(w: u32, h: u32, bg: [u8; 3], fg: [u8; 3]) -> Raster {
    // Background dominates; a minority stripe of foreground is the "text".
    let mut r = solid_raster(w, h, bg);
    for y in 0..h {
        for x in 0..(w / 4) {
            let i = ((y * w + x) * 4) as usize;
            r.rgba[i] = fg[0];
            r.rgba[i + 1] = fg[1];
            r.rgba[i + 2] = fg[2];
        }
    }
    r
}

#[test]
fn color_catches_a_seeded_color_swap() {
    let comp = component(r#"{"selector":".t","node":"n","classes":["color"]}"#);
    // Shell paints red text; mockup expects white.
    let shell = text_on_bg(20, 20, [10, 10, 10], [220, 20, 20]);
    let bbox = BBox {
        x: 0.0,
        y: 0.0,
        w: 20.0,
        h: 20.0,
    };
    let finding = compare::color("r", &comp, "rgb(244, 239, 230)", &shell, &bbox)
        .expect("color swap must be caught");
    assert_eq!(finding.fact_class, "color");
}

#[test]
fn color_matches_within_tolerance() {
    let comp = component(r#"{"selector":".t","node":"n","classes":["color"]}"#);
    let shell = text_on_bg(20, 20, [10, 10, 10], [244, 239, 230]);
    let bbox = BBox {
        x: 0.0,
        y: 0.0,
        w: 20.0,
        h: 20.0,
    };
    // Off by less than the per-channel tolerance.
    let expected = format!("rgb({}, {}, {})", 244 - (COLOR_TOL / 2), 239, 230);
    assert!(compare::color("r", &comp, &expected, &shell, &bbox).is_none());
}

// ---------------------------------------------------------------- crop

#[test]
fn crop_catches_a_seeded_treatment_and_writes_an_artifact() {
    let comp = component(r#"{"selector":".hero-title","node":"hero-title","classes":["crop"]}"#);
    let golden = solid_raster(32, 32, [20, 20, 20]);
    // Seed a bright treatment block (an underline the golden lacks).
    let mut shell = golden.clone();
    for y in 24u32..32 {
        for x in 0u32..32 {
            let i = ((y * 32 + x) * 4) as usize;
            shell.rgba[i] = 255;
            shell.rgba[i + 1] = 255;
            shell.rgba[i + 2] = 255;
        }
    }
    let bbox = BBox {
        x: 0.0,
        y: 0.0,
        w: 32.0,
        h: 32.0,
    };
    let out = tempfile::tempdir().unwrap();
    let finding = compare::crop("r", &comp, &bbox, &golden, &shell, out.path())
        .expect("crop ok")
        .expect("divergence must be caught");
    assert_eq!(finding.fact_class, "crop");
    let artifact = finding.artifact.expect("artifact path");
    assert!(
        std::path::Path::new(&artifact).exists(),
        "delta png written"
    );
}

#[test]
fn crop_matches_identical_renders() {
    let comp = component(r#"{"selector":".x","node":"n","classes":["crop"]}"#);
    let golden = solid_raster(16, 16, [30, 40, 50]);
    let shell = golden.clone();
    let bbox = BBox {
        x: 0.0,
        y: 0.0,
        w: 16.0,
        h: 16.0,
    };
    let out = tempfile::tempdir().unwrap();
    assert!(
        compare::crop("r", &comp, &bbox, &golden, &shell, out.path())
            .unwrap()
            .is_none()
    );
    let _ = CROP_MAE_THRESHOLD; // referenced for documentation of the gate constant
}

// ---------------------------------------------------------------- parsing

#[test]
fn parses_css_rgb_and_rgba() {
    assert_eq!(
        parse_css_rgb("rgb(244, 239, 230)"),
        Some([244, 239, 230, 255])
    );
    assert_eq!(parse_css_rgb("rgba(0, 0, 0, 0)"), Some([0, 0, 0, 0]));
    assert_eq!(
        parse_css_rgb("rgba(10, 20, 30, 0.5)"),
        Some([10, 20, 30, 128])
    );
    assert_eq!(parse_css_rgb("not-a-color"), None);
}
