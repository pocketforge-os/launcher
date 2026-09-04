//! The frozen, cited accepted-divergence baseline. In `--gate` mode, a
//! divergence whose `(route, selector, fact_class)` triple is listed here is
//! downgraded to accepted; any other divergence fails the audit.

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
    /// Required rationale (cited). Parsed to enforce presence at load time.
    #[serde(default)]
    #[allow(dead_code)]
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
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read baseline {}: {e}", path.display()))?;
        let file: BaselineFile = serde_json::from_str(&raw)
            .map_err(|e| format!("parse baseline {}: {e}", path.display()))?;
        let accepted = file
            .accepted
            .into_iter()
            .map(|e| (e.route, e.selector, e.fact_class))
            .collect();
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
