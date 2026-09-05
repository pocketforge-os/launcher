//! The shell scene, parsed from the per-route `<slug>.semantic.txt` snapshot
//! emitted by `pf-shell --offscreen --out <dir>` (`semantic_snapshot`).
//!
//! The audit reads this text ARTIFACT rather than linking pf-shell-core, so it
//! stays decoupled from scene-construction internals (which sibling lanes churn)
//! and never touches pixels. The snapshot carries id, role, label, post-layout
//! bounds and node state per line — everything geometry and decoration need.
//!
//! It does NOT carry `type_role` or theme tokens; the type-role font-size
//! comparator (implemented and unit-tested in [`crate::compare::font_size`])
//! therefore activates only when a richer scene dump is available — see the
//! crate README. `SceneNode` keeps a `type_role` field (empty from this parser,
//! settable in tests) so that comparator and its fixtures compile against the
//! same type.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// One scene node. Populated from a semantic-snapshot line; also `Deserialize`
/// so comparator fixtures can build one directly.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SceneNode {
    pub id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub accessible_label: String,
    pub bounds: Bounds,
    /// Empty when parsed from a semantic snapshot; set in tests / a future
    /// richer dump. Drives the font-size comparator.
    #[serde(default)]
    pub type_role: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub selected: bool,
}

/// Post-layout node rect (top-left origin, logical pixels).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A parsed scene: the flat node set of one route's semantic snapshot.
#[derive(Debug, Clone, Default)]
pub struct SceneTree {
    pub nodes: Vec<SceneNode>,
    /// Text scale as a fraction (`1.0` == 100%); offscreen renders are 100%.
    pub text_scale: f64,
}

impl SceneTree {
    /// Load and parse a `<slug>.semantic.txt` snapshot.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read, or if any non-blank line is a
    /// malformed record — a format change or truncation must be a hard input
    /// error, never silently dropped into "missing nodes".
    pub fn load_semantic(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read semantic snapshot {}: {e}", path.display()))?;
        Self::from_semantic_str(&raw)
            .map_err(|e| format!("semantic snapshot {}: {e}", path.display()))
    }

    /// Parse a semantic snapshot from text, failing on any malformed non-blank
    /// record (blank lines are skipped).
    ///
    /// # Errors
    /// Returns an error naming the offending line number and content.
    pub fn from_semantic_str(raw: &str) -> Result<Self, String> {
        let mut nodes = Vec::new();
        for (i, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(line) {
                Ok(node) => nodes.push(node),
                Err(e) => {
                    return Err(format!(
                        "malformed record at line {}: {e} — line: {line:?}",
                        i + 1
                    ));
                }
            }
        }
        Ok(Self {
            nodes,
            text_scale: 1.0,
        })
    }

    /// Build an id -> node index for lookup.
    #[must_use]
    pub fn index(&self) -> BTreeMap<&str, &SceneNode> {
        self.nodes.iter().map(|n| (n.id.as_str(), n)).collect()
    }

    /// Assert the scene's coordinate space is the documented audit space.
    ///
    /// `pf-shell`'s `semantic_snapshot` emits the scene root first (depth 0), so
    /// the first parsed node's bounds are the whole coordinate space every other
    /// node's bounds live in — the same space the design-facts geometry is
    /// compared against. A scene rendered at any other viewport would produce a
    /// plausible-but-wrong geometry ledger, so a root at the wrong size is a hard
    /// input error, not silently-shifted bounds. `w`/`h` are the audit space.
    ///
    /// # Errors
    /// Returns an error if the scene is empty or its root is not `w`x`h`.
    pub fn require_coordinate_space(&self, w: u32, h: u32) -> Result<(), String> {
        let root = self
            .nodes
            .first()
            .ok_or("scene snapshot has no nodes (cannot establish a coordinate space)")?;
        let (rw, rh) = (root.bounds.width, root.bounds.height);
        if (rw - f64::from(w)).abs() > f64::EPSILON || (rh - f64::from(h)).abs() > f64::EPSILON {
            return Err(format!(
                "scene root `{}` is {rw}x{rh}, not the documented audit coordinate space {w}x{h}; \
                 the shell was rendered at the wrong viewport and its bounds are meaningless here",
                root.id
            ));
        }
        Ok(())
    }
}

