//! W4b setup-action tests (tsp-ziac.4.3.8): intent → effect, answer → UI state, N6 watch.

use super::*;
use crate::SETUP_SETTLE_MS;
use crate::{Route, ShellAction};
use pf_catalog::{
    AppKind, AppManifestRef, Availability, CatalogItem, CatalogSnapshot, FetchSetupIntent,
    FetchSetupState, Presentation as CP, Provenance, UserProjection, Variant,
};
use pf_scene::{Node, SurfaceMetrics};
use std::path::PathBuf;

const APP: &str = "com.example.fetch";
const TITLE: &str = "Fetched";

fn setup(state: FetchSetupState, run_id: Option<&str>) -> FetchSetup {
    FetchSetup {
        intent: FetchSetupIntent {
            app_id: APP.into(),
            unit: format!("pf-app-fetch@{APP}.service"),
        },
        state,
        reason: None,
        class: None,
        message: "Download game data".into(),
        retry_offered: !matches!(
            state,
            FetchSetupState::NotStarted | FetchSetupState::SettingUp
        ),
        run_id: run_id.map(str::to_owned),
    }
}

fn item(fetch: Option<FetchSetup>, availability: Availability) -> CatalogItem {
    CatalogItem {
        id: format!("installed:{APP}"),
        title: TITLE.into(),
        kind: AppKind::Game,
        presentation: CP {
            icon_reference: None,
            icon_decodable: false,
        },
        tags: vec![],
        variants: vec![Variant {
            id: "default".into(),
            provider_id: "installed".into(),
            availability,
            needs_network: false,
            requirements: vec![],
            provenance: Provenance {
                provider_id: "installed".into(),
                app_version: None,
                upstream_version: None,
                runtime_family: "pocketforge/a133-powervr".into(),
                runtime_abi: "1".into(),
                platform_version: None,
            },
            launch_target: AppManifestRef {
                app_id: APP.into(),
                descriptor_path: PathBuf::from("app.toml"),
                observed_digest: "x".into(),
            },
            fetch_setup: fetch,
        }],
    }
}

fn needs_setup() -> Availability {
    Availability::NeedsSetup {
        reason: "Download game data".into(),
    }
}

fn snapshot(items: Vec<CatalogItem>) -> CatalogSnapshot {
    CatalogSnapshot {
        revision: 1,
        observed_at_unix_seconds: 0,
        provider_results: vec![],
        user_projection: UserProjection::default(),
        items,
    }
}

/// A core showing the fetch item's Details page with the first slot focused.
fn details(state: FetchSetupState, run_id: Option<&str>) -> ShellCore {
    let mut core = ShellCore::boot(
        &snapshot(vec![item(Some(setup(state, run_id)), needs_setup())]),
        &pf_theme::flagship(),
        false,
    );
    core.authority_snapshot(false);
    core.selected_item = Some(0);
    core.go(Route::Details);
    core.focus = 0;
    core
}

fn intent() -> FetchSetupIntent {
    setup(FetchSetupState::NotStarted, None).intent
}

fn probe(lock_held: bool, outcome: Option<&str>, run_id: Option<&str>) -> FetchRunProbe {
    FetchRunProbe {
        lock_held,
        outcome: outcome.map(str::to_owned),
        run_id: run_id.map(str::to_owned),
    }
}

fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
    if node.id.as_str() == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, id))
}

fn metrics() -> SurfaceMetrics {
    SurfaceMetrics {
        logical_width: 1280.,
        logical_height: 720.,
        scale: 1.,
        safe_insets: Default::default(),
        orientation: pf_scene::Orientation::Landscape,
    }
}

// --- intent → effect -------------------------------------------------------------------

#[test]
fn details_setup_button_turns_the_w4_intent_into_start_setup() {
    let mut core = details(FetchSetupState::NotStarted, None);
    assert_eq!(
        core.action(&ShellAction::Activate),
        Some(Effect::StartSetup(intent()))
    );
}

