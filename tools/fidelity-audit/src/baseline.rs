//! The frozen, cited accepted-divergence baseline. In `--gate` mode, a
//! divergence whose `(route, selector, fact_class)` triple is listed here is
//! downgraded to accepted; any other divergence fails the audit.
//!
//! The every-accepted-divergence-carries-a-cited-reason contract is ENFORCED,
//! not advertised: an entry that omits `reason` fails to parse, and an
//! empty/whitespace `reason` is rejected at load — otherwise an unexplained
//! entry could silently disable the gate for its triple.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;

use crate::ledger::Ledger;

#[derive(Debug, Clone, Deserialize)]
struct BaselineFile {
    #[serde(default)]
    accepted: Vec<AcceptedEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct AcceptedEntry {
    route: String,
    selector: String,
    fact_class: String,
    /// Required rationale — a bead id, a design ruling, or an intentional
    /// representational difference. No serde default: a missing `reason` is a
    /// parse error, and an empty/whitespace one is rejected in [`Baseline::from_json`].
    reason: String,
}

/// A set of accepted `(route, selector, fact_class)` triples.
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    accepted: HashSet<(String, String, String)>,
}

impl Baseline {
    /// Load the baseline from `accepted.json`.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or parsed, or if any accepted
    /// entry has a missing (parse error) or empty/whitespace `reason`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read baseline {}: {e}", path.display()))?;
        Self::from_json(&raw).map_err(|e| format!("baseline {}: {e}", path.display()))
    }

    /// Parse and validate a baseline from JSON text.
    ///
    /// # Errors
    /// Returns an error on invalid JSON or an entry with a blank `reason`.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let file: BaselineFile = serde_json::from_str(raw).map_err(|e| format!("parse: {e}"))?;
        let mut accepted = HashSet::new();
        for e in file.accepted {
            if e.reason.trim().is_empty() {
                return Err(format!(
                    "accepted entry ({}, {}, {}) has an empty reason — every accepted \
                     divergence must cite why (a bead id, a design ruling, or an intentional \
                     representational difference)",
                    e.route, e.selector, e.fact_class
                ));
            }
            accepted.insert((e.route, e.selector, e.fact_class));
        }
        Ok(Self { accepted })
    }

    /// Mark every ledger finding whose triple is accepted.
    pub fn apply(&self, ledger: &mut Ledger) {
        for f in &mut ledger.findings {
            let triple = (f.route.clone(), f.selector.clone(), f.fact_class.clone());
            if self.accepted.contains(&triple) {
                f.accepted = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_entry_with_a_cited_reason() {
        let b = Baseline::from_json(
            r#"{"accepted":[{"route":"r","selector":".x","fact_class":"geometry","reason":"tsp-op5a.999 representational box-model difference"}]}"#,
        )
        .expect("valid baseline loads");
        assert!(
            b.accepted
                .contains(&("r".into(), ".x".into(), "geometry".into()))
        );
    }

    #[test]
    fn empty_accepted_is_valid() {
        Baseline::from_json(r#"{"accepted":[]}"#).expect("empty accepted loads");
    }

    #[test]
    fn rejects_a_missing_reason() {
        // No `reason` key at all -> serde parse error (field is required).
        let err = Baseline::from_json(
            r#"{"accepted":[{"route":"r","selector":".x","fact_class":"geometry"}]}"#,
        )
        .expect_err("missing reason must be rejected");
        assert!(err.contains("parse"), "err={err}");
    }

    #[test]
    fn rejects_a_blank_reason() {
        let err = Baseline::from_json(
            r#"{"accepted":[{"route":"r","selector":".x","fact_class":"geometry","reason":"   "}]}"#,
        )
        .expect_err("blank reason must be rejected");
        assert!(err.contains("empty reason"), "err={err}");
    }
}
