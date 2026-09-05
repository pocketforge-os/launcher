//! The fact comparators. Each returns at most one [`Finding`] for a component:
//! `None` when the fact is within tolerance, `Some` when it diverges (or an
//! informational note when a comparator could not run).

use std::path::Path;

use crate::facts::{BBox, Facts};
use crate::ledger::{Finding, Severity};
use crate::mapping::{Component, FactClass};
use crate::raster::Raster;
use crate::scene::{Bounds, SceneNode};

/// Alignment/geometry tolerance at 100% scale (bead: +-1px).
pub const GEOMETRY_TOL_PX: f64 = 1.0;
/// Font-size tolerance (bead: exact; a small epsilon absorbs float rounding).
pub const FONT_TOL_PX: f64 = 0.5;
/// Per-channel color tolerance for render-sampled color (absorbs AA/dither).
pub const COLOR_TOL: u32 = 8;
/// Perceptual crop mean-abs-error threshold (normalised 0..1).
pub const CROP_MAE_THRESHOLD: f64 = 0.02;

fn base(finding_class: FactClass, route: &str, comp: &Component, node_id: &str) -> Finding {
    Finding {
        route: route.to_string(),
        selector: comp.label(),
        node: node_id.to_string(),
        fact_class: finding_class.as_str().to_string(),
        expected: String::new(),
        shipped: String::new(),
        delta: String::new(),
        severity: Severity::Divergence,
        artifact: None,
        accepted: false,
    }
}

/// Bounding box vs mockup bbox, per axis, +-`GEOMETRY_TOL_PX`.
#[must_use]
pub fn geometry(route: &str, comp: &Component, mockup: &BBox, node: &Bounds) -> Option<Finding> {
    let dx = node.x - mockup.x;
    let dy = node.y - mockup.y;
    let dw = node.width - mockup.w;
    let dh = node.height - mockup.h;
    let worst = [dx, dy, dw, dh]
        .into_iter()
        .map(f64::abs)
        .fold(0.0_f64, f64::max);
    if worst <= GEOMETRY_TOL_PX {
        return None;
    }
    let mut f = base(FactClass::Geometry, route, comp, &comp.node);
    f.expected = format!(
        "{:.1},{:.1},{:.1},{:.1}",
        mockup.x, mockup.y, mockup.w, mockup.h
    );
    f.shipped = format!(
        "{:.1},{:.1},{:.1},{:.1}",
        node.x, node.y, node.width, node.height
    );
    f.delta = format!("dx={dx:+.1} dy={dy:+.1} dw={dw:+.1} dh={dh:+.1}");
    Some(f)
}

/// Computed font size (`type_role` base size x text scale) vs mockup px.
#[must_use]
pub fn font_size(
    route: &str,
    comp: &Component,
    mockup_px: f64,
    node: &SceneNode,
    scale: f64,
) -> Option<Finding> {
    let Some(base_px) = base_size(&node.type_role) else {
        let mut f = base(FactClass::FontSize, route, comp, &node.id);
        f.severity = Severity::Info;
        f.expected = format!("{mockup_px}px");
        f.shipped = format!("type_role={}", node.type_role);
        f.delta = "no base-size mapping for this type_role".to_string();
        return Some(f);
    };
    let shipped = base_px * scale;
    if (shipped - mockup_px).abs() <= FONT_TOL_PX {
        return None;
    }
    let mut f = base(FactClass::FontSize, route, comp, &node.id);
    f.expected = format!("{mockup_px}px");
    f.shipped = format!("{shipped}px ({} @ {:.0}%)", node.type_role, scale * 100.0);
    f.delta = format!("{:+.2}px", shipped - mockup_px);
    Some(f)
}

/// Structural underline treatment: does the shell paint an underline node where
/// the mockup shows none (or vice versa)? Catches the "focus underline" class.
#[must_use]
pub fn decoration(
    route: &str,
    comp: &Component,
    mockup: &Facts,
    scene_has_underline: bool,
) -> Option<Finding> {
    let mockup_underline = mockup.text_decoration.is_underline() || mockup.border.has_bottom_line();
    if mockup_underline == scene_has_underline {
        return None;
    }
    let mut f = base(FactClass::Decoration, route, comp, &comp.node);
    f.expected = if mockup_underline {
        "underline".to_string()
    } else {
        "no-underline".to_string()
    };
    f.shipped = if scene_has_underline {
        format!(
            "underline node `{}`",
            comp.underline_node.as_deref().unwrap_or("?")
        )
    } else {
        "no-underline".to_string()
    };
    f.delta = "treatment mismatch (mockup facts vs shell scene)".to_string();
    Some(f)
}

