//! The committed component MAPPING table: mockup selector <-> shell scene node id,
//! per route, with the comparator classes to run for each component.

use std::path::Path;

use serde::Deserialize;

/// The mapping-table schema version this consumer understands. A committed
/// mapping carrying any other value is a hard input error — the audit trusts the
/// mapping's shape (selector/node correspondence, comparator classes), so a
/// schema it does not recognise must fail loudly, never be read on faith.
pub const SUPPORTED_MAPPING_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Mapping {
    /// Mapping schema version. REQUIRED (no serde default): a missing version is
    /// the same trusted-input failure as an unsupported one.
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
        let mapping: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("parse mapping {}: {e}", path.display()))?;
        mapping.validate()?;
        Ok(mapping)
    }

    /// Reject an unsupported mapping schema version before any route is audited.
    ///
    /// # Errors
    /// Returns an error naming the got-vs-supported schema version.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SUPPORTED_MAPPING_SCHEMA {
            return Err(format!(
                "mapping schema_version {} unsupported (this audit understands {SUPPORTED_MAPPING_SCHEMA}); \
                 the mapping table's shape may have changed — update the tool, not just the file",
                self.schema_version
            ));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_schema_version_fails_to_deserialize() {
        // No serde default: a mapping with no schema_version is an input failure,
        // never a silent default-to-0 that would then fail validation with a
        // confusing "0 unsupported" instead of naming the real problem.
        let json = r#"{"routes":[]}"#;
        let err = serde_json::from_str::<Mapping>(json)
            .expect_err("a missing schema_version must not deserialize");
        assert!(err.to_string().contains("schema_version"), "err={err}");
    }

    #[test]
    fn an_unsupported_schema_version_is_rejected() {
        let json = format!(
            r#"{{"schema_version":{},"routes":[]}}"#,
            SUPPORTED_MAPPING_SCHEMA + 1
        );
        let mapping: Mapping = serde_json::from_str(&json).expect("parses");
        let err = mapping
            .validate()
            .expect_err("an unsupported schema_version must be rejected");
        assert!(err.contains("schema_version"), "err={err}");
        assert!(
            err.contains(&(SUPPORTED_MAPPING_SCHEMA + 1).to_string()),
            "err names the got version: {err}"
        );
    }

    #[test]
    fn the_supported_schema_version_validates() {
        let json = format!(r#"{{"schema_version":{SUPPORTED_MAPPING_SCHEMA},"routes":[]}}"#);
        let mapping: Mapping = serde_json::from_str(&json).expect("parses");
        mapping
            .validate()
            .expect("the supported schema must stay green");
    }
}
