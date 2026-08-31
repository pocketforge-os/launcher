//! Curated design values which cannot be extracted as simple CSS declarations.
//!
//! Keep this module small: every value needs a source line citation.

/// Renderer-independent conservative glyph advance for the 14px label role.
/// `shell.css:292-299` defines the role but CSS cannot express raster glyph advance.
pub const LABEL_GLYPH_ADVANCE: f32 = 8.0;

/// Renderer-independent conservative glyph advance for the caption role.
/// `shell.css:301-310` defines the role but CSS cannot express raster glyph advance.
pub const CAPTION_GLYPH_ADVANCE: f32 = 6.5;

/// Gap between a chip label and its optional count; shell.css composes these with flex
/// rather than prescribing a label-to-count measurement (`shell.css:438-457`).
pub const CHIP_COUNT_GAP: f32 = 8.0;

/// Runtime scene sentinel for nodes which intentionally paint no surface. This is not a
/// Quiet Console CSS token and therefore is not generated from tokens.css.
pub const SCENE_TRANSPARENT_TOKEN: &str = "--color-transparent";