/// Render-sampled dominant component color vs mockup computed color (opt-in).
///
/// # Errors
/// An enabled color check must be PERFORMED or fail loudly — it must never
/// silently pass. So an unparseable trusted mockup color, or a component bbox
/// that clamps to an empty crop (nothing to sample), is a hard input error, not
/// a `None` that `run_comparators` would read as "no divergence".
pub fn color(
    route: &str,
    comp: &Component,
    mockup_color: &str,
    shell: &Raster,
    bbox: &BBox,
) -> Result<Option<Finding>, String> {
    let expected = parse_css_rgb(mockup_color).ok_or_else(|| {
        format!(
            "{route} {}: color enabled but mockup color {mockup_color:?} is unparseable",
            comp.label()
        )
    })?;
    let rect = shell.clamp_rect(bbox);
    let sampled = shell.dominant_foreground(rect).ok_or_else(|| {
        format!(
            "{route} {}: color enabled but the component bbox clamps to an empty crop \
             ({}x{} at {},{}) — nothing to sample",
            comp.label(),
            rect.w,
            rect.h,
            rect.x,
            rect.y
        )
    })?;
    let dist: u32 = (0..3)
        .map(|c| u32::from(expected[c].abs_diff(sampled[c])))
        .sum();
    if dist <= COLOR_TOL * 3 {
        return Ok(None);
    }
    let mut f = base(FactClass::Color, route, comp, &comp.node);
    f.expected = format!("rgb({},{},{})", expected[0], expected[1], expected[2]);
    f.shipped = format!("rgb({},{},{})", sampled[0], sampled[1], sampled[2]);
    f.delta = format!("sum|d|={dist}");
    Ok(Some(f))
}

/// Perceptual per-component crop diff: golden vs shell render over the mockup
/// bbox. Writes a delta artifact and reports when MAE exceeds the threshold.
///
/// # Errors
/// Returns an error if the delta artifact cannot be written.
pub fn crop(
    route: &str,
    comp: &Component,
    bbox: &BBox,
    golden: &Raster,
    shell: &Raster,
    out_dir: &Path,
) -> Result<Option<Finding>, String> {
    if golden.width != shell.width || golden.height != shell.height {
        // A resolution mismatch is an INPUT failure (renders staged at the wrong
        // size), not a finding — hard-error rather than emit a downgraded row.
        return Err(format!(
            "{route} {}: golden/shell render size mismatch golden={}x{} shell={}x{}",
            comp.label(),
            golden.width,
            golden.height,
            shell.width,
            shell.height
        ));
    }
    let rect = golden.clamp_rect(bbox);
    let mae = golden.crop_mae(shell, rect);
    if mae <= CROP_MAE_THRESHOLD {
        return Ok(None);
    }
    let artifact = out_dir.join(format!(
        "{}__{}__crop-diff.png",
        route.replace('/', "-"),
        sanitize(&comp.label())
    ));
    golden.write_delta(shell, rect, &artifact)?;
    let mut f = base(FactClass::Crop, route, comp, &comp.node);
    f.expected = "golden crop".to_string();
    f.shipped = "shell crop".to_string();
    f.delta = format!("mae={mae:.4}");
    f.artifact = Some(artifact.display().to_string());
    Ok(Some(f))
}

/// Base type-role sizes, cited from
/// `crates/pf-shell-core/src/design_generated.rs` (`TYPE_*_SIZE`). These are the
/// generated design tokens; a divergence here means a component was assigned the
/// wrong type role (the "text slightly smaller than design" class).
#[must_use]
pub fn base_size(type_role: &str) -> Option<f64> {
    match type_role {
        "hero" => Some(52.0),
        "title" => Some(34.0),
        "h1" => Some(22.0),
        "body" => Some(15.0),
        "label" => Some(14.0),
        "caption" => Some(12.5),
        "eyebrow" => Some(11.5),
        _ => None,
    }
}

/// Parse a CSS `rgb(...)` / `rgba(...)` color to `[r, g, b, a]`.
#[must_use]
pub fn parse_css_rgb(s: &str) -> Option<[u8; 4]> {
    let inner = s.trim();
    let inner = inner
        .strip_prefix("rgba(")
        .or_else(|| inner.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let mut parts = inner.split(',').map(str::trim);
    let r = parts.next()?.parse::<f64>().ok()?;
    let g = parts.next()?.parse::<f64>().ok()?;
    let b = parts.next()?.parse::<f64>().ok()?;
    let a = parts
        .next()
        .and_then(|p| p.parse::<f64>().ok())
        .unwrap_or(1.0);
    Some([clamp_u8(r), clamp_u8(g), clamp_u8(b), clamp_u8(a * 255.0)])
}

fn clamp_u8(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
