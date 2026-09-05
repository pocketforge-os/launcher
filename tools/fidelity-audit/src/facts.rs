//! Parse the committed design-facts ground truth (`design-facts/<route>.json`).
//!
//! Schema mirrors the design repo generator (`tools/design-facts/generate.py`):
//! a `header`, a sorted `unique` list, a per-match `instances` list, and a
//! `self_check` block. Only the fields the audit consumes are modelled; unknown
//! keys are ignored so a generator schema bump that only adds fields still parses.

use std::path::Path;

use serde::Deserialize;

use crate::{AUDIT_H, AUDIT_W};

/// The design-facts generator schema version this consumer understands. A file
/// carrying any other version is a hard input error — its extraction semantics
/// may differ, so trusting it would produce a plausible-but-wrong ledger.
pub const SUPPORTED_GENERATOR_VERSION: u32 = 1;

/// A whole route's ground-truth facts, as committed by the design generator.
#[derive(Debug, Clone, Deserialize)]
pub struct DesignFacts {
    pub header: Header,
    #[serde(default)]
    pub unique: Vec<UniqueEntry>,
    #[serde(default)]
    pub instances: Vec<InstanceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub route: String,
    #[serde(default)]
    pub source: String,
    pub viewport: Viewport,
    /// Generator schema version. REQUIRED (no default): a missing version is the
    /// same input failure as an unsupported one.
    pub generator_version: u32,
}

/// The mockup viewport the facts were measured at (fixed 1280x720).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Viewport {
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UniqueEntry {
    pub selector: String,
    pub facts: Facts,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstanceEntry {
    pub selector: String,
    pub index: usize,
    pub facts: Facts,
}

/// The per-element structural facts the audit compares against the shell.
#[derive(Debug, Clone, Deserialize)]
pub struct Facts {
    pub bbox: BBox,
    #[serde(default)]
    pub visible: bool,
    pub font: Font,
    /// Computed CSS color string, e.g. `rgb(244, 239, 230)`.
    pub color: String,
    pub border: Border,
    #[serde(rename = "textDecoration")]
    pub text_decoration: TextDecoration,
}

/// A bounding box in mockup pixel space (top-left origin).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct BBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Font {
    /// Computed font size as a CSS string, e.g. `15px`.
    pub size: String,
    #[serde(default)]
    pub weight: String,
}

/// Per-edge border facts (only the bottom edge is consumed by the decoration
/// comparator today, but the whole record is kept for future comparators).
#[derive(Debug, Clone, Deserialize)]
pub struct Border {
    #[serde(rename = "bottomWidth", default)]
    pub bottom_width: String,
    #[serde(rename = "bottomStyle", default)]
    pub bottom_style: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextDecoration {
    #[serde(default)]
    pub line: String,
}

impl DesignFacts {
    /// Load and parse a design-facts file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or is not valid facts JSON.
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read design-facts {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("parse design-facts {}: {e}", path.display()))
    }

    /// Validate the trusted ground-truth contract before any comparison runs.
    ///
    /// Wrong ground truth produces a plausible-but-wrong ledger rather than a
    /// hard error, so every trusted header invariant is enforced here, not
    /// assumed: the generator schema version must be exactly the one this
    /// consumer understands, the header route must be the route we are auditing,
    /// and the viewport must be the documented audit coordinate space.
    ///
    /// # Errors
    /// Returns an error naming the specific violated invariant.
    pub fn validate(&self, expected_route: &str) -> Result<(), String> {
        if self.header.generator_version != SUPPORTED_GENERATOR_VERSION {
            return Err(format!(
                "design-facts generator_version {} unsupported (this audit understands {SUPPORTED_GENERATOR_VERSION}); \
                 regenerate the vendored facts with a matching generator",
                self.header.generator_version
            ));
        }
        if self.header.route != expected_route {
            return Err(format!(
                "design-facts header.route {:?} does not match the audited route {expected_route:?} \
                 (wrong facts file vendored for this route)",
                self.header.route
            ));
        }
        let (vw, vh) = (self.header.viewport.w, self.header.viewport.h);
        if (vw - f64::from(AUDIT_W)).abs() > f64::EPSILON
            || (vh - f64::from(AUDIT_H)).abs() > f64::EPSILON
        {
            return Err(format!(
                "design-facts viewport {vw}x{vh} is not the documented audit coordinate space \
                 {AUDIT_W}x{AUDIT_H}; geometry/crop comparisons would be meaningless"
            ));
        }
        Ok(())
    }