#[test]
fn failed_and_interrupted_runs_offer_a_retry_that_starts_setup_again() {
    for state in [FetchSetupState::Failed, FetchSetupState::Interrupted] {
        let mut core = details(state, Some("run-1"));
        assert_eq!(
            core.setup_view(0),
            Some(SetupView {
                label: "Retry setup",
                action: Some(SetupAction::Start)
            }),
            "{state:?}"
        );
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::StartSetup(intent())),
            "{state:?}"
        );
    }
}

#[test]
fn a_running_setup_offers_cancel_which_turns_into_cancel_setup() {
    let mut core = details(FetchSetupState::SettingUp, Some("run-1"));
    assert_eq!(
        core.setup_view(0),
        Some(SetupView {
            label: "Cancel setup",
            action: Some(SetupAction::Cancel)
        })
    );
    assert_eq!(
        core.action(&ShellAction::Activate),
        Some(Effect::CancelSetup(intent()))
    );
}

#[test]
fn ready_items_and_items_without_fetch_have_no_setup_button() {
    let core = ShellCore::boot(
        &snapshot(vec![
            item(None, needs_setup()),
            item(None, Availability::Ready),
            // A Ready fetch item keeps Play; W4 sets no setup for a valid receipt anyway.
            item(
                Some(setup(FetchSetupState::NotStarted, None)),
                Availability::Ready,
            ),
        ]),
        &pf_theme::flagship(),
        false,
    );
    assert_eq!(core.setup_view(0), None);
    assert_eq!(core.setup_view(1), None);
    assert_eq!(core.setup_view(2), None);
}

#[test]
fn the_pin_stays_reachable_below_the_setup_button() {
    let mut core = details(FetchSetupState::NotStarted, None);
    core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    assert_eq!(core.focus, 1);
    assert_eq!(
        core.action(&ShellAction::Activate),
        Some(Effect::ToggleFavorite {
            item_id: format!("installed:{APP}"),
            favorite: true
        })
    );
    core.action(&ShellAction::Move(pf_scene::AxisMove::Up));
    assert_eq!(core.focus, 0);
}

#[test]
fn details_scene_draws_the_setup_button_focused() {
    let core = details(FetchSetupState::NotStarted, None);
    let scene = core.scene(metrics(), "A").expect("shell frame");
    let button = find(scene.root(), "detail-setup").expect("setup button");
    assert_eq!(button.accessible_label, "Set up");
    assert!(button.state.focused);
    assert!(find(scene.root(), "detail-unavailable").is_none());
    assert!(!find(scene.root(), "detail-pin").unwrap().state.focused);
}

// --- answer → UI state ----------------------------------------------------------------

/// Activates setup, then applies `answer`.
fn answered(answer: Result<SetupStart, &str>) -> ShellCore {
    let mut core = details(FetchSetupState::NotStarted, Some("run-0"));
    assert!(matches!(
        core.action(&ShellAction::Activate),
        Some(Effect::StartSetup(_))
    ));
    core.setup_start_result(answer, 1_000);
    core
}

#[test]
fn started_and_running_answers_watch_the_item_and_offer_cancel() {
    for answer in [SetupStart::Started, SetupStart::AlreadyRunning] {
        let core = answered(Ok(answer));
        assert!(core.setup_watching(APP), "{answer:?}");
        assert_eq!(
            core.setup_view(0),
            Some(SetupView {
                label: "Cancel setup",
                action: Some(SetupAction::Cancel)
            }),
            "{answer:?}"
        );
        assert_eq!(
            core.session_status(),
            Some("Setting up Fetched…"),
            "{answer:?}"
        );
    }
}

#[test]
fn refusals_without_a_retry_disable_the_button() {
    for (answer, status) in [
        (
            SetupStart::NotFetchEnabled,
            "Setup isn't available for Fetched",
        ),
        (SetupStart::ItemUnavailable, "Fetched isn't installed"),
    ] {
        let mut core = answered(Ok(answer));
        assert!(!core.setup_watching(APP));
        assert_eq!(core.session_status(), Some(status));
        assert_eq!(
            core.setup_view(0),
            Some(SetupView {
                label: "Setup unavailable",
                action: None
            })
        );
        assert_eq!(core.action(&ShellAction::Activate), None);
    }
}

