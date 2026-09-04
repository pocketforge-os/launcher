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
    /// Returns an error if the file cannot be read.
    pub fn load_semantic(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read semantic snapshot {}: {e}", path.display()))?;
        let nodes = raw.lines().filter_map(parse_line).collect();
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
}

/// Parse one semantic-snapshot line:
/// `  <id> role=<Role> label="<label>" bounds=<x>,<y>,<w>,<h> state=NodeState { .. } action=<..>`
fn parse_line(line: &str) -> Option<SceneNode> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let (id, rest) = trimmed.split_once(" role=")?;
    let role = rest.split(' ').next().unwrap_or("").to_string();
    let accessible_label = between(rest, "label=\"", "\" bounds=").unwrap_or_default();
    let bounds_str = after(rest, "bounds=")?;
    let bounds_str = bounds_str.split(" state=").next().unwrap_or(bounds_str);
    let bounds = parse_bounds(bounds_str)?;
    Some(SceneNode {
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
    let mut it = s.split(',').map(str::trim);
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    let width = it.next()?.parse().ok()?;
    let height = it.next()?.parse().ok()?;
    Some(Bounds {
        x,
        y,
        width,
        height,
    })
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
    fn blank_line_is_skipped() {
        assert!(parse_line("   ").is_none());
    }

    #[test]
    fn indexes_by_id() {
        let tree = SceneTree {
            nodes: vec![parse_line(LINE).unwrap()],
            text_scale: 1.0,
        };
        assert!(tree.index().contains_key("wifi-glyph"));
    }
}
