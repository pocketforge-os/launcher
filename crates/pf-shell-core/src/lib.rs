//! Product shell state/event/effect reducer. Runtime lifecycle remains authority-owned.
#![allow(
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::struct_excessive_bools,
    clippy::semicolon_if_nothing_returned,
    clippy::cast_precision_loss,
    clippy::items_after_statements,
    clippy::default_trait_access,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use pf_catalog::{AppKind, Availability, CatalogSnapshot, Variant};
use pf_ports::{
    AppliedIdlePolicy, AppliedTransferState, AppliedValue, ChangeAuthority, ConnectResult,
    Deadline, EffectivePreference, IdlePolicy, LaunchRequest, LaunchResult, MonotonicTime,
    NetworkPort, NetworkState, NtpState, ObservedSessionState, PowerAction, PowerCapability,
    PowerError, PowerPort, PowerRequestResult, PreferenceChange, PreferenceKey, PreferencePoll,
    PreferencePort, PreferenceValue, SessionEvent, SessionPoll, SessionPort, ShellAction, Support,
    TerminalReceipt, TimeCapabilities, TimePort, TransferPort, TransferService,
    TransferServiceState, WifiCredential, WifiNetwork,
};
use pf_scene::{
    AxisMove, Bounds, ImageFit, ImageSource, Node, NodeAction, NodeId, Role, Scene, SurfaceMetrics,
};
use pf_session_authority::{EndPrecision, HistoryEntry};
use pf_theme::Theme;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const TIMEZONES: [&str; 4] = ["UTC", "America/New_York", "Europe/London", "Asia/Tokyo"];