#[test]
fn busy_and_errors_keep_a_retry() {
    for (answer, status) in [
        (
            Ok(SetupStart::RejectedBusy),
            "Close the running app, then set up Fetched",
        ),
        (
            Err("command exited 5"),
            "Setup couldn't start for Fetched. Try again.",
        ),
    ] {
        let mut core = answered(answer);
        assert!(!core.setup_watching(APP));
        assert_eq!(core.session_status(), Some(status));
        assert_eq!(
            core.setup_view(0).and_then(|view| view.action),
            Some(SetupAction::Start)
        );
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::StartSetup(intent()))
        );
    }
}

#[test]
fn cancel_answers_map_to_status_and_keep_the_watch_until_the_run_ends() {
    let mut core = answered(Ok(SetupStart::Started));
    assert_eq!(
        core.action(&ShellAction::Activate),
        Some(Effect::CancelSetup(intent()))
    );
    core.setup_cancel_result(Ok(SetupCancel::Requested));
    assert_eq!(core.session_status(), Some("Cancelling setup for Fetched…"));
    assert!(core.setup_watching(APP), "the run reports cancelled itself");

    core.action(&ShellAction::Activate);
    core.setup_cancel_result(Err("command exited 1"));
    assert_eq!(
        core.session_status(),
        Some("Setup couldn't be cancelled for Fetched. Try again.")
    );
}

#[test]
fn a_catalog_reload_clears_a_no_retry_block() {
    let mut core = answered(Ok(SetupStart::NotFetchEnabled));
    core.reload_catalog_with_art(
        &snapshot(vec![item(
            Some(setup(FetchSetupState::NotStarted, None)),
            needs_setup(),
        )]),
        |_, _| None,
    );
    assert_eq!(
        core.setup_view(0).and_then(|view| view.action),
        Some(SetupAction::Start)
    );
}

// --- N6: re-check on the tick until the run ends, then one full re-scan --------------

#[test]
fn watch_probes_on_an_interval() {
    let mut watch = SetupWatch::new(APP, None, 1_000);
    assert!(watch.due(1_000));
    assert!(!watch.observe(1_000, &probe(true, Some("running"), Some("run-1"))));
    assert!(!watch.due(1_000 + SETUP_PROBE_INTERVAL_MS - 1));
    assert!(watch.due(1_000 + SETUP_PROBE_INTERVAL_MS));
}

#[test]
fn watch_ends_when_the_lock_it_saw_is_released() {
    let mut watch = SetupWatch::new(APP, Some("run-0".into()), 0);
    assert!(!watch.observe(0, &probe(false, Some("failed"), Some("run-0"))));
    assert!(!watch.observe(500, &probe(true, Some("running"), Some("run-1"))));
    // Long downloads outlive the start grace while the lock is held.
    assert!(!watch.observe(
        SETUP_START_GRACE_MS * 10,
        &probe(true, Some("running"), Some("run-1"))
    ));
    assert!(watch.observe(
        SETUP_START_GRACE_MS * 10 + 500,
        &probe(false, Some("ready"), Some("run-1"))
    ));
}

#[test]
fn watch_ends_on_a_final_result_with_a_new_run_id_even_if_the_lock_was_never_seen() {
    for outcome in ["ready", "failed"] {
        let mut watch = SetupWatch::new(APP, Some("run-0".into()), 0);
        assert!(
            watch.observe(500, &probe(false, Some(outcome), Some("run-1"))),
            "{outcome}"
        );
    }
    // A new run that reads running without the lock may be between dropping the lock and
    // writing its final result (review r1 P2-1): it gets the settle, then counts as crashed.
    let mut watch = SetupWatch::new(APP, Some("run-0".into()), 0);
    let unlocked = probe(false, Some("running"), Some("run-1"));
    assert!(!watch.observe(500, &unlocked));
    assert!(!watch.observe(500 + SETUP_SETTLE_MS - 1, &unlocked));
    assert!(watch.observe(500 + SETUP_SETTLE_MS, &unlocked));
}

