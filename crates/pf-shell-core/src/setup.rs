//! W4b (tsp-ziac.4.3.8): the launcher's `[fetch]` setup action.
//!
//! W4 (`pf-catalog`) turns each fetch-enabled variant into a [`FetchSetup`]. This module
//! turns it into the Details setup button, maps the session authority's `start_setup` /
//! `cancel_setup` answers to UI state, and re-checks the item while its unit runs (N6).

use super::{Effect, ShellCore};
use pf_catalog::{FetchRunProbe, FetchSetup, FetchSetupIntent};
use pf_session_authority::{SetupCancel, SetupStart};

/// How often a watched item's `.pf-fetch/` state is re-read.
pub const SETUP_PROBE_INTERVAL_MS: u64 = 500;
/// How long a start may take to show a run (the lock, or a new `result.json`) before the
/// launcher stops watching and re-scans anyway.
pub const SETUP_START_GRACE_MS: u64 = 15_000;

/// What the setup button does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupAction {
    Start,
    Cancel,
}

/// The Details setup button for one item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupView {
    pub label: &'static str,
    /// `None` when setup cannot be offered (the authority refused it with no retry).
    pub action: Option<SetupAction>,
}

/// The N6 re-check of one item after `start_setup` was accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupWatch {
    app_id: String,
    baseline_run_id: Option<String>,
    started_ms: u64,
    next_probe_ms: u64,
    lock_seen: bool,
}

impl SetupWatch {
    #[must_use]
    pub fn new(app_id: impl Into<String>, baseline_run_id: Option<String>, now_ms: u64) -> Self {
        Self {
            app_id: app_id.into(),
            baseline_run_id,
            started_ms: now_ms,
            next_probe_ms: now_ms,
            lock_seen: false,
        }
    }
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    #[must_use]
    pub fn due(&self, _now_ms: u64) -> bool {
        false
    }
    /// Feeds one probe; `true` means the run is over and the catalog must be re-scanned.
    pub fn observe(&mut self, _now_ms: u64, _probe: &FetchRunProbe) -> bool {
        false
    }
}

impl ShellCore {
    /// The setup button for `item`, when it has no Ready variant and a fetch setup.
    #[must_use]
    pub fn setup_view(&self, _item: usize) -> Option<SetupView> {
        None
    }

    #[allow(dead_code)]
    pub(crate) fn item_fetch_setup(&self, item: usize) -> Option<&FetchSetup> {
        self.items
            .get(item)?
            .variants
            .iter()
            .find_map(|variant| variant.fetch_setup.as_ref())
    }

    #[allow(dead_code)]
    pub(crate) fn setup_effect(&mut self, _item: usize) -> Option<Effect> {
        None
    }

    /// Applies the authority's answer to the last [`Effect::StartSetup`].
    pub fn setup_start_result(&mut self, _result: Result<SetupStart, String>, _now_ms: u64) {}

    /// Applies the authority's answer to the last [`Effect::CancelSetup`].
    pub fn setup_cancel_result(&mut self, _result: Result<SetupCancel, String>) {}

    /// App ids whose re-check is due.
    #[must_use]
    pub fn setup_probes_due(&self, _now_ms: u64) -> Vec<String> {
        Vec::new()
    }

    /// Feeds a probe for `app_id`; `true` means re-scan the catalog now.
    pub fn observe_setup(&mut self, _app_id: &str, _now_ms: u64, _probe: &FetchRunProbe) -> bool {
        false
    }

    #[must_use]
    pub fn setup_watching(&self, _app_id: &str) -> bool {
        false
    }

    #[allow(dead_code)]
    fn pending_intent(&self) -> Option<&FetchSetupIntent> {
        None
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
