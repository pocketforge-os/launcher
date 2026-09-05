//! The divergence LEDGER: structured per-component findings and their emitters
//! (a machine-readable JSON array + a human table).

use serde::Serialize;

/// Whether a finding is a real divergence or informational (e.g. a mapped node
/// the shell scene did not contain, or a comparator that could not run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// A measured divergence beyond tolerance.
    Divergence,
    /// A note that does not, on its own, indicate a fidelity defect.
    Info,
}

impl Severity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Divergence => "DIVERGE",
            Self::Info => "info",
        }
    }
}

/// One row of the ledger: a single (route, component, fact class) observation.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub route: String,
    pub selector: String,
    pub node: String,
    pub fact_class: String,
    pub expected: String,
    pub shipped: String,
    pub delta: String,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// Set to `true` by the baseline pass when this triple is accepted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub accepted: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if requires &bool
fn is_false(b: &bool) -> bool {
    !*b
}

impl Finding {
    /// Whether this finding is an unaccepted divergence (what `--gate` fails on).
    #[must_use]
    pub fn is_gating(&self) -> bool {
        self.severity == Severity::Divergence && !self.accepted
    }
}

/// The full ledger for one audit run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Ledger {
    pub findings: Vec<Finding>,
}

impl Ledger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend(&mut self, more: impl IntoIterator<Item = Finding>) {
        self.findings.extend(more);
    }

    #[must_use]
    pub fn divergences(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Divergence)
            .count()
    }

    #[must_use]
    pub fn gating(&self) -> usize {
        self.findings.iter().filter(|f| f.is_gating()).count()
    }

    /// Serialize the ledger to pretty JSON.
    ///
    /// # Errors
    /// Returns an error if serialization fails (should not happen for this shape).
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.findings).map_err(|e| format!("serialize ledger: {e}"))
    }

    /// Render a compact human table.
    #[must_use]
    pub fn to_table(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<8} {:<26} {:<24} {:<10} {:<20} {:<20} delta",
            "SEV", "route", "component", "class", "mockup", "shipped"
        );
        for f in &self.findings {
            let accepted = if f.accepted { " (accepted)" } else { "" };
            let _ = writeln!(
                out,
                "{:<8} {:<26} {:<24} {:<10} {:<20} {:<20} {}{accepted}",
                f.severity.as_str(),
                truncate(&f.route, 26),
                truncate(&f.selector, 24),
                truncate(&f.fact_class, 10),
                truncate(&f.expected, 20),
                truncate(&f.shipped, 20),
                f.delta,
            );
        }
        out
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('~');
        t
    }
}