#[test]
fn watch_ignores_the_previous_verdict_until_the_start_grace_expires() {
    let mut watch = SetupWatch::new(APP, Some("run-0".into()), 0);
    let stale = probe(false, Some("failed"), Some("run-0"));
    assert!(!watch.observe(500, &stale));
    assert!(!watch.observe(SETUP_START_GRACE_MS - 1, &stale));
    assert!(watch.observe(SETUP_START_GRACE_MS, &stale));
    // No result at all (first ever run that never started) also ends at the grace.
    let mut watch = SetupWatch::new(APP, None, 0);
    assert!(!watch.observe(500, &FetchRunProbe::default()));
    assert!(watch.observe(SETUP_START_GRACE_MS, &FetchRunProbe::default()));
}

#[test]
fn core_rechecks_due_items_and_asks_for_one_rescan_when_the_run_ends() {
    let mut core = answered(Ok(SetupStart::Started));
    assert_eq!(core.setup_probes_due(1_000), vec![APP.to_owned()]);
    assert!(!core.observe_setup(APP, 1_000, &probe(true, Some("running"), Some("run-1"))));
    assert_eq!(core.setup_probes_due(1_200), Vec::<String>::new());
    assert_eq!(core.setup_probes_due(1_500), vec![APP.to_owned()]);
    assert!(core.observe_setup(APP, 1_500, &probe(false, Some("ready"), Some("run-1"))));
    assert!(!core.setup_watching(APP));
    assert_eq!(core.setup_probes_due(10_000), Vec::<String>::new());
    // The watch is gone, so later probes ask for nothing.
    assert!(!core.observe_setup(APP, 10_000, &FetchRunProbe::default()));
}

#[test]
fn a_running_answer_uses_the_current_run_id_as_the_baseline() {
    // setup_running while run-1 holds the lock: its own final result ends the watch.
    let mut core = details(FetchSetupState::SettingUp, Some("run-1"));
    core.setup_watches.clear();
    core.pending_setup = Some(PendingSetup {
        intent: intent(),
        baseline_run_id: Some("run-1".into()),
        baseline_running: true,
    });
    core.setup_start_result(Ok(SetupStart::AlreadyRunning), 0);
    assert!(!core.observe_setup(APP, 0, &probe(true, Some("running"), Some("run-1"))));
    assert!(core.observe_setup(APP, 500, &probe(false, Some("ready"), Some("run-1"))));
}

// --- review r1 --------------------------------------------------------------------------

#[test]
fn p2_1_a_released_lock_waits_for_the_final_result() {
    // pf-app-fetch drops the flock before finish() writes the final result.json.
    let mut watch = SetupWatch::new(APP, Some("run-0".into()), 0);
    assert!(!watch.observe(0, &probe(true, Some("running"), Some("run-1"))));
    let between = probe(false, Some("running"), Some("run-1"));
    assert!(
        !watch.observe(500, &between),
        "lock gone, result not final yet"
    );
    assert!(!watch.observe(1_000, &between));
    assert!(watch.observe(1_500, &probe(false, Some("failed"), Some("run-1"))));
}

#[test]
fn p2_1_the_settle_ends_a_run_that_crashed_after_its_lock_was_seen() {
    let mut watch = SetupWatch::new(APP, None, 0);
    assert!(!watch.observe(0, &probe(true, Some("running"), Some("run-1"))));
    let crashed = probe(false, Some("running"), Some("run-1"));
    assert!(!watch.observe(500, &crashed));
    assert!(!watch.observe(500 + SETUP_SETTLE_MS - 1, &crashed));
    assert!(watch.observe(500 + SETUP_SETTLE_MS, &crashed));
}

