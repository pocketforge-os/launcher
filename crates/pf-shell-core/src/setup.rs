//! W4b (tsp-ziac.4.3.8): the launcher's `[fetch]` setup action.
//!
//! W4 (`pf-catalog`) turns each fetch-enabled variant into a [`FetchSetup`]. This module
//! turns it into the Details setup button, maps the session authority's `start_setup` /
//! `cancel_setup` answers to UI state, and re-checks the item while its unit runs (N6).

use super::{Effect, ShellCore};
use pf_catalog::{Availability, FetchRunProbe, FetchSetup, FetchSetupState};
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
    pub const fn due(&self, now_ms: u64) -> bool {
        now_ms >= self.next_probe_ms
    }
    /// Feeds one probe; `true` means the run is over and the catalog must be re-scanned.
    ///
    /// Over means: the lock this watch saw is released; or a `result.json` from a run other
    /// than the one before the call is there without the lock (ready, failed, or an
    /// interrupted `running`); or no run showed itself within [`SETUP_START_GRACE_MS`]. A
    /// held lock keeps the watch alive for as long as the download takes.
    pub fn observe(&mut self, now_ms: u64, probe: &FetchRunProbe) -> bool {
        self.next_probe_ms = now_ms.saturating_add(SETUP_PROBE_INTERVAL_MS);
        if probe.lock_held {
            self.lock_seen = true;
            return false;
        }
        if self.lock_seen {
            return true;
        }
        if probe.run_id.is_some() && probe.run_id != self.baseline_run_id {
            return true;
        }
        now_ms.saturating_sub(self.started_ms) >= SETUP_START_GRACE_MS
    }
}

impl ShellCore {
    /// The setup button for `item`, when it has no Ready variant and a fetch setup.
    #[must_use]
    pub fn setup_view(&self, item: usize) -> Option<SetupView> {
        let entry = self.items.get(item)?;
        if entry
            .variants
            .iter()
            .any(|variant| matches!(variant.availability, Availability::Ready))
        {
            return None;
        }
        let setup = self.item_fetch_setup(item)?;
        let app_id = setup.intent.app_id.as_str();
        if self.setup_watching(app_id) || setup.state == FetchSetupState::SettingUp {
            return Some(SetupView {
                label: "Cancel setup",
                action: Some(SetupAction::Cancel),
            });
        }
        if self.setup_blocked.contains(app_id) {
            return Some(SetupView {
                label: "Setup unavailable",
                action: None,
            });
        }
        Some(SetupView {
            label: match setup.state {
                FetchSetupState::Failed | FetchSetupState::Interrupted => "Retry setup",
                FetchSetupState::NotStarted | FetchSetupState::SettingUp => "Set up",
            },
            action: Some(SetupAction::Start),
        })
    }

    pub(crate) fn item_fetch_setup(&self, item: usize) -> Option<&FetchSetup> {
        self.items
            .get(item)?
            .variants
            .iter()
            .find_map(|variant| variant.fetch_setup.as_ref())
    }

    /// The Details setup button was activated: emit the effect and remember the intent and
    /// the `run_id` seen before the call (the N6 watch baseline).
    pub(crate) fn setup_effect(&mut self, item: usize) -> Option<Effect> {
        let action = self.setup_view(item)?.action?;
        let setup = self.item_fetch_setup(item)?.clone();
        self.pending_setup = Some((setup.intent.clone(), setup.run_id));
        Some(match action {
            SetupAction::Start => Effect::StartSetup(setup.intent),
            SetupAction::Cancel => Effect::CancelSetup(setup.intent),
        })
    }

    fn setup_title(&self, app_id: &str) -> String {
        self.items
            .iter()
            .find(|item| {
                item.variants.iter().any(|variant| {
                    variant
                        .fetch_setup
                        .as_ref()
                        .is_some_and(|setup| setup.intent.app_id == app_id)
                })
            })
            .map_or_else(|| app_id.to_owned(), |item| item.title.clone())
    }

    /// Applies the authority's answer to the last [`Effect::StartSetup`]. Started and
    /// already-running both watch the item; not-enabled and unavailable leave no retry
    /// until the next catalog reload; busy and errors keep the retry.
    pub fn setup_start_result(&mut self, result: Result<SetupStart, &str>, now_ms: u64) {
        let Some((intent, baseline)) = self.pending_setup.take() else {
            return;
        };
        self.bump_revision();
        let app_id = intent.app_id;
        let title = self.setup_title(&app_id);
        self.session_status = Some(match result {
            Ok(SetupStart::Started | SetupStart::AlreadyRunning) => {
                self.setup_watches.retain(|watch| watch.app_id() != app_id);
                self.setup_watches
                    .push(SetupWatch::new(app_id, baseline, now_ms));
                format!("Setting up {title}…")
            }
            Ok(SetupStart::NotFetchEnabled) => {
                self.setup_blocked.insert(app_id);
                format!("Setup isn't available for {title}")
            }
            Ok(SetupStart::ItemUnavailable) => {
                self.setup_blocked.insert(app_id);
                format!("{title} isn't installed")
            }
            Ok(SetupStart::RejectedBusy) => {
                format!("Close the running app, then set up {title}")
            }
            Err(_) => format!("Setup couldn't start for {title}. Try again."),
        });
    }

    /// Applies the authority's answer to the last [`Effect::CancelSetup`]. The watch stays:
    /// the run itself reports `cancelled` and releases the lock.
    pub fn setup_cancel_result(&mut self, result: Result<SetupCancel, &str>) {
        let Some((intent, _)) = self.pending_setup.take() else {
            return;
        };
        self.bump_revision();
        let app_id = intent.app_id;
        let title = self.setup_title(&app_id);
        self.session_status = Some(match result {
            Ok(SetupCancel::Requested) => format!("Cancelling setup for {title}…"),
            Ok(SetupCancel::NotFetchEnabled) => {
                self.setup_blocked.insert(app_id);
                format!("Setup isn't available for {title}")
            }
            Ok(SetupCancel::ItemUnavailable) => {
                self.setup_blocked.insert(app_id);
                format!("{title} isn't installed")
            }
            Err(_) => format!("Setup couldn't be cancelled for {title}. Try again."),
        });
    }

    /// App ids whose re-check is due.
    #[must_use]
    pub fn setup_probes_due(&self, now_ms: u64) -> Vec<String> {
        self.setup_watches
            .iter()
            .filter(|watch| watch.due(now_ms))
            .map(|watch| watch.app_id().to_owned())
            .collect()
    }

    /// Feeds a probe for `app_id`; `true` means its run ended and the catalog must be
    /// re-scanned now (the watch is dropped).
    pub fn observe_setup(&mut self, app_id: &str, now_ms: u64, probe: &FetchRunProbe) -> bool {
        let Some(index) = self
            .setup_watches
            .iter()
            .position(|watch| watch.app_id() == app_id)
        else {
            return false;
        };
        if !self.setup_watches[index].observe(now_ms, probe) {
            return false;
        }
        self.setup_watches.remove(index);
        self.bump_revision();
        true
    }

    #[must_use]
    pub fn setup_watching(&self, app_id: &str) -> bool {
        self.setup_watches
            .iter()
            .any(|watch| watch.app_id() == app_id)
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