fn action_label(action: &str) -> String {
    if action == "SafeReturn" {
        return "Safe Return".into();
    }
    action.strip_prefix("Move.").map_or_else(
        || action.to_owned(),
        |direction| format!("Move {direction}"),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Playtime {
    pub duration: Duration,
    pub approximate: bool,
}

/// Derives authority-owned playtime totals. Entries without both wall-clock stamps do not
/// contribute; backwards clocks contribute a known zero rather than a negative duration.
#[must_use]
pub fn derive_playtime(entries: &[HistoryEntry]) -> HashMap<String, Playtime> {
    let mut totals = HashMap::<String, Playtime>::new();
    for entry in entries {
        let (Some(started_at), Some(ended_at)) = (entry.started_at, entry.ended_at) else {
            continue;
        };
        let total = totals.entry(entry.item_id.clone()).or_insert(Playtime {
            duration: Duration::ZERO,
            approximate: false,
        });
        total.duration = total
            .duration
            .saturating_add(ended_at.at.duration_since(started_at).unwrap_or_default());
        total.approximate |= ended_at.precision == EndPrecision::Approximate;
    }
    totals
}

#[must_use]
pub fn format_playtime(playtime: Playtime) -> String {
    let prefix = if playtime.approximate { "~" } else { "" };
    let minutes = playtime.duration.as_secs() / 60;
    if minutes == 0 {
        return format!("Played {prefix}<1m");
    }
    let hours = minutes / 60;
    if hours == 0 {
        format!("Played {prefix}{minutes}m")
    } else {
        format!("Played {prefix}{hours}h {}m", minutes % 60)
    }
}

fn applied_bool_status(
    label: &str,
    result: &Result<AppliedValue<bool>, pf_ports::TimeError>,
) -> String {
    match result {
        Ok(value) if value.requested == value.applied => format!(
            "{label} applied · {}",
            if value.applied { "On" } else { "Off" }
        ),
        Ok(value) => format!(
            "{label} requested {} · applied {}",
            if value.requested { "On" } else { "Off" },
            if value.applied { "On" } else { "Off" }
        ),
        Err(error) => format!("{label} unavailable · {error:?}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    Home,
    Library,
    Search,
    Details,
    VariantChooser,
    Settings,
    Quick,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Presentation {
    FirstRun,
    Booting,
    Ready,
    Starting,
    Running,
    Returned,
    ForcedClose,
    Crash,
    RecoveryRequired,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Launch(LaunchRequest),
    SafeReturn,
    EnterRecovery,
    ChangePreference(PreferenceChange),
    ResetFirstRun,
    CaptureRemap,
    BeginRemap {
        context: String,
        action: String,
        control: String,
    },
    ConfirmRemap,
    RollbackRemap,
    ResetRemaps,
    CompleteFirstRun,
    ToggleFavorite {
        item_id: String,
        favorite: bool,
    },
    SetPinnedVariant {
        item_id: String,
        variant_id: Option<String>,
    },
    CaptureScreenshot,
    RequestPower(PowerAction),
    SetIdlePolicy(IdlePolicy),
    ConnectWifi {
        ssid: String,
        credential: WifiCredential,
    },
    SetTimezone(String),
    SetNtp(bool),
    SetManualTime(SystemTime),
    SetTransfer {
        service: TransferService,
        enabled: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PowerDialog {
    Closed,
    Confirm(PowerAction),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LibraryFilter {
    #[default]
    All,
    Favorites,
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlsFlow {
    Rows,
    Capture,
    RemapPreview,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlBinding {
    pub context: String,
    pub action: String,
    pub label: String,
    pub binding: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NetworkFlow {
    Rows,
    Credential,
}

#[derive(Clone, Copy)]
enum SystemRow {
    Timezone,
    Ntp,
    ManualTime,
    Transfer(TransferService),
    Accessibility,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsRoom {
    Display,
    Controls,
    Network,
    System,
}

#[derive(Clone, Debug, PartialEq)]
struct DisplayPreference {
    key: &'static str,
    label: &'static str,
    effective: PreferenceValue,
    interactive: bool,
}

#[derive(Clone, Debug)]
struct Item {
    id: String,
    title: String,
    kind: AppKind,
    tags: Vec<String>,
    art: Option<ImageSource>,
    art_failed: bool,
    variants: Vec<Variant>,
    favorite: bool,
    pinned_variant_id: Option<String>,
}

impl Item {
    fn has_real_art(&self) -> bool {
        self.art.is_some() && !self.art_failed
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtTreatment {
    CatalogArt,
    EditionPlate { palette: u8, motif: u8 },
}

pub struct ShellCore {
    revision: u64,
    route: Route,
    previous_route: Route,
    presentation: Presentation,
    items: Vec<Item>,
    focus: usize,
    saved_focus: [usize; 7],
    caller_route: Route,
    caller_focus: usize,
    selected_item: Option<usize>,
    search_query: String,
    search_results: Vec<usize>,
    library_filter: LibraryFilter,
    library_items: Vec<usize>,
    launch_focus: usize,
    active_title: String,
    crash_summary: String,
    crash_receipt_id: String,
    crash_exit_detail: String,
    recovery_available: bool,
    authority_unavailable: bool,
    session_status: Option<String>,
    pending_ack: bool,
    just_returned: bool,
    motion_ms: u32,
    reduced_motion: bool,
    high_contrast: bool,
    reduce_flashing: bool,
    text_scale: u16,
    settings_room: SettingsRoom,
    display_preferences: Vec<DisplayPreference>,
    first_run_complete: bool,
    safe_return_binding: String,
    safe_return_options: Vec<String>,
    controls_flow: ControlsFlow,
    control_bindings: Vec<ControlBinding>,
    controls_status: Option<String>,
    power_capabilities: Vec<PowerCapability>,
    applied_idle_policy: IdlePolicy,
    idle_policy_loaded: bool,
    power_status: Option<String>,
    power_dialog: PowerDialog,
    network_flow: NetworkFlow,
    network_state: Result<NetworkState, String>,
    wifi_networks: Vec<WifiNetwork>,
    selected_wifi: Option<usize>,
    wifi_credential: WifiCredential,
    network_status: Option<String>,
    time_capabilities: Result<TimeCapabilities, String>,
    time_state: Result<pf_ports::TimeState, String>,
    transfer_services: Result<Vec<TransferServiceState>, String>,
    system_status: Option<String>,
    playtime: HashMap<String, Playtime>,
}

impl ShellCore {
    #[must_use]
    pub fn boot(snapshot: &CatalogSnapshot, theme: &Theme, reduced_motion: bool) -> Self {
        Self::boot_with_art(snapshot, theme, reduced_motion, |_| None)
    }

    #[must_use]
    pub fn boot_with_art<F>(
        snapshot: &CatalogSnapshot,
        theme: &Theme,
        reduced_motion: bool,
        mut resolve_art: F,
    ) -> Self
    where
        F: FnMut(&str) -> Option<Arc<[u8]>>,
    {
        let favorites = &snapshot.user_projection.favorite_item_ids;
        let pins = &snapshot.user_projection.pinned_variant_ids;
        let items: Vec<_> = snapshot
            .items
            .iter()
            .map(|item| {
                let art = item
                    .presentation
                    .icon_reference
                    .as_deref()
                    .filter(|_| item.presentation.icon_decodable)
                    .and_then(&mut resolve_art)
                    .map(|bytes| {
                        let digest = Sha256::digest(&bytes);
                        ImageSource::new(format!("sha256:{digest:x}"), bytes)
                    });
                Item {
                    id: item.id.clone(),
                    title: item.title.clone(),
                    kind: item.kind.clone(),
                    tags: item.tags.clone(),
                    art,
                    art_failed: false,
                    variants: item.variants.clone(),
                    favorite: favorites.binary_search(&item.id).is_ok(),
                    pinned_variant_id: pins.get(&item.id).cloned(),
                }
            })
            .collect();
        Self {
            revision: 0,
            route: Route::Home,
            previous_route: Route::Home,
            presentation: Presentation::Booting,
            items,
            focus: 0,
            saved_focus: [0; 7],
            caller_route: Route::Home,
            caller_focus: 0,
            selected_item: None,
            search_query: String::new(),
            search_results: (0..snapshot.items.len()).collect(),
            library_filter: LibraryFilter::All,
            library_items: (0..snapshot.items.len()).collect(),
            launch_focus: 0,
            active_title: String::new(),
            crash_summary: String::new(),
            crash_receipt_id: String::new(),
            crash_exit_detail: String::new(),
            recovery_available: false,
            authority_unavailable: false,
            session_status: None,
            pending_ack: false,
            just_returned: false,
            motion_ms: theme
                .resolve_motion("launch", reduced_motion)
                .expect("motion.launch")
                .duration_ms,
            reduced_motion,
            high_contrast: false,
            reduce_flashing: false,
            text_scale: 100,
            settings_room: SettingsRoom::Display,
            display_preferences: Vec::new(),
            first_run_complete: true,
            safe_return_binding: "PF · the button below the d-pad".into(),
            safe_return_options: Vec::new(),
            controls_flow: ControlsFlow::Rows,
            control_bindings: Vec::new(),
            controls_status: None,
            power_capabilities: Vec::new(),
            applied_idle_policy: IdlePolicy::default(),
            idle_policy_loaded: false,
            power_status: None,
            power_dialog: PowerDialog::Closed,
            network_flow: NetworkFlow::Rows,
            network_state: Err("Network status unavailable".into()),
            wifi_networks: Vec::new(),
            selected_wifi: None,
            wifi_credential: WifiCredential::new(Vec::new()),
            network_status: None,
            time_capabilities: Err("Time controls unavailable".into()),
            time_state: Err("Time status unavailable".into()),
            transfer_services: Err("Transfer status unavailable".into()),
            system_status: None,
            playtime: HashMap::new(),
        }
    }

    pub fn load_history(&mut self, entries: &[HistoryEntry]) {
        let playtime = derive_playtime(entries);
        if self.playtime != playtime {
            self.playtime = playtime;
            self.bump_revision();
        }
    }

    pub fn load_network(&mut self, port: &mut dyn NetworkPort) {
        self.bump_revision();
        self.network_state = port
            .state()
            .map_err(|error| format!("Network unavailable · {error:?}"));
        self.wifi_networks = port.scan().unwrap_or_else(|error| {
            self.network_status = Some(format!("Scan unavailable · {error:?}"));
            Vec::new()
        });
    }

    pub fn load_system(&mut self, time: &dyn TimePort, transfer: &dyn TransferPort) {
        self.bump_revision();
        self.time_capabilities = time
            .capabilities()
            .map_err(|e| format!("Time controls unavailable · {e:?}"));
        self.time_state = time
            .read()
            .map_err(|e| format!("Time status unavailable · {e:?}"));
        self.transfer_services = transfer
            .services()
            .map_err(|e| format!("Transfer unavailable · {e:?}"));
    }

    pub fn set_wifi_passphrase(&mut self, secret: impl Into<Vec<u8>>) {
        self.bump_revision();
        self.wifi_credential = WifiCredential::new(secret);
    }

    pub fn network_result(&mut self, result: Result<ConnectResult, pf_ports::NetworkError>) {
        self.bump_revision();
        self.network_status = Some(match result {
            Ok(ConnectResult::Progress(progress)) => format!("Joining · {progress:?}"),
            Ok(ConnectResult::Connected { ssid }) => format!("Connected · {ssid}"),
            Ok(ConnectResult::Refused) => "Connection failed · authentication refused".into(),
            Ok(ConnectResult::NetworkNotFound) => "Connection failed · network not found".into(),
            Err(error) => format!("Connection unavailable · {error:?}"),
        });
        self.network_flow = NetworkFlow::Rows;
        self.wifi_credential = WifiCredential::new(Vec::new());
    }

    pub fn timezone_result(&mut self, result: Result<AppliedValue<String>, pf_ports::TimeError>) {
        self.system_status = Some(match &result {
            Ok(value) if value.requested == value.applied => {
                format!("Timezone applied · {}", value.applied)
            }
            Ok(value) => format!("Requested {} · applied {}", value.requested, value.applied),
            Err(error) => format!("Timezone unavailable · {error:?}"),
        });
        if let (Some(state), Ok(value)) = (self.time_state.as_mut().ok(), result) {
            state.timezone = value.applied;
        }
        self.bump_revision();
    }

    pub fn ntp_result(&mut self, result: Result<AppliedValue<bool>, pf_ports::TimeError>) {
        self.system_status = Some(applied_bool_status("Automatic time", &result));
        if let (Some(state), Ok(value)) = (self.time_state.as_mut().ok(), result) {
            state.ntp_state = if value.applied {
                NtpState::Active
            } else {
                NtpState::Inactive
            };
        }
        self.bump_revision();
    }

    pub fn manual_time_result(
        &mut self,
        result: Result<AppliedValue<SystemTime>, pf_ports::TimeError>,
    ) {
        self.system_status = Some(match &result {
            Ok(value) if value.requested == value.applied => "Manual time applied".into(),
            Ok(_) => "Manual time requested · device applied a different time".into(),
            Err(error) => format!("Manual time unavailable · {error:?}"),
        });
        if let (Ok(state), Ok(value)) = (&mut self.time_state, result) {
            state.wall_clock = value.applied;
        }
        self.bump_revision();
    }

    pub fn transfer_result(
        &mut self,
        result: Result<AppliedTransferState, pf_ports::TransferError>,
    ) {
        self.system_status = Some(match &result {
            Ok(value) => applied_bool_status(
                "File transfer",
                &Ok(AppliedValue {
                    requested: value.requested,
                    applied: value.applied.enabled,
                }),
            ),
            Err(error) => format!("File transfer unavailable · {error:?}"),
        });
        if let (Ok(states), Ok(value)) = (&mut self.transfer_services, result) {
            if let Some(state) = states
                .iter_mut()
                .find(|state| state.service == value.applied.service)
            {
                *state = value.applied;
            }
        }
        self.bump_revision();
    }

    fn system_rows(&self) -> Vec<SystemRow> {
        let mut rows = Vec::new();
        if self.time_state.is_ok() {
            rows.push(SystemRow::Timezone);
        }
        if self
            .time_state
            .as_ref()
            .is_ok_and(|state| state.ntp_state != NtpState::Unsupported)
        {
            rows.push(SystemRow::Ntp);
        }
        if self
            .time_capabilities
            .as_ref()
            .is_ok_and(|capabilities| capabilities.manual_set_time == Support::Supported)
            && self
                .time_state
                .as_ref()
                .is_ok_and(|state| state.ntp_state != NtpState::Active)
        {
            rows.push(SystemRow::ManualTime);
        }
        if let Ok(services) = &self.transfer_services {
            rows.extend(
                services
                    .iter()
                    .filter(|state| state.support == Support::Supported)
                    .map(|state| SystemRow::Transfer(state.service)),
            );
        }
        rows.push(SystemRow::Accessibility);
        rows
    }

    /// Loads Settings exclusively through the runtime preference boundary. A row is interactive
    /// only when the port reports an applied value; stored-only values remain visibly honest.
    pub fn load_preferences(
        &mut self,
        port: &dyn PreferencePort,
        first_run_complete: bool,
    ) -> Result<(), pf_ports::PreferenceError> {
        self.bump_revision();
        self.display_preferences.clear();
        for (key, label, default) in [
            (
                "textScale",
                "Text size",
                PreferenceValue::Text("100%".into()),
            ),
            (
                "highContrast",
                "High contrast",
                PreferenceValue::Bool(false),
            ),
            (
                "reduceMotion",
                "Reduce motion",
                PreferenceValue::Bool(false),
            ),
            (
                "reduceFlashing",
                "Reduce flashing",
                PreferenceValue::Bool(false),
            ),
        ] {
            let observed = port.read(&PreferenceKey(key.into()))?;
            let effective = observed.as_ref().map_or(default, |p| p.effective.clone());
            let interactive = observed.as_ref().is_some_and(|p| p.applied);
            self.apply_effective(key, &effective);
            self.display_preferences.push(DisplayPreference {
                key,
                label,
                effective,
                interactive,
            });
        }
        self.first_run_complete = first_run_complete;
        if !first_run_complete {
            self.presentation = Presentation::FirstRun;
            self.focus = 0;
        }
        Ok(())
    }

    pub fn preference_changed(&mut self, change: &EffectivePreference) {
        if !change.applied {
            return;
        }
        self.bump_revision();
        self.apply_effective(&change.key.0, &change.effective);
        if let Some(row) = self
            .display_preferences
            .iter_mut()
            .find(|row| row.key == change.key.0)
        {
            row.effective = change.effective.clone();
            row.interactive = true;
        }
    }

    pub fn drive_preferences(
        &mut self,
        port: &mut dyn PreferencePort,
    ) -> Result<(), pf_ports::PreferenceError> {
        while let PreferencePoll::Changed(change) =
            port.next_change(Deadline(MonotonicTime::ZERO))?
        {
            self.preference_changed(&change);
        }
        Ok(())
    }

    pub fn load_power(&mut self, port: &dyn PowerPort) {
        self.bump_revision();
        let capabilities = port.capabilities();
        let idle_policy = port.idle_policy();

        match &capabilities {
            Ok(capabilities) => self.power_capabilities.clone_from(capabilities),
            Err(_) => self.power_capabilities.clear(),
        }
        self.idle_policy_loaded = idle_policy.is_ok();
        if let Ok(policy) = idle_policy {
            self.applied_idle_policy = policy;
        }
        self.power_status = match (capabilities.is_err(), self.idle_policy_loaded) {
            (false, true) => None,
            (true, true) => Some("Power actions are unavailable".into()),
            (false, false) => Some("Auto-sleep is unavailable".into()),
            (true, false) => Some("Power controls are unavailable".into()),
        };
    }

    pub fn power_request_result(&mut self, result: Result<PowerRequestResult, PowerError>) {
        self.bump_revision();
        self.power_status = match result {
            Ok(PowerRequestResult::Accepted) => None,
            Ok(PowerRequestResult::Unsupported) => Some("That power action is unavailable".into()),
            Ok(PowerRequestResult::Refused { reason }) => {
                Some(format!("Power action refused · {reason}"))
            }
            Err(_) => Some("Power controls are unavailable".into()),
        };
    }

    pub fn idle_policy_result(&mut self, result: Result<AppliedIdlePolicy, PowerError>) {
        self.bump_revision();
        match result {
            Ok(result) => {
                self.applied_idle_policy = result.applied;
                self.idle_policy_loaded = true;
                self.power_status = None;
            }
            Err(_) => self.power_status = Some("Auto-sleep could not be changed".into()),
        }
    }

    fn supports_power(&self, action: PowerAction) -> bool {
        self.power_capabilities.iter().any(|capability| {
            capability.action == action && capability.support == Support::Supported
        })
    }

    fn sleep_row(&self) -> Option<usize> {
        self.supports_power(PowerAction::Sleep).then_some(4)
    }

    fn idle_row(&self) -> usize {
        4 + usize::from(self.sleep_row().is_some())
    }

    fn screenshot_row(&self) -> usize {
        self.idle_row() + usize::from(self.idle_policy_loaded)
    }

    fn apply_effective(&mut self, key: &str, value: &PreferenceValue) {
        match (key, value) {
            ("textScale", PreferenceValue::Text(value)) => {
                self.text_scale = value.trim_end_matches('%').parse().unwrap_or(100)
            }
            ("highContrast", PreferenceValue::Bool(value)) => self.high_contrast = *value,
            ("reduceMotion", PreferenceValue::Bool(value)) => {
                self.reduced_motion = *value;
                self.motion_ms = if *value { 0 } else { 180 };
            }
            ("reduceFlashing", PreferenceValue::Bool(value)) => self.reduce_flashing = *value,
            _ => {}
        }
    }

    pub fn set_safe_return_binding(&mut self, label: impl Into<String>) {
        self.bump_revision();
        self.safe_return_binding = label.into();
    }
    pub fn set_safe_return_options(&mut self, options: impl IntoIterator<Item = String>) {
        self.bump_revision();
        self.safe_return_options = options.into_iter().collect();
    }

    pub fn set_control_bindings(&mut self, bindings: Vec<ControlBinding>) {
        self.bump_revision();
        self.control_bindings = bindings;
    }

    pub fn remap_refused(&mut self, conflicting_action: &str) {
        self.bump_revision();
        self.controls_flow = ControlsFlow::Rows;
        self.controls_status = Some(format!(
            "That button is already bound to {} · choose another button",
            action_label(conflicting_action)
        ));
    }

    pub fn remap_committed(&mut self, bindings: Vec<ControlBinding>) {
        self.bump_revision();
        self.control_bindings = bindings;
        self.controls_status = Some("Binding saved".into());
    }

    pub fn remaps_reset(&mut self, bindings: Vec<ControlBinding>) {
        self.bump_revision();
        self.control_bindings = bindings;
        self.controls_status = Some("Controls reset to defaults".into());
    }
    fn first_run_preferences(&self) -> Vec<&DisplayPreference> {
        self.display_preferences
            .iter()
            .filter(|row| row.interactive)
            .collect()
    }
    pub fn reset_first_run(&mut self) {
        self.bump_revision();
        self.first_run_complete = false;
        self.presentation = Presentation::FirstRun;
        self.focus = 0;
    }
    #[must_use]
    pub const fn text_scale(&self) -> u16 {
        self.text_scale
    }
    #[must_use]
    pub const fn high_contrast(&self) -> bool {
        self.high_contrast
    }
    #[must_use]
    pub const fn reduce_flashing(&self) -> bool {
        self.reduce_flashing
    }

    pub fn authority_snapshot(&mut self, recovery_available: bool) {
        self.bump_revision();
        self.recovery_available = recovery_available;
        if self.presentation == Presentation::Booting {
            self.presentation = Presentation::Ready;
        }
    }
    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }
    #[must_use]
    pub const fn presentation(&self) -> &Presentation {
        &self.presentation
    }
    #[must_use]
    pub const fn focus(&self) -> usize {
        self.focus
    }
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    #[must_use]
    pub fn search_query(&self) -> &str {
        &self.search_query
    }
    #[must_use]
    pub fn search_result_ids(&self) -> Vec<&str> {
        self.search_results
            .iter()
            .map(|&index| self.items[index].id.as_str())
            .collect()
    }
    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.bump_revision();
        self.search_query = query.into();
        let words = self
            .search_query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        self.search_results = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let haystack = format!("{} {}", item.title, item.tags.join(" ")).to_lowercase();
                let filter_matches = match self.library_filter {
                    LibraryFilter::All => true,
                    LibraryFilter::Favorites => item.favorite,
                    LibraryFilter::Ready => item
                        .variants
                        .iter()
                        .any(|variant| matches!(variant.availability, Availability::Ready)),
                };
                filter_matches && words.iter().all(|word| haystack.contains(word))
            })
            .map(|(index, _)| index)
            .collect();
        self.focus = 0;
    }

    #[must_use]
    pub fn is_favorite(&self, item_id: &str) -> bool {
        self.items
            .iter()
            .any(|item| item.id == item_id && item.favorite)
    }

    pub fn favorite_committed(&mut self, item_id: &str, favorite: bool) {
        self.bump_revision();
        if let Some(item) = self.items.iter_mut().find(|item| item.id == item_id) {
            item.favorite = favorite;
        }
        self.refresh_library_items();
        self.focus = self.focus.min(self.focus_count().saturating_sub(1));
        self.session_status = None;
    }

    pub fn favorite_failed(&mut self, status: impl Into<String>) {
        self.bump_revision();
        self.session_status = Some(status.into());
    }

    pub fn pinned_variant_committed(&mut self, item_id: &str, variant_id: Option<String>) {
        self.bump_revision();
        if let Some(item) = self.items.iter_mut().find(|item| item.id == item_id) {
            item.pinned_variant_id = variant_id;
        }
        self.session_status = None;
    }

    pub fn pinned_variant_failed(&mut self, status: impl Into<String>) {
        self.bump_revision();
        self.session_status = Some(status.into());
    }

    pub fn screenshot_result(&mut self, result: Result<&str, ()>) {
        self.bump_revision();
        self.session_status = Some(match result {
            Ok(file_name) => format!("Screenshot saved · {file_name}"),
            Err(()) => "Screenshot could not be saved".into(),
        });
    }

    fn refresh_library_items(&mut self) {
        self.library_items = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let included = match self.library_filter {
                    LibraryFilter::All => true,
                    LibraryFilter::Favorites => item.favorite,
                    LibraryFilter::Ready => item
                        .variants
                        .iter()
                        .any(|variant| matches!(variant.availability, Availability::Ready)),
                };
                included.then_some(index)
            })
            .collect();
    }

    fn focused_item_index(&self) -> Option<usize> {
        match self.route {
            Route::Library => self
                .focus
                .checked_sub(4)
                .and_then(|i| self.library_items.get(i))
                .copied(),
            Route::Search => self.search_results.get(self.focus).copied(),
            Route::Details | Route::VariantChooser => self.selected_item,
            Route::Home => {
                if self.focus < self.items.len() {
                    Some(self.focus)
                } else {
                    self.items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| item.favorite)
                        .nth(self.focus - self.items.len())
                        .map(|(i, _)| i)
                }
            }
            _ => None,
        }
    }
    #[must_use]
    pub fn art_treatment(&self, item_id: &str) -> Option<ArtTreatment> {
        let item = self.items.iter().find(|item| item.id == item_id)?;
        if item.art.is_some() && !item.art_failed {
            return Some(ArtTreatment::CatalogArt);
        }
        let hash = item
            .id
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
            });
        Some(ArtTreatment::EditionPlate {
            palette: (hash % 6) as u8,
            motif: ((hash / 6) % 6) as u8,
        })
    }

    /// Marks image sources rejected by the rasterizer so the next scene uses its plate fallback.
    /// Returns whether the visible model changed and therefore needs another presentation.
    pub fn reject_art_sources<'a>(
        &mut self,
        source_ids: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        let source_ids = source_ids.into_iter().collect::<Vec<_>>();
        let mut changed = false;
        for item in &mut self.items {
            if !item.art_failed
                && item
                    .art
                    .as_ref()
                    .is_some_and(|art| source_ids.contains(&art.id.as_str()))
            {
                item.art_failed = true;
                changed = true;
            }
        }
        if changed {
            self.bump_revision();
        }
        changed
    }
    #[must_use]
    pub const fn motion_duration_ms(&self) -> u32 {
        self.motion_ms
    }
    #[must_use]
    pub const fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }
    #[must_use]
    pub const fn recovery_available(&self) -> bool {
        self.recovery_available
    }
    #[must_use]
    pub const fn authority_unavailable(&self) -> bool {
        self.authority_unavailable
    }
    #[must_use]
    pub fn session_status(&self) -> Option<&str> {
        self.session_status.as_deref()
    }
    pub fn session_backend_unavailable_at_boot(&mut self) {
        self.bump_revision();
        self.authority_unavailable = true;
        self.session_status = None;
        self.presentation = Presentation::Ready;
    }
    pub fn session_backend_unavailable(&mut self) {
        self.bump_revision();
        self.authority_unavailable = true;
        self.session_status = Some("The session service isn't reachable".into());
        self.presentation = Presentation::Ready;
    }
    pub fn session_backend_reachable(&mut self) {
        if self.authority_unavailable || self.session_status.is_some() {
            self.bump_revision();
        }
        self.authority_unavailable = false;
        self.session_status = None;
    }
    #[must_use]
    pub const fn needs_presentation_ack(&self) -> bool {
        self.pending_ack
    }
    #[must_use]
    pub const fn has_shell_frame(&self) -> bool {
        !matches!(
            self.presentation,
            Presentation::Running | Presentation::RecoveryRequired
        )
    }

    pub fn acknowledge_presentation(&mut self) -> bool {
        std::mem::take(&mut self.pending_ack)
    }

    pub fn action(&mut self, action: &ShellAction) -> Option<Effect> {
        self.bump_revision();
        if matches!(action, ShellAction::Custom(name) if name == "SafeReturn") {
            return Some(Effect::SafeReturn);
        }
        if !self.has_shell_frame() {
            return None;
        }
        if let PowerDialog::Confirm(power_action) = self.power_dialog {
            return match action {
                ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                    self.focus = 1;
                    None
                }
                ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                    self.focus = 0;
                    None
                }
                ShellAction::Back => {
                    self.power_dialog = PowerDialog::Closed;
                    self.focus = match power_action {
                        PowerAction::PowerOff => 2,
                        PowerAction::Restart => 3,
                        PowerAction::Sleep => self.sleep_row().unwrap_or(2),
                    };
                    None
                }
                ShellAction::Activate if self.focus == 0 => {
                    self.power_dialog = PowerDialog::Closed;
                    self.focus = match power_action {
                        PowerAction::PowerOff => 2,
                        PowerAction::Restart => 3,
                        PowerAction::Sleep => self.sleep_row().unwrap_or(2),
                    };
                    None
                }
                ShellAction::Activate => {
                    self.power_dialog = PowerDialog::Closed;
                    self.focus = 0;
                    Some(Effect::RequestPower(power_action))
                }
                ShellAction::Custom(_) => None,
            };
        }
        if self.presentation == Presentation::FirstRun {
            let rows = self.first_run_preferences();
            let row_count = rows.len();
            return match action {
                ShellAction::Custom(name) if name == "Start" => {
                    self.first_run_complete = true;
                    self.presentation = Presentation::Ready;
                    self.focus = 0;
                    Some(Effect::CompleteFirstRun)
                }
                ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                    self.focus = (self.focus + 1).min(row_count);
                    None
                }
                ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                    self.focus = self.focus.saturating_sub(1);
                    None
                }
                ShellAction::Activate if self.focus == row_count => {
                    self.first_run_complete = true;
                    self.presentation = Presentation::Ready;
                    self.focus = 0;
                    Some(Effect::CompleteFirstRun)
                }
                ShellAction::Activate => rows
                    .get(self.focus)
                    .and_then(|row| Self::preference_effect_for(row)),
                _ => None,
            };
        }
        if self.route == Route::Settings && self.settings_room == SettingsRoom::Controls {
            match self.controls_flow {
                ControlsFlow::Capture => {
                    return match action {
                        ShellAction::Back => {
                            self.controls_flow = ControlsFlow::Rows;
                            self.controls_status = None;
                            None
                        }
                        ShellAction::Custom(control) if control.starts_with("Capture.") => {
                            let selected = self.control_bindings.get(self.focus)?;
                            let effect = Effect::BeginRemap {
                                context: selected.context.clone(),
                                action: selected.action.clone(),
                                control: control.trim_start_matches("Capture.").into(),
                            };
                            self.controls_flow = ControlsFlow::RemapPreview;
                            Some(effect)
                        }
                        _ => None,
                    };
                }
                ControlsFlow::RemapPreview => {
                    return match action {
                        ShellAction::Activate => {
                            self.controls_flow = ControlsFlow::Rows;
                            Some(Effect::ConfirmRemap)
                        }
                        ShellAction::Back => {
                            self.controls_flow = ControlsFlow::Rows;
                            Some(Effect::RollbackRemap)
                        }
                        _ => None,
                    };
                }
                ControlsFlow::Rows => {}
            }
        }
        if self.route == Route::Settings
            && self.settings_room == SettingsRoom::Network
            && self.network_flow == NetworkFlow::Credential
        {
            return match action {
                ShellAction::Back => {
                    self.network_flow = NetworkFlow::Rows;
                    self.wifi_credential = WifiCredential::new(Vec::new());
                    None
                }
                ShellAction::Activate => {
                    let ssid = self.wifi_networks.get(self.selected_wifi?)?.ssid.clone();
                    let credential = std::mem::replace(
                        &mut self.wifi_credential,
                        WifiCredential::new(Vec::new()),
                    );
                    Some(Effect::ConnectWifi { ssid, credential })
                }
                _ => None,
            };
        }
        if matches!(self.presentation, Presentation::Crash) {
            return match action {
                ShellAction::Back => {
                    self.presentation = Presentation::Ready;
                    self.go(Route::Home);
                    None
                }
                ShellAction::Activate if self.focus == 0 => {
                    self.presentation = Presentation::Ready;
                    self.go(Route::Home);
                    None
                }
                ShellAction::Activate => {
                    self.focus = self.launch_focus;
                    self.presentation = Presentation::Ready;
                    self.go(Route::Home);
                    self.activate()
                }
                ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                    self.focus = 1;
                    None
                }
                ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                    self.focus = 0;
                    None
                }
                ShellAction::Custom(_) => None,
            };
        }
        match action {
            ShellAction::Custom(name) if name == "Favorite" => {
                if self.route == Route::VariantChooser {
                    let item = self.selected_item?;
                    let ready = self.ready_variants(item);
                    let variant = self.items[item].variants.get(*ready.get(self.focus)?)?;
                    let variant_id = (self.items[item].pinned_variant_id.as_deref()
                        != Some(variant.id.as_str()))
                    .then(|| variant.id.clone());
                    return Some(Effect::SetPinnedVariant {
                        item_id: self.items[item].id.clone(),
                        variant_id,
                    });
                }
                if let Some(item) = self.focused_item_index() {
                    return Some(Effect::ToggleFavorite {
                        item_id: self.items[item].id.clone(),
                        favorite: !self.items[item].favorite,
                    });
                }
            }
            ShellAction::Custom(name) if name == "Search" => {
                self.caller_route = self.route;
                self.caller_focus = self.focus;
                self.go(Route::Search);
            }
            ShellAction::Custom(name) if name == "Quick" => {
                self.go(Route::Quick);
            }
            ShellAction::Back if self.route == Route::Quick => {
                let route = self.previous_route;
                self.go(route);
            }
            ShellAction::Back if matches!(self.route, Route::Details | Route::Search) => {
                self.route = self.caller_route;
                self.focus = self.caller_focus.min(self.focus_count().saturating_sub(1));
            }
            ShellAction::Back if self.route == Route::VariantChooser => self.go(Route::Details),
            ShellAction::Back if self.route != Route::Home => self.go(Route::Home),
            ShellAction::Move(AxisMove::Right) if self.route == Route::Home => {
                self.go(Route::Library)
            }
            ShellAction::Move(AxisMove::Right)
                if self.route == Route::Library && (1..=3).contains(&self.focus) =>
            {
                self.focus = (self.focus + 1).min(3);
            }
            ShellAction::Move(AxisMove::Right) if self.route == Route::Library => {
                self.go(Route::Settings)
            }
            ShellAction::Move(AxisMove::Right) if self.route == Route::Settings => {
                self.settings_room = match self.settings_room {
                    SettingsRoom::Display => SettingsRoom::Controls,
                    SettingsRoom::Controls => SettingsRoom::Network,
                    SettingsRoom::Network | SettingsRoom::System => SettingsRoom::System,
                };
                self.focus = 0;
            }
            ShellAction::Move(AxisMove::Left)
                if self.route == Route::Settings && self.settings_room != SettingsRoom::Display =>
            {
                self.settings_room = match self.settings_room {
                    SettingsRoom::System => SettingsRoom::Network,
                    SettingsRoom::Network => SettingsRoom::Controls,
                    SettingsRoom::Controls | SettingsRoom::Display => SettingsRoom::Display,
                };
                self.focus = 0;
            }
            ShellAction::Move(AxisMove::Left) if self.route == Route::Settings => {
                self.go(Route::Library)
            }
            ShellAction::Move(AxisMove::Left)
                if self.route == Route::Library && (2..=3).contains(&self.focus) =>
            {
                self.focus -= 1;
            }
            ShellAction::Move(AxisMove::Left) if self.route == Route::Library => {
                self.go(Route::Home)
            }
            ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                self.focus = (self.focus + 1).min(self.focus_count().saturating_sub(1))
            }
            ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                self.focus = self.focus.saturating_sub(1)
            }
            ShellAction::Activate => return self.activate(),
            ShellAction::Back | ShellAction::Custom(_) => {}
        }
        None
    }

    fn activate(&mut self) -> Option<Effect> {
        if self.route == Route::Settings {
            if self.recovery_available
                && self.settings_room == SettingsRoom::Display
                && self.focus == self.display_preferences.len().max(1)
            {
                return Some(Effect::EnterRecovery);
            }
            return match self.settings_room {
                SettingsRoom::Display => self.preference_effect(self.focus),
                SettingsRoom::Controls if self.focus < self.control_bindings.len() => {
                    self.controls_flow = ControlsFlow::Capture;
                    self.controls_status = None;
                    Some(Effect::CaptureRemap)
                }
                SettingsRoom::Controls => Some(Effect::ResetRemaps),
                SettingsRoom::Network => {
                    self.selected_wifi = Some(self.focus);
                    self.network_flow = NetworkFlow::Credential;
                    self.wifi_credential = WifiCredential::new(Vec::new());
                    None
                }
                SettingsRoom::System => match self.system_rows().get(self.focus).copied()? {
                    SystemRow::Timezone => {
                        let current = self.time_state.as_ref().ok()?.timezone.as_str();
                        let next = TIMEZONES
                            .iter()
                            .position(|zone| *zone == current)
                            .map_or(0, |index| (index + 1) % TIMEZONES.len());
                        Some(Effect::SetTimezone(TIMEZONES[next].into()))
                    }
                    SystemRow::Ntp => Some(Effect::SetNtp(
                        self.time_state.as_ref().ok()?.ntp_state != NtpState::Active,
                    )),
                    SystemRow::ManualTime => Some(Effect::SetManualTime(
                        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
                    )),
                    SystemRow::Transfer(service) => {
                        let enabled = self
                            .transfer_services
                            .as_ref()
                            .ok()?
                            .iter()
                            .find(|state| state.service == service)?
                            .enabled;
                        Some(Effect::SetTransfer {
                            service,
                            enabled: !enabled,
                        })
                    }
                    SystemRow::Accessibility => Some(Effect::ResetFirstRun),
                },
            };
        }
        if self.route == Route::Settings
            && self.recovery_available
            && self.focus + 1 == self.focus_count()
        {
            return Some(Effect::EnterRecovery);
        }
        if self.route == Route::Quick {
            return match self.focus {
                0 => {
                    self.go(self.previous_route);
                    self.activate()
                }
                1 => {
                    self.go(Route::Library);
                    None
                }
                2 if self.supports_power(PowerAction::PowerOff) => {
                    self.power_dialog = PowerDialog::Confirm(PowerAction::PowerOff);
                    self.focus = 0;
                    None
                }
                3 if self.supports_power(PowerAction::Restart) => {
                    self.power_dialog = PowerDialog::Confirm(PowerAction::Restart);
                    self.focus = 0;
                    None
                }
                row if self.sleep_row() == Some(row) => {
                    Some(Effect::RequestPower(PowerAction::Sleep))
                }
                row if self.idle_policy_loaded && row == self.idle_row() => {
                    let minutes = self
                        .applied_idle_policy
                        .sleep_after
                        .map(|duration| duration.as_secs() / 60);
                    let next = match minutes {
                        None => Some(5),
                        Some(5) => Some(10),
                        Some(10) => Some(15),
                        _ => None,
                    };
                    Some(Effect::SetIdlePolicy(IdlePolicy {
                        sleep_after: next.map(|minutes| Duration::from_secs(minutes * 60)),
                        power_off_after: self.applied_idle_policy.power_off_after,
                    }))
                }
                row if row == self.screenshot_row() => Some(Effect::CaptureScreenshot),
                _ => None,
            };
        }
        if self.route == Route::Library {
            if self.focus == 0 {
                self.caller_route = Route::Library;
                self.caller_focus = 0;
                self.go(Route::Search);
                return None;
            }
            if (1..=3).contains(&self.focus) {
                self.library_filter = [
                    LibraryFilter::All,
                    LibraryFilter::Favorites,
                    LibraryFilter::Ready,
                ][self.focus - 1];
                self.refresh_library_items();
                return None;
            }
            self.selected_item = self.library_items.get(self.focus - 4).copied();
            self.caller_route = Route::Library;
            self.caller_focus = self.focus;
            self.go(Route::Details);
            return None;
        }
        if self.route == Route::Search {
            let &item = self.search_results.get(self.focus)?;
            self.selected_item = Some(item);
            self.caller_route = Route::Search;
            self.caller_focus = self.focus;
            self.go(Route::Details);
            return None;
        }
        if self.route == Route::Details {
            let item = self.selected_item?;
            if let Some(variant) = self.pinned_ready_variant(item) {
                return self.launch_variant(item, variant);
            }
            let ready = self.ready_variants(item);
            if self.items[item].pinned_variant_id.is_some() {
                self.go(Route::VariantChooser);
                self.focus = 0;
                return None;
            }
            return match ready.len() {
                0 => None,
                1 => self.launch_variant(item, ready[0]),
                _ => {
                    self.go(Route::VariantChooser);
                    self.focus = 0;
                    None
                }
            };
        }
        if self.route == Route::VariantChooser {
            let item = self.selected_item?;
            let ready = self.ready_variants(item);
            let variant = *ready.get(self.focus)?;
            return self.launch_variant(item, variant);
        }
        if self.route != Route::Home {
            return None;
        }
        if self.items.is_empty() {
            return None;
        }
        let item = self.focused_item_index()?;
        if let Some(variant) = self.pinned_ready_variant(item) {
            return self.launch_variant(item, variant);
        }
        let ready = self.ready_variants(item);
        if self.items[item].pinned_variant_id.is_some() {
            self.selected_item = Some(item);
            self.caller_route = Route::Home;
            self.caller_focus = self.focus;
            self.go(Route::VariantChooser);
            self.focus = 0;
            return None;
        }
        match ready.len() {
            0 => None,
            1 => self.launch_variant(item, ready[0]),
            _ => {
                self.selected_item = Some(item);
                self.caller_route = Route::Home;
                self.caller_focus = self.focus;
                self.go(Route::VariantChooser);
                self.focus = 0;
                None
            }
        }
    }

    fn ready_variants(&self, item: usize) -> Vec<usize> {
        self.items[item]
            .variants
            .iter()
            .enumerate()
            .filter(|(_, variant)| matches!(variant.availability, Availability::Ready))
            .map(|(index, _)| index)
            .collect()
    }

    fn pinned_ready_variant(&self, item: usize) -> Option<usize> {
        let pinned = self.items[item].pinned_variant_id.as_deref()?;
        self.items[item].variants.iter().position(|variant| {
            variant.id == pinned && matches!(variant.availability, Availability::Ready)
        })
    }

    fn launch_variant(&mut self, item: usize, variant: usize) -> Option<Effect> {
        let selected = &self.items[item];
        let request = selected.variants.get(variant)?.launch_target.app_id.clone();
        self.launch_focus = if self.caller_route == Route::Home {
            item
        } else {
            self.caller_focus
        };
        self.active_title.clone_from(&selected.title);
        self.presentation = Presentation::Starting;
        Some(Effect::Launch(LaunchRequest { item_id: request }))
    }

    fn preference_effect(&self, index: usize) -> Option<Effect> {
        let row = self.display_preferences.get(index)?;
        Self::preference_effect_for(row)
    }

    fn preference_effect_for(row: &DisplayPreference) -> Option<Effect> {
        if !row.interactive {
            return None;
        }
        let value = match &row.effective {
            PreferenceValue::Bool(value) => PreferenceValue::Bool(!value),
            PreferenceValue::Text(value) => PreferenceValue::Text(
                match value.as_str() {
                    "100%" => "150%",
                    "150%" => "200%",
                    _ => "100%",
                }
                .into(),
            ),
            PreferenceValue::Integer(value) => PreferenceValue::Integer(*value),
        };
        Some(Effect::ChangePreference(PreferenceChange {
            key: PreferenceKey(row.key.into()),
            value,
            authority: ChangeAuthority("user".into()),
        }))
    }

    fn go(&mut self, route: Route) {
        self.saved_focus[self.route_index()] = self.focus;
        if route == Route::Quick {
            self.previous_route = self.route;
        }
        self.route = route;
        self.focus = self.saved_focus[self.route_index()].min(self.focus_count().saturating_sub(1));
    }
    fn route_index(&self) -> usize {
        match self.route {
            Route::Home => 0,
            Route::Library => 1,
            Route::Search => 2,
            Route::Details => 3,
            Route::VariantChooser => 4,
            Route::Settings => 5,
            Route::Quick => 6,
        }
    }
    fn focus_count(&self) -> usize {
        match self.route {
            Route::Home => {
                (self.items.len() + self.items.iter().filter(|item| item.favorite).count()).max(1)
            }
            Route::Library => self.library_items.len() + 4,
            Route::Search => self.search_results.len().max(1),
            Route::Details => 1,
            Route::VariantChooser => self
                .selected_item
                .map_or(0, |item| self.ready_variants(item).len())
                .max(1),
            Route::Settings => match self.settings_room {
                SettingsRoom::Display => {
                    self.display_preferences.len().max(1) + usize::from(self.recovery_available)
                }
                SettingsRoom::Controls => match self.controls_flow {
                    ControlsFlow::Rows | ControlsFlow::Capture | ControlsFlow::RemapPreview => {
                        self.control_bindings.len() + 1
                    }
                },
                SettingsRoom::Network => match self.network_flow {
                    NetworkFlow::Rows => self.wifi_networks.len().max(1),
                    NetworkFlow::Credential => 1,
                },
                SettingsRoom::System => self.system_rows().len().max(1),
            },
            Route::Quick => self.screenshot_row() + 1,
        }
    }

    pub fn launch_result(&mut self, result: &LaunchResult) {
        self.bump_revision();
        match result {
            LaunchResult::Accepted { .. } => self.presentation = Presentation::Starting,
            _ => self.presentation = Presentation::Ready,
        }
    }
    pub fn session_event(&mut self, event: &SessionEvent) {
        self.bump_revision();
        match event {
            SessionEvent::Observed(ObservedSessionState::Starting) => {
                self.presentation = Presentation::Starting
            }
            SessionEvent::Observed(ObservedSessionState::Running) => {
                self.presentation = Presentation::Running
            }
            SessionEvent::Terminal(TerminalReceipt::Returned { .. }) => {
                self.presentation = Presentation::Returned;
                self.focus = self.launch_focus;
                self.just_returned = true;
                self.pending_ack = true;
            }
            SessionEvent::Terminal(TerminalReceipt::ForcedClose { .. }) => {
                self.presentation = Presentation::ForcedClose;
                self.focus = self.launch_focus;
                self.pending_ack = true;
            }
            SessionEvent::Terminal(TerminalReceipt::Crash {
                session_id,
                summary,
            }) => {
                self.presentation = Presentation::Crash;
                self.crash_summary.clone_from(summary);
                self.crash_receipt_id.clone_from(session_id);
                self.crash_exit_detail.clone_from(summary);
                self.focus = 0;
                self.pending_ack = true;
            }
            SessionEvent::RecoveryRequired(_) => {
                self.presentation = Presentation::RecoveryRequired;
                self.pending_ack = false;
            }
            SessionEvent::Observed(
                ObservedSessionState::Suspended | ObservedSessionState::ObservationComplete,
            ) => {}
        }
    }

    pub fn drive_session(
        &mut self,
        port: &mut dyn SessionPort,
    ) -> Result<(), pf_ports::SessionError> {
        while let SessionPoll::Event(event) = port.next_event(Deadline(MonotonicTime::ZERO))? {
            self.session_event(&event);
            if matches!(self.presentation, Presentation::RecoveryRequired) {
                break;
            }
        }
        Ok(())
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    #[must_use]
    pub fn scene(&self, metrics: SurfaceMetrics, footer: &str) -> Option<Scene> {
        if !self.has_shell_frame() {
            return None;
        }
        let (w, h) = (metrics.logical_width, metrics.logical_height);
        let mut children = vec![
            node(
                "rooms",
                Role::Text,
                "L     Home     Library     Settings     R",
                w / 2.0 - 220.0,
                16.0,
                440.0,
                32.0,
                "--state-rest-text",
            ),
            node(
                "status-cluster",
                Role::Text,
                if self.authority_unavailable {
                    "Wi-Fi   82%   !   9:41"
                } else {
                    "Wi-Fi   82%   9:41"
                },
                w - 248.0,
                16.0,
                200.0,
                32.0,
                "--color-text-secondary",
            ),
        ];
        if let Some(status) = &self.session_status {
            children.push(node(
                "session-status",
                Role::Text,
                status,
                48.0,
                266.0,
                520.0,
                32.0,
                "--color-text-secondary",
            ));
        }
        match self.presentation {
            Presentation::FirstRun => self.first_run_nodes(&mut children, w),
            Presentation::Crash => self.crash_nodes(&mut children, w, h),
            _ if self.route == Route::Quick => self.quick_nodes(&mut children, w, h),
            _ => self.route_nodes(&mut children, w, h),
        }
        let footer = if let Some((base, glyph)) = footer.split_once('\u{1f}') {
            self.focused_item_index().map_or_else(
                || base.to_owned(),
                |item| {
                    let label = if self.route == Route::VariantChooser {
                        let ready = self.ready_variants(item);
                        ready
                            .get(self.focus)
                            .and_then(|index| self.items[item].variants.get(*index))
                            .map_or("Set as default", |variant| {
                                if self.items[item].pinned_variant_id.as_deref()
                                    == Some(variant.id.as_str())
                                {
                                    "Remove default"
                                } else {
                                    "Set as default"
                                }
                            })
                    } else if self.items[item].favorite {
                        "Unfavorite"
                    } else {
                        "Favorite"
                    };
                    format!("{base}     {glyph}  {label}")
                },
            )
        } else {
            footer.to_owned()
        };
        children.push(node(
            "prompts",
            Role::Text,
            &footer,
            w - 600.0,
            h - 48.0,
            552.0,
            32.0,
            "--color-text-secondary",
        ));
        let focus_id = children
            .iter()
            .find(|n| n.state.focused)
            .map_or("quiet-console", |n| n.id.as_str())
            .to_owned();
        let root = Node::new(
            NodeId::new("quiet-console").unwrap(),
            Role::Group,
            "",
            Bounds::new(0.0, 0.0, w, h),
            "--color-surface-canvas",
        )
        .with_children(children);
        Some(
            Scene::new(root, NodeId::new(focus_id).unwrap())
                .expect("one deterministic focus owner"),
        )
    }

    fn route_nodes(&self, out: &mut Vec<Node>, w: f32, h: f32) {
        let heading = match self.route {
            Route::Home => {
                if self.just_returned {
                    "RECENT · JUST NOW"
                } else {
                    "RECENT · TONIGHT"
                }
            }
            Route::Library => "LIBRARY",
            Route::Search => "SEARCH",
            Route::Details => "DETAILS",
            Route::VariantChooser => "HOW DO YOU WANT TO PLAY?",
            Route::Settings => match self.settings_room {
                SettingsRoom::Display => "SETTINGS · DISPLAY",
                SettingsRoom::Controls => "SETTINGS · CONTROLS",
                SettingsRoom::Network => "SETTINGS · NETWORK",
                SettingsRoom::System => "SETTINGS · SYSTEM",
            },
            Route::Quick => unreachable!(),
        };
        out.push(node(
            "route-heading",
            Role::Heading,
            heading,
            48.0,
            112.0,
            500.0,
            48.0,
            "--state-rest-text",
        ));
        if self.route == Route::Home {
            let focused = self
                .focused_item_index()
                .and_then(|index| self.items.get(index));
            let ready_count = self
                .items
                .iter()
                .filter(|item| {
                    item.variants
                        .iter()
                        .any(|variant| matches!(variant.availability, Availability::Ready))
                })
                .count();
            out.extend([
                node(
                    "hero-title",
                    Role::Heading,
                    focused.map_or("Nothing ready", |item| item.title.as_str()),
                    48.0,
                    154.0,
                    620.0,
                    64.0,
                    "--state-rest-text",
                ),
                node(
                    "hero-status",
                    Role::Text,
                    if matches!(self.presentation, Presentation::Starting) {
                        "● Starting · Game · Installed"
                    } else {
                        "● Ready · Game · Installed"
                    },
                    48.0,
                    226.0,
                    480.0,
                    32.0,
                    if matches!(self.presentation, Presentation::Starting) {
                        "--color-text-muted"
                    } else {
                        "--color-status-ready"
                    },
                ),
                node(
                    "ready-now-label",
                    Role::Heading,
                    &format!("READY NOW · {ready_count}"),
                    48.0,
                    398.0,
                    220.0,
                    28.0,
                    "--color-text-muted",
                ),
            ]);
            let gap = 24.0;
            let card_width = 158.0_f32.min((w - 96.0) / self.items.len().max(1) as f32 - gap);
            for (i, item) in self.items.iter().enumerate() {
                let availability = best_availability(item);
                let status = availability_text(availability, &self.presentation);
                let x = 48.0 + i as f32 * (card_width + gap);
                let card_label = if item.has_real_art() {
                    String::new()
                } else {
                    format!("{} — {status}", item.title)
                };
                let mut n = node(
                    &format!("item-{}", item.id),
                    Role::Button,
                    &card_label,
                    x,
                    430.0,
                    card_width,
                    210.0,
                    state_token(availability, i == self.focus),
                );
                n.action = Some(NodeAction::Activate);
                n.state.focused = i == self.focus;
                n.state.disabled = !item
                    .variants
                    .iter()
                    .any(|variant| matches!(variant.availability, Availability::Ready));
                n.children = art_nodes(item, "home-card", x, 438.0, card_width, i == self.focus);
                out.push(n);
            }
            let favorite_items: Vec<_> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.favorite)
                .collect();
            if !favorite_items.is_empty() {
                out.push(node(
                    "favorites-label",
                    Role::Heading,
                    &format!("FAVORITES · {}", favorite_items.len()),
                    48.0,
                    646.0,
                    220.0,
                    28.0,
                    "--color-text-muted",
                ));
                for (shelf_index, (item_index, item)) in favorite_items.into_iter().enumerate() {
                    let x = 286.0 + shelf_index as f32 * 174.0;
                    let focused = self.focus == self.items.len() + shelf_index;
                    let mut card = node(
                        &format!("favorite-item-{}", item.id),
                        Role::Button,
                        if item.has_real_art() { "" } else { &item.title },
                        x,
                        638.0,
                        158.0,
                        72.0,
                        state_token(best_availability(item), focused),
                    );
                    card.state.focused = focused;
                    card.action = Some(NodeAction::Activate);
                    card.children = art_nodes(item, "favorite-card", x, 640.0, 158.0, focused);
                    let _ = item_index;
                    out.push(card);
                }
            }
            if self.presentation == Presentation::ForcedClose {
                out.push(node(
                    "attention",
                    Role::Text,
                    &format!("Attention · {} didn't close cleanly", self.active_title),
                    48.0,
                    500.0,
                    w - 96.0,
                    42.0,
                    "--color-status-attention",
                ));
            }
        } else if self.route == Route::Library {
            let mut search = node(
                "library-search",
                Role::Button,
                &format!("Search {} titles", self.items.len()),
                48.0,
                170.0,
                w - 96.0,
                52.0,
                if self.focus == 0 {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            search.state.focused = self.focus == 0;
            search.action = Some(NodeAction::Activate);
            out.push(search);
            for (index, (label, filter)) in [
                ("All", LibraryFilter::All),
                ("Favorites", LibraryFilter::Favorites),
                ("Ready", LibraryFilter::Ready),
            ]
            .into_iter()
            .enumerate()
            {
                let focused = self.focus == index + 1;
                let active = self.library_filter == filter;
                let mut chip = node(
                    &format!("library-filter-{}", label.to_lowercase()),
                    Role::Button,
                    label,
                    48.0 + index as f32 * 142.0,
                    232.0,
                    126.0,
                    38.0,
                    if focused {
                        "--state-focused-ring"
                    } else if active {
                        "--state-selected-surface"
                    } else {
                        "--state-rest-surface"
                    },
                );
                chip.state.focused = focused;
                chip.state.selected = active;
                chip.action = Some(NodeAction::Activate);
                out.push(chip);
            }
            let columns = if w >= 1100.0 {
                6
            } else if w >= 760.0 {
                4
            } else {
                3
            };
            let card_width = (w - 96.0 - (columns - 1) as f32 * 16.0) / columns as f32;
            let row_height = 250.0;
            let card_top = 286.0;
            let mut visible_rows: usize = 1;
            while card_top + (visible_rows + 1) as f32 * row_height - 18.0 <= h {
                visible_rows += 1;
            }
            let focused_row = self.focus.saturating_sub(4) / columns;
            let first_visible_row = focused_row.saturating_sub(visible_rows.saturating_sub(1));
            for (i, &item_index) in self.library_items.iter().enumerate() {
                let item = &self.items[item_index];
                let availability = best_availability(item);
                let column = i % columns;
                let row = i / columns;
                let card_label = if item.has_real_art() {
                    String::new()
                } else {
                    format!(
                        "{} · {}",
                        item.title,
                        availability_text(availability, &self.presentation)
                    )
                };
                let mut card = node(
                    &format!("library-item-{}", item.id),
                    Role::Button,
                    &card_label,
                    48.0 + column as f32 * (card_width + 16.0),
                    card_top + (row as f32 - first_visible_row as f32) * row_height,
                    card_width,
                    232.0,
                    state_token(availability, self.focus == i + 4),
                );
                card.state.focused = self.focus == i + 4;
                card.action = Some(NodeAction::Activate);
                card.children = art_nodes(
                    item,
                    "library-card",
                    48.0 + column as f32 * (card_width + 16.0),
                    card_top + 8.0 + (row as f32 - first_visible_row as f32) * row_height,
                    card_width,
                    self.focus == i + 4,
                );
                out.push(card);
            }
        } else if self.route == Route::Search {
            out.push(node(
                "search-query",
                Role::Text,
                &format!(
                    "{}│ · Titles and tags · Back returns to where you were",
                    self.search_query
                ),
                48.0,
                165.0,
                w - 96.0,
                54.0,
                "--state-rest-text",
            ));
            if self.search_results.is_empty() {
                out.push(node(
                    "search-empty",
                    Role::Text,
                    if self.items.is_empty() {
                        "Your shelf is empty — nothing to search yet."
                    } else {
                        "Nothing matches — check the spelling, or browse the Library."
                    },
                    48.0,
                    250.0,
                    w - 96.0,
                    70.0,
                    "--color-text-secondary",
                ));
            }
            for (result, &item_index) in self.search_results.iter().enumerate() {
                let item = &self.items[item_index];
                let availability = best_availability(item);
                let mut row = node(
                    &format!("search-result-{}", item.id),
                    Role::Button,
                    &format!(
                        "{} · {} · {}",
                        item.title,
                        kind_text(&item.kind),
                        availability_text(availability, &self.presentation)
                    ),
                    w * 0.46,
                    230.0 + result as f32 * 68.0,
                    w * 0.48,
                    58.0,
                    if self.focus == result {
                        "--state-focused-ring"
                    } else {
                        state_token(availability, false)
                    },
                );
                row.state.focused = self.focus == result;
                row.action = Some(NodeAction::Activate);
                out.push(row);
            }
        } else if matches!(self.route, Route::Details | Route::VariantChooser) {
            let Some(item_index) = self.selected_item else {
                return;
            };
            let item = &self.items[item_index];
            let has_playtime = self.playtime.contains_key(&item.id);
            let detail_offset = if has_playtime { 16.0 } else { 0.0 };
            let provider = item.variants.first().map_or("No provider", |variant| {
                variant.provenance.provider_id.as_str()
            });
            let descriptor = item
                .variants
                .first()
                .map_or("No descriptor".to_owned(), |variant| {
                    variant.launch_target.descriptor_path.display().to_string()
                });
            out.push(node(
                "detail-provenance",
                Role::Text,
                &format!(
                    "{} · Provider {} · {}",
                    kind_text(&item.kind),
                    provider,
                    descriptor
                ),
                400.0,
                165.0,
                w - 448.0,
                42.0,
                "--color-text-secondary",
            ));
            out.push(node(
                "detail-title",
                Role::Heading,
                &item.title,
                400.0,
                212.0,
                w - 448.0,
                58.0,
                "--state-rest-text",
            ));
            if let Some(playtime) = self.playtime.get(&item.id).copied() {
                out.push(node(
                    "detail-playtime",
                    Role::Text,
                    &format_playtime(playtime),
                    400.0,
                    258.0,
                    w - 448.0,
                    30.0,
                    "--color-text-secondary",
                ));
            }
            out.extend(art_nodes(item, "detail-art", 48.0, 165.0, 304.0, false));
            if let Some(pinned) = &item.pinned_variant_id {
                out.push(node(
                    "detail-pinned-variant",
                    Role::Text,
                    &format!("Default version · {pinned}"),
                    400.0,
                    350.0 + detail_offset,
                    w - 448.0,
                    34.0,
                    "--state-rest-surface",
                ));
            }
            if let Some(variant) = item.variants.first() {
                let availability = availability_text(&variant.availability, &self.presentation);
                let state = availability.split(" — ").next().unwrap_or(&availability);
                for (index, label) in [
                    state.to_owned(),
                    format!("Provider {}", variant.provenance.provider_id),
                    format!("Edition {}", variant.id),
                ]
                .into_iter()
                .enumerate()
                {
                    out.push(node(
                        &format!("detail-badge-{index}"),
                        Role::Text,
                        &label,
                        400.0 + index as f32 * 190.0,
                        278.0 + detail_offset,
                        174.0,
                        36.0,
                        state_token(&variant.availability, false),
                    ));
                }
                out.push(node(
                    "detail-availability-reason",
                    Role::Text,
                    &availability,
                    400.0,
                    320.0 + detail_offset,
                    w - 448.0,
                    38.0,
                    "--color-text-secondary",
                ));
            }
            for (variant_index, variant) in item.variants.iter().enumerate() {
                out.push(node(
                    &format!("detail-variant-{variant_index}"),
                    Role::Text,
                    &format!(
                        "{} · Provider {} · {}",
                        variant.id,
                        variant.provenance.provider_id,
                        availability_text(&variant.availability, &self.presentation)
                    ),
                    400.0,
                    390.0 + detail_offset + variant_index as f32 * 55.0,
                    w - 448.0,
                    48.0,
                    state_token(&variant.availability, false),
                ));
            }
            let ready = self.ready_variants(item_index);
            if self.route == Route::VariantChooser {
                let pin_unavailable = item.pinned_variant_id.is_some()
                    && self.pinned_ready_variant(item_index).is_none();
                out.push(node(
                    "chooser-note",
                    Role::Text,
                    if pin_unavailable {
                        "Default unavailable — choose a version"
                    } else {
                        "Ready right now. Back leaves without opening anything."
                    },
                    360.0,
                    235.0,
                    w - 720.0,
                    40.0,
                    "--color-text-secondary",
                ));
                for (choice, &variant_index) in ready.iter().enumerate() {
                    let variant = &item.variants[variant_index];
                    let mut row = node(
                        &format!("chooser-{}", variant.id),
                        Role::Button,
                        &format!(
                            "{} · Provider {}{}",
                            variant.id,
                            variant.provenance.provider_id,
                            if item.pinned_variant_id.as_deref() == Some(&variant.id) {
                                " · Default"
                            } else {
                                ""
                            }
                        ),
                        360.0,
                        300.0 + choice as f32 * 64.0,
                        w - 720.0,
                        54.0,
                        if self.focus == choice {
                            "--state-focused-ring"
                        } else {
                            "--state-rest-surface"
                        },
                    );
                    row.state.focused = self.focus == choice;
                    row.action = Some(NodeAction::Activate);
                    out.push(row);
                }
            } else if !ready.is_empty() {
                let mut open = node(
                    "detail-open",
                    Role::Button,
                    if ready.len() == 1 {
                        "Open"
                    } else {
                        "Choose how to play"
                    },
                    400.0,
                    510.0,
                    360.0,
                    54.0,
                    "--state-focused-ring",
                );
                open.state.focused = true;
                open.action = Some(NodeAction::Activate);
                out.push(open);
            } else {
                out.push(node(
                    "detail-unavailable",
                    Role::Text,
                    "Unavailable · No usable way to play right now",
                    400.0,
                    510.0,
                    w - 448.0,
                    60.0,
                    "--state-unavailable-surface",
                ));
            }
        } else if self.route == Route::Settings {
            self.settings_nodes(out, w);
        } else {
            let labels: Vec<&str> = if self.recovery_available {
                vec![
                    "Accessibility and controls arrive in Settings",
                    "Open independent recovery",
                ]
            } else {
                vec!["Accessibility and controls arrive in Settings"]
            };
            for (i, label) in labels.iter().enumerate() {
                let mut n = node(
                    &format!("link-{i}"),
                    Role::Button,
                    label,
                    48.0,
                    190.0 + i as f32 * 70.0,
                    w - 96.0,
                    54.0,
                    if i == self.focus {
                        "--state-focused-ring"
                    } else {
                        "--state-rest-surface"
                    },
                );
                n.state.focused = i == self.focus;
                n.action = Some(NodeAction::Activate);
                out.push(n);
            }
        }
    }

    fn settings_nodes(&self, out: &mut Vec<Node>, w: f32) {
        if self.settings_room == SettingsRoom::Network
            && self.network_flow == NetworkFlow::Credential
        {
            let ssid = self
                .selected_wifi
                .and_then(|index| self.wifi_networks.get(index))
                .map_or("Network", |network| network.ssid.as_str());
            let mask = "•".repeat(self.wifi_credential.expose_secret().len());
            let mut entry = node(
                "wifi-credential-entry",
                Role::Button,
                &format!("Join {ssid} · Passphrase {mask}"),
                48.0,
                180.0,
                w - 96.0,
                72.0,
                "--state-focused-ring",
            );
            entry.state.focused = true;
            entry.action = Some(NodeAction::Activate);
            out.push(entry);
            out.push(node(
                "wifi-credential-privacy",
                Role::Text,
                "Passphrase hidden · Activate to connect · Back to cancel",
                48.0,
                270.0,
                w - 96.0,
                44.0,
                "--color-text-secondary",
            ));
            return;
        }
        if self.settings_room == SettingsRoom::Controls {
            match self.controls_flow {
                ControlsFlow::Capture => {
                    let action = self
                        .control_bindings
                        .get(self.focus)
                        .map_or("control", |binding| binding.label.as_str());
                    let mut capture = node(
                        "remap-capture",
                        Role::Button,
                        &format!("{action} · Press a button… · Back to cancel"),
                        48.0,
                        180.0,
                        w - 96.0,
                        72.0,
                        "--state-focused-ring",
                    );
                    capture.state.focused = true;
                    out.push(capture);
                    return;
                }
                ControlsFlow::RemapPreview => {
                    let selected = self.control_bindings.get(self.focus);
                    let mut preview = node(
                        "remap-preview",
                        Role::Button,
                        &format!(
                            "Previewing {} · Activate to confirm · Back to roll back",
                            selected.map_or("new binding", |binding| binding.label.as_str())
                        ),
                        48.0,
                        180.0,
                        w - 96.0,
                        72.0,
                        "--state-focused-ring",
                    );
                    preview.state.focused = true;
                    preview.action = Some(NodeAction::Activate);
                    out.push(preview);
                    return;
                }
                ControlsFlow::Rows => {}
            }
        }
        let labels: Vec<(String, bool)> = match self.settings_room {
            SettingsRoom::Display => self
                .display_preferences
                .iter()
                .map(|row| {
                    let value = match &row.effective {
                        PreferenceValue::Bool(v) => {
                            if *v {
                                "On".into()
                            } else {
                                "Off".into()
                            }
                        }
                        PreferenceValue::Text(v) => v.clone(),
                        PreferenceValue::Integer(v) => v.to_string(),
                    };
                    (
                        if row.interactive {
                            format!("{} · {value}", row.label)
                        } else {
                            format!("{} · — Not supported on this device", row.label)
                        },
                        row.interactive,
                    )
                })
                .collect(),
            SettingsRoom::Controls => self
                .control_bindings
                .iter()
                .map(|binding| (format!("{} · {}", binding.label, binding.binding), true))
                .chain(std::iter::once(("Reset to defaults".into(), true)))
                .collect(),
            SettingsRoom::Network => {
                if self.wifi_networks.is_empty() {
                    vec![(
                        self.network_status
                            .clone()
                            .or_else(|| self.network_state.as_ref().err().cloned())
                            .unwrap_or_else(|| "No networks found".into()),
                        false,
                    )]
                } else {
                    self.wifi_networks
                        .iter()
                        .map(|network| {
                            let security = match network.security {
                                pf_ports::WifiSecurity::Open => "Open",
                                pf_ports::WifiSecurity::Personal => "Personal",
                                pf_ports::WifiSecurity::Enterprise => "Enterprise",
                                pf_ports::WifiSecurity::Unknown => "Unknown security",
                            };
                            let connected = self
                                .network_state
                                .as_ref()
                                .ok()
                                .and_then(|state| state.connected_ssid.as_deref())
                                .is_some_and(|ssid| ssid == network.ssid);
                            (
                                format!(
                                    "{} · {security} · {}%{}",
                                    network.ssid,
                                    network.strength,
                                    if connected { " · Connected" } else { "" }
                                ),
                                true,
                            )
                        })
                        .collect()
                }
            }
            SettingsRoom::System if self.system_rows().is_empty() => vec![(
                self.time_state
                    .as_ref()
                    .err()
                    .cloned()
                    .or_else(|| self.transfer_services.as_ref().err().cloned())
                    .unwrap_or_else(|| "System controls unavailable".into()),
                false,
            )],
            SettingsRoom::System => self
                .system_rows()
                .into_iter()
                .map(|row| match row {
                    SystemRow::Timezone => (
                        self.time_state.as_ref().map_or_else(Clone::clone, |state| {
                            format!("Timezone · {}", state.timezone)
                        }),
                        true,
                    ),
                    SystemRow::Ntp => (
                        format!(
                            "Automatic time · {}",
                            if self
                                .time_state
                                .as_ref()
                                .is_ok_and(|state| state.ntp_state == NtpState::Active)
                            {
                                "On"
                            } else {
                                "Off"
                            }
                        ),
                        true,
                    ),
                    SystemRow::ManualTime => ("Set time manually · 2027-01-15 08:00".into(), true),
                    SystemRow::Transfer(service) => {
                        let state = self.transfer_services.as_ref().ok().and_then(|states| {
                            states.iter().find(|state| state.service == service)
                        });
                        let name = match service {
                            TransferService::Sftp => "SFTP transfer",
                            TransferService::UsbMassStorage => "USB storage transfer",
                        };
                        (
                            format!(
                                "{name} · {}",
                                if state.is_some_and(|state| state.enabled) {
                                    "On"
                                } else {
                                    "Off"
                                }
                            ),
                            true,
                        )
                    }
                    SystemRow::Accessibility => ("Accessibility & comfort".into(), true),
                })
                .collect(),
        };
        for (i, (label, interactive)) in labels.into_iter().enumerate() {
            let (row_start, row_step, row_height) = if self.settings_room == SettingsRoom::Controls
            {
                (170.0, 52.0, 44.0)
            } else {
                (180.0, 72.0, 58.0)
            };
            let mut n = node(
                &format!("settings-row-{i}"),
                if interactive {
                    Role::Button
                } else {
                    Role::Text
                },
                &label,
                48.0,
                row_start + i as f32 * row_step * f32::from(self.text_scale) / 100.0,
                w - 96.0,
                row_height * f32::from(self.text_scale) / 100.0,
                if !interactive {
                    "--state-unavailable-surface"
                } else if i == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            n.state.focused = i == self.focus;
            n.state.disabled = !interactive;
            if interactive {
                n.action = Some(NodeAction::Activate);
            }
            out.push(n);
        }
        if self.settings_room == SettingsRoom::Display {
            out.push(node(
                "unsupported-note",
                Role::Text,
                "ⓘ Brightness and mono audio are unavailable; no control is shown.",
                48.0,
                590.0,
                w - 96.0,
                44.0,
                "--color-text-secondary",
            ));
            if self.recovery_available {
                let i = self.display_preferences.len().max(1);
                let mut recovery = node(
                    "settings-recovery",
                    Role::Button,
                    "Open independent recovery",
                    48.0,
                    520.0,
                    w - 96.0,
                    54.0,
                    if self.focus == i {
                        "--state-focused-ring"
                    } else {
                        "--state-rest-surface"
                    },
                );
                recovery.state.focused = self.focus == i;
                recovery.action = Some(NodeAction::Activate);
                out.push(recovery);
            }
        }
        if self.settings_room == SettingsRoom::Controls {
            if let Some(status) = &self.controls_status {
                out.push(node(
                    "controls-status",
                    Role::Text,
                    status,
                    48.0,
                    660.0,
                    w - 96.0,
                    36.0,
                    "--color-text-secondary",
                ));
            }
        }
        if self.settings_room == SettingsRoom::Network {
            if let Some(status) = &self.network_status {
                out.push(node(
                    "network-status",
                    Role::Text,
                    status,
                    48.0,
                    540.0,
                    w - 96.0,
                    44.0,
                    "--color-text-secondary",
                ));
            }
        }
        if self.settings_room == SettingsRoom::System {
            if self
                .time_state
                .as_ref()
                .is_ok_and(|state| state.ntp_state == NtpState::Unsupported)
            {
                out.push(node(
                    "ntp-unsupported-note",
                    Role::Text,
                    "Automatic time unavailable on this device",
                    48.0,
                    500.0,
                    w - 96.0,
                    40.0,
                    "--color-text-secondary",
                ));
            }
            if let Some(status) = &self.system_status {
                out.push(node(
                    "system-status",
                    Role::Text,
                    status,
                    48.0,
                    550.0,
                    w - 96.0,
                    44.0,
                    "--color-text-secondary",
                ));
            }
        }
    }

    fn first_run_nodes(&self, out: &mut Vec<Node>, w: f32) {
        out.push(node(
            "first-run-title",
            Role::Heading,
            "FIRST RUN · Make it comfortable",
            w / 2.0 - 340.0,
            54.0,
            680.0,
            56.0,
            "--state-rest-text",
        ));
        out.push(node(
            "first-run-copy",
            Role::Text,
            "All of this lives in Settings → Accessibility and can change any time.",
            w / 2.0 - 340.0,
            112.0,
            680.0,
            48.0,
            "--color-text-secondary",
        ));
        let rows = self.first_run_preferences();
        for (i, row) in rows.iter().enumerate() {
            let mut n = node(
                &format!("comfort-{i}"),
                Role::Button,
                row.label,
                w / 2.0 - 340.0,
                180.0 + i as f32 * 64.0,
                680.0,
                52.0,
                if i == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            n.state.focused = i == self.focus;
            n.action = Some(NodeAction::Activate);
            out.push(n);
        }
        out.push(node(
            "safe-return-teach",
            Role::Text,
            &format!("{} returns you here.", self.safe_return_binding),
            w / 2.0 - 340.0,
            470.0,
            680.0,
            48.0,
            "--color-text-secondary",
        ));
        let mut continue_node = node(
            "continue",
            Role::Button,
            "Continue · START",
            w / 2.0 - 340.0,
            540.0,
            680.0,
            54.0,
            if self.focus == rows.len() {
                "--state-focused-ring"
            } else {
                "--state-rest-surface"
            },
        );
        continue_node.state.focused = self.focus == rows.len();
        continue_node.action = Some(NodeAction::Activate);
        out.push(continue_node);
    }
    fn quick_nodes(&self, out: &mut Vec<Node>, w: f32, h: f32) {
        if let PowerDialog::Confirm(action) = self.power_dialog {
            let verb = match action {
                PowerAction::PowerOff => "power off",
                PowerAction::Restart => "restart",
                PowerAction::Sleep => "sleep",
            };
            out.push(node(
                "power-confirm-title",
                Role::Heading,
                &format!("Ready to {verb}?"),
                w - 460.0,
                180.0,
                412.0,
                52.0,
                "--state-rest-text",
            ));
            for (index, label) in ["Cancel", "Confirm"].iter().enumerate() {
                let mut button = node(
                    &format!("power-confirm-{index}"),
                    Role::Button,
                    label,
                    w - 460.0,
                    258.0 + index as f32 * 64.0,
                    412.0,
                    52.0,
                    if self.focus == index {
                        "--state-focused-ring"
                    } else {
                        "--state-rest-surface"
                    },
                );
                button.state.focused = self.focus == index;
                button.action = Some(NodeAction::Activate);
                out.push(button);
            }
            return;
        }
        // Intentionally no title: §4.2/§4.7 makes the first contextual action the top edge.
        for (i, label) in ["Open focused item", "Browse the library"]
            .iter()
            .enumerate()
        {
            let mut n = node(
                &format!("quick-{i}"),
                Role::Button,
                label,
                w - 400.0,
                96.0 + i as f32 * 64.0,
                352.0,
                52.0,
                if i == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            n.state.focused = i == self.focus;
            n.action = Some(NodeAction::Activate);
            out.push(n);
        }
        out.push(node(
            "quick-power-heading",
            Role::Heading,
            "Power",
            w - 400.0,
            232.0,
            352.0,
            34.0,
            "--state-rest-text",
        ));
        let mut rows = vec![(2, "power-off", "Power off"), (3, "restart", "Restart")];
        if let Some(index) = self.sleep_row() {
            rows.push((index, "sleep", "Sleep"));
        }
        let idle_label = self.idle_policy_loaded.then(|| {
            match self
                .applied_idle_policy
                .sleep_after
                .map(|value| value.as_secs() / 60)
            {
                None => "Auto-sleep · Off".to_owned(),
                Some(minutes) => format!("Auto-sleep · {minutes} min"),
            }
        });
        if let Some(label) = &idle_label {
            rows.push((self.idle_row(), "idle", label));
        }
        for (index, id, label) in rows {
            let enabled = match index {
                2 => self.supports_power(PowerAction::PowerOff),
                3 => self.supports_power(PowerAction::Restart),
                _ => true,
            };
            let mut row = node(
                &format!("quick-power-{id}"),
                Role::Button,
                label,
                w - 400.0,
                274.0 + (index - 2) as f32 * 58.0,
                352.0,
                48.0,
                if index == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            row.state.focused = index == self.focus;
            row.state.disabled = !enabled;
            if enabled {
                row.action = Some(NodeAction::Activate);
            }
            out.push(row);
        }
        let screenshot_index = self.screenshot_row();
        let mut screenshot = node(
            "quick-capture-screenshot",
            Role::Button,
            "Capture screenshot",
            w - 400.0,
            274.0 + (screenshot_index - 2) as f32 * 58.0,
            352.0,
            48.0,
            if screenshot_index == self.focus {
                "--state-focused-ring"
            } else {
                "--state-rest-surface"
            },
        );
        screenshot.state.focused = screenshot_index == self.focus;
        screenshot.action = Some(NodeAction::Activate);
        out.push(screenshot);
        if let Some(status) = &self.power_status {
            out.push(node(
                "quick-power-status",
                Role::Text,
                status,
                w - 400.0,
                h - 142.0,
                352.0,
                32.0,
                "--color-status-attention",
            ));
        }
        out.push(node(
            "quick-truth",
            Role::Text,
            "Nothing is running now. Quick shows only what applies right here.",
            w - 400.0,
            h - 110.0,
            352.0,
            60.0,
            "--color-text-secondary",
        ));
    }
    fn crash_nodes(&self, out: &mut Vec<Node>, w: f32, _h: f32) {
        out.push(node(
            "crash-eyebrow",
            Role::Text,
            "⚠ Closed unexpectedly",
            180.0,
            100.0,
            w - 360.0,
            40.0,
            "--color-status-attention",
        ));
        out.push(node(
            "crash-title",
            Role::Heading,
            &self.active_title,
            180.0,
            150.0,
            w - 360.0,
            54.0,
            "--state-rest-text",
        ));
        out.push(node("crash-copy", Role::Text, &format!("{} stopped on its own and the shelf took the screen back. Nothing else was affected, and it's ready to open again.", self.active_title), 180.0, 220.0, w - 360.0, 70.0, "--color-text-secondary"));
        out.push(node(
            "crash-facts",
            Role::Text,
            &format!("Session · Ended · What happened · {}", self.crash_summary),
            180.0,
            310.0,
            w - 360.0,
            50.0,
            "--color-status-attention",
        ));
        out.push(node(
            "crash-diagnostic",
            Role::Text,
            &format!(
                "{} · kept on this device · {}",
                self.crash_receipt_id, self.crash_exit_detail
            ),
            180.0,
            370.0,
            w - 360.0,
            40.0,
            "--color-text-secondary",
        ));
        out.push(node("crash-honesty", Role::Text, "This record stays on the device — there's nowhere it gets sent, so there's no Report button to press.", 180.0, 420.0, w - 360.0, 60.0, "--color-text-secondary"));
        for (i, label) in ["Back to Home", "Open again"].iter().enumerate() {
            let mut n = node(
                &format!("crash-action-{i}"),
                Role::Button,
                label,
                180.0,
                480.0 + i as f32 * 62.0,
                360.0,
                50.0,
                if i == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            n.state.focused = i == self.focus;
            n.action = Some(NodeAction::Activate);
            out.push(n);
        }
    }
}

fn procedural_art_nodes(
    id: &str,
    title: &str,
    edition: Option<&str>,
    context: &str,
    x: f32,
    y: f32,
    width: f32,
    focused: bool,
) -> Vec<Node> {
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
    });
    let motif = match (hash / 6) % 6 {
        0 => "╱  ╱  ╱\n  ╱  ╱",
        1 => "≈ ≈ ≈\n ≈ ≈ ≈",
        2 => "· · · ·\n · · ·",
        3 => "○   ◌\n  ◉",
        4 => "⌁ ⌁ ⌁\n ⌁ ⌁",
        _ => "\\ | /\n— ◉ —",
    };
    let token = [
        "--deco-plate-a-bg",
        "--deco-plate-b-bg",
        "--deco-plate-c-bg",
        "--deco-plate-d-bg",
        "--deco-plate-e-bg",
        "--deco-plate-f-bg",
    ][(hash % 6) as usize];
    let monogram = title.chars().next().unwrap_or('·').to_string();
    let home = context == "home-card";
    let kind_y = if home { y + 166.0 } else { y + 142.0 };
    let label_y = if home { y + 210.0 } else { y + 176.0 };
    let label_mask = node(
        // Card labels remain available to assistive consumers, but the current renderer
        // also paints them. Mask the full art region before painting the inset art so a
        // wrapped second line cannot remain visible in the art's eight-pixel gutters.
        &format!("{context}-label-mask-{id}"),
        Role::Group,
        "",
        x,
        y - 8.0,
        width,
        if home { 166.0 } else { 144.0 },
        token,
    );
    let mut nodes = vec![
        label_mask,
        node(
            &format!("{context}-art-{id}"),
            Role::Group,
            "",
            x + 8.0,
            y,
            width - 16.0,
            if home { 158.0 } else { 136.0 },
            token,
        ),
        node(
            &format!("{context}-motif-{id}"),
            Role::Text,
            motif,
            x + 8.0,
            y,
            width - 16.0,
            60.0,
            token,
        ),
        node(
            &format!("{context}-initial-{id}"),
            Role::Text,
            &monogram,
            x + width * 0.27,
            y + 72.0,
            width * 0.46,
            58.0,
            token,
        ),
    ];
    if let Some(edition) = edition {
        nodes.push(node(
            &format!("{context}-plate-{id}"),
            Role::Text,
            edition,
            x + 12.0,
            kind_y,
            width - 24.0,
            24.0,
            token,
        ));
    }
    nodes.push(node(
        &format!("{context}-title-{id}"),
        Role::Text,
        title,
        x,
        label_y,
        width,
        28.0,
        if focused {
            "--state-focused-text"
        } else {
            "--color-text-secondary"
        },
    ));
    nodes
}

fn art_nodes(item: &Item, context: &str, x: f32, y: f32, width: f32, focused: bool) -> Vec<Node> {
    if let Some(art) = item.art.as_ref().filter(|_| !item.art_failed) {
        let home = context == "home-card";
        let art_height = if home { 158.0 } else { 136.0 };
        let kind_y = if home { y + 166.0 } else { y + 142.0 };
        let label_y = if home { y + 210.0 } else { y + 176.0 };
        let mut image = node(
            &format!("{context}-art-{}", item.id),
            Role::Group,
            &format!("{} cover art", item.title),
            x + 8.0,
            y,
            width - 16.0,
            art_height,
            "--state-rest-surface",
        );
        image = image.with_image(art.clone(), ImageFit::Cover);
        return vec![
            image,
            node(
                &format!("{context}-plate-{}", item.id),
                Role::Text,
                kind_text(&item.kind),
                x + 12.0,
                kind_y,
                width - 24.0,
                24.0,
                "--state-rest-surface",
            ),
            node(
                &format!("{context}-title-{}", item.id),
                Role::Text,
                &item.title,
                x,
                label_y,
                width,
                28.0,
                if focused {
                    "--state-focused-text"
                } else {
                    "--color-text-secondary"
                },
            ),
        ];
    }
    procedural_art_nodes(
        &item.id,
        &item.title,
        Some(kind_text(&item.kind)),
        context,
        x,
        y,
        width,
        focused,
    )
}

fn availability_text(a: &Availability, p: &Presentation) -> String {
    match a {
        Availability::Ready if matches!(p, Presentation::Starting) => "Starting".into(),
        Availability::Ready => "Ready".into(),
        Availability::NeedsNetwork { reason } => format!("Network required — {reason}"),
        Availability::NeedsSetup { reason } => format!("Finish setup — {reason}"),
        Availability::UnsupportedCapability { capability } => {
            format!("Not supported on this device — {capability}")
        }
        Availability::IncompatibleRuntime {
            required,
            available,
        } => format!("Not supported — requires {required}; found {available}"),
    }
}
fn best_availability(item: &Item) -> &Availability {
    item.variants
        .iter()
        .find(|variant| matches!(variant.availability, Availability::Ready))
        .or_else(|| item.variants.first())
        .map_or_else(
            || panic!("catalog item {} has no variants", item.id),
            |variant| &variant.availability,
        )
}
fn kind_text(kind: &AppKind) -> &'static str {
    match kind {
        AppKind::Media => "MEDIA",
        AppKind::Stream => "STREAM",
        AppKind::Game => "GAME",
        AppKind::System => "TOOL",
        AppKind::Settings => "SETTINGS",
    }
}
fn state_token(a: &Availability, focused: bool) -> &'static str {
    if focused {
        "--state-focused-ring"
    } else {
        match a {
            Availability::Ready => "--state-rest-surface",
            Availability::NeedsNetwork { .. } | Availability::NeedsSetup { .. } => {
                "--state-attention-surface"
            }
            Availability::UnsupportedCapability { .. }
            | Availability::IncompatibleRuntime { .. } => "--state-unavailable-surface",
        }
    }
}
fn node(id: &str, role: Role, label: &str, x: f32, y: f32, w: f32, h: f32, token: &str) -> Node {
    Node::new(
        NodeId::new(id).unwrap(),
        role,
        label,
        Bounds::new(x, y, w, h),
        token,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_session_authority::{EndStamp, Receipt};

    fn history_entry(
        item_id: &str,
        started_at: Option<SystemTime>,
        ended_at: Option<(SystemTime, EndPrecision)>,
    ) -> HistoryEntry {
        HistoryEntry {
            session_id: format!("session-{item_id}"),
            item_id: item_id.into(),
            receipt: Some(Receipt::Returned),
            started_at,
            ended_at: ended_at.map(|(at, precision)| EndStamp { at, precision }),
        }
    }

    #[test]
    fn playtime_requires_both_stamps_and_unknown_is_absent() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let totals = derive_playtime(&[
            history_entry("no-start", None, Some((now, EndPrecision::Observed))),
            history_entry("no-end", Some(now), None),
        ]);

        assert!(totals.is_empty());
    }

    #[test]
    fn playtime_sums_clamps_and_propagates_approximate_precision() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let totals = derive_playtime(&[
            history_entry(
                "ridgeline-rally",
                Some(now),
                Some((now + Duration::from_secs(3_600), EndPrecision::Observed)),
            ),
            history_entry(
                "ridgeline-rally",
                Some(now),
                Some((now + Duration::from_secs(600), EndPrecision::Approximate)),
            ),
            history_entry(
                "clock-went-back",
                Some(now),
                Some((now - Duration::from_secs(5), EndPrecision::Observed)),
            ),
        ]);

        assert_eq!(
            totals["ridgeline-rally"],
            Playtime {
                duration: Duration::from_secs(4_200),
                approximate: true,
            }
        );
        assert_eq!(totals["clock-went-back"].duration, Duration::ZERO);
        assert_eq!(format_playtime(totals["ridgeline-rally"]), "Played ~1h 10m");
        assert_eq!(format_playtime(totals["clock-went-back"]), "Played <1m");
    }

    #[test]
    fn playtime_format_omits_seconds_and_handles_sub_minute() {
        assert_eq!(
            format_playtime(Playtime {
                duration: Duration::from_secs(59),
                approximate: false,
            }),
            "Played <1m"
        );
        assert_eq!(
            format_playtime(Playtime {
                duration: Duration::from_secs(3 * 3_600 + 20 * 60 + 59),
                approximate: false,
            }),
            "Played 3h 20m"
        );
    }
    use pf_catalog::{
        AppKind, AppManifestRef, Presentation as CP, Provenance, UserProjection, Variant,
    };
    use pf_ports::{FakePowerPort, FakePreferencePort, PreferenceChangeResult};
    use std::path::PathBuf;
    fn snapshot() -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            user_projection: UserProjection::default(),
            items: ["Ridgeline", "Hollow Tides"]
                .into_iter()
                .enumerate()
                .map(|(i, t)| pf_catalog::CatalogItem {
                    id: format!("i{i}"),
                    title: t.into(),
                    kind: AppKind::Game,
                    presentation: CP {
                        icon_reference: None,
                        icon_decodable: false,
                    },
                    tags: vec![],
                    variants: vec![Variant {
                        id: "default".into(),
                        provider_id: "fixture".into(),
                        availability: Availability::Ready,
                        requirements: vec![],
                        provenance: Provenance {
                            provider_id: "fixture".into(),
                            app_version: None,
                            upstream_version: None,
                            runtime_family: "native".into(),
                            runtime_abi: "aarch64".into(),
                            platform_version: None,
                        },
                        launch_target: AppManifestRef {
                            app_id: format!("app-{i}"),
                            descriptor_path: PathBuf::from("app.toml"),
                            observed_digest: "x".into(),
                        },
                    }],
                })
                .collect(),
        }
    }
    fn core() -> ShellCore {
        let mut c = ShellCore::boot(&snapshot(), &pf_theme::flagship(), false);
        c.authority_snapshot(false);
        c
    }
    fn preferences(applied: bool) -> FakePreferencePort {
        FakePreferencePort::new(
            [
                EffectivePreference {
                    key: PreferenceKey("textScale".into()),
                    effective: PreferenceValue::Text("100%".into()),
                    stored: PreferenceValue::Text("100%".into()),
                    applied,
                },
                EffectivePreference {
                    key: PreferenceKey("highContrast".into()),
                    effective: PreferenceValue::Bool(false),
                    stored: PreferenceValue::Bool(false),
                    applied,
                },
                EffectivePreference {
                    key: PreferenceKey("reduceMotion".into()),
                    effective: PreferenceValue::Bool(false),
                    stored: PreferenceValue::Bool(false),
                    applied,
                },
                EffectivePreference {
                    key: PreferenceKey("reduceFlashing".into()),
                    effective: PreferenceValue::Bool(false),
                    stored: PreferenceValue::Bool(false),
                    applied,
                },
            ],
            ChangeAuthority("user".into()),
        )
    }
    #[test]
    fn supported_rows_change_only_after_applied_observation() {
        let mut c = core();
        let mut port = preferences(true);
        c.load_preferences(&port, true).unwrap();
        c.go(Route::Settings);
        let Some(Effect::ChangePreference(change)) = c.action(&ShellAction::Activate) else {
            panic!("supported row must be interactive")
        };
        assert_eq!(
            c.text_scale(),
            100,
            "submission is never rendered optimistically"
        );
        assert_eq!(
            port.submit_change(change).unwrap(),
            PreferenceChangeResult::Accepted
        );
        c.drive_preferences(&mut port).unwrap();
        assert_eq!(c.text_scale(), 150);

        let mut unsupported = core();
        unsupported
            .load_preferences(&preferences(false), true)
            .unwrap();
        unsupported.go(Route::Settings);
        assert_eq!(unsupported.action(&ShellAction::Activate), None);
        let scene = unsupported
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.,
                    logical_height: 720.,
                    scale: 1.,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        assert!(format!("{scene:?}").contains("Not supported on this device"));
    }
    #[test]
    fn first_run_uses_one_filtered_index_space() {
        let values = preferences(true);
        let mut observed = Vec::new();
        for key in [
            "textScale",
            "highContrast",
            "reduceMotion",
            "reduceFlashing",
        ] {
            observed.push(values.read(&PreferenceKey(key.into())).unwrap().unwrap());
        }
        observed[1].applied = false;
        observed[3].applied = false;
        let mixed = FakePreferencePort::new(observed, ChangeAuthority("user".into()));
        let mut c = core();
        c.load_preferences(&mixed, false).unwrap();

        let scene = c
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.,
                    logical_height: 720.,
                    scale: 1.,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        let debug = format!("{scene:?}");
        assert!(debug.contains("rooms"));
        assert!(debug.contains("status-cluster"));

        let Some(Effect::ChangePreference(first)) = c.action(&ShellAction::Activate) else {
            panic!("first visible row must activate");
        };
        assert_eq!(first.key, PreferenceKey("textScale".into()));
        c.action(&ShellAction::Move(AxisMove::Down));
        let Some(Effect::ChangePreference(second)) = c.action(&ShellAction::Activate) else {
            panic!("second visible row must activate");
        };
        assert_eq!(second.key, PreferenceKey("reduceMotion".into()));
        c.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            c.action(&ShellAction::Activate),
            Some(Effect::CompleteFirstRun)
        );
    }

    #[test]
    fn controls_rows_derive_bindings_and_drive_capture_preview_rollback_and_reset() {
        let mut c = core();
        c.set_control_bindings(vec![ControlBinding {
            context: "global".into(),
            action: "Activate".into(),
            label: "Activate".into(),
            binding: "A".into(),
        }]);
        c.go(Route::Settings);
        c.settings_room = SettingsRoom::Controls;
        let debug = format!("{:?}", settings_scene(&c));
        assert!(debug.contains("Activate · A"));
        assert!(debug.contains("Reset to defaults"));
        assert!(debug.contains("settings-row-1"));

        assert_eq!(c.action(&ShellAction::Activate), Some(Effect::CaptureRemap));
        assert!(format!("{:?}", settings_scene(&c)).contains("Press a button"));
        assert_eq!(
            c.action(&ShellAction::Custom("Capture.north".into())),
            Some(Effect::BeginRemap {
                context: "global".into(),
                action: "Activate".into(),
                control: "north".into(),
            })
        );
        assert_eq!(c.action(&ShellAction::Back), Some(Effect::RollbackRemap));

        c.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(c.action(&ShellAction::Activate), Some(Effect::ResetRemaps));

        c.remap_refused("Back");
        assert!(format!("{:?}", settings_scene(&c)).contains("already bound to Back"));
    }

    fn device_ports(
        ntp: NtpState,
    ) -> (
        pf_ports::FakeNetworkPort,
        pf_ports::FakeTimePort,
        pf_ports::FakeTransferPort,
    ) {
        let mut network = pf_ports::FakeNetworkPort::new(NetworkState {
            interface_present: true,
            enabled: true,
            connected_ssid: None,
            signal: None,
        });
        network.script_scan(Ok(vec![WifiNetwork {
            ssid: "Cedar Workshop".into(),
            security: pf_ports::WifiSecurity::Personal,
            strength: 64,
        }]));
        let time = pf_ports::FakeTimePort::new(
            TimeCapabilities {
                manual_set_time: Support::Supported,
            },
            pf_ports::TimeState {
                wall_clock: SystemTime::UNIX_EPOCH,
                timezone: "UTC".into(),
                ntp_state: ntp,
            },
        );
        let transfer = pf_ports::FakeTransferPort::new(vec![TransferServiceState {
            service: TransferService::Sftp,
            support: Support::Supported,
            enabled: false,
            endpoint_info: None,
        }]);
        (network, time, transfer)
    }

    fn settings_scene(core: &ShellCore) -> Scene {
        core.scene(
            SurfaceMetrics {
                logical_width: 1280.,
                logical_height: 720.,
                scale: 1.,
                safe_insets: Default::default(),
                orientation: pf_scene::Orientation::Landscape,
            },
            "A Open · B Back",
        )
        .unwrap()
    }

    #[test]
    fn network_room_masks_credentials_and_reports_contract_degradation_states() {
        let (mut network, _, _) = device_ports(NtpState::Inactive);
        let mut core = core();
        core.load_network(&mut network);
        core.go(Route::Settings);
        core.settings_room = SettingsRoom::Network;
        core.action(&ShellAction::Activate);
        core.set_wifi_passphrase(b"pine-secret".to_vec());
        let debug = format!("{:?}", settings_scene(&core));
        assert!(debug.contains("•••••••••••"));
        assert!(!debug.contains("pine-secret"));
        let effect = core.action(&ShellAction::Activate).unwrap();
        let Effect::ConnectWifi { credential, .. } = effect else {
            panic!("credential flow must connect")
        };
        assert_eq!(credential.expose_secret(), b"pine-secret");
        assert!(!format!("{credential:?}").contains("pine-secret"));

        for (result, label) in [
            (
                Ok(ConnectResult::Progress(
                    pf_ports::ConnectProgress::Authenticating,
                )),
                "Joining · Authenticating",
            ),
            (Ok(ConnectResult::Refused), "authentication refused"),
            (
                Ok(ConnectResult::Connected {
                    ssid: "Cedar Workshop".into(),
                }),
                "Connected · Cedar Workshop",
            ),
        ] {
            core.network_result(result);
            assert!(format!("{:?}", settings_scene(&core)).contains(label));
        }
    }

    #[test]
    fn system_room_gates_ntp_and_manual_time_and_renders_ruled_anatomy() {
        let (_, unsupported_time, transfer) = device_ports(NtpState::Unsupported);
        let mut core = core();
        core.load_system(&unsupported_time, &transfer);
        core.go(Route::Settings);
        core.settings_room = SettingsRoom::System;
        let debug = format!("{:?}", settings_scene(&core));
        assert!(
            !debug.contains("settings-row-1\", role: Button, accessible_label: \"Automatic time")
        );
        assert!(debug.contains("ntp-unsupported-note"));
        assert!(debug.contains("Set time manually"));

        let (_, active_time, transfer) = device_ports(NtpState::Active);
        core.load_system(&active_time, &transfer);
        let active = format!("{:?}", settings_scene(&core));
        assert!(active.contains("Automatic time · On"));
        assert!(!active.contains("Set time manually"));
        assert!(active.contains("Accessibility & comfort"));
        assert_eq!(core.focus_count(), 4);
        for id in [
            "settings-row-0",
            "settings-row-1",
            "settings-row-2",
            "settings-row-3",
        ] {
            assert!(active.contains(id));
        }

        for _ in 0..3 {
            core.action(&ShellAction::Move(AxisMove::Down));
        }
        assert_eq!(core.focus(), 3);
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::ResetFirstRun)
        );
    }

    #[test]
    fn system_requested_vs_applied_divergence_is_explicit() {
        let (_, time, transfer) = device_ports(NtpState::Inactive);
        let mut core = core();
        core.load_system(&time, &transfer);
        core.go(Route::Settings);
        core.settings_room = SettingsRoom::System;
        core.timezone_result(Ok(AppliedValue {
            requested: "Europe/London".into(),
            applied: "UTC".into(),
        }));
        assert!(
            format!("{:?}", settings_scene(&core))
                .contains("Requested Europe/London · applied UTC")
        );
        core.ntp_result(Ok(AppliedValue {
            requested: true,
            applied: false,
        }));
        assert!(
            format!("{:?}", settings_scene(&core))
                .contains("Automatic time requested On · applied Off")
        );
        core.transfer_result(Ok(AppliedTransferState {
            requested: true,
            applied: TransferServiceState {
                service: TransferService::Sftp,
                support: Support::Supported,
                enabled: false,
                endpoint_info: None,
            },
            warning: None,
        }));
        assert!(
            format!("{:?}", settings_scene(&core))
                .contains("File transfer requested On · applied Off")
        );
    }
    #[test]
    fn applied_accessibility_fixture_is_live_and_first_run_is_once_only() {
        let mut c = core();
        c.load_preferences(&preferences(true), false).unwrap();
        assert_eq!(c.presentation(), &Presentation::FirstRun);
        for change in [
            ("textScale", PreferenceValue::Text("200%".into())),
            ("highContrast", PreferenceValue::Bool(true)),
            ("reduceMotion", PreferenceValue::Bool(true)),
            ("reduceFlashing", PreferenceValue::Bool(true)),
        ] {
            c.preference_changed(&EffectivePreference {
                key: PreferenceKey(change.0.into()),
                effective: change.1.clone(),
                stored: change.1,
                applied: true,
            });
        }
        assert_eq!(
            (
                c.text_scale(),
                c.high_contrast(),
                c.reduced_motion(),
                c.reduce_flashing()
            ),
            (200, true, true, true)
        );
        c.action(&ShellAction::Custom("Start".into()));
        assert_eq!(c.presentation(), &Presentation::Ready);
        c.load_preferences(&preferences(true), true).unwrap();
        assert_eq!(c.presentation(), &Presentation::Ready);
        c.reset_first_run();
        assert_eq!(c.presentation(), &Presentation::FirstRun);
        assert_eq!(
            c.action(&ShellAction::Back),
            None,
            "Back cannot abandon first run"
        );
    }
    #[test]
    fn back_restores_route_focus_and_one_owner() {
        let mut c = core();
        c.action(&ShellAction::Move(AxisMove::Down));
        c.action(&ShellAction::Custom("Quick".into()));
        c.action(&ShellAction::Back);
        assert_eq!((c.route(), c.focus()), (Route::Home, 1));
        let s = c
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.,
                    logical_height: 720.,
                    scale: 1.,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        fn count(n: &Node) -> usize {
            usize::from(n.state.focused) + n.children.iter().map(count).sum::<usize>()
        }
        assert_eq!(count(s.root()), 1);
    }
    #[test]
    fn all_presentations_route_safe_return() {
        let mut c = core();
        for p in [
            Presentation::Ready,
            Presentation::Starting,
            Presentation::Running,
            Presentation::Returned,
            Presentation::ForcedClose,
            Presentation::Crash,
            Presentation::RecoveryRequired,
        ] {
            c.presentation = p;
            assert_eq!(
                c.action(&ShellAction::Custom("SafeReturn".into())),
                Some(Effect::SafeReturn)
            );
        }
    }
    #[test]
    fn receipts_wait_for_ack_and_recovery_has_no_frame() {
        let mut c = core();
        c.session_event(&SessionEvent::Terminal(TerminalReceipt::Crash {
            session_id: "s".into(),
            summary: "exit status 9".into(),
        }));
        assert!(c.needs_presentation_ack());
        assert!(
            c.scene(
                SurfaceMetrics {
                    logical_width: 1280.,
                    logical_height: 720.,
                    scale: 1.,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape
                },
                ""
            )
            .is_some()
        );
        assert!(c.acknowledge_presentation());
        c.session_event(&SessionEvent::RecoveryRequired(
            pf_ports::RecoveryRequired {
                session_id: "s".into(),
                reason: "owner unavailable".into(),
            },
        ));
        assert!(!c.has_shell_frame());
    }
    #[test]
    fn crash_actions_relaunch_or_dismiss_by_focus() {
        let mut c = core();
        c.focus = 1;
        assert_eq!(
            c.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "app-1".into()
            }))
        );
        c.session_event(&SessionEvent::Terminal(TerminalReceipt::Crash {
            session_id: "receipt-7".into(),
            summary: "exit status 9".into(),
        }));

        c.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            c.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "app-1".into()
            }))
        );
        assert_eq!(c.presentation(), &Presentation::Starting);

        c.session_event(&SessionEvent::Terminal(TerminalReceipt::Crash {
            session_id: "receipt-8".into(),
            summary: "signal 11".into(),
        }));
        assert_eq!(c.focus(), 0);
        assert_eq!(c.action(&ShellAction::Activate), None);
        assert_eq!(
            (c.route(), c.presentation()),
            (Route::Home, &Presentation::Ready)
        );
    }
    #[test]
    fn crash_scene_includes_local_receipt_diagnostic() {
        let mut c = core();
        c.session_event(&SessionEvent::Terminal(TerminalReceipt::Crash {
            session_id: "receipt-7".into(),
            summary: "exit status 9".into(),
        }));
        let scene = c
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.,
                    logical_height: 720.,
                    scale: 1.,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        let diagnostic = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "crash-diagnostic")
            .expect("crash diagnostic row");
        assert_eq!(
            diagnostic.accessible_label,
            "receipt-7 · kept on this device · exit status 9"
        );
        assert_eq!(diagnostic.style_token, "--color-text-secondary");
    }
    #[test]
    fn recovery_entry_is_authority_gated() {
        let mut c = core();
        c.go(Route::Settings);
        assert_eq!(c.focus_count(), 1);
        c.authority_snapshot(true);
        assert_eq!(c.focus_count(), 2);
        c.focus = 1;
        assert_eq!(
            c.action(&ShellAction::Activate),
            Some(Effect::EnterRecovery)
        );
    }

    fn variant(id: &str, app_id: &str, availability: Availability) -> Variant {
        Variant {
            id: id.into(),
            provider_id: format!("provider-{id}"),
            availability,
            requirements: vec![],
            provenance: Provenance {
                provider_id: format!("provider-{id}"),
                app_version: Some("1.0".into()),
                upstream_version: None,
                runtime_family: "native".into(),
                runtime_abi: "aarch64".into(),
                platform_version: None,
            },
            launch_target: AppManifestRef {
                app_id: app_id.into(),
                descriptor_path: PathBuf::from(format!("fixtures/{id}.toml")),
                observed_digest: "fixture".into(),
            },
        }
    }

    fn item(id: &str, title: &str, variants: Vec<Variant>) -> pf_catalog::CatalogItem {
        pf_catalog::CatalogItem {
            id: id.into(),
            title: title.into(),
            kind: AppKind::Game,
            presentation: CP {
                icon_reference: None,
                icon_decodable: false,
            },
            tags: vec!["fictional".into(), format!("group-{id}")],
            variants,
        }
    }

    fn fixture_core(items: Vec<pf_catalog::CatalogItem>) -> ShellCore {
        let snapshot = CatalogSnapshot {
            revision: 10,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            items,
            user_projection: UserProjection::default(),
        };
        let mut core = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        core
    }

    #[test]
    fn five_hundred_items_filter_deterministically_and_search_opens_details_only() {
        let items = (0..500)
            .map(|index| {
                item(
                    &format!("title-{index:03}"),
                    &format!("Fictional Title {index:03}"),
                    vec![variant(
                        "native",
                        &format!("app-{index:03}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.action(&ShellAction::Custom("Search".into()));
        core.set_search_query("Title 042 fictional");
        assert_eq!(core.search_result_ids(), vec!["title-042"]);
        assert_eq!(core.action(&ShellAction::Activate), None);
        assert_eq!(core.route(), Route::Details);
        assert_eq!(core.presentation(), &Presentation::Ready);
    }

    #[test]
    fn one_many_and_no_usable_variant_flows_are_per_launch() {
        let unavailable = Availability::NeedsNetwork {
            reason: "connect to Wi-Fi".into(),
        };
        let mut one = fixture_core(vec![item(
            "one",
            "One Lantern",
            vec![
                variant("offline", "one-offline", Availability::Ready),
                variant("cloud", "one-cloud", unavailable.clone()),
            ],
        )]);
        assert_eq!(
            one.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "one-offline".into()
            }))
        );

        let mut many = fixture_core(vec![item(
            "many",
            "Many Moons",
            vec![
                variant("native", "many-native", Availability::Ready),
                variant("stream", "many-stream", Availability::Ready),
                variant("blocked", "many-blocked", unavailable.clone()),
            ],
        )]);
        assert_eq!(many.action(&ShellAction::Activate), None);
        assert_eq!(many.route(), Route::VariantChooser);
        many.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            many.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "many-stream".into()
            }))
        );
        many.launch_result(&LaunchResult::RejectedBusy);
        many.go(Route::Home);
        assert_eq!(many.action(&ShellAction::Activate), None);
        assert_eq!((many.route(), many.focus()), (Route::VariantChooser, 0));

        let mut none = fixture_core(vec![item(
            "none",
            "Quiet Orbit",
            vec![variant("cloud", "none-cloud", unavailable)],
        )]);
        assert_eq!(none.action(&ShellAction::Activate), None);
        assert_eq!(none.presentation(), &Presentation::Ready);
    }

    #[test]
    fn ready_pin_launches_directly_and_unavailable_pin_falls_back_honestly() {
        let variants = vec![
            variant("native", "many-native", Availability::Ready),
            variant("stream", "many-stream", Availability::Ready),
        ];
        let mut snapshot = CatalogSnapshot {
            revision: 10,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            items: vec![item("many", "Many Moons", variants.clone())],
            user_projection: UserProjection::default(),
        };
        snapshot
            .user_projection
            .pinned_variant_ids
            .insert("many".into(), "stream".into());
        let mut pinned = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        pinned.authority_snapshot(false);
        pinned.selected_item = Some(0);
        pinned.go(Route::Details);
        let details = pinned
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        assert!(details.root().children.iter().any(|node| {
            node.id.as_str() == "detail-pinned-variant"
                && node.accessible_label == "Default version · stream"
        }));
        pinned.go(Route::Home);
        assert_eq!(
            pinned.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "many-stream".into()
            }))
        );

        snapshot.items[0].variants[1].availability = Availability::NeedsNetwork {
            reason: "offline".into(),
        };
        let mut fallback = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        fallback.authority_snapshot(false);
        assert_eq!(fallback.action(&ShellAction::Activate), None);
        assert_eq!(fallback.route(), Route::VariantChooser);
        let scene = fallback
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        let note = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "chooser-note")
            .unwrap();
        assert_eq!(
            note.accessible_label,
            "Default unavailable — choose a version"
        );
    }

    #[test]
    fn chooser_default_affordance_pins_and_unpins_the_focused_ready_variant() {
        let mut core = fixture_core(vec![item(
            "many",
            "Many Moons",
            vec![
                variant("native", "many-native", Availability::Ready),
                variant("stream", "many-stream", Availability::Ready),
            ],
        )]);
        core.action(&ShellAction::Activate);
        assert_eq!(
            core.action(&ShellAction::Custom("Favorite".into())),
            Some(Effect::SetPinnedVariant {
                item_id: "many".into(),
                variant_id: Some("native".into()),
            })
        );
        core.pinned_variant_committed("many", Some("native".into()));
        assert_eq!(
            core.action(&ShellAction::Custom("Favorite".into())),
            Some(Effect::SetPinnedVariant {
                item_id: "many".into(),
                variant_id: None,
            })
        );
    }

    #[test]
    fn activating_empty_catalog_is_a_no_op() {
        let mut core = fixture_core(vec![]);
        assert_eq!(core.focus_count(), 1);
        assert_eq!(core.action(&ShellAction::Activate), None);
        assert_eq!(core.route(), Route::Home);
    }

    #[test]
    fn real_art_uses_an_image_node_and_missing_bytes_use_a_plate() {
        let mut catalog_item = item(
            "plate-id",
            "Paper Comet",
            vec![variant("native", "paper-comet", Availability::Ready)],
        );
        catalog_item.presentation.icon_reference = Some("art/paper-comet.png".into());
        catalog_item.presentation.icon_decodable = true;
        let snapshot = CatalogSnapshot {
            revision: 10,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            items: vec![catalog_item],
            user_projection: UserProjection::default(),
        };
        let mut core =
            ShellCore::boot_with_art(&snapshot, &pf_theme::flagship(), false, |reference| {
                (reference == "art/paper-comet.png").then(|| Arc::from(&b"png"[..]))
            });
        core.authority_snapshot(false);
        assert_eq!(
            core.art_treatment("plate-id"),
            Some(ArtTreatment::CatalogArt)
        );
        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "A Open · B Back",
            )
            .unwrap();
        let card = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "item-plate-id")
            .unwrap();
        let art = card
            .children
            .iter()
            .find(|node| node.id.as_str() == "home-card-art-plate-id")
            .unwrap();
        assert_eq!(art.accessible_label, "Paper Comet cover art");
        assert!(matches!(
            art.content,
            pf_scene::NodeContent::Image {
                fit: ImageFit::Cover,
                ..
            }
        ));
        assert!(!card.children.iter().any(|node| {
            matches!(
                node.id.as_str(),
                "home-card-label-mask-plate-id"
                    | "home-card-motif-plate-id"
                    | "home-card-initial-plate-id"
            )
        }));
        assert!(card.accessible_label.is_empty());
        assert!(card.children.iter().any(|node| {
            node.id.as_str() == "home-card-plate-plate-id" && node.accessible_label == "GAME"
        }));
        assert!(card.children.iter().all(|node| {
            !node.accessible_label.contains("art/paper-comet.png")
                && !node.accessible_label.contains("Catalog art")
        }));

        let mut corrupt_item = item(
            "plate-id",
            "Paper Comet",
            vec![variant("native", "paper-comet", Availability::Ready)],
        );
        corrupt_item.presentation.icon_reference = Some("art/paper-comet.png".into());
        let corrupt_core = fixture_core(vec![corrupt_item]);
        let corrupt = corrupt_core.art_treatment("plate-id").unwrap();
        assert!(matches!(
            corrupt,
            ArtTreatment::EditionPlate {
                palette: 0..=5,
                motif: 0..=5
            }
        ));
        assert_eq!(corrupt, corrupt_core.art_treatment("plate-id").unwrap());
    }

    #[test]
    fn deep_library_focus_is_windowed_inside_surface() {
        let items = (0..500)
            .map(|index| {
                item(
                    &format!("title-{index:03}"),
                    &format!("Fictional Title {index:03}"),
                    vec![variant(
                        "native",
                        &format!("app-{index:03}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);
        // Search plus three filter chips precede the deterministic item index space.
        for _ in 0..484 {
            core.action(&ShellAction::Move(AxisMove::Down));
        }
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let scene = core.scene(metrics, "A Open · B Back").unwrap();
        let focused = scene
            .root()
            .children
            .iter()
            .find(|node| node.state.focused && node.id.as_str().starts_with("library-item-"))
            .unwrap();
        assert!(focused.bounds.y >= 0.0);
        assert!(focused.bounds.y + focused.bounds.height <= metrics.logical_height);
        assert_eq!(focused.id.as_str(), "library-item-title-480");
    }

    #[test]
    fn home_scene_preserves_full_anatomy() {
        fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            if node.id.as_str() == id {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, id))
        }

        let core = fixture_core(vec![
            item(
                "ridge",
                "Ridgeline",
                vec![variant("native", "ridge", Availability::Ready)],
            ),
            item(
                "tides",
                "Hollow Tides",
                vec![variant("native", "tides", Availability::Ready)],
            ),
        ]);
        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "A Open · B Back",
            )
            .unwrap();
        for (id, role) in [
            ("hero-title", Role::Heading),
            ("hero-status", Role::Text),
            ("ready-now-label", Role::Heading),
            ("status-cluster", Role::Text),
        ] {
            assert_eq!(find(scene.root(), id).map(|node| node.role), Some(role));
        }
        for id in ["ridge", "tides"] {
            for (part, role) in [
                ("art", Role::Group),
                ("initial", Role::Text),
                ("plate", Role::Text),
                ("title", Role::Text),
            ] {
                let node_id = format!("home-card-{part}-{id}");
                assert_eq!(
                    find(scene.root(), &node_id).map(|node| node.role),
                    Some(role),
                    "missing Home card anatomy node {node_id}"
                );
            }
        }
    }

    #[test]
    fn max_text_scale_routes_keep_full_truthful_labels() {
        let mut core = fixture_core(vec![item(
            "long",
            "The Unabridged Cartographer of Hollow Tides",
            vec![variant(
                "network-edition",
                "long-network",
                Availability::NeedsNetwork {
                    reason: "connect to Wi-Fi to use this edition".into(),
                },
            )],
        )]);
        core.go(Route::Library);
        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 640.0,
                    logical_height: 720.0,
                    scale: 2.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Portrait,
                },
                "A Open · B Back",
            )
            .unwrap();
        let card = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "library-item-long")
            .unwrap();
        assert!(
            card.accessible_label
                .contains("connect to Wi-Fi to use this edition")
        );
        assert!(!card.accessible_label.contains("EDITION PLATE"));
        assert!(
            card.children
                .iter()
                .any(|node| node.id.as_str() == "library-card-plate-long")
        );
    }

    #[test]
    fn favorites_shelf_filters_and_details_badges_have_ruled_anatomy() {
        let mut snapshot = CatalogSnapshot {
            revision: 1,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            items: vec![
                item(
                    "ridge",
                    "Ridgeline",
                    vec![variant("standard", "ridge", Availability::Ready)],
                ),
                item(
                    "tides",
                    "Hollow Tides",
                    vec![variant(
                        "standard",
                        "tides",
                        Availability::NeedsSetup {
                            reason: "choose a profile".into(),
                        },
                    )],
                ),
            ],
            user_projection: UserProjection {
                favorite_item_ids: vec!["ridge".into()],
                ..UserProjection::default()
            },
        };
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let mut core = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let home = core.scene(metrics, "").unwrap();
        let ids: Vec<_> = home
            .root()
            .children
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        assert!(ids.contains(&"favorites-label"));
        let favorite = home
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "favorite-item-ridge")
            .unwrap();
        for part in ["art", "initial", "plate", "title"] {
            assert!(
                favorite
                    .children
                    .iter()
                    .any(|node| node.id.as_str() == format!("favorite-card-{part}-ridge"))
            );
        }
        snapshot.user_projection.favorite_item_ids.clear();
        let empty = ShellCore::boot(&snapshot, &pf_theme::flagship(), false)
            .scene(metrics, "")
            .unwrap();
        assert!(
            !empty
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "favorites-label")
        );

        core.go(Route::Library);
        core.focus = 2;
        core.action(&ShellAction::Activate);
        assert_eq!(core.library_items, vec![0]);
        core.set_search_query("ridge");
        assert_eq!(core.search_result_ids(), vec!["ridge"]);
        core.set_search_query("tides");
        assert!(core.search_result_ids().is_empty());

        core.selected_item = Some(1);
        core.go(Route::Details);
        let unknown_details = core.scene(metrics, "").unwrap();
        assert!(
            !unknown_details
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "detail-playtime"),
            "unknown playtime must not render as zero"
        );
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        core.load_history(&[history_entry(
            "tides",
            Some(start),
            Some((
                start + Duration::from_secs(3 * 3_600 + 20 * 60),
                EndPrecision::Approximate,
            )),
        )]);
        let details = core.scene(metrics, "").unwrap();
        let played = details
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "detail-playtime")
            .expect("known playtime anatomy");
        assert_eq!(played.accessible_label, "Played ~3h 20m");
        for id in [
            "detail-badge-0",
            "detail-badge-1",
            "detail-badge-2",
            "detail-availability-reason",
        ] {
            assert!(
                details
                    .root()
                    .children
                    .iter()
                    .any(|node| node.id.as_str() == id),
                "missing {id}"
            );
        }
        let reason = details
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "detail-availability-reason")
            .unwrap();
        assert!(reason.accessible_label.contains("choose a profile"));
    }

    fn quick_scene(core: &ShellCore) -> Scene {
        core.scene(
            SurfaceMetrics {
                logical_width: 1280.0,
                logical_height: 720.0,
                scale: 1.0,
                safe_insets: Default::default(),
                orientation: pf_scene::Orientation::Landscape,
            },
            "A Open · B Back",
        )
        .unwrap()
    }

    #[test]
    fn quick_power_anatomy_is_capability_gated() {
        let mut core = core();
        let unsupported = FakePowerPort::new(
            vec![
                PowerCapability {
                    action: PowerAction::PowerOff,
                    support: Support::Supported,
                },
                PowerCapability {
                    action: PowerAction::Restart,
                    support: Support::Supported,
                },
                PowerCapability {
                    action: PowerAction::Sleep,
                    support: Support::Unsupported,
                },
            ],
            IdlePolicy::default(),
        );
        core.load_power(&unsupported);
        core.go(Route::Quick);
        let unsupported_scene = quick_scene(&core);
        let ids: Vec<_> = unsupported_scene
            .root()
            .children
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        for required in [
            "quick-power-heading",
            "quick-power-power-off",
            "quick-power-restart",
            "quick-power-idle",
        ] {
            assert!(
                ids.contains(&required),
                "missing semantic anatomy {required}"
            );
        }
        assert!(!ids.contains(&"quick-power-sleep"));

        let supported = FakePowerPort::new(
            vec![PowerCapability {
                action: PowerAction::Sleep,
                support: Support::Supported,
            }],
            IdlePolicy::default(),
        );
        core.load_power(&supported);
        assert!(
            quick_scene(&core)
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "quick-power-sleep")
        );
    }

    #[test]
    fn quick_capture_row_emits_capture_and_reports_honest_results() {
        let mut core = core();
        core.go(Route::Quick);
        let scene = quick_scene(&core);
        let row = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "quick-capture-screenshot")
            .expect("Quick must expose screenshot capture");
        assert_eq!(row.accessible_label, "Capture screenshot");
        assert_eq!(row.action, Some(NodeAction::Activate));

        core.focus = core.screenshot_row();
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::CaptureScreenshot)
        );
        core.screenshot_result(Ok("screenshot-42.png"));
        assert_eq!(
            core.session_status(),
            Some("Screenshot saved · screenshot-42.png")
        );
        core.screenshot_result(Err(()));
        assert_eq!(core.session_status(), Some("Screenshot could not be saved"));
    }

    #[test]
    fn quick_power_keeps_capabilities_when_idle_policy_load_fails() {
        let mut core = core();
        let mut power = FakePowerPort::new(
            vec![
                PowerCapability {
                    action: PowerAction::PowerOff,
                    support: Support::Supported,
                },
                PowerCapability {
                    action: PowerAction::Restart,
                    support: Support::Supported,
                },
            ],
            IdlePolicy::default(),
        );
        power.idle_policy_result = Err(PowerError::BackendUnavailable);

        core.load_power(&power);
        core.go(Route::Quick);
        let scene = quick_scene(&core);
        for id in ["quick-power-power-off", "quick-power-restart"] {
            let row = scene
                .root()
                .children
                .iter()
                .find(|node| node.id.as_str() == id)
                .unwrap();
            assert!(!row.state.disabled, "{id} should retain capability result");
            assert_eq!(row.action, Some(NodeAction::Activate));
        }
        assert!(
            !scene
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "quick-power-idle")
        );
        assert!(scene.root().children.iter().any(|node| {
            node.id.as_str() == "quick-power-status"
                && node.accessible_label == "Auto-sleep is unavailable"
        }));
    }

    #[test]
    fn quick_power_keeps_idle_policy_when_capabilities_load_fails() {
        let mut core = core();
        let mut power = FakePowerPort::new(
            vec![],
            IdlePolicy {
                sleep_after: Some(Duration::from_secs(15 * 60)),
                power_off_after: None,
            },
        );
        power.capabilities_result = Err(PowerError::BackendUnavailable);

        core.load_power(&power);
        core.go(Route::Quick);
        let scene = quick_scene(&core);
        let idle = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "quick-power-idle")
            .unwrap();
        assert_eq!(idle.accessible_label, "Auto-sleep · 15 min");
        assert!(!idle.state.disabled);
        for id in ["quick-power-power-off", "quick-power-restart"] {
            let row = scene
                .root()
                .children
                .iter()
                .find(|node| node.id.as_str() == id)
                .unwrap();
            assert!(
                row.state.disabled,
                "{id} should degrade without capabilities"
            );
            assert_eq!(row.action, None);
        }
        assert!(scene.root().children.iter().any(|node| {
            node.id.as_str() == "quick-power-status"
                && node.accessible_label == "Power actions are unavailable"
        }));
    }

    #[test]
    fn destructive_power_defaults_to_cancel_and_cancel_has_no_effect() {
        let mut core = core();
        let power = FakePowerPort::new(
            vec![PowerCapability {
                action: PowerAction::PowerOff,
                support: Support::Supported,
            }],
            IdlePolicy::default(),
        );
        core.load_power(&power);
        core.go(Route::Quick);
        core.focus = 2;
        assert_eq!(core.action(&ShellAction::Activate), None);
        let scene = quick_scene(&core);
        assert!(scene.root().children.iter().any(|node| {
            node.id.as_str() == "power-confirm-0"
                && node.accessible_label == "Cancel"
                && node.state.focused
        }));
        assert_eq!(core.action(&ShellAction::Activate), None);
        assert_eq!(core.power_dialog, PowerDialog::Closed);

        core.action(&ShellAction::Activate);
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::RequestPower(PowerAction::PowerOff))
        );
    }

    #[test]
    fn idle_selector_displays_applied_not_requested_policy() {
        let mut core = core();
        let mut power = FakePowerPort::new(
            vec![],
            IdlePolicy {
                sleep_after: Some(Duration::from_secs(5 * 60)),
                power_off_after: None,
            },
        );
        power.script_policy_write(Ok(AppliedIdlePolicy {
            requested: IdlePolicy {
                sleep_after: Some(Duration::from_secs(10 * 60)),
                power_off_after: None,
            },
            applied: IdlePolicy {
                sleep_after: Some(Duration::from_secs(15 * 60)),
                power_off_after: None,
            },
        }));
        core.load_power(&power);
        core.go(Route::Quick);
        core.focus = core.idle_row();
        let Effect::SetIdlePolicy(requested) = core.action(&ShellAction::Activate).unwrap() else {
            panic!("idle row must write through the power port");
        };
        assert_eq!(requested.sleep_after, Some(Duration::from_secs(10 * 60)));
        let applied = power.set_idle_policy(requested).unwrap();
        core.idle_policy_result(Ok(applied));
        let applied_scene = quick_scene(&core);
        let row = applied_scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "quick-power-idle")
            .unwrap();
        assert_eq!(row.accessible_label, "Auto-sleep · 15 min");
        assert!(!row.accessible_label.contains("10 min"));
    }
}