/// Parse one non-blank semantic-snapshot line:
/// `  <id> role=<Role> label="<label>" bounds=<x>,<y>,<w>,<h> state=NodeState { .. } action=<..>`
///
/// # Errors
/// Returns the reason a non-blank line does not match the expected record shape.
fn parse_line(line: &str) -> Result<SceneNode, String> {
    let trimmed = line.trim_start();
    let (id, rest) = trimmed.split_once(" role=").ok_or("missing ' role='")?;
    let role = rest.split(' ').next().unwrap_or("").to_string();
    let accessible_label = between(rest, "label=\"", "\" bounds=").unwrap_or_default();
    let bounds_str = after(rest, "bounds=").ok_or("missing 'bounds='")?;
    let bounds_str = bounds_str.split(" state=").next().unwrap_or(bounds_str);
    let bounds = parse_bounds(bounds_str).ok_or("unparseable bounds")?;
    Ok(SceneNode {
        id: id.trim().to_string(),
        role,
        accessible_label,
        bounds,
        type_role: String::new(),
        focused: state_flag(rest, "focused"),
        selected: state_flag(rest, "selected"),
    })
}

fn parse_bounds(s: &str) -> Option<Bounds> {
    // Exactly four fields — a record with more or fewer is malformed, never
    // silently truncated to the first four.
    let parts: [&str; 4] = s
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>()
        .try_into()
        .ok()?;
    Some(Bounds {
        x: finite(parts[0])?,
        y: finite(parts[1])?,
        width: finite(parts[2])?,
        height: finite(parts[3])?,
    })
}

/// Parse one bounds coordinate, rejecting non-finite values. Rust's float parser
/// ACCEPTS `NaN`/`inf`/`-inf`, and every `> epsilon` tolerance comparison
/// downstream (`require_coordinate_space`, the geometry comparator) is FALSE for
/// NaN — so a non-finite coordinate would parse, pass validation, and emit NO
/// divergence, silently trusting an invalid ledger. Reject it at the boundary.
fn finite(s: &str) -> Option<f64> {
    let v: f64 = s.parse().ok()?;
    v.is_finite().then_some(v)
}

fn state_flag(s: &str, flag: &str) -> bool {
    between(s, &format!("{flag}: "), ",").is_some_and(|v| v.trim() == "true")
}

fn between(s: &str, start: &str, end: &str) -> Option<String> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    let j = rest.find(end)?;
    Some(rest[..j].to_string())
}

fn after<'a>(s: &'a str, start: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    Some(&s[i..])
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // parsed literals compare exactly
mod tests {
    use super::*;

    const LINE: &str = "  wifi-glyph role=Group label=\"Wi-Fi connected\" bounds=1107.0,24.5,9.0,7.0 state=NodeState { focused: false, pressed: false, disabled: false, selected: true, unavailable: false, destructive: false, scrimmed: false, checked: false, expanded: false } action=None";

    #[test]
    fn parses_a_semantic_line() {
        let n = parse_line(LINE).expect("line parses");
        assert_eq!(n.id, "wifi-glyph");
        assert_eq!(n.role, "Group");
        assert_eq!(n.accessible_label, "Wi-Fi connected");
        assert_eq!(n.bounds.x, 1107.0);
        assert_eq!(n.bounds.width, 9.0);
        assert_eq!(n.bounds.height, 7.0);
        assert!(!n.focused);
        assert!(n.selected);
        assert!(n.type_role.is_empty());
    }

    #[test]
    fn parses_a_no_label_line() {
        let line = "    battery-level role=Group label=\"\" bounds=1125.5,26.5,7.4,3.0 state=NodeState { focused: false, selected: false } action=None";
        let n = parse_line(line).expect("line parses");
        assert_eq!(n.id, "battery-level");
        assert_eq!(n.accessible_label, "");
        assert_eq!(n.bounds.y, 26.5);
    }

