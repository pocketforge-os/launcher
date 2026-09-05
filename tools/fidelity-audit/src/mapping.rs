//! The committed component MAPPING table: mockup selector <-> shell scene node id,
//! per route, with the comparator classes to run for each component.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Mapping {
    #[serde(default)]
    pub schema_version: u32,
    pub routes: Vec<RouteMap>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteMap {
    /// Audit route id, matching the vendored `design-facts/<route>.json` path
    /// (without extension) and the design-facts `header.route`.
    pub route: String,
    /// Path under `design-facts/` to the vendored ground truth.
    pub design_facts: String,
    /// The `pf-shell --offscreen` slug producing `<slug>.json` + `<slug>.png`.
    pub shell_slug: String,
    /// Path (relative to the launcher repo root) to the approved mockup render,
    /// used by the perceptual crop comparator. Absent => no crop comparison.
    #[serde(default)]
    pub golden_png: Option<String>,
    /// Extra `pf-shell --offscreen` flags this route's slug requires.
    #[serde(default)]
    pub render_flags: Vec<String>,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Component {
    /// The design-facts selector string (the long-lived contract).
    pub selector: String,
    /// Document-order index into `instances`; absent => a `unique` selector.
    #[serde(default)]
    pub index: Option<usize>,
    /// The shell scene node id this selector corresponds to.
    pub node: String,
    #[serde(default)]
    pub classes: Vec<FactClass>,
    /// A shell child node whose presence encodes the active/focused underline
    /// treatment (consumed by the decoration comparator).
    #[serde(default)]
    pub underline_node: Option<String>,
    /// When true, the mapped scene node being ABSENT is a DECLARED non-gating
    /// carve-out (a component the shell intentionally does not render on this
    /// route yet). When false (the default), a missing node is a gating
    /// divergence — a node rename/removal must not silently skip comparators.
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// The comparator classes a component opts into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactClass {
    /// Bounding box vs mockup bbox (+-1px at 100%).
    Geometry,
    /// Computed font size vs mockup (type-role -> base size x text scale).
    FontSize,
    /// Render-sampled component color vs mockup computed color (opt-in).
    Color,
    /// Structural underline/decoration treatment.
    Decoration,
    /// Perceptual per-component crop diff (golden vs shell render).
    Crop,
}

impl FactClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Geometry => "geometry",
            Self::FontSize => "font-size",
            Self::Color => "color",
            Self::Decoration => "decoration",
            Self::Crop => "crop",
        }
    }
}

impl Mapping {
    /// Load and parse the mapping table.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or is not valid mapping JSON.
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read mapping {}: {e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("parse mapping {}: {e}", path.display()))
    }
}

impl Component {
    /// Whether the component opts into a comparator class.
    #[must_use]
    pub fn wants(&self, class: FactClass) -> bool {
        self.classes.contains(&class)
    }

    /// A stable human label for the component (`selector` or `selector[iN]`).
    #[must_use]
    pub fn label(&self) -> String {
        match self.index {
            Some(i) => format!("{}[i{i}]", self.selector),
            None => self.selector.clone(),
        }
    }
}