    /// Look up a `unique` selector's facts.
    #[must_use]
    pub fn unique(&self, selector: &str) -> Option<&Facts> {
        self.unique
            .iter()
            .find(|e| e.selector == selector)
            .map(|e| &e.facts)
    }

    /// Look up an `instances` selector's facts at a document-order index.
    #[must_use]
    pub fn instance(&self, selector: &str, index: usize) -> Option<&Facts> {
        self.instances
            .iter()
            .find(|e| e.selector == selector && e.index == index)
            .map(|e| &e.facts)
    }
}

impl Font {
    /// Parse the computed font size (`"15px"`) into pixels.
    #[must_use]
    pub fn size_px(&self) -> Option<f64> {
        self.size.strip_suffix("px")?.trim().parse::<f64>().ok()
    }
}

impl TextDecoration {
    /// Whether the mockup element carries an underline text-decoration.
    #[must_use]
    pub fn is_underline(&self) -> bool {
        self.line.split_whitespace().any(|w| w == "underline")
    }
}

impl Border {
    /// Whether the bottom edge paints a visible border line (a possible
    /// underline treatment expressed as a border rather than a decoration).
    #[must_use]
    pub fn has_bottom_line(&self) -> bool {
        let width = self
            .bottom_width
            .strip_suffix("px")
            .and_then(|w| w.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        self.bottom_style != "none" && self.bottom_style != "hidden" && width > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal facts document with a controllable header.
    fn facts_json(generator_version: u32, route: &str, vw: f64, vh: f64) -> String {
        format!(
            r#"{{"header":{{"route":"{route}","viewport":{{"w":{vw},"h":{vh}}},
               "generator_version":{generator_version}}},"unique":[],"instances":[]}}"#
        )
    }

    fn load(json: &str) -> DesignFacts {
        serde_json::from_str(json).expect("facts fixture parses")
    }

    #[test]
    fn a_missing_generator_version_fails_to_deserialize() {
        // No serde default: a facts file with no generator_version is as much an
        // input failure as an unsupported one, never a silent default-to-1.
        let json = r#"{"header":{"route":"quiet-console/home","viewport":{"w":1280,"h":720}}}"#;
        let err = serde_json::from_str::<DesignFacts>(json)
            .expect_err("a missing generator_version must not deserialize");
        assert!(err.to_string().contains("generator_version"), "err={err}");
    }

    #[test]
    fn an_unsupported_generator_version_is_rejected() {
        let facts = load(&facts_json(
            SUPPORTED_GENERATOR_VERSION + 1,
            "quiet-console/home",
            1280.0,
            720.0,
        ));
        let err = facts
            .validate("quiet-console/home")
            .expect_err("an unsupported generator_version must be rejected");
        assert!(err.contains("generator_version"), "err={err}");
        assert!(
            err.contains(&(SUPPORTED_GENERATOR_VERSION + 1).to_string()),
            "err names the got version: {err}"
        );
    }

    #[test]
    fn a_route_mismatch_is_rejected() {
        let facts = load(&facts_json(
            SUPPORTED_GENERATOR_VERSION,
            "quiet-console/library",
            1280.0,
            720.0,
        ));
        let err = facts
            .validate("quiet-console/home")
            .expect_err("wrong facts file for the route must be rejected");
        assert!(err.contains("header.route"), "err={err}");
    }

    #[test]
    fn a_wrong_viewport_is_rejected() {
        let facts = load(&facts_json(
            SUPPORTED_GENERATOR_VERSION,
            "quiet-console/home",
            1024.0,
            768.0,
        ));
        let err = facts
            .validate("quiet-console/home")
            .expect_err("a non-audit viewport must be rejected");
        assert!(err.contains("viewport"), "err={err}");
    }

    #[test]
    fn a_conformant_header_validates() {
        let facts = load(&facts_json(
            SUPPORTED_GENERATOR_VERSION,
            "quiet-console/home",
            f64::from(AUDIT_W),
            f64::from(AUDIT_H),
        ));
        facts
            .validate("quiet-console/home")
            .expect("the happy path must stay green");
    }
}