    #[test]
    fn skips_blank_lines_between_records() {
        let raw = format!("{LINE}\n\n   \n{LINE}\n");
        let tree = SceneTree::from_semantic_str(&raw).expect("parses with blanks");
        assert_eq!(tree.nodes.len(), 2);
    }

    #[test]
    fn errors_on_a_garbled_nonblank_line() {
        let raw = format!("{LINE}\ngarbage line with no role or bounds\n");
        let err = SceneTree::from_semantic_str(&raw)
            .expect_err("a malformed non-blank record must be a hard error");
        assert!(err.contains("line 2"), "err={err}");
        assert!(err.contains("missing ' role='"), "err={err}");
    }

    #[test]
    fn errors_on_unparseable_bounds() {
        let raw = "  n role=Group label=\"\" bounds=nan,x,y,z state=NodeState { } action=None";
        let err = SceneTree::from_semantic_str(raw).expect_err("bad bounds must error");
        assert!(err.contains("unparseable bounds"), "err={err}");
    }

    #[test]
    fn errors_on_a_non_finite_bounds_coordinate() {
        // A FULLY PARSEABLE record whose bounds carry NaN: Rust's float parser
        // accepts "NaN", and `> epsilon` is false for NaN, so without an
        // is_finite() guard this record would pass require_coordinate_space and
        // emit no divergence — silently trusting an invalid ledger.
        let raw = "root role=Group label=\"\" bounds=0,0,NaN,720 state=NodeState { } action=None";
        let err = SceneTree::from_semantic_str(raw)
            .expect_err("a non-finite bounds coordinate must be a hard error");
        assert!(err.contains("unparseable bounds"), "err={err}");
    }

    #[test]
    fn errors_on_an_infinite_bounds_coordinate() {
        let raw = "root role=Group label=\"\" bounds=0,0,inf,720 state=NodeState { } action=None";
        let err = SceneTree::from_semantic_str(raw)
            .expect_err("an infinite bounds coordinate must be a hard error");
        assert!(err.contains("unparseable bounds"), "err={err}");
    }

    #[test]
    fn errors_on_the_wrong_bounds_field_count() {
        // Five comma-separated values is a malformed record, not a bounds
        // silently truncated to its first four fields.
        let raw =
            "root role=Group label=\"\" bounds=0,0,1280,720,5 state=NodeState { } action=None";
        let err = SceneTree::from_semantic_str(raw)
            .expect_err("a wrong bounds field count must be a hard error");
        assert!(err.contains("unparseable bounds"), "err={err}");
    }

    #[test]
    fn indexes_by_id() {
        let tree = SceneTree::from_semantic_str(LINE).expect("parses");
        assert!(tree.index().contains_key("wifi-glyph"));
    }

    /// A root line (depth 0) with controllable bounds, as `semantic_snapshot`
    /// emits it first.
    fn root_line(w: f64, h: f64) -> String {
        format!(
            "root role=Group label=\"\" bounds=0.0,0.0,{w:.1},{h:.1} state=NodeState {{ }} action=None"
        )
    }

    #[test]
    fn a_root_at_the_audit_space_validates() {
        let tree = SceneTree::from_semantic_str(&root_line(1280.0, 720.0)).expect("parses");
        tree.require_coordinate_space(1280, 720)
            .expect("a 1280x720 root must validate");
    }

    #[test]
    fn a_root_at_the_wrong_viewport_is_rejected() {
        let tree = SceneTree::from_semantic_str(&root_line(1024.0, 768.0)).expect("parses");
        let err = tree
            .require_coordinate_space(1280, 720)
            .expect_err("a wrong-viewport root must be a hard input error");
        assert!(err.contains("coordinate space"), "err={err}");
        assert!(err.contains("root"), "err={err}");
    }

    #[test]
    fn an_empty_scene_has_no_coordinate_space() {
        let tree = SceneTree::from_semantic_str("").expect("empty parses");
        let err = tree
            .require_coordinate_space(1280, 720)
            .expect_err("an empty scene cannot establish a coordinate space");
        assert!(err.contains("no nodes"), "err={err}");
    }
}
