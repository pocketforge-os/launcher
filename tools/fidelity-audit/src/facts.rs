//! Parse the committed design-facts ground truth (`design-facts/<route>.json`).
//!
//! Schema mirrors the design repo generator (`tools/design-facts/generate.py`):
//! a `header`, a sorted `unique` list, a per-match `instances` list, and a
//! `self_check` block. Only the fields the audit consumes are modelled; unknown
//! keys are ignored so a generator schema bump that only adds fields still parses.

use std::path::Path;

use serde::Deserialize;

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