#[test]
fn p2_1_a_relocked_run_restarts_the_settle() {
    let mut watch = SetupWatch::new(APP, None, 0);
    assert!(!watch.observe(0, &probe(true, Some("running"), Some("run-1"))));
    assert!(!watch.observe(500, &probe(false, Some("running"), Some("run-1"))));
    assert!(!watch.observe(1_000, &probe(true, Some("running"), Some("run-2"))));
    let unlocked = probe(false, Some("running"), Some("run-2"));
    assert!(!watch.observe(1_000 + SETUP_SETTLE_MS, &unlocked));
    assert!(watch.observe(1_500 + SETUP_SETTLE_MS, &unlocked));
}

#[test]
fn n2_a_same_run_leaving_running_ends_the_watch_without_the_lock_ever_seen() {
    let mut watch = SetupWatch::new(APP, Some("run-1".into()), 0).with_baseline_running(true);
    assert!(!watch.observe(0, &probe(false, Some("running"), Some("run-1"))));
    assert!(watch.observe(500, &probe(false, Some("failed"), Some("run-1"))));
    // A baseline that was already final under that run_id is the stale verdict, not news.
    let mut watch = SetupWatch::new(APP, Some("run-1".into()), 0);
    assert!(!watch.observe(500, &probe(false, Some("failed"), Some("run-1"))));
}

#[test]
fn n2_a_start_from_an_interrupted_run_carries_the_running_baseline() {
    let mut core = details(FetchSetupState::Interrupted, Some("run-1"));
    assert!(matches!(
        core.action(&ShellAction::Activate),
        Some(Effect::StartSetup(_))
    ));
    core.setup_start_result(Ok(SetupStart::AlreadyRunning), 0);
    assert!(core.observe_setup(APP, 0, &probe(false, Some("ready"), Some("run-1"))));
}

#[test]
fn p2_2_setting_up_items_are_adopted_on_boot_and_reload_once() {
    let mut core = details(FetchSetupState::SettingUp, Some("run-1"));
    assert!(
        core.setup_watching(APP),
        "a run started before boot is watched"
    );
    core.reload_catalog_with_art(
        &snapshot(vec![item(
            Some(setup(FetchSetupState::SettingUp, Some("run-1"))),
            needs_setup(),
        )]),
        |_, _| None,
    );
    assert_eq!(core.setup_probes_due(0), vec![APP.to_owned()], "one watch");
    // Idle items are not adopted.
    let idle = details(FetchSetupState::Failed, Some("run-1"));
    assert!(!idle.setup_watching(APP));
}

#[test]
fn p2_2_cancelling_an_adopted_run_ends_with_exactly_one_rescan() {
    let mut core = details(FetchSetupState::SettingUp, Some("run-1"));
    assert_eq!(
        core.action(&ShellAction::Activate),
        Some(Effect::CancelSetup(intent()))
    );
    core.setup_cancel_result(Ok(SetupCancel::Requested));
    assert!(!core.observe_setup(APP, 0, &probe(true, Some("running"), Some("run-1"))));
    let cancelled = probe(false, Some("failed"), Some("run-1"));
    assert!(core.observe_setup(APP, 500, &cancelled), "one re-scan");
    assert!(!core.setup_watching(APP));
    assert!(!core.observe_setup(APP, 1_000, &cancelled));
    assert_eq!(core.setup_probes_due(10_000), Vec::<String>::new());
}

#[test]
fn p2_2_a_cancel_arms_a_watch_when_none_exists() {
    let mut core = details(FetchSetupState::SettingUp, Some("run-1"));
    core.setup_watches.clear();
    assert_eq!(
        core.action(&ShellAction::Activate),
        Some(Effect::CancelSetup(intent()))
    );
    core.setup_cancel_result(Ok(SetupCancel::Requested));
    assert!(core.setup_watching(APP));
    assert!(core.observe_setup(APP, 0, &probe(false, Some("failed"), Some("run-1"))));
}
