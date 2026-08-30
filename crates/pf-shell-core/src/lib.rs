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
    AxisMove, Bounds, Elevation, ImageFit, ImageSource, Node, NodeAction, NodeId, Role, Scene,
    SurfaceMetrics, TypeRole,
};
use pf_session_authority::{EndPrecision, HistoryEntry};
use pf_theme::{Base, Theme};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

const STATUS_BAR_HEIGHT: f32 = 64.0;
const PROMPTS_AREA_HEIGHT: f32 = 60.0;
const HOME_SHELF_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
struct LibraryGeometry {
    columns: usize,
    card_width: f32,
    card_gap: f32,
    card_left: f32,
    card_top: f32,
    toolbar_columns: usize,
}

impl LibraryGeometry {
    const TOOLBAR_ITEM_COUNT: usize = 4;

    fn toolbar_rows(self) -> usize {
        Self::TOOLBAR_ITEM_COUNT.div_ceil(self.toolbar_columns)
    }

    fn toolbar_row(self, item: usize) -> usize {
        item / self.toolbar_columns
    }

    fn toolbar_column(self, item: usize) -> usize {
        item % self.toolbar_columns
    }

    fn wrapped_toolbar(self) -> bool {
        self.toolbar_rows() > 1
    }

    fn toolbar_to_grid_column(self, toolbar_column: usize) -> usize {
        ((2 * toolbar_column + 1) * self.columns / (2 * self.toolbar_columns)).min(self.columns - 1)
    }

    fn grid_to_toolbar_column(self, grid_column: usize) -> usize {
        ((2 * grid_column + 1) * self.toolbar_columns / (2 * self.columns))
            .min(self.toolbar_columns - 1)
    }
}

fn library_geometry(surface_width: f32) -> LibraryGeometry {
    let (columns, toolbar_columns, card_top) = if surface_width >= 1100.0 {
        (6, 4, 184.0)
    } else if surface_width >= 760.0 {
        (4, 4, 252.0)
    } else {
        (3, 2, 320.0)
    };
    let card_left = 48.0;
    let card_gap = 16.0;
    let card_width =
        (surface_width - 2.0 * card_left - (columns - 1) as f32 * card_gap) / columns as f32;
    LibraryGeometry {
        columns,
        card_width,
        card_gap,
        card_left,
        card_top,
        toolbar_columns,
    }
}

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
    RefreshManualTime,
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
    Recent,
    Alphabetical,
    Games,
    EverythingElse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlsFlow {
    Rows,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SystemFlow {
    Rows,
    ManualTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManualTimeField {
    Year,
    Month,
    Day,
    Hour,
    Minute,
}

impl ManualTimeField {
    const ALL: [Self; 5] = [Self::Year, Self::Month, Self::Day, Self::Hour, Self::Minute];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManualTimePicker {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    field: usize,
}

impl ManualTimePicker {
    const DEFAULT: Self = Self {
        year: 2027,
        month: 1,
        day: 15,
        hour: 8,
        minute: 0,
        field: 0,
    };

    fn from_system_time(time: SystemTime) -> Self {
        let Ok(since_epoch) = time.duration_since(SystemTime::UNIX_EPOCH) else {
            return Self::DEFAULT;
        };
        let Ok(days) = i64::try_from(since_epoch.as_secs() / 86_400) else {
            return Self::DEFAULT;
        };
        let seconds = since_epoch.as_secs() % 86_400;
        let (year, month, day) = civil_from_days(days);
        if !(1970..=9999).contains(&year) {
            return Self::DEFAULT;
        }
        Self {
            year,
            month,
            day,
            hour: (seconds / 3_600) as u8,
            minute: ((seconds % 3_600) / 60) as u8,
            field: 0,
        }
    }

    fn adjust(&mut self, delta: i32) {
        match ManualTimeField::ALL[self.field] {
            ManualTimeField::Year => self.year = wrap_i32(self.year, delta, 1970, 9999),
            ManualTimeField::Month => self.month = wrap_u8(self.month, delta, 1, 12),
            ManualTimeField::Day => {
                self.day = wrap_u8(self.day, delta, 1, days_in_month(self.year, self.month));
                return;
            }
            ManualTimeField::Hour => self.hour = wrap_u8(self.hour, delta, 0, 23),
            ManualTimeField::Minute => self.minute = wrap_u8(self.minute, delta, 0, 59),
        }
        self.day = self.day.min(days_in_month(self.year, self.month));
    }

    fn system_time(self) -> SystemTime {
        let days = u64::try_from(days_from_civil(self.year, self.month, self.day))
            .expect("picker years are at or after the Unix epoch");
        let seconds = days * 86_400 + u64::from(self.hour) * 3_600 + u64::from(self.minute) * 60;
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn label(self) -> String {
        let values = [
            format!("{:04}", self.year),
            format!("{:02}", self.month),
            format!("{:02}", self.day),
            format!("{:02}", self.hour),
            format!("{:02}", self.minute),
        ];
        let marked = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                if index == self.field {
                    format!("[{value}]")
                } else {
                    value
                }
            })
            .collect::<Vec<_>>();
        format!(
            "{}-{}-{} {}:{}",
            marked[0], marked[1], marked[2], marked[3], marked[4]
        )
    }
}

fn wrap_i32(value: i32, delta: i32, min: i32, max: i32) -> i32 {
    (value - min + delta).rem_euclid(max - min + 1) + min
}

fn wrap_u8(value: u8, delta: i32, min: u8, max: u8) -> u8 {
    u8::try_from(wrap_i32(
        i32::from(value),
        delta,
        i32::from(min),
        i32::from(max),
    ))
    .expect("wrapped byte range remains a byte")
}

const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = i32::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i32::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    i64::from(era * 146_097 + day_of_era - 719_468)
}

fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (
        i32::try_from(year).expect("SystemTime calendar year fits i32"),
        u8::try_from(month).expect("calendar month fits u8"),
        u8::try_from(day).expect("calendar day fits u8"),
    )
}

#[derive(Clone, Copy)]
enum SystemRow {
    TimeUnavailable,
    TransferUnavailable,
    Timezone,
    Ntp,
    ManualTime,
    Transfer(TransferService),
    Accessibility,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsRoom {
    Accessibility,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsRowAction {
    Preference(usize),
    Appearance,
    Recovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SettingsSceneRow {
    id: String,
    label: String,
    action: Option<SettingsRowAction>,
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
    caller_focus_id: Option<String>,
    selected_item: Option<usize>,
    search_query: String,
    search_results: Vec<usize>,
    library_filter: LibraryFilter,
    library_items: Vec<usize>,
    library_surface_width: Cell<f32>,
    launch_focus: usize,
    active_title: String,
    crash_summary: String,
    crash_receipt_id: String,
    crash_exit_detail: String,
    recovery_available: bool,
    authority_unavailable: bool,
    safe_return_failed: bool,
    session_status: Option<String>,
    pending_ack: bool,
    just_returned: bool,
    motion_ms: u32,
    normal_motion_ms: u32,
    env_reduced_motion: bool,
    reduced_motion: bool,
    high_contrast: bool,
    reduce_flashing: bool,
    text_scale: u16,
    appearance_day: bool,
    settings_room: SettingsRoom,
    settings_in_rows: bool,
    settings_row_focused: bool,
    settings_saved_focus: [usize; 5],
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
    system_flow: SystemFlow,
    manual_time_picker: ManualTimePicker,
    time_capabilities: Result<TimeCapabilities, String>,
    time_state: Result<pf_ports::TimeState, String>,
    transfer_services: Result<Vec<TransferServiceState>, String>,
    system_status: Option<String>,
    playtime: HashMap<String, Playtime>,
    recent_use: HashMap<String, SystemTime>,
}

impl ShellCore {
    fn preference_is_applied(key: &str) -> bool {
        matches!(
            key,
            "textScale" | "highContrast" | "reduceMotion" | "appearance"
        )
    }

    fn preference_label(row: &DisplayPreference) -> String {
        let value = match &row.effective {
            PreferenceValue::Bool(value) => if *value { "On" } else { "Off" }.into(),
            PreferenceValue::Text(value) => value.clone(),
            PreferenceValue::Integer(value) => value.to_string(),
        };
        if Self::preference_is_applied(row.key) {
            format!("{} · {value}", row.label)
        } else {
            format!("{} · {value} · not applied on this build", row.label)
        }
    }

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
            caller_focus_id: None,
            selected_item: None,
            search_query: String::new(),
            search_results: (0..snapshot.items.len()).collect(),
            library_filter: LibraryFilter::Recent,
            library_items: (0..snapshot.items.len()).collect(),
            library_surface_width: Cell::new(1280.0),
            launch_focus: 0,
            active_title: String::new(),
            crash_summary: String::new(),
            crash_receipt_id: String::new(),
            crash_exit_detail: String::new(),
            recovery_available: false,
            authority_unavailable: false,
            safe_return_failed: false,
            session_status: None,
            pending_ack: false,
            just_returned: false,
            motion_ms: theme
                .resolve_motion("launch", reduced_motion)
                .expect("motion.launch")
                .duration_ms,
            normal_motion_ms: theme
                .resolve_motion("launch", false)
                .expect("motion.launch")
                .duration_ms,
            env_reduced_motion: reduced_motion,
            reduced_motion,
            high_contrast: false,
            reduce_flashing: false,
            text_scale: 100,
            appearance_day: false,
            settings_room: SettingsRoom::Accessibility,
            settings_in_rows: false,
            settings_row_focused: false,
            settings_saved_focus: [0; 5],
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
            system_flow: SystemFlow::Rows,
            manual_time_picker: ManualTimePicker::DEFAULT,
            time_capabilities: Err("Time controls unavailable".into()),
            time_state: Err("Time status unavailable".into()),
            transfer_services: Err("Transfer status unavailable".into()),
            system_status: None,
            playtime: HashMap::new(),
            recent_use: HashMap::new(),
        }
    }

    pub fn load_history(&mut self, entries: &[HistoryEntry]) {
        let playtime = derive_playtime(entries);
        let mut target_recent_use = HashMap::<String, SystemTime>::new();
        for entry in entries {
            let Some(used_at) = entry.ended_at.map(|end| end.at).or(entry.started_at) else {
                continue;
            };
            target_recent_use
                .entry(entry.item_id.clone())
                .and_modify(|latest| *latest = (*latest).max(used_at))
                .or_insert(used_at);
        }
        let recent_use = self
            .items
            .iter()
            .filter_map(|item| {
                item.variants
                    .iter()
                    .filter_map(|variant| target_recent_use.get(&variant.launch_target.app_id))
                    .max()
                    .copied()
                    .map(|used_at| (item.id.clone(), used_at))
            })
            .collect();
        if self.playtime != playtime || self.recent_use != recent_use {
            self.playtime = playtime;
            self.recent_use = recent_use;
            self.refresh_library_items();
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

    pub fn manual_time_refresh_result(
        &mut self,
        result: Result<pf_ports::TimeState, pf_ports::TimeError>,
    ) {
        match result {
            Ok(state) => {
                self.manual_time_picker = ManualTimePicker::from_system_time(state.wall_clock);
                self.time_state = Ok(state);
                self.system_flow = SystemFlow::ManualTime;
            }
            Err(error) => {
                self.time_state = Err(format!("Time status unavailable · {error:?}"));
                self.system_status = Some(format!("Manual time unavailable · {error:?}"));
            }
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
        if self.time_state.is_err() {
            rows.push(SystemRow::TimeUnavailable);
        }
        if self.transfer_services.is_err() {
            rows.push(SystemRow::TransferUnavailable);
        }
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
        if let Some(observed) = port.read(&PreferenceKey("appearance".into()))? {
            self.apply_effective("appearance", &observed.effective);
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
            ("highContrast", PreferenceValue::Bool(value)) => self.high_contrast = *value,
            ("reduceMotion", PreferenceValue::Bool(value)) => {
                self.reduced_motion = self.env_reduced_motion || *value;
                self.motion_ms = if self.reduced_motion {
                    0
                } else {
                    self.normal_motion_ms
                };
            }
            ("reduceFlashing", PreferenceValue::Bool(value)) => self.reduce_flashing = *value,
            ("textScale", PreferenceValue::Text(value)) => {
                self.text_scale = value
                    .trim_end_matches('%')
                    .parse()
                    .unwrap_or(100)
                    .clamp(100, 200);
            }
            ("appearance", PreferenceValue::Text(value)) => self.appearance_day = value == "Day",
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
    pub const fn theme_base(&self) -> Base {
        if self.high_contrast {
            Base::HighContrast
        } else if self.appearance_day {
            Base::Day
        } else {
            Base::Dusk
        }
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
                words.iter().all(|word| haystack.contains(word))
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
                    LibraryFilter::Recent | LibraryFilter::Alphabetical => true,
                    LibraryFilter::Games => matches!(item.kind, AppKind::Game),
                    LibraryFilter::EverythingElse => !matches!(item.kind, AppKind::Game),
                };
                included.then_some(index)
            })
            .collect();
        match self.library_filter {
            LibraryFilter::Recent => self.library_items.sort_by(|left, right| {
                self.recent_use
                    .get(&self.items[*right].id)
                    .cmp(&self.recent_use.get(&self.items[*left].id))
                    .then_with(|| left.cmp(right))
            }),
            LibraryFilter::Alphabetical => self.library_items.sort_by(|left, right| {
                self.items[*left]
                    .title
                    .to_lowercase()
                    .cmp(&self.items[*right].title.to_lowercase())
                    .then_with(|| self.items[*left].id.cmp(&self.items[*right].id))
            }),
            LibraryFilter::Games | LibraryFilter::EverythingElse => {}
        }
    }

    fn focused_item_index(&self) -> Option<usize> {
        match self.route {
            Route::Library => self
                .focus
                .checked_sub(5)
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

    fn focus_object_id(&self) -> String {
        self.focused_item_index().map_or_else(
            || format!("route-{:?}-{}", self.route, self.focus),
            |index| format!("item-{}", self.items[index].id),
        )
    }

    fn remember_caller(&mut self) {
        self.caller_route = self.route;
        self.caller_focus = self.focus;
        self.caller_focus_id = Some(self.focus_object_id());
    }

    fn restore_caller_focus(&mut self) {
        self.route = self.caller_route;
        if let Some(id) = self.caller_focus_id.as_deref()
            && let Some(item_id) = id.strip_prefix("item-")
            && let Some(index) = self.items.iter().position(|item| item.id == item_id)
            && let Some(focus) = match self.caller_route {
                Route::Home => (index < self.focus_count()).then_some(index),
                Route::Library => self
                    .library_items
                    .iter()
                    .position(|&item| item == index)
                    .map(|position| position + 5),
                Route::Search => self.search_results.iter().position(|&item| item == index),
                _ => None,
            }
        {
            self.focus = focus;
            return;
        }
        self.focus = match self.caller_route {
            Route::Library if !self.library_items.is_empty() => {
                self.caller_focus.clamp(5, self.library_items.len() + 4)
            }
            Route::Search if !self.search_results.is_empty() => self
                .caller_focus
                .min(self.search_results.len().saturating_sub(1)),
            _ => self.caller_focus.min(self.focus_count().saturating_sub(1)),
        };
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
        self.authority_unavailable || self.safe_return_failed
    }
    #[must_use]
    pub fn session_status(&self) -> Option<&str> {
        if self.safe_return_failed {
            Some("The session service isn't reachable; Safe Return can retry")
        } else {
            self.session_status.as_deref()
        }
    }
    #[must_use]
    pub const fn session_active(&self) -> bool {
        matches!(
            self.presentation,
            Presentation::Starting | Presentation::Running
        )
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
    pub fn active_session_backend_unavailable(&mut self) {
        self.bump_revision();
        self.authority_unavailable = true;
        self.session_status =
            Some("The session service isn't reachable; Safe Return can retry".into());
    }
    pub fn safe_return_failed(&mut self) {
        self.bump_revision();
        self.safe_return_failed = true;
    }
    pub fn safe_return_succeeded(&mut self) {
        if self.safe_return_failed {
            self.bump_revision();
        }
        self.safe_return_failed = false;
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
        if self.route == Route::Settings
            && self.settings_room == SettingsRoom::System
            && self.system_flow == SystemFlow::ManualTime
        {
            return match action {
                ShellAction::Move(AxisMove::Left) => {
                    self.manual_time_picker.field = self.manual_time_picker.field.saturating_sub(1);
                    None
                }
                ShellAction::Move(AxisMove::Right) => {
                    self.manual_time_picker.field =
                        (self.manual_time_picker.field + 1).min(ManualTimeField::ALL.len() - 1);
                    None
                }
                ShellAction::Move(AxisMove::Up) => {
                    self.manual_time_picker.adjust(1);
                    None
                }
                ShellAction::Move(AxisMove::Down) => {
                    self.manual_time_picker.adjust(-1);
                    None
                }
                ShellAction::Activate => {
                    self.system_flow = SystemFlow::Rows;
                    Some(Effect::SetManualTime(self.manual_time_picker.system_time()))
                }
                ShellAction::Back => {
                    self.system_flow = SystemFlow::Rows;
                    None
                }
                ShellAction::Custom(_) => None,
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
                if let Some(item) = self.focused_item_index() {
                    return Some(Effect::ToggleFavorite {
                        item_id: self.items[item].id.clone(),
                        favorite: !self.items[item].favorite,
                    });
                }
            }
            ShellAction::Custom(name) if name == "Search" => {
                self.remember_caller();
                self.go(Route::Search);
            }
            ShellAction::Custom(name) if name == "Room.next" => self.next_room(),
            ShellAction::Custom(name) if name == "Room.previous" => self.previous_room(),
            ShellAction::Custom(name) if name == "Quick" => {
                self.go(Route::Quick);
            }
            ShellAction::Back if self.route == Route::Quick => {
                let route = self.previous_route;
                self.go(route);
            }
            ShellAction::Back if matches!(self.route, Route::Details | Route::Search) => {
                self.restore_caller_focus();
            }
            ShellAction::Back if self.route == Route::VariantChooser => self.go(Route::Details),
            ShellAction::Back if self.route == Route::Settings && self.settings_in_rows => {
                if self.settings_row_focused {
                    self.settings_saved_focus[Self::settings_room_slot(self.settings_room)] =
                        self.focus;
                }
                self.settings_in_rows = false;
                self.settings_row_focused = false;
                self.focus = self
                    .settings_rooms()
                    .iter()
                    .position(|room| *room == self.settings_room)
                    .unwrap_or(0);
            }
            ShellAction::Back if self.route != Route::Home => self.go(Route::Home),
            ShellAction::Move(AxisMove::Right) if self.route == Route::Home => {
                self.focus = (self.focus + 1).min(self.focus_count().saturating_sub(1));
            }
            ShellAction::Move(AxisMove::Right)
                if self.route == Route::Library && (1..=4).contains(&self.focus) =>
            {
                let geometry = library_geometry(self.library_surface_width.get());
                let item = self.focus - 1;
                if !geometry.wrapped_toolbar()
                    || geometry.toolbar_column(item) + 1 < geometry.toolbar_columns
                        && item + 1 < LibraryGeometry::TOOLBAR_ITEM_COUNT
                {
                    self.focus = (self.focus + 1).min(4);
                }
            }
            ShellAction::Move(AxisMove::Right)
                if self.route == Route::Library && self.focus >= 5 =>
            {
                let item = self.focus - 5;
                let columns = library_geometry(self.library_surface_width.get()).columns;
                if item % columns + 1 < columns && item + 1 < self.library_items.len() {
                    self.focus += 1;
                }
            }
            ShellAction::Move(AxisMove::Right) if self.route == Route::Settings => {
                self.enter_settings_rows();
            }
            ShellAction::Move(AxisMove::Left)
                if self.route == Route::Settings && self.settings_in_rows =>
            {
                if self.settings_row_focused {
                    self.settings_saved_focus[Self::settings_room_slot(self.settings_room)] =
                        self.focus;
                }
                self.settings_in_rows = false;
                self.settings_row_focused = false;
                self.focus = self
                    .settings_rooms()
                    .iter()
                    .position(|room| *room == self.settings_room)
                    .unwrap_or(0);
            }
            ShellAction::Move(AxisMove::Left)
                if self.route == Route::Library && (2..=4).contains(&self.focus) =>
            {
                let geometry = library_geometry(self.library_surface_width.get());
                if !geometry.wrapped_toolbar() || geometry.toolbar_column(self.focus - 1) > 0 {
                    self.focus -= 1;
                }
            }
            ShellAction::Move(AxisMove::Left)
                if self.route == Route::Library && self.focus >= 5 =>
            {
                let columns = library_geometry(self.library_surface_width.get()).columns;
                if (self.focus - 5) % columns > 0 {
                    self.focus -= 1;
                }
            }
            ShellAction::Move(AxisMove::Right) if self.route == Route::Details => {
                let play = self.detail_play_focus();
                if play.is_some_and(|play| self.focus == play) {
                    self.focus += 1;
                }
            }
            ShellAction::Move(AxisMove::Left) if self.route == Route::Details => {
                if let Some(play) = self.detail_play_focus()
                    && self.focus == play + 1
                {
                    self.focus = play;
                }
            }
            ShellAction::Move(AxisMove::Down) if self.route == Route::Details => {
                let variant_count = self.detail_focusable_variants().len();
                if self.focus < variant_count {
                    self.focus = (self.focus + 1).min(self.detail_pin_focus());
                }
            }
            ShellAction::Move(AxisMove::Up) if self.route == Route::Details => {
                let variant_count = self.detail_focusable_variants().len();
                if variant_count > 0 && self.focus >= variant_count {
                    self.focus = variant_count - 1;
                } else {
                    self.focus = self.focus.saturating_sub(1);
                }
            }
            ShellAction::Move(AxisMove::Down) if self.route == Route::Library && self.focus < 5 => {
                if !self.library_items.is_empty() {
                    let geometry = library_geometry(self.library_surface_width.get());
                    if geometry.wrapped_toolbar() && self.focus == 0 {
                        self.focus = 1;
                    } else if geometry.wrapped_toolbar()
                        && self.focus - 1 + geometry.toolbar_columns
                            < LibraryGeometry::TOOLBAR_ITEM_COUNT
                    {
                        self.focus += geometry.toolbar_columns;
                    } else {
                        let grid_column = if geometry.wrapped_toolbar() {
                            geometry.toolbar_to_grid_column(
                                geometry.toolbar_column(self.focus.saturating_sub(1)),
                            )
                        } else {
                            self.focus.saturating_sub(1)
                        };
                        self.focus =
                            5 + grid_column.min(self.library_items.len().saturating_sub(1));
                    }
                }
            }
            ShellAction::Move(AxisMove::Down)
                if self.route == Route::Library && self.focus >= 5 =>
            {
                let columns = library_geometry(self.library_surface_width.get()).columns;
                if self.focus - 5 + columns < self.library_items.len() {
                    self.focus += columns;
                }
            }
            ShellAction::Move(AxisMove::Up) if self.route == Route::Library && self.focus >= 5 => {
                let columns = library_geometry(self.library_surface_width.get()).columns;
                if self.focus - 5 >= columns {
                    self.focus -= columns;
                } else {
                    let geometry = library_geometry(self.library_surface_width.get());
                    if geometry.wrapped_toolbar() {
                        let toolbar_column =
                            geometry.grid_to_toolbar_column((self.focus - 5) % columns);
                        let toolbar_item = (geometry.toolbar_rows() - 1) * geometry.toolbar_columns
                            + toolbar_column;
                        self.focus = toolbar_item.min(LibraryGeometry::TOOLBAR_ITEM_COUNT - 1) + 1;
                    } else {
                        self.focus = (self.focus - 5).min(3) + 1;
                    }
                }
            }
            ShellAction::Move(AxisMove::Up)
                if self.route == Route::Library && (1..=4).contains(&self.focus) =>
            {
                let geometry = library_geometry(self.library_surface_width.get());
                let item = self.focus - 1;
                if geometry.wrapped_toolbar() {
                    if geometry.toolbar_row(item) > 0 {
                        self.focus -= geometry.toolbar_columns;
                    } else {
                        self.focus = 0;
                    }
                } else {
                    self.focus = self.focus.saturating_sub(1);
                }
            }
            ShellAction::Move(AxisMove::Down)
                if self.route == Route::Settings && !self.settings_in_rows =>
            {
                let rooms = self.settings_rooms();
                self.focus = (self.focus + 1).min(rooms.len().saturating_sub(1));
                self.settings_room = rooms[self.focus];
            }
            ShellAction::Move(AxisMove::Up)
                if self.route == Route::Settings && !self.settings_in_rows =>
            {
                let rooms = self.settings_rooms();
                self.focus = self.focus.saturating_sub(1);
                self.settings_room = rooms[self.focus];
            }
            ShellAction::Move(AxisMove::Down | AxisMove::Right)
                if self.route == Route::Settings && self.settings_in_rows =>
            {
                self.move_settings_focus(true);
            }
            ShellAction::Move(AxisMove::Up | AxisMove::Left)
                if self.route == Route::Settings && self.settings_in_rows =>
            {
                self.move_settings_focus(false);
            }
            ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                self.focus = (self.focus + 1).min(self.focus_count().saturating_sub(1))
            }
            ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                self.focus = self.focus.saturating_sub(1)
            }
            ShellAction::Activate if self.route == Route::Settings && !self.settings_in_rows => {
                self.enter_settings_rows();
            }
            ShellAction::Activate => return self.activate(),
            ShellAction::Back | ShellAction::Custom(_) => {}
        }
        None
    }

    fn activate(&mut self) -> Option<Effect> {
        if self.route == Route::Settings {
            if !self.settings_row_focused {
                return None;
            }
            return match self.settings_scene_rows().get(self.focus)?.action? {
                SettingsRowAction::Preference(index) => self.preference_effect(index),
                SettingsRowAction::Appearance => Some(Effect::ChangePreference(PreferenceChange {
                    key: PreferenceKey("appearance".into()),
                    value: PreferenceValue::Text(
                        if self.appearance_day { "Dusk" } else { "Day" }.into(),
                    ),
                    authority: ChangeAuthority("user".into()),
                })),
                SettingsRowAction::Recovery => Some(Effect::EnterRecovery),
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
                self.remember_caller();
                self.go(Route::Search);
                return None;
            }
            if (1..=4).contains(&self.focus) {
                self.library_filter = [
                    LibraryFilter::Recent,
                    LibraryFilter::Alphabetical,
                    LibraryFilter::Games,
                    LibraryFilter::EverythingElse,
                ][self.focus - 1];
                self.refresh_library_items();
                return None;
            }
            self.selected_item = self.library_items.get(self.focus - 5).copied();
            self.remember_caller();
            self.go(Route::Details);
            return None;
        }
        if self.route == Route::Search {
            let &item = self.search_results.get(self.focus)?;
            self.selected_item = Some(item);
            self.remember_caller();
            self.go(Route::Details);
            return None;
        }
        if self.route == Route::Details {
            let item = self.selected_item?;
            let variants = self.detail_focusable_variants();
            if let Some(&variant) = variants.get(self.focus) {
                return self.launch_variant(item, variant);
            }
            if self.focus == self.detail_pin_focus() {
                return Some(Effect::ToggleFavorite {
                    item_id: self.items[item].id.clone(),
                    favorite: !self.items[item].favorite,
                });
            }
            let ready = self.ready_variants(item);
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
        let ready = self.ready_variants(item);
        match ready.len() {
            0 => None,
            1 => self.launch_variant(item, ready[0]),
            _ => {
                self.selected_item = Some(item);
                self.remember_caller();
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

    fn detail_focusable_variants(&self) -> Vec<usize> {
        self.selected_item
            .and_then(|item| self.items.get(item))
            .into_iter()
            .flat_map(|item| item.variants.iter().take(2).enumerate())
            .filter_map(|(index, variant)| {
                matches!(variant.availability, Availability::Ready).then_some(index)
            })
            .collect()
    }

    fn detail_play_focus(&self) -> Option<usize> {
        self.selected_item
            .is_some_and(|item| !self.ready_variants(item).is_empty())
            .then(|| self.detail_focusable_variants().len())
    }

    fn detail_pin_focus(&self) -> usize {
        self.detail_play_focus()
            .map_or_else(|| self.detail_focusable_variants().len(), |play| play + 1)
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
    fn next_room(&mut self) {
        match self.route {
            Route::Home => self.go(Route::Library),
            Route::Library => self.go(Route::Settings),
            _ => {}
        }
    }
    fn previous_room(&mut self) {
        match self.route {
            Route::Settings => self.go(Route::Library),
            Route::Library => self.go(Route::Home),
            _ => {}
        }
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
            Route::Home => self.items.len().clamp(1, HOME_SHELF_LIMIT),
            Route::Library => self.library_items.len() + 5,
            Route::Search => self.search_results.len().max(1),
            Route::Details => self.detail_pin_focus() + 1,
            Route::VariantChooser => self
                .selected_item
                .map_or(0, |item| self.ready_variants(item).len())
                .max(1),
            Route::Settings => self.settings_scene_rows().len().max(1),
            Route::Quick => self.screenshot_row() + 1,
        }
    }

    fn enter_settings_rows(&mut self) {
        if self.settings_in_rows {
            return;
        }
        let rows = self.settings_scene_rows();
        let saved = self.settings_saved_focus[Self::settings_room_slot(self.settings_room)];
        self.settings_in_rows = true;
        let row_focus = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.action.is_some())
            .min_by_key(|(index, _)| index.abs_diff(saved))
            .map(|(index, _)| index);
        self.settings_row_focused = row_focus.is_some();
        if let Some(index) = row_focus {
            self.focus = index;
        } else {
            self.focus = 0;
        }
    }

    fn move_settings_focus(&mut self, forward: bool) {
        if !self.settings_row_focused {
            return;
        }
        let focus = self.focus;
        let rows = self.settings_scene_rows();
        let next = if forward {
            rows.iter()
                .enumerate()
                .skip(focus + 1)
                .find(|(_, row)| row.action.is_some())
        } else {
            rows.iter()
                .enumerate()
                .take(focus)
                .rev()
                .find(|(_, row)| row.action.is_some())
        };
        if let Some((index, _)) = next {
            self.focus = index;
        }
    }

    fn settings_scene_rows(&self) -> Vec<SettingsSceneRow> {
        let disabled = |id: &str, label: String| SettingsSceneRow {
            id: id.into(),
            label,
            action: None,
        };
        match self.settings_room {
            SettingsRoom::Accessibility => self
                .display_preferences
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let consequence = match row.key {
                        "textScale" => "Reflows labels and rows live",
                        "highContrast" => "Uses the strongest text and focus contrast",
                        "reduceMotion" => "Completes transitions without movement",
                        _ => "Prevents future flashing treatments",
                    };
                    let control = match &row.effective {
                        PreferenceValue::Bool(value) => format!(
                            "{} {}",
                            if *value { "ON" } else { "OFF" },
                            if *value { "——●" } else { "●——" }
                        ),
                        PreferenceValue::Text(value) => format!("100% / 150% / 200% · {value}"),
                        PreferenceValue::Integer(value) => value.to_string(),
                    };
                    SettingsSceneRow {
                        id: format!("accessibility-{}", row.key),
                        label: if row.interactive {
                            format!("{}\n{consequence}\n{control}", row.label)
                        } else {
                            format!("{}\nStored, but this build cannot apply it\n—", row.label)
                        },
                        action: row
                            .interactive
                            .then_some(SettingsRowAction::Preference(index)),
                    }
                })
                .chain([
                    disabled(
                        "accessibility-remap",
                        "Button remap\nThe remap flow is not available here yet\n—".into(),
                    ),
                    disabled(
                        "accessibility-diagnostic",
                        "ⓘ Mono audio and brightness controls are not available on this device."
                            .into(),
                    ),
                ])
                .collect(),
            SettingsRoom::Controls => {
                let preview = if self.control_bindings.is_empty() {
                    "No input map reported".into()
                } else {
                    self.control_bindings
                        .iter()
                        .map(|binding| binding.binding.as_str())
                        .collect::<Vec<_>>()
                        .join("  ")
                };
                vec![
                    disabled(
                        "controls-remap",
                        format!("Remap buttons\nCurrent map: {preview}\n—"),
                    ),
                    disabled(
                        "controls-safe-return",
                        format!(
                            "Safe Return button\nReturns safely from any game\n{}  —",
                            self.safe_return_binding
                        ),
                    ),
                    disabled(
                        "controls-source",
                        "ⓘ Input source facts come from the active device descriptor.".into(),
                    ),
                ]
            }
            SettingsRoom::Display => vec![SettingsSceneRow {
                id: "display-appearance".into(),
                label: format!(
                    "Appearance\nChanges the room palette; High Contrast composes over it\nDusk / Day · {}",
                    if self.appearance_day { "Day" } else { "Dusk" }
                ),
                action: Some(SettingsRowAction::Appearance),
            }],
            SettingsRoom::Network => {
                let state = self
                    .network_state
                    .as_ref()
                    .expect("filtered network section has authority");
                vec![
                    disabled(
                        "network-ssid",
                        format!(
                            "SSID\nObserved connected network\n{}\n—",
                            state.connected_ssid.as_deref().unwrap_or("Not connected")
                        ),
                    ),
                    disabled(
                        "network-signal",
                        "ⓘ Signal and address are not reported by this authority.\n—".into(),
                    ),
                ]
            }
            SettingsRoom::System => {
                let mut rows = vec![
                    disabled(
                        "system-about",
                        format!(
                            "About\nPocketForge shell version\n{}\n—",
                            env!("CARGO_PKG_VERSION")
                        ),
                    ),
                    disabled(
                        "system-device",
                        "Device and storage\nDesktop simulator fixture\nSimulated device · storage not reported\n—".into(),
                    ),
                    disabled(
                        "system-licenses",
                        "Licenses\nOpen-source notices\n—".into(),
                    ),
                ];
                if self.recovery_available {
                    rows.push(SettingsSceneRow {
                        id: "system-recovery".into(),
                        label: "Recovery\nOpen the independent recovery entry\n›".into(),
                        action: Some(SettingsRowAction::Recovery),
                    });
                }
                rows
            }
        }
    }

    fn settings_rooms(&self) -> Vec<SettingsRoom> {
        let mut rooms = vec![
            SettingsRoom::Accessibility,
            SettingsRoom::Controls,
            SettingsRoom::Display,
        ];
        if self.network_state.is_ok() {
            rooms.push(SettingsRoom::Network);
        }
        rooms.push(SettingsRoom::System);
        rooms
    }

    const fn settings_room_slot(room: SettingsRoom) -> usize {
        match room {
            SettingsRoom::Accessibility => 0,
            SettingsRoom::Controls => 1,
            SettingsRoom::Display => 2,
            SettingsRoom::Network => 3,
            SettingsRoom::System => 4,
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
        if matches!(event, SessionEvent::Terminal(_)) {
            self.safe_return_failed = false;
        }
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
        self.library_surface_width.set(w);
        let mut children = vec![
            node(
                "status-bar",
                Role::Group,
                "",
                0.0,
                0.0,
                w,
                STATUS_BAR_HEIGHT,
                "--color-surface-raised",
            ),
            node(
                "status-left-spacer",
                Role::Group,
                "",
                0.0,
                0.0,
                200.0,
                STATUS_BAR_HEIGHT,
                "--color-surface-raised",
            ),
            node(
                "status-cluster",
                Role::Text,
                if self.authority_unavailable() {
                    "Wi-Fi   82%   !   9:41"
                } else {
                    "Wi-Fi   82%   9:41"
                },
                w - 248.0,
                16.0,
                200.0,
                32.0,
                "--color-surface-raised",
            )
            .with_type_role(TypeRole::Caption),
        ];
        let room_left = w / 2.0 - 220.0;
        children.push(
            node(
                "rooms",
                Role::Group,
                "",
                room_left,
                12.0,
                424.0,
                40.0,
                "--color-surface-raised",
            )
            .with_type_role(TypeRole::Label),
        );
        for (id, label, x, width) in [
            ("room-keycap-left", "L", room_left + 8.0, 32.0),
            ("room-home", "Home", room_left + 56.0, 72.0),
            ("room-library", "Library", room_left + 152.0, 88.0),
            ("room-settings", "Settings", room_left + 264.0, 92.0),
            ("room-keycap-right", "R", room_left + 384.0, 32.0),
        ] {
            let keycap = id.contains("keycap");
            let selected = matches!(
                (id, self.route),
                ("room-home", Route::Home)
                    | (
                        "room-library",
                        Route::Library | Route::Search | Route::Details | Route::VariantChooser
                    )
                    | ("room-settings", Route::Settings)
            );
            if keycap {
                children.push(node(
                    &format!("{id}-border"),
                    Role::Group,
                    "",
                    x - 4.0,
                    12.0,
                    width + 8.0,
                    34.0,
                    "--color-border-strong",
                ));
                children.push(node(
                    &format!("{id}-fill"),
                    Role::Group,
                    "",
                    x - 2.0,
                    14.0,
                    width + 4.0,
                    30.0,
                    "--color-surface-raised",
                ));
                children.push(
                    node(
                        id,
                        Role::Text,
                        label,
                        x,
                        16.0,
                        width,
                        26.0,
                        "--state-rest-text",
                    )
                    .with_type_role(TypeRole::Caption),
                );
            } else {
                children.push(
                    node(
                        id,
                        Role::Text,
                        label,
                        x,
                        16.0,
                        width,
                        32.0,
                        "--color-surface-raised",
                    )
                    .with_type_role(TypeRole::Label),
                );
                if selected {
                    children.push(node(
                        &format!("{id}-underline"),
                        Role::Group,
                        "",
                        x,
                        49.0,
                        width,
                        3.0,
                        "--state-selected-accent",
                    ));
                }
            }
        }
        if let Some(status) = self.session_status() {
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
        let supplied_footer = if let Some((base, glyph)) = footer.split_once('\u{1f}') {
            self.focused_item_index().map_or_else(
                || base.to_owned(),
                |item| {
                    let label = if self.items[item].favorite {
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
        let footer = match self.route {
            Route::Library => (self.focus >= 5)
                .then(|| self.binding_prompt("Activate", "Details"))
                .flatten()
                .unwrap_or_default(),
            Route::Details => {
                let ready = self
                    .selected_item
                    .is_some_and(|index| !self.ready_variants(index).is_empty());
                let mut prompts = Vec::new();
                if let Some(prompt) = self.binding_prompt("Back", "Back") {
                    prompts.push(prompt);
                }
                if let Some(item) = self.selected_item.and_then(|index| self.items.get(index))
                    && let Some(prompt) = self.binding_prompt(
                        "Quick",
                        if item.favorite {
                            "Unfavorite"
                        } else {
                            "Favorite"
                        },
                    )
                {
                    prompts.push(prompt);
                }
                let activate_label = if self.focus == self.detail_pin_focus() {
                    self.selected_item
                        .and_then(|index| self.items.get(index))
                        .map(|item| if item.favorite { "Unpin" } else { "Pin" })
                } else if ready {
                    Some("Play")
                } else {
                    None
                };
                if let Some(prompt) =
                    activate_label.and_then(|label| self.binding_prompt("Activate", label))
                {
                    prompts.push(prompt);
                }
                prompts.join(" · ")
            }
            Route::Settings => {
                let mut prompts = self
                    .binding_prompt("Back", "Back")
                    .into_iter()
                    .collect::<Vec<_>>();
                if self.settings_in_rows
                    && self.settings_row_focused
                    && self
                        .settings_scene_rows()
                        .get(self.focus)
                        .is_some_and(|row| row.action.is_some())
                    && let Some(prompt) = self.binding_prompt("Activate", "Change")
                {
                    prompts.push(prompt);
                }
                prompts.join(" · ")
            }
            _ => supplied_footer,
        };
        children.push(node(
            "prompt-bar",
            Role::Group,
            "",
            0.0,
            h - PROMPTS_AREA_HEIGHT,
            w,
            PROMPTS_AREA_HEIGHT,
            "--color-surface-raised",
        ));
        children.push(
            node(
                "prompts",
                Role::Text,
                &footer,
                w - 600.0,
                h - PROMPTS_AREA_HEIGHT,
                552.0,
                32.0,
                "--color-surface-raised",
            )
            .with_type_role(TypeRole::Label),
        );
        let focus_id = children
            .iter()
            .find_map(focused_node_id)
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

    fn binding_prompt(&self, action: &str, label: &str) -> Option<String> {
        self.control_bindings
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| format!("{} {label}", binding.binding))
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
                SettingsRoom::Accessibility => "SETTINGS · ACCESSIBILITY",
                SettingsRoom::Display => "SETTINGS · DISPLAY",
                SettingsRoom::Controls => "SETTINGS · CONTROLS",
                SettingsRoom::Network => "SETTINGS · NETWORK",
                SettingsRoom::System => "SETTINGS · SYSTEM",
            },
            Route::Quick => unreachable!(),
        };
        if self.route != Route::Settings {
            out.push(
                node(
                    "route-heading",
                    Role::Heading,
                    heading,
                    48.0,
                    112.0,
                    500.0,
                    48.0,
                    "--color-text-primary",
                )
                .with_type_role(TypeRole::Eyebrow),
            );
        }
        if self.route == Route::Home {
            let heading = out.pop().expect("Home route heading was just added");
            let focused = self
                .focused_item_index()
                .and_then(|index| self.items.get(index));
            let mut content = vec![
                heading,
                node(
                    "hero-title",
                    Role::Heading,
                    focused.map_or("Nothing ready", |item| item.title.as_str()),
                    48.0,
                    144.0,
                    w - 96.0,
                    72.0,
                    "--color-surface-canvas",
                )
                .with_type_role(TypeRole::Hero)
                .with_line_height(1.04),
                node(
                    "hero-status",
                    Role::Text,
                    if matches!(self.presentation, Presentation::Starting) {
                        "● Starting · Game · Installed"
                    } else {
                        "● Ready · Game · Installed"
                    },
                    48.0,
                    224.0,
                    480.0,
                    32.0,
                    "--color-surface-canvas",
                )
                .with_type_role(TypeRole::Label),
            ];
            if self.presentation == Presentation::ForcedClose {
                content.push(node(
                    "attention",
                    Role::Text,
                    &format!("● Attention · {} didn't close cleanly", self.active_title),
                    w - 480.0,
                    96.0,
                    432.0,
                    36.0,
                    "--color-status-attention",
                ));
            }
            let ready_count = self
                .items
                .iter()
                .filter(|item| matches!(best_availability(item), Availability::Ready))
                .count();
            content.push(
                node(
                    "home-shelf-label",
                    Role::Heading,
                    &format!("READY NOW · {ready_count}"),
                    48.0,
                    344.0,
                    220.0,
                    28.0,
                    "--color-surface-canvas",
                )
                .with_type_role(TypeRole::Eyebrow),
            );
            let count = self.items.len().min(HOME_SHELF_LIMIT);
            let gap = 16.0;
            let card_width = ((w - 96.0 - gap * count.saturating_sub(1) as f32)
                / count.max(1) as f32)
                .min(136.0);
            for (i, item) in self.items.iter().take(HOME_SHELF_LIMIT).enumerate() {
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
                    382.0,
                    card_width,
                    220.0,
                    state_token(availability, i == self.focus),
                );
                n.action = Some(NodeAction::Activate);
                n.state.focused = i == self.focus;
                n.state.disabled = !item
                    .variants
                    .iter()
                    .any(|variant| matches!(variant.availability, Availability::Ready));
                n.children = art_nodes(
                    item,
                    "home-card",
                    x,
                    390.0,
                    card_width,
                    158.0,
                    i == self.focus,
                );
                add_unavailable_card_cues(
                    &mut n.children,
                    item,
                    availability,
                    "home-card",
                    x,
                    390.0,
                    card_width,
                );
                if item.favorite {
                    n.children.push(
                        node(
                            &format!("favorite-pin-{}", item.id),
                            Role::Text,
                            "★",
                            x + card_width - 28.0,
                            398.0,
                            20.0,
                            20.0,
                            "--color-surface-scrim",
                        )
                        .with_type_role(TypeRole::Caption),
                    );
                }
                content.push(n);
            }
            out.push(Node::new(
                NodeId::new("home-scroll-region").unwrap(),
                Role::Group,
                "",
                Bounds::new(
                    0.0,
                    STATUS_BAR_HEIGHT,
                    w,
                    h - STATUS_BAR_HEIGHT - PROMPTS_AREA_HEIGHT,
                ),
                "--color-surface-canvas",
            ));
            out.extend(content);
        } else if self.route == Route::Library {
            let geometry = library_geometry(w);
            let games = self
                .items
                .iter()
                .filter(|item| matches!(item.kind, AppKind::Game))
                .count();
            let other = self.items.len() - games;
            let compact_toolbar = geometry.columns < 6;
            let search_width = if compact_toolbar { w - 96.0 } else { w * 0.55 };
            let mut search = node(
                "library-search",
                Role::Button,
                &format!("⌕  Search {} titles", self.items.len()),
                48.0,
                112.0,
                search_width,
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
            for (index, (label, count, filter)) in [
                ("Recent".to_owned(), None, LibraryFilter::Recent),
                ("A–Z".to_owned(), None, LibraryFilter::Alphabetical),
                ("Games".to_owned(), Some(games), LibraryFilter::Games),
                (
                    "Everything else".to_owned(),
                    Some(other),
                    LibraryFilter::EverythingElse,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let toolbar_left = if compact_toolbar {
                    48.0
                } else {
                    search_width + 64.0
                };
                let toolbar_top = if compact_toolbar { 180.0 } else { 112.0 };
                let toolbar_width = if compact_toolbar {
                    w - 96.0
                } else {
                    w - toolbar_left - 48.0
                };
                let chip_width = (toolbar_width
                    - (geometry.toolbar_columns - 1) as f32 * geometry.card_gap)
                    / geometry.toolbar_columns as f32;
                let chip_column = index % geometry.toolbar_columns;
                let chip_row = index / geometry.toolbar_columns;
                let focused = self.focus == index + 1;
                let active = self.library_filter == filter;
                let chip_height = 36.0;
                let mut chip = node(
                    &format!("library-filter-{index}"),
                    Role::Button,
                    "",
                    toolbar_left + chip_column as f32 * (chip_width + geometry.card_gap),
                    toolbar_top + chip_row as f32 * 68.0,
                    chip_width,
                    chip_height,
                    if focused {
                        "--state-focused-ring"
                    } else {
                        "--state-rest-surface"
                    },
                );
                chip.state.focused = focused;
                chip.state.selected = active;
                chip.action = Some(NodeAction::Activate);
                chip.children.push(node(
                    &format!("library-filter-{index}-label"),
                    Role::Text,
                    &label,
                    chip.bounds.x + 12.0,
                    chip.bounds.y + 5.0,
                    chip_width - 36.0,
                    26.0,
                    if focused {
                        "--color-text-inverse"
                    } else {
                        "--state-rest-text"
                    },
                ));
                if let Some(count) = count {
                    chip.children.push(node(
                        &format!("library-filter-{index}-count"),
                        Role::Text,
                        &count.to_string(),
                        chip.bounds.x + chip_width - 30.0,
                        chip.bounds.y + 5.0,
                        22.0,
                        26.0,
                        if focused {
                            "--color-text-inverse"
                        } else {
                            "--color-text-muted"
                        },
                    ));
                }
                out.push(chip);
                if active {
                    out.push(node(
                        &format!("library-selected-underline-{index}"),
                        Role::Group,
                        "",
                        toolbar_left + chip_column as f32 * (chip_width + geometry.card_gap) + 12.0,
                        toolbar_top + chip_row as f32 * 68.0 + chip_height - 3.0,
                        chip_width - 24.0,
                        3.0,
                        "--state-selected-accent",
                    ));
                }
            }
            let row_height = 292.0;
            let card_top = geometry.card_top;
            let mut visible_rows: usize = 1;
            while card_top + (visible_rows + 1) as f32 * row_height - 18.0
                <= h - PROMPTS_AREA_HEIGHT
            {
                visible_rows += 1;
            }
            let focused_row = self.focus.saturating_sub(5) / geometry.columns;
            let first_visible_row = focused_row.saturating_sub(visible_rows.saturating_sub(1));
            out.push(node(
                "library-grid-scroll",
                Role::Group,
                "Library covers",
                48.0,
                card_top,
                w - 96.0,
                h - card_top - PROMPTS_AREA_HEIGHT,
                "--color-surface-canvas",
            ));
            for (i, &item_index) in self.library_items.iter().enumerate() {
                let column = i % geometry.columns;
                let row = i / geometry.columns;
                if row < first_visible_row || row >= first_visible_row + visible_rows {
                    continue;
                }
                let item = &self.items[item_index];
                let availability = best_availability(item);
                let mut card = node(
                    &format!("library-item-{}", item.id),
                    Role::ListItem,
                    "",
                    geometry.card_left + column as f32 * (geometry.card_width + geometry.card_gap),
                    card_top + (row as f32 - first_visible_row as f32) * row_height,
                    geometry.card_width,
                    276.0,
                    state_token(availability, self.focus == i + 5),
                );
                card.state.focused = self.focus == i + 5;
                card.state.unavailable = !matches!(availability, Availability::Ready);
                card.elevation = Elevation::Elev1;
                card.action = Some(NodeAction::Activate);
                card.children = art_nodes(
                    item,
                    "library-card",
                    geometry.card_left + column as f32 * (geometry.card_width + geometry.card_gap),
                    card_top + 8.0 + (row as f32 - first_visible_row as f32) * row_height,
                    geometry.card_width,
                    136.0,
                    self.focus == i + 5,
                );
                add_unavailable_card_cues(
                    &mut card.children,
                    item,
                    availability,
                    "library-card",
                    geometry.card_left + column as f32 * (geometry.card_width + geometry.card_gap),
                    card_top + 8.0 + (row as f32 - first_visible_row as f32) * row_height,
                    geometry.card_width,
                );
                card.children
                    .retain(|child| !child.id.as_str().contains("-title-"));
                card.children.push(
                    node(
                        &format!("library-title-{}", item.id),
                        Role::Text,
                        &item.title,
                        card.bounds.x,
                        card.bounds.y + 154.0,
                        geometry.card_width,
                        34.0,
                        "--color-text-primary",
                    )
                    .with_type_role(TypeRole::Label),
                );
                out.push(card);
            }
            let total_rows = self.library_items.len().div_ceil(geometry.columns);
            if first_visible_row + visible_rows < total_rows {
                out.push(node(
                    "library-fold-fade",
                    Role::Group,
                    "More titles below",
                    48.0,
                    h - PROMPTS_AREA_HEIGHT - 32.0,
                    w - 96.0,
                    32.0,
                    "--color-surface-scrim",
                ));
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
            let first_variant = item.variants.first();
            let provenance = first_variant.map_or_else(
                || kind_text(&item.kind).to_owned(),
                |variant| {
                    format!(
                        "{} · {} · {}",
                        sentence_kind(&item.kind),
                        humanize_identifier(&variant.provenance.provider_id),
                        descriptor_file(variant)
                    )
                },
            );
            let compact = library_geometry(w).columns < 6;
            let cover_left = 48.0;
            let cover_top = 112.0;
            let cover_width = if compact { 240.0 } else { 320.0 };
            let cover_height = if compact { 321.0 } else { 428.0 };
            let detail_column_left = cover_left + cover_width + 32.0;
            let detail_column_width = w - detail_column_left - 48.0;
            out.push(node(
                "detail-provenance",
                Role::Text,
                &provenance,
                detail_column_left,
                112.0,
                detail_column_width,
                30.0,
                "--color-text-secondary",
            ));
            out.push(node(
                "detail-title",
                Role::Heading,
                &item.title,
                detail_column_left,
                148.0,
                detail_column_width,
                64.0,
                "--color-surface-canvas",
            ));
            let mut cover = node(
                "detail-cover",
                Role::Group,
                &format!("Cover for {}", item.title),
                cover_left,
                cover_top,
                cover_width,
                cover_height,
                "--color-surface-raised",
            )
            .with_elevation(Elevation::Elev2);
            cover.children = art_nodes(
                item,
                "detail-art",
                cover_left,
                cover_top,
                cover_width,
                cover_height,
                false,
            );
            out.push(cover);
            let detail_availability = best_availability(item);
            let availability = if matches!(detail_availability, Availability::Ready) {
                "● Ready · Installed on this device".to_owned()
            } else {
                format!(
                    "⊘ {}",
                    availability_text(detail_availability, &self.presentation)
                )
            };
            let mut availability_node = node(
                "detail-availability-reason",
                Role::Text,
                &availability,
                detail_column_left,
                218.0,
                detail_column_width,
                30.0,
                if matches!(detail_availability, Availability::Ready) {
                    "--color-text-primary"
                } else {
                    "--state-unavailable-text"
                },
            );
            availability_node.state.unavailable =
                !matches!(detail_availability, Availability::Ready);
            out.push(availability_node);
            let description = if item.tags.is_empty() {
                format!("{} is ready from the installed catalog.", item.title)
            } else {
                format!("{} · {}", sentence_kind(&item.kind), item.tags.join(" · "))
            };
            out.push(node(
                "detail-description",
                Role::Text,
                &description,
                detail_column_left,
                252.0,
                detail_column_width,
                42.0,
                "--color-text-secondary",
            ));
            out.push(
                node(
                    "detail-ways-heading",
                    Role::Heading,
                    "WAYS TO PLAY",
                    detail_column_left,
                    294.0,
                    detail_column_width,
                    28.0,
                    "--color-text-muted",
                )
                .with_type_role(TypeRole::Eyebrow),
            );
            let ready = self.ready_variants(item_index);
            let variant_row_height = 66.0;
            let variant_row_gap = 7.0;
            let variant_rows_top = 326.0;
            let detail_variant_capacity = 2;
            let visible_detail_variants = item.variants.len().min(detail_variant_capacity);
            if self.route == Route::Details {
                for (variant_index, variant) in item
                    .variants
                    .iter()
                    .take(visible_detail_variants)
                    .enumerate()
                {
                    let (variant_name, variant_sub) =
                        if matches!(variant.availability, Availability::Ready) {
                            (
                                "Installed on this device".to_owned(),
                                format!(
                                    "{}{} · works offline",
                                    variant.provenance.app_version.as_deref().map_or_else(
                                        || "Current version".to_owned(),
                                        |version| format!("Version {version}"),
                                    ),
                                    if variant.provenance.runtime_family.is_empty() {
                                        String::new()
                                    } else {
                                        format!(
                                            " · {}",
                                            humanize_identifier(&variant.provenance.runtime_family)
                                        )
                                    }
                                ),
                            )
                        } else {
                            (
                                format!("⊘ {}", humanize_identifier(&variant.id)),
                                availability_text(&variant.availability, &self.presentation),
                            )
                        };
                    let variant_focus = item
                        .variants
                        .iter()
                        .take(variant_index + 1)
                        .filter(|variant| matches!(variant.availability, Availability::Ready))
                        .count()
                        .checked_sub(1);
                    let focused = matches!(variant.availability, Availability::Ready)
                        && variant_focus == Some(self.focus);
                    let mut variant_node = node(
                        &format!("detail-variant-{variant_index}"),
                        if matches!(variant.availability, Availability::Ready) {
                            Role::Button
                        } else {
                            Role::Text
                        },
                        "",
                        detail_column_left,
                        variant_rows_top
                            + variant_index as f32 * (variant_row_height + variant_row_gap),
                        detail_column_width,
                        variant_row_height,
                        if focused {
                            "--state-focused-ring"
                        } else {
                            "--state-rest-surface"
                        },
                    );
                    variant_node.state.focused = focused;
                    variant_node.state.unavailable =
                        !matches!(variant.availability, Availability::Ready);
                    variant_node.state.selected = variant_index == 0;
                    let text_token = if focused {
                        "--color-text-inverse"
                    } else {
                        "--state-rest-text"
                    };
                    variant_node.children = vec![
                        node(
                            &format!("detail-variant-{variant_index}-name"),
                            Role::Text,
                            &variant_name,
                            detail_column_left + 16.0,
                            variant_node.bounds.y + 6.0,
                            detail_column_width - 32.0,
                            26.0,
                            text_token,
                        ),
                        node(
                            &format!("detail-variant-{variant_index}-sub"),
                            Role::Text,
                            &variant_sub,
                            detail_column_left + 16.0,
                            variant_node.bounds.y + 34.0,
                            detail_column_width - 32.0,
                            24.0,
                            if focused {
                                "--color-text-inverse"
                            } else {
                                "--color-text-secondary"
                            },
                        ),
                    ];
                    if matches!(variant.availability, Availability::Ready) {
                        variant_node.action = Some(NodeAction::Activate);
                    }
                    out.push(variant_node);
                }
                if item.variants.len() > visible_detail_variants {
                    out.push(node(
                        "detail-variant-fold",
                        Role::Text,
                        &format!(
                            "+{} more ways to play",
                            item.variants.len() - visible_detail_variants
                        ),
                        detail_column_left,
                        variant_rows_top
                            + visible_detail_variants as f32
                                * (variant_row_height + variant_row_gap),
                        detail_column_width,
                        28.0,
                        "--color-text-muted",
                    ));
                }
            }
            if self.route == Route::VariantChooser {
                // The chooser replaces the Details panel instead of painting another
                // interactive stack over it.
                out.retain(|node| !node.id.as_str().starts_with("detail-"));
                let chooser_top = 300.0;
                let chooser_row_height = 54.0;
                let chooser_row_gap = 10.0;
                let chooser_capacity = 5;
                let chooser_compact = library_geometry(w).columns < 6;
                let chooser_left = if chooser_compact { 48.0 } else { 360.0 };
                let chooser_width = if chooser_compact { w - 96.0 } else { w - 720.0 };
                let chooser_start = self
                    .focus
                    .saturating_add(1)
                    .saturating_sub(chooser_capacity)
                    .min(ready.len().saturating_sub(chooser_capacity));
                out.push(node(
                    "chooser-note",
                    Role::Text,
                    "Ready right now. Back leaves without opening anything.",
                    chooser_left,
                    235.0,
                    chooser_width,
                    40.0,
                    "--color-text-secondary",
                ));
                out.push(node(
                    "chooser-scroll-region",
                    Role::Group,
                    "",
                    chooser_left,
                    chooser_top,
                    chooser_width,
                    chooser_capacity as f32 * chooser_row_height
                        + chooser_capacity.saturating_sub(1) as f32 * chooser_row_gap,
                    "--color-surface-canvas",
                ));
                for (choice, &variant_index) in ready
                    .iter()
                    .enumerate()
                    .skip(chooser_start)
                    .take(chooser_capacity)
                {
                    let variant = &item.variants[variant_index];
                    let mut row = node(
                        &format!("chooser-{}", variant.id),
                        Role::Button,
                        &format!("{} · Ready", humanize_identifier(&variant.id)),
                        chooser_left,
                        chooser_top
                            + (choice - chooser_start) as f32
                                * (chooser_row_height + chooser_row_gap),
                        chooser_width,
                        chooser_row_height,
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
            } else {
                let actions_bottom = if let Some(_ready_variant) = ready.first() {
                    let variants_bottom = variant_rows_top
                        + visible_detail_variants as f32 * (variant_row_height + variant_row_gap)
                        + if item.variants.len() > visible_detail_variants {
                            35.0
                        } else {
                            0.0
                        };
                    let button_gap = 16.0;
                    let stack_buttons = compact && detail_column_width < 336.0;
                    let button_width = if stack_buttons {
                        detail_column_width
                    } else {
                        (detail_column_width - button_gap) / 2.0
                    };
                    let buttons_top = variants_bottom.max(430.0);
                    let play_focus = self.detail_play_focus().unwrap_or(0);
                    let mut open = node(
                        "detail-open",
                        Role::Button,
                        if ready.len() == 1 {
                            "▶ Play"
                        } else {
                            "Choose how to play"
                        },
                        detail_column_left,
                        buttons_top,
                        button_width,
                        54.0,
                        if self.focus == play_focus {
                            "--state-focused-ring"
                        } else {
                            "--state-rest-surface"
                        },
                    );
                    open.state.focused = self.focus == play_focus;
                    open.action = Some(NodeAction::Activate);
                    out.push(open);
                    let pin_label = if item.favorite {
                        "★ Unpin"
                    } else {
                        "★ Pin to favorites"
                    };
                    let mut pin = node(
                        "detail-pin",
                        Role::Button,
                        pin_label,
                        if stack_buttons {
                            detail_column_left
                        } else {
                            detail_column_left + button_width + button_gap
                        },
                        if stack_buttons {
                            buttons_top + 54.0 + button_gap
                        } else {
                            buttons_top
                        },
                        button_width,
                        54.0,
                        if self.focus == self.detail_pin_focus() {
                            "--state-focused-ring"
                        } else {
                            "--state-rest-surface"
                        },
                    );
                    pin.state.focused = self.focus == self.detail_pin_focus();
                    pin.action = Some(NodeAction::Activate);
                    out.push(pin);
                    buttons_top + if stack_buttons { 124.0 } else { 54.0 }
                } else {
                    let mut unavailable = node(
                        "detail-unavailable",
                        Role::Text,
                        "No launch action is available",
                        detail_column_left,
                        510.0,
                        detail_column_width,
                        60.0,
                        "--state-unavailable-text",
                    );
                    unavailable.state.unavailable = true;
                    out.push(unavailable);
                    let mut pin = node(
                        "detail-pin",
                        Role::Button,
                        if item.favorite {
                            "★ Unpin"
                        } else {
                            "★ Pin to favorites"
                        },
                        detail_column_left,
                        580.0,
                        detail_column_width,
                        54.0,
                        "--state-focused-ring",
                    );
                    pin.state.focused = true;
                    pin.action = Some(NodeAction::Activate);
                    out.push(pin);
                    634.0
                };
                if let Some(playtime) = self.playtime.get(&item.id).copied() {
                    let facts_top = if compact {
                        actions_bottom + 16.0
                    } else {
                        540.0
                    };
                    out.push(
                        node(
                            "detail-time-played-heading",
                            Role::Heading,
                            "TIME PLAYED",
                            detail_column_left,
                            facts_top,
                            detail_column_width,
                            22.0,
                            "--color-text-muted",
                        )
                        .with_type_role(TypeRole::Eyebrow),
                    );
                    out.push(node(
                        "detail-playtime",
                        Role::Text,
                        &format_playtime(playtime),
                        detail_column_left,
                        facts_top + 26.0,
                        detail_column_width,
                        28.0,
                        "--color-text-primary",
                    ));
                }
                let facts_height = 54.0;
                let facts_top = actions_bottom + 16.0;
                let footer_top = h - PROMPTS_AREA_HEIGHT;
                if let Some(variant) = ready
                    .first()
                    .map(|&variant_index| &item.variants[variant_index])
                    .filter(|_| !compact && facts_top + facts_height <= footer_top)
                {
                    let facts = [
                        (
                            "developer",
                            "DEVELOPER",
                            humanize_identifier(&variant.provenance.provider_id),
                        ),
                        (
                            "installed",
                            "INSTALLED",
                            variant.provenance.app_version.as_deref().map_or_else(
                                || "Current version".to_owned(),
                                |version| format!("Version {version}"),
                            ),
                        ),
                        ("offline", "WORKS OFFLINE", "Yes".to_owned()),
                    ];
                    for (column, (id, eyebrow, value)) in facts.into_iter().enumerate() {
                        let left = detail_column_left + column as f32 * detail_column_width / 3.0;
                        out.push(
                            node(
                                &format!("detail-fact-{id}-heading"),
                                Role::Heading,
                                eyebrow,
                                left,
                                facts_top,
                                detail_column_width / 3.0 - 8.0,
                                22.0,
                                "--color-text-muted",
                            )
                            .with_type_role(TypeRole::Eyebrow),
                        );
                        out.push(node(
                            &format!("detail-fact-{id}"),
                            Role::Text,
                            &value,
                            left,
                            facts_top + 26.0,
                            detail_column_width / 3.0 - 8.0,
                            28.0,
                            "--color-text-primary",
                        ));
                    }
                }
            }
        } else if self.route == Route::Settings {
            self.quiet_settings_nodes(out, w, h);
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

    fn quiet_settings_nodes(&self, out: &mut Vec<Node>, w: f32, h: f32) {
        if self.system_flow == SystemFlow::ManualTime
            || self.network_flow == NetworkFlow::Credential
        {
            self.settings_nodes(out, w);
            return;
        }
        let portrait = h > w;
        let nav_width = if w >= 960.0 { 260.0 } else { 220.0 };
        let rooms = self.settings_rooms();
        if !portrait || !self.settings_in_rows {
            let nav_left = 32.0;
            let nav_top = 168.0;
            for (index, room) in rooms.iter().copied().enumerate() {
                let selected = room == self.settings_room;
                let focused = !self.settings_in_rows && self.focus == index;
                let name = match room {
                    SettingsRoom::Accessibility => "Accessibility",
                    SettingsRoom::Controls => "Controls",
                    SettingsRoom::Display => "Display",
                    SettingsRoom::Network => "Network",
                    SettingsRoom::System => "System",
                };
                let mut nav = node(
                    &format!("settings-nav-{}", name.to_ascii_lowercase()),
                    Role::Button,
                    "",
                    nav_left,
                    nav_top + index as f32 * 62.0,
                    nav_width - 32.0,
                    50.0,
                    if focused {
                        "--state-focused-ring"
                    } else if selected {
                        "--state-selected-accent"
                    } else {
                        "--state-rest-surface"
                    },
                );
                nav.state.focused = focused;
                nav.state.selected = selected;
                nav.action = Some(NodeAction::Activate);
                nav.children.push(node(
                    &format!("settings-nav-{}-label", name.to_ascii_lowercase()),
                    Role::Text,
                    &format!("{} {name}", if selected { "▌" } else { " " }),
                    nav.bounds.x + 12.0,
                    nav.bounds.y + 10.0,
                    nav.bounds.width - 24.0,
                    30.0,
                    if focused || selected {
                        "--color-text-inverse"
                    } else {
                        "--state-rest-text"
                    },
                ));
                out.push(nav);
            }
            if portrait && !self.settings_in_rows {
                return;
            }
        }

        let content_left = if portrait { 32.0 } else { nav_width + 56.0 };
        let content_width = w - content_left - 40.0;
        let title = match self.settings_room {
            SettingsRoom::Accessibility => "Accessibility",
            SettingsRoom::Controls => "Controls",
            SettingsRoom::Display => "Display",
            SettingsRoom::Network => "Network status",
            SettingsRoom::System => "System",
        };
        out.push(node(
            "settings-section-title",
            Role::Heading,
            title,
            content_left,
            112.0,
            content_width,
            48.0,
            "--color-text-primary",
        ));
        if portrait {
            out.push(node(
                "settings-section-back",
                Role::Text,
                "‹ Back to sections",
                content_left,
                82.0,
                content_width,
                28.0,
                "--color-text-secondary",
            ));
        }

        let mut rows = self.settings_scene_rows();
        let scale = f32::from(self.text_scale) / 100.0;
        let row_height = 74.0 * scale;
        let row_gap = 12.0;
        let rows_top = 174.0;
        let rows_bottom = h - PROMPTS_AREA_HEIGHT - 16.0;
        let visible_rows = ((rows_bottom - rows_top + row_gap) / (row_height + row_gap))
            .floor()
            .max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let capacity = (visible_rows as usize).max(1);
        let first = self
            .focus
            .saturating_sub(capacity.saturating_sub(1))
            .min(rows.len().saturating_sub(capacity));
        out.push(node(
            "settings-rows-scroll-region",
            Role::Group,
            "Settings rows",
            content_left,
            rows_top,
            content_width,
            rows_bottom - rows_top,
            "--color-surface-canvas",
        ));
        for (index, row) in rows.drain(..).enumerate().skip(first).take(capacity) {
            let interactive = row.action.is_some();
            let focused = self.settings_row_focused && self.focus == index && interactive;
            let mut scene_row = node(
                &format!("settings-row-{}", row.id),
                if interactive {
                    Role::Button
                } else {
                    Role::Text
                },
                "",
                content_left,
                rows_top + (index - first) as f32 * (row_height + row_gap),
                content_width,
                row_height,
                if focused {
                    "--state-focused-ring"
                } else if interactive {
                    "--state-rest-surface"
                } else {
                    "--state-disabled-border"
                },
            );
            scene_row.state.focused = focused;
            scene_row.state.disabled = !interactive;
            if interactive {
                scene_row.action = Some(NodeAction::Activate);
            }
            let text_token = if focused {
                "--color-text-inverse"
            } else if interactive {
                "--state-rest-text"
            } else {
                "--color-text-secondary"
            };
            let lines = row.label.lines().collect::<Vec<_>>();
            for (line_index, line) in lines.iter().take(2).enumerate() {
                scene_row.children.push(node(
                    &format!("settings-row-{}-line-{line_index}", row.id),
                    Role::Text,
                    line,
                    content_left + 16.0,
                    scene_row.bounds.y + 7.0 + line_index as f32 * 25.0,
                    content_width - 150.0,
                    24.0,
                    text_token,
                ));
            }
            if row.id == "accessibility-textScale" {
                let selected_value = lines
                    .last()
                    .and_then(|line| line.rsplit_once(" · "))
                    .map_or("100%", |(_, effective)| effective);
                for (segment, value) in ["100%", "150%", "200%"].into_iter().enumerate() {
                    let selected = selected_value == value;
                    let x = content_left + content_width - 240.0 + segment as f32 * 72.0;
                    scene_row.children.push(node(
                        &format!("settings-text-scale-segment-{value}"),
                        Role::Group,
                        "",
                        x,
                        scene_row.bounds.y + 20.0,
                        68.0,
                        34.0,
                        if selected {
                            "--state-selected-accent"
                        } else {
                            "--color-surface-sunken"
                        },
                    ));
                    scene_row.children.push(node(
                        &format!("settings-text-scale-value-{value}"),
                        Role::Text,
                        value,
                        x + 6.0,
                        scene_row.bounds.y + 24.0,
                        56.0,
                        26.0,
                        if selected {
                            "--color-text-inverse"
                        } else {
                            "--color-text-primary"
                        },
                    ));
                }
            } else if row.id.starts_with("accessibility-")
                && lines
                    .last()
                    .is_some_and(|line| line.starts_with("ON") || line.starts_with("OFF"))
            {
                let on = lines.last().is_some_and(|line| line.starts_with("ON"));
                let control_left = content_left + content_width - 128.0;
                scene_row.children.push(node(
                    &format!("settings-toggle-{}-state", row.id),
                    Role::Text,
                    if on { "ON" } else { "OFF" },
                    control_left,
                    scene_row.bounds.y + 24.0,
                    38.0,
                    26.0,
                    text_token,
                ));
                scene_row.children.push(node(
                    &format!("settings-toggle-{}-track", row.id),
                    Role::Group,
                    "",
                    control_left + 44.0,
                    scene_row.bounds.y + 25.0,
                    58.0,
                    28.0,
                    if on {
                        "--state-selected-accent"
                    } else {
                        "--color-surface-sunken"
                    },
                ));
                scene_row.children.push(node(
                    &format!("settings-toggle-{}-knob", row.id),
                    Role::Group,
                    "",
                    control_left + if on { 78.0 } else { 48.0 },
                    scene_row.bounds.y + 29.0,
                    20.0,
                    20.0,
                    if on {
                        "--color-text-inverse"
                    } else {
                        "--color-text-primary"
                    },
                ));
            } else if let Some(control) = lines.get(2) {
                scene_row.children.push(node(
                    &format!("settings-row-{}-control", row.id),
                    Role::Text,
                    control,
                    content_left + content_width - 120.0,
                    scene_row.bounds.y + 24.0,
                    104.0,
                    26.0,
                    text_token,
                ));
            }
            out.push(scene_row);
        }
    }

    fn settings_nodes(&self, out: &mut Vec<Node>, w: f32) {
        if self.settings_room == SettingsRoom::System && self.system_flow == SystemFlow::ManualTime
        {
            out.push(node(
                "manual-time-title",
                Role::Heading,
                "SET MANUAL TIME · UTC",
                48.0,
                72.0,
                w - 96.0,
                54.0,
                "--state-rest-text",
            ));
            let mut fields = node(
                "manual-time-fields",
                Role::Button,
                &self.manual_time_picker.label(),
                48.0,
                180.0,
                w - 96.0,
                74.0,
                "--state-focused-ring",
            );
            fields.state.focused = true;
            fields.action = Some(NodeAction::Activate);
            out.push(fields);
            out.push(node(
                "manual-time-help",
                Role::Text,
                "← → field   ↑ ↓ value   Activate apply   Back cancel",
                48.0,
                280.0,
                w - 96.0,
                44.0,
                "--color-text-secondary",
            ));
            return;
        }
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
        let labels: Vec<(String, bool)> = match self.settings_room {
            SettingsRoom::Accessibility => self
                .display_preferences
                .iter()
                .map(|row| (Self::preference_label(row), row.interactive))
                .collect(),
            SettingsRoom::Display => self
                .display_preferences
                .iter()
                .map(|row| {
                    (
                        if row.interactive {
                            Self::preference_label(row)
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
            SettingsRoom::System => self
                .system_rows()
                .into_iter()
                .map(|row| match row {
                    SystemRow::TimeUnavailable => (
                        self.time_state
                            .as_ref()
                            .err()
                            .cloned()
                            .unwrap_or_else(|| "Time status unavailable".into()),
                        false,
                    ),
                    SystemRow::TransferUnavailable => (
                        self.transfer_services
                            .as_ref()
                            .err()
                            .cloned()
                            .unwrap_or_else(|| "Transfer unavailable".into()),
                        false,
                    ),
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
                    SystemRow::ManualTime => (
                        format!(
                            "Set time manually · {}",
                            ManualTimePicker::from_system_time(
                                self.time_state
                                    .as_ref()
                                    .expect("manual row requires time state")
                                    .wall_clock
                            )
                            .label()
                            .replace(['[', ']'], "")
                        ),
                        true,
                    ),
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
                    "--state-rest-surface"
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
                &Self::preference_label(row),
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
    art_height: f32,
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
    let favorite = context == "favorite-card";
    let detail = context == "detail-art";
    let kind_y = if home {
        y + 166.0
    } else if favorite {
        y + 8.0
    } else if detail {
        y + art_height - 68.0
    } else {
        y + 142.0
    };
    let label_y = if home {
        y + 210.0
    } else if favorite {
        y + 32.0
    } else if detail {
        y + art_height - 36.0
    } else {
        y + 176.0
    };
    let art_x = if favorite { x + 4.0 } else { x + 8.0 };
    let art_width = if favorite { 56.0 } else { width - 16.0 };
    let label_mask = node(
        // Card labels remain available to assistive consumers, but the current renderer
        // also paints them. Mask the full art region before painting the inset art so a
        // wrapped second line cannot remain visible in the art's eight-pixel gutters.
        &format!("{context}-label-mask-{id}"),
        Role::Group,
        "",
        if favorite { art_x } else { x },
        if favorite {
            y + 4.0
        } else if detail {
            y
        } else {
            y - 8.0
        },
        if favorite { art_width } else { width },
        if detail {
            art_height
        } else if home {
            166.0
        } else if favorite {
            64.0
        } else {
            144.0
        },
        token,
    );
    let mut nodes = vec![
        label_mask,
        node(
            &format!("{context}-art-{id}"),
            Role::Group,
            "",
            art_x,
            if favorite { y + 4.0 } else { y },
            art_width,
            if detail {
                art_height
            } else if home {
                158.0
            } else if favorite {
                64.0
            } else {
                136.0
            },
            token,
        ),
        node(
            &format!("{context}-motif-{id}"),
            Role::Text,
            motif,
            art_x,
            if favorite { y + 4.0 } else { y },
            art_width,
            if favorite { 28.0 } else { 60.0 },
            token,
        ),
        node(
            &format!("{context}-initial-{id}"),
            Role::Text,
            &monogram,
            if favorite {
                art_x + 16.0
            } else {
                x + width * 0.27
            },
            if favorite { y + 30.0 } else { y + 72.0 },
            if favorite { 24.0 } else { width * 0.46 },
            if favorite { 28.0 } else { 58.0 },
            token,
        )
        .with_type_role(TypeRole::Plate),
    ];
    if let Some(edition) = edition {
        nodes.push(
            node(
                &format!("{context}-plate-{id}"),
                Role::Text,
                edition,
                if favorite { x + 68.0 } else { x + 12.0 },
                kind_y,
                if favorite { width - 72.0 } else { width - 24.0 },
                if favorite { 20.0 } else { 24.0 },
                token,
            )
            .with_type_role(TypeRole::Eyebrow),
        );
    }
    nodes.push(
        node(
            &format!("{context}-title-{id}"),
            Role::Text,
            title,
            if favorite { x + 68.0 } else { x },
            label_y,
            if favorite { width - 72.0 } else { width },
            if favorite { 32.0 } else { 28.0 },
            if context == "home-card" {
                "--color-surface-canvas"
            } else if focused {
                "--state-focused-text"
            } else {
                "--color-text-secondary"
            },
        )
        .with_type_role(TypeRole::Label),
    );
    nodes
}

fn art_nodes(
    item: &Item,
    context: &str,
    x: f32,
    y: f32,
    width: f32,
    art_height: f32,
    focused: bool,
) -> Vec<Node> {
    if let Some(art) = item.art.as_ref().filter(|_| !item.art_failed) {
        let home = context == "home-card";
        let favorite = context == "favorite-card";
        let detail = context == "detail-art";
        let label_y = if home {
            y + 210.0
        } else if favorite {
            y + 32.0
        } else {
            y + 176.0
        };
        let mut image = node(
            &format!("{context}-art-{}", item.id),
            Role::Group,
            &format!("{} cover art", item.title),
            if favorite { x + 4.0 } else { x + 8.0 },
            if favorite { y + 4.0 } else { y },
            if favorite { 56.0 } else { width - 16.0 },
            art_height,
            "--state-rest-surface",
        );
        image = image.with_image(art.clone(), ImageFit::Cover);
        if detail {
            return vec![image];
        }
        return vec![
            image,
            node(
                &format!("{context}-title-{}", item.id),
                Role::Text,
                &item.title,
                if favorite { x + 68.0 } else { x },
                label_y,
                if favorite { width - 72.0 } else { width },
                if favorite { 32.0 } else { 28.0 },
                "--color-text-primary",
            )
            .with_type_role(TypeRole::Label),
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
        art_height,
        focused,
    )
}

fn add_unavailable_card_cues(
    nodes: &mut Vec<Node>,
    item: &Item,
    availability: &Availability,
    context: &str,
    x: f32,
    y: f32,
    width: f32,
) {
    if matches!(availability, Availability::Ready) {
        return;
    }
    let home = context == "home-card";
    let art_height = if home { 158.0 } else { 136.0 };
    let badge = match availability {
        Availability::NeedsNetwork { .. } => "Network",
        Availability::NeedsSetup { .. } => "Setup",
        Availability::IncompatibleRuntime { .. } => "Update",
        Availability::UnsupportedCapability { .. } => "Unavailable",
        Availability::Ready => return,
    };
    nodes.push(node(
        &format!("{context}-veil-{}", item.id),
        Role::Group,
        "",
        x + 8.0,
        y,
        width - 16.0,
        art_height,
        "--state-unavailable-veil",
    ));
    nodes.push(
        node(
            &format!("{context}-badge-{}", item.id),
            Role::Text,
            &format!("⊘ {badge}"),
            x + width - 98.0,
            y + 10.0,
            84.0,
            28.0,
            "--color-surface-scrim",
        )
        .with_type_role(TypeRole::Caption),
    );
    nodes.push(
        node(
            &format!("{context}-reason-{}", item.id),
            Role::Text,
            &format!(
                "⊘ {}",
                availability_text(availability, &Presentation::Ready)
            ),
            x,
            if home { y + 238.0 } else { y + 204.0 },
            width,
            28.0,
            "--color-text-muted",
        )
        .with_type_role(TypeRole::Caption),
    );
}

#[cfg(test)]
fn home_row_vertical_extent(row: &[Node]) -> (f32, f32) {
    fn node_vertical_extent(node: &Node) -> (f32, f32) {
        node.children.iter().fold(
            (node.bounds.y, node.bounds.y + node.bounds.height),
            |(top, bottom), child| {
                let (child_top, child_bottom) = node_vertical_extent(child);
                (top.min(child_top), bottom.max(child_bottom))
            },
        )
    }

    row.iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(top, bottom), node| {
            let (node_top, node_bottom) = node_vertical_extent(node);
            (top.min(node_top), bottom.max(node_bottom))
        })
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
    static NO_VARIANTS: OnceLock<Availability> = OnceLock::new();

    item.variants
        .iter()
        .find(|variant| matches!(variant.availability, Availability::Ready))
        .or_else(|| item.variants.first())
        .map_or_else(
            || {
                eprintln!(
                    "pf-shell: catalog item {} has no variants; rendering it unavailable",
                    item.id
                );
                NO_VARIANTS.get_or_init(|| Availability::UnsupportedCapability {
                    capability: "catalog item has no variants".into(),
                })
            },
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
fn sentence_kind(kind: &AppKind) -> &'static str {
    match kind {
        AppKind::Media => "Media",
        AppKind::Stream => "Stream",
        AppKind::Game => "Game",
        AppKind::System => "Tool",
        AppKind::Settings => "Settings",
    }
}
fn humanize_identifier(value: &str) -> String {
    let words = value
        .split(['-', '_', '.', '/'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}
fn descriptor_file(variant: &Variant) -> String {
    variant
        .launch_target
        .descriptor_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("App descriptor")
        .to_owned()
}
fn state_token(a: &Availability, focused: bool) -> &'static str {
    if focused {
        "--state-focused-ring"
    } else {
        match a {
            Availability::NeedsNetwork { .. } | Availability::NeedsSetup { .. } => {
                "--color-status-attention"
            }
            Availability::Ready
            | Availability::UnsupportedCapability { .. }
            | Availability::IncompatibleRuntime { .. } => "--state-rest-surface",
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

fn focused_node_id(node: &Node) -> Option<&Node> {
    node.state
        .focused
        .then_some(node)
        .or_else(|| node.children.iter().find_map(focused_node_id))
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
        c.action(&ShellAction::Move(AxisMove::Right));
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
        assert!(format!("{:?}", settings_scene(&c)).contains("settings-text-scale-value-150%"));

        let mut unsupported = core();
        unsupported
            .load_preferences(&preferences(false), true)
            .unwrap();
        unsupported.go(Route::Settings);
        unsupported.action(&ShellAction::Move(AxisMove::Right));
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
        assert!(format!("{scene:?}").contains("cannot apply"));
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
    fn controls_and_licenses_are_honest_disabled_rows_for_any_binding_count() {
        for bindings in [
            vec![ControlBinding {
                context: "global".into(),
                action: "Activate".into(),
                label: "Activate".into(),
                binding: "A".into(),
            }],
            vec![
                ControlBinding {
                    context: "global".into(),
                    action: "Activate".into(),
                    label: "Activate".into(),
                    binding: "A".into(),
                },
                ControlBinding {
                    context: "global".into(),
                    action: "Back".into(),
                    label: "Back".into(),
                    binding: "B".into(),
                },
            ],
        ] {
            let mut core = core();
            core.set_control_bindings(bindings);
            core.go(Route::Settings);
            core.settings_room = SettingsRoom::Controls;
            core.settings_in_rows = false;

            assert_eq!(core.action(&ShellAction::Activate), None);
            assert!(core.settings_in_rows, "disabled sections still open");
            assert!(
                !core.settings_row_focused,
                "disabled rows cannot receive focus"
            );

            let scene = format!("{:?}", settings_scene(&core));
            assert!(scene.contains("Current map:"));
            assert!(scene.contains("settings-row-controls-remap"));
            assert!(scene.contains("settings-row-controls-safe-return"));
            assert!(scene.contains("--state-disabled-border"));
            for focus in 0..core.settings_scene_rows().len() {
                core.focus = focus;
                assert_eq!(core.action(&ShellAction::Activate), None);
            }
            assert_eq!(core.controls_flow, ControlsFlow::Rows);
        }

        let mut core = core();
        core.go(Route::Settings);
        core.settings_room = SettingsRoom::System;
        core.settings_in_rows = false;
        assert_eq!(core.action(&ShellAction::Activate), None);
        assert!(core.settings_in_rows, "System still opens without Recovery");
        assert!(!core.settings_row_focused, "Licenses cannot receive focus");
        core.focus = 2;
        let scene = format!("{:?}", settings_scene(&core));
        assert!(scene.contains("settings-row-system-licenses"));
        assert!(scene.contains("Licenses"));
        assert!(scene.contains("Open-source notices"));
        assert_eq!(core.action(&ShellAction::Activate), None);
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

    fn portrait_settings_scene(core: &ShellCore) -> Scene {
        core.scene(
            SurfaceMetrics {
                logical_width: 480.,
                logical_height: 800.,
                scale: 1.,
                safe_insets: Default::default(),
                orientation: pf_scene::Orientation::Portrait,
            },
            "A Open · B Back",
        )
        .unwrap()
    }

    #[test]
    fn settings_section_visibility_is_independent_of_row_focus() {
        let mut portrait = core();
        portrait.go(Route::Settings);
        portrait.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(portrait.settings_room, SettingsRoom::Controls);

        portrait.action(&ShellAction::Activate);
        assert!(portrait.settings_in_rows);
        assert!(!portrait.settings_row_focused);
        let section = format!("{:?}", portrait_settings_scene(&portrait));
        assert!(section.contains("settings-row-controls-remap"));
        assert!(section.contains("settings-row-controls-source"));
        assert!(!section.contains("focused: true"));
        assert_eq!(portrait.action(&ShellAction::Activate), None);

        portrait.action(&ShellAction::Back);
        assert!(!portrait.settings_in_rows);
        assert_eq!(portrait.focus, 1, "section position is held on Back");
        let nav = format!("{:?}", portrait_settings_scene(&portrait));
        assert!(nav.contains("settings-nav-controls"));
        assert!(!nav.contains("settings-row-controls-remap"));

        let mut wide = core();
        wide.go(Route::Settings);
        wide.action(&ShellAction::Move(AxisMove::Down));
        assert!(!wide.settings_in_rows);
        let wide_scene = format!("{:?}", settings_scene(&wide));
        assert!(wide_scene.contains("settings-row-controls-remap"));
        assert!(wide_scene.contains("settings-row-controls-source"));

        let mut focusable = core();
        focusable
            .load_preferences(&preferences(true), true)
            .unwrap();
        focusable.go(Route::Settings);
        focusable.action(&ShellAction::Activate);
        assert!(focusable.settings_in_rows);
        assert!(focusable.settings_row_focused);
        assert_eq!(focusable.focus, 0);
    }

    #[test]
    #[ignore = "superseded by Settings §4.6 observation-only Network status"]
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
    #[ignore = "superseded by Settings §4.6 System About surface"]
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
    #[ignore = "manual time controls are outside Settings §4.6 v1"]
    fn manual_time_picker_navigates_wraps_clamps_composes_and_cancels() {
        let (_, mut time, transfer) = device_ports(NtpState::Inactive);
        let mut core = core();
        core.load_system(&time, &transfer);
        assert_eq!(
            core.time_state.as_ref().unwrap().wall_clock,
            SystemTime::UNIX_EPOCH
        );
        core.go(Route::Settings);
        core.settings_room = SettingsRoom::System;
        core.focus = 2;

        let fresh_wall_clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_210_040);
        time.state_result = Ok(pf_ports::TimeState {
            wall_clock: fresh_wall_clock,
            timezone: "UTC".into(),
            ntp_state: NtpState::Inactive,
        });
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::RefreshManualTime)
        );
        core.manual_time_refresh_result(time.read());
        assert_eq!(core.system_flow, SystemFlow::ManualTime);
        assert!(format!("{:?}", settings_scene(&core)).contains("manual-time-fields"));
        assert_eq!(
            core.manual_time_picker,
            ManualTimePicker::from_system_time(fresh_wall_clock)
        );

        core.manual_time_picker = ManualTimePicker {
            year: 2024,
            month: 2,
            day: 29,
            hour: 23,
            minute: 59,
            field: 0,
        };
        core.action(&ShellAction::Move(AxisMove::Right));
        core.action(&ShellAction::Move(AxisMove::Right));
        assert_eq!(core.manual_time_picker.field, 2);
        core.action(&ShellAction::Move(AxisMove::Left));
        assert_eq!(core.manual_time_picker.field, 1);

        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            (core.manual_time_picker.month, core.manual_time_picker.day),
            (1, 29)
        );
        core.manual_time_picker.month = 1;
        core.manual_time_picker.day = 31;
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(
            (core.manual_time_picker.month, core.manual_time_picker.day),
            (2, 29)
        );
        core.manual_time_picker.field = 0;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            (core.manual_time_picker.year, core.manual_time_picker.day),
            (2023, 28)
        );

        core.manual_time_picker = ManualTimePicker {
            year: 2024,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            field: 1,
        };
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.manual_time_picker.month, 12);
        core.manual_time_picker.field = 3;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.manual_time_picker.hour, 23);
        core.manual_time_picker.field = 4;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.manual_time_picker.minute, 59);
        core.manual_time_picker.field = 2;
        core.manual_time_picker.day = 1;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.manual_time_picker.day, 31);
        core.manual_time_picker.field = 0;
        core.manual_time_picker.year = 1970;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.manual_time_picker.year, 9999);

        core.manual_time_picker = ManualTimePicker {
            year: 2024,
            month: 2,
            day: 29,
            hour: 12,
            minute: 34,
            field: 4,
        };
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::SetManualTime(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_210_040)
            ))
        );
        assert_eq!(core.system_flow, SystemFlow::Rows);

        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::RefreshManualTime)
        );
        core.manual_time_refresh_result(time.read());
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(core.action(&ShellAction::Back), None);
        assert_eq!(core.system_flow, SystemFlow::Rows);
        assert_eq!(core.focus, 2);
        assert!(format!("{:?}", settings_scene(&core)).contains("Set time manually"));
        assert!(!format!("{:?}", settings_scene(&core)).contains("manual-time-fields"));
    }

    #[test]
    #[ignore = "time and transfer apply status moved outside Settings §4.6 v1"]
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
        assert_eq!(c.text_scale(), 200);
        assert_eq!(c.theme_base(), Base::HighContrast);
        assert!(c.reduced_motion());
        assert!(c.reduce_flashing());
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
    fn first_run_rows_show_current_values_and_redraw_after_toggle() {
        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        core.go(Route::Settings);
        let before = format!("{:?}", settings_scene(&core));
        assert!(before.contains("Reduce motion"));
        assert!(before.contains("settings-toggle-accessibility-reduceMotion-state"));
        assert!(before.contains("accessible_label: \"OFF\""));

        core.preference_changed(&EffectivePreference {
            key: PreferenceKey("reduceMotion".into()),
            effective: PreferenceValue::Bool(true),
            stored: PreferenceValue::Bool(true),
            applied: true,
        });
        let after = format!("{:?}", settings_scene(&core));
        assert!(after.contains("Reduce motion"));
        assert!(after.contains("accessible_label: \"ON\""));
    }

    #[test]
    fn accessibility_rows_distinguish_applied_and_stored_only_values() {
        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        core.go(Route::Settings);
        let scene = format!("{:?}", settings_scene(&core));
        assert!(scene.contains("High contrast"));
        assert!(scene.contains("settings-toggle-accessibility-highContrast-state"));
        assert!(scene.contains("Reduce motion"));
        assert!(scene.contains("Reduce flashing"));
        assert!(scene.contains("settings-text-scale-value-100%"));
    }

    #[test]
    fn text_scale_control_selects_exactly_one_effective_segment() {
        fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            (node.id.as_str() == id)
                .then_some(node)
                .or_else(|| node.children.iter().find_map(|child| find(child, id)))
        }

        for effective in ["100%", "150%", "200%"] {
            let mut core = core();
            core.load_preferences(&preferences(true), true).unwrap();
            core.go(Route::Settings);
            core.preference_changed(&EffectivePreference {
                key: PreferenceKey("textScale".into()),
                effective: PreferenceValue::Text(effective.into()),
                stored: PreferenceValue::Text(effective.into()),
                applied: true,
            });

            let scene = settings_scene(&core);
            let selected_segments = ["100%", "150%", "200%"]
                .into_iter()
                .filter(|value| {
                    find(
                        scene.root(),
                        &format!("settings-text-scale-segment-{value}"),
                    )
                    .is_some_and(|node| node.style_token == "--state-selected-accent")
                })
                .collect::<Vec<_>>();
            let inverse_labels = ["100%", "150%", "200%"]
                .into_iter()
                .filter(|value| {
                    find(scene.root(), &format!("settings-text-scale-value-{value}"))
                        .is_some_and(|node| node.style_token == "--color-text-inverse")
                })
                .collect::<Vec<_>>();

            assert_eq!(selected_segments, [effective]);
            assert_eq!(inverse_labels, [effective]);
        }
    }

    #[test]
    fn settings_rows_reserve_scaled_text_height_at_two_hundred_percent() {
        fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            (node.id.as_str() == id)
                .then_some(node)
                .or_else(|| node.children.iter().find_map(|child| find(child, id)))
        }

        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        core.go(Route::Settings);
        core.preference_changed(&EffectivePreference {
            key: PreferenceKey("textScale".into()),
            effective: PreferenceValue::Text("200%".into()),
            stored: PreferenceValue::Text("200%".into()),
            applied: true,
        });

        let scene = settings_scene(&core);
        let row = find(scene.root(), "settings-row-accessibility-textScale").unwrap();
        assert!((row.bounds.height - 148.0).abs() < f32::EPSILON);
    }

    #[test]
    fn quiet_settings_navigation_cues_filtering_and_scroll_window_conform() {
        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        core.go(Route::Settings);
        let nav = format!("{:?}", settings_scene(&core));
        assert!(nav.contains("settings-nav-accessibility"));
        assert!(nav.contains("settings-nav-controls"));
        assert!(nav.contains("settings-nav-display"));
        assert!(!nav.contains("settings-nav-network"));
        assert_eq!(nav.matches("settings-rows-scroll-region").count(), 1);

        core.action(&ShellAction::Move(AxisMove::Right));
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus(), 1);
        core.action(&ShellAction::Move(AxisMove::Left));
        assert!(!core.settings_in_rows);
        core.action(&ShellAction::Move(AxisMove::Right));
        assert_eq!(
            core.focus(),
            1,
            "row position is held when returning from nav"
        );

        core.preference_changed(&EffectivePreference {
            key: PreferenceKey("textScale".into()),
            effective: PreferenceValue::Text("200%".into()),
            stored: PreferenceValue::Text("200%".into()),
            applied: true,
        });
        core.focus = 0;
        let top = format!("{:?}", settings_scene(&core));
        assert!(top.contains("settings-row-accessibility-textScale"));
        assert!(!top.contains("settings-row-accessibility-diagnostic"));
        core.focus = core.focus_count() - 1;
        let bottom = format!("{:?}", settings_scene(&core));
        assert!(bottom.contains("settings-row-accessibility-diagnostic"));
        assert!(!bottom.contains("settings-row-accessibility-textScale"));
        assert!(bottom.contains("--state-disabled-border"));
        assert!(bottom.contains('—'));

        let compact = core
            .scene(
                SurfaceMetrics {
                    logical_width: 640.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "A Open · B Back",
            )
            .unwrap();
        for row in compact
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("settings-row-"))
        {
            assert!(row.bounds.x + row.bounds.width <= 640.0);
            assert!(row.bounds.y + row.bounds.height <= 660.0);
        }
    }

    #[test]
    fn quiet_settings_toggle_and_appearance_have_redundant_real_state() {
        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        core.go(Route::Settings);
        core.action(&ShellAction::Move(AxisMove::Right));
        let off = format!("{:?}", settings_scene(&core));
        assert!(off.contains("accessible_label: \"OFF\""));
        core.focus = 1;
        let Some(Effect::ChangePreference(change)) = core.action(&ShellAction::Activate) else {
            panic!("high contrast toggle")
        };
        core.preference_changed(&EffectivePreference {
            key: change.key,
            effective: change.value.clone(),
            stored: change.value,
            applied: true,
        });
        assert!(format!("{:?}", settings_scene(&core)).contains("accessible_label: \"ON\""));

        core.settings_room = SettingsRoom::Display;
        core.focus = 0;
        let Some(Effect::ChangePreference(change)) = core.action(&ShellAction::Activate) else {
            panic!("appearance segment")
        };
        core.preference_changed(&EffectivePreference {
            key: change.key,
            effective: change.value.clone(),
            stored: change.value,
            applied: true,
        });
        assert_eq!(
            core.theme_base(),
            Base::HighContrast,
            "high contrast composes over Day"
        );
        core.preference_changed(&EffectivePreference {
            key: PreferenceKey("highContrast".into()),
            effective: PreferenceValue::Bool(false),
            stored: PreferenceValue::Bool(false),
            applied: true,
        });
        assert_eq!(core.theme_base(), Base::Day);
    }

    #[test]
    fn high_contrast_selects_theme_base() {
        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        assert_eq!(core.theme_base(), Base::Dusk);

        core.preference_changed(&EffectivePreference {
            key: PreferenceKey("highContrast".into()),
            effective: PreferenceValue::Bool(true),
            stored: PreferenceValue::Bool(true),
            applied: true,
        });
        assert_eq!(core.theme_base(), Base::HighContrast);
        assert_ne!(
            pf_theme::flagship()
                .resolve(Base::Dusk, "--color-surface-canvas")
                .unwrap(),
            pf_theme::flagship()
                .resolve(Base::HighContrast, "--color-surface-canvas")
                .unwrap()
        );
    }

    #[test]
    fn reduced_motion_is_preference_or_environment_override() {
        let mut env_override = ShellCore::boot(&snapshot(), &pf_theme::flagship(), true);
        env_override
            .load_preferences(&preferences(true), true)
            .unwrap();
        assert!(env_override.reduced_motion());
        assert_eq!(env_override.motion_duration_ms(), 0);

        let mut preference = core();
        preference
            .load_preferences(&preferences(true), true)
            .unwrap();
        assert!(!preference.reduced_motion());
        preference.preference_changed(&EffectivePreference {
            key: PreferenceKey("reduceMotion".into()),
            effective: PreferenceValue::Bool(true),
            stored: PreferenceValue::Bool(true),
            applied: true,
        });
        assert!(preference.reduced_motion());
        assert_eq!(preference.motion_duration_ms(), 0);
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
    fn details_back_restores_library_items_in_route_local_space() {
        for (focus, expected_item) in [(5, 0), (6, 1)] {
            let mut c = core();
            c.go(Route::Library);
            c.focus = focus;
            c.action(&ShellAction::Activate);
            assert_eq!(
                (c.route(), c.selected_item),
                (Route::Details, Some(expected_item))
            );

            c.action(&ShellAction::Back);
            assert_eq!((c.route(), c.focus()), (Route::Library, focus));
        }
    }

    #[test]
    fn details_back_restores_item_in_current_search_results() {
        let mut c = core();
        c.go(Route::Search);
        c.search_results = vec![1, 0];
        c.focus = 0;
        c.action(&ShellAction::Activate);
        assert_eq!((c.route(), c.selected_item), (Route::Details, Some(1)));

        c.action(&ShellAction::Back);
        assert_eq!((c.route(), c.focus()), (Route::Search, 0));
        assert_eq!(c.focused_item_index(), Some(1));
    }

    #[test]
    fn details_back_clamps_to_search_item_when_result_vanished() {
        let mut c = core();
        c.go(Route::Search);
        c.focus = 1;
        c.action(&ShellAction::Activate);
        assert_eq!((c.route(), c.selected_item), (Route::Details, Some(1)));
        c.search_results = vec![0];

        c.action(&ShellAction::Back);
        assert_eq!((c.route(), c.focus()), (Route::Search, 0));
        assert_eq!(c.focused_item_index(), Some(0));
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
        c.settings_room = SettingsRoom::System;
        assert_eq!(c.focus_count(), 3);
        c.authority_snapshot(true);
        assert_eq!(c.focus_count(), 4);
        c.enter_settings_rows();
        assert!(c.settings_row_focused);
        assert_eq!(c.focus, 3);
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
    fn library_grid_navigation_is_row_major_with_hard_edges() {
        let items = (0..12)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);
        core.focus = 5;
        core.action(&ShellAction::Move(AxisMove::Left));
        assert_eq!(core.focus, 5, "left edge is hard");
        core.action(&ShellAction::Move(AxisMove::Right));
        assert_eq!(core.focus, 6);
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus, 12);
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(core.focus, 6);
        core.focus = 10;
        core.action(&ShellAction::Move(AxisMove::Right));
        assert_eq!(core.focus, 10, "right edge is hard");
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(core.focus, 4, "top row returns to the nearest chip");
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus, 8, "chip enters the nearest grid column");
    }

    #[test]
    fn library_grid_navigation_uses_rendered_breakpoint_geometry() {
        let items = (0..18)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);

        for (width, columns) in [(640.0, 3), (800.0, 4), (1280.0, 6)] {
            core.scene(
                SurfaceMetrics {
                    logical_width: width,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();

            core.focus = 5 + columns - 1;
            core.action(&ShellAction::Move(AxisMove::Right));
            assert_eq!(core.focus, 5 + columns - 1, "right edge at {width}px");

            core.focus = 6;
            core.action(&ShellAction::Move(AxisMove::Right));
            core.action(&ShellAction::Move(AxisMove::Left));
            assert_eq!(core.focus, 6, "left/right inverse at {width}px");

            core.action(&ShellAction::Move(AxisMove::Down));
            assert_eq!(core.focus, 6 + columns, "down at {width}px");
            core.action(&ShellAction::Move(AxisMove::Up));
            assert_eq!(core.focus, 6, "up/down inverse at {width}px");
        }
    }

    #[test]
    fn library_toolbar_stays_inside_each_breakpoint_viewport() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.go(Route::Library);

        for width in [640.0, 800.0, 1280.0] {
            let scene = core
                .scene(
                    SurfaceMetrics {
                        logical_width: width,
                        logical_height: 720.0,
                        scale: 1.0,
                        safe_insets: Default::default(),
                        orientation: pf_scene::Orientation::Landscape,
                    },
                    "",
                )
                .unwrap();
            let chips = scene
                .root()
                .children
                .iter()
                .filter(|node| node.id.as_str().starts_with("library-filter-"))
                .collect::<Vec<_>>();
            assert_eq!(chips.len(), 4);
            for chip in chips {
                assert!(
                    chip.bounds.x >= 0.0,
                    "{} starts off {width}px",
                    chip.id.as_str()
                );
                assert!(
                    chip.bounds.x + chip.bounds.width <= width,
                    "{} ends off {width}px",
                    chip.id.as_str()
                );
            }
        }
    }

    #[test]
    fn library_wrapped_toolbar_navigation_matches_rendered_rows_and_columns() {
        let items = (0..6)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);
        core.scene(
            SurfaceMetrics {
                logical_width: 640.0,
                logical_height: 720.0,
                scale: 1.0,
                safe_insets: Default::default(),
                orientation: pf_scene::Orientation::Landscape,
            },
            "",
        )
        .unwrap();

        core.focus = 1;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            core.focus, 3,
            "first chip moves to the chip rendered below it"
        );
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(core.focus, 1);
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(core.focus, 0, "top toolbar row returns to search");

        core.focus = 2;
        core.action(&ShellAction::Move(AxisMove::Right));
        assert_eq!(
            core.focus, 2,
            "right edge of the first rendered row is hard"
        );
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus, 4);
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus, 7, "right chip enters the rightmost grid column");
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(
            core.focus, 4,
            "right grid column returns to the lower-right chip"
        );

        core.focus = 3;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus, 5, "left chip enters the leftmost grid column");
        core.action(&ShellAction::Move(AxisMove::Up));
        assert_eq!(
            core.focus, 3,
            "left grid column returns to the lower-left chip"
        );
    }

    #[test]
    fn library_fold_only_appears_when_content_exceeds_visible_capacity() {
        let scene_for_count = |count| {
            let items = (0..count)
                .map(|index| {
                    item(
                        &format!("item-{index}"),
                        &format!("Item {index}"),
                        vec![variant(
                            "native",
                            &format!("app-{index}"),
                            Availability::Ready,
                        )],
                    )
                })
                .collect();
            let mut core = fixture_core(items);
            core.go(Route::Library);
            core.scene(
                SurfaceMetrics {
                    logical_width: 640.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap()
        };
        let has_fold = |scene: &Scene| {
            scene
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "library-fold-fade")
        };

        assert!(!has_fold(&scene_for_count(3)));
        assert!(has_fold(&scene_for_count(4)));

        let items = (0..4)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);
        core.scene(
            SurfaceMetrics {
                logical_width: 640.0,
                logical_height: 720.0,
                scale: 1.0,
                safe_insets: Default::default(),
                orientation: pf_scene::Orientation::Landscape,
            },
            "",
        )
        .unwrap();
        core.focus = 5;
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus, 8, "content below the fold is navigable");
        let scrolled = core
            .scene(
                SurfaceMetrics {
                    logical_width: 640.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        let focused = scrolled
            .root()
            .children
            .iter()
            .find(|node| node.state.focused)
            .expect("focused item below the initial fold");
        assert_eq!(focused.id.as_str(), "library-item-item-3");
        assert!((focused.bounds.y - library_geometry(640.0).card_top).abs() < f32::EPSILON);
        assert!(
            !has_fold(&scrolled),
            "fold hides when the final row is fully visible"
        );

        core.action(&ShellAction::Move(AxisMove::Up));
        let scrolled_back = core
            .scene(
                SurfaceMetrics {
                    logical_width: 640.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        assert_eq!(core.focus, 5);
        assert!(
            has_fold(&scrolled_back),
            "fold reappears when scrolling above the final row"
        );
    }

    #[test]
    fn library_filter_counts_match_the_catalog_snapshot() {
        let mut tool = item(
            "tool",
            "Button Tester",
            vec![variant("native", "tool", Availability::Ready)],
        );
        tool.kind = AppKind::System;
        let mut core = fixture_core(vec![
            item(
                "game-a",
                "Game A",
                vec![variant("native", "game-a", Availability::Ready)],
            ),
            item(
                "game-b",
                "Game B",
                vec![variant("native", "game-b", Availability::Ready)],
            ),
            tool,
        ]);
        core.go(Route::Library);
        let scene = core
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
        let debug = format!("{scene:?}");
        for label in ["Recent", "A–Z", "Games", "Everything else"] {
            assert!(debug.contains(&format!("accessible_label: \"{label}\"")));
        }
        assert!(debug.contains("library-filter-2-count"));
        assert!(debug.contains("library-filter-3-count"));
    }

    #[test]
    fn library_recent_orders_by_latest_use_and_trails_unplayed_titles() {
        let mut core = fixture_core(vec![
            item(
                "unplayed-first",
                "Unplayed First",
                vec![variant("native", "unplayed-first", Availability::Ready)],
            ),
            item(
                "older",
                "Older",
                vec![variant("native", "older", Availability::Ready)],
            ),
            item(
                "newest",
                "Newest",
                vec![variant("native", "newest", Availability::Ready)],
            ),
            item(
                "unplayed-last",
                "Unplayed Last",
                vec![variant("native", "unplayed-last", Availability::Ready)],
            ),
        ]);
        let epoch = SystemTime::UNIX_EPOCH;

        core.load_history(&[
            history_entry(
                "newest",
                Some(epoch + Duration::from_secs(100)),
                Some((epoch + Duration::from_secs(200), EndPrecision::Observed)),
            ),
            history_entry(
                "older",
                Some(epoch + Duration::from_secs(300)),
                Some((epoch + Duration::from_secs(400), EndPrecision::Observed)),
            ),
            history_entry("newest", Some(epoch + Duration::from_secs(500)), None),
        ]);

        assert_eq!(core.library_items, vec![2, 1, 0, 3]);
    }

    #[test]
    fn library_recent_maps_non_default_variant_history_to_catalog_item() {
        let mut core = fixture_core(vec![
            item(
                "multi-title",
                "Multi Title",
                vec![
                    variant("default", "multi-default", Availability::Ready),
                    variant("alternate", "multi-alternate", Availability::Ready),
                ],
            ),
            item(
                "single-title",
                "Single Title",
                vec![variant("default", "single-title", Availability::Ready)],
            ),
            item(
                "unplayed-title",
                "Unplayed Title",
                vec![variant("default", "unplayed-title", Availability::Ready)],
            ),
        ]);
        let epoch = SystemTime::UNIX_EPOCH;

        core.load_history(&[
            history_entry(
                "multi-alternate",
                Some(epoch + Duration::from_secs(200)),
                None,
            ),
            history_entry("single-title", Some(epoch + Duration::from_secs(100)), None),
        ]);

        assert_eq!(core.library_items, vec![0, 1, 2]);
    }

    #[test]
    fn library_recent_uses_newest_timestamp_across_variants() {
        let mut core = fixture_core(vec![
            item(
                "comparison-title",
                "Comparison Title",
                vec![variant("default", "comparison", Availability::Ready)],
            ),
            item(
                "multi-title",
                "Multi Title",
                vec![
                    variant("default", "multi-default", Availability::Ready),
                    variant("alternate", "multi-alternate", Availability::Ready),
                ],
            ),
        ]);
        let epoch = SystemTime::UNIX_EPOCH;

        core.load_history(&[
            history_entry(
                "multi-default",
                Some(epoch + Duration::from_secs(100)),
                None,
            ),
            history_entry("comparison", Some(epoch + Duration::from_secs(200)), None),
            history_entry(
                "multi-alternate",
                Some(epoch + Duration::from_secs(300)),
                None,
            ),
        ]);

        assert_eq!(core.library_items, vec![1, 0]);
    }

    #[test]
    fn library_recent_empty_history_keeps_catalog_order() {
        let mut core = fixture_core(vec![
            item(
                "third",
                "Third",
                vec![variant("native", "third", Availability::Ready)],
            ),
            item(
                "first",
                "First",
                vec![variant("native", "first", Availability::Ready)],
            ),
            item(
                "second",
                "Second",
                vec![variant("native", "second", Availability::Ready)],
            ),
        ]);

        core.load_history(&[]);

        assert_eq!(core.library_items, vec![0, 1, 2]);
    }

    #[test]
    fn library_non_recent_filters_ignore_history_order() {
        let mut tool = item(
            "tool",
            "Alpha Tool",
            vec![variant("native", "tool", Availability::Ready)],
        );
        tool.kind = AppKind::System;
        let mut core = fixture_core(vec![
            item(
                "game-z",
                "Zulu Game",
                vec![variant("native", "game-z", Availability::Ready)],
            ),
            tool,
            item(
                "game-b",
                "Beta Game",
                vec![variant("native", "game-b", Availability::Ready)],
            ),
        ]);
        let epoch = SystemTime::UNIX_EPOCH;
        core.load_history(&[
            history_entry("game-b", Some(epoch + Duration::from_secs(300)), None),
            history_entry("tool", Some(epoch + Duration::from_secs(200)), None),
            history_entry("game-z", Some(epoch + Duration::from_secs(100)), None),
        ]);

        core.library_filter = LibraryFilter::Alphabetical;
        core.refresh_library_items();
        assert_eq!(core.library_items, vec![1, 2, 0]);
        core.library_filter = LibraryFilter::Games;
        core.refresh_library_items();
        assert_eq!(core.library_items, vec![0, 2]);
        core.library_filter = LibraryFilter::EverythingElse;
        core.refresh_library_items();
        assert_eq!(core.library_items, vec![1]);
    }

    #[test]
    fn search_ignores_the_active_library_filter() {
        let mut tool = item(
            "tool",
            "Shared Lantern",
            vec![variant("native", "tool", Availability::Ready)],
        );
        tool.kind = AppKind::System;
        tool.tags.push("shared-alias".into());
        let mut core = fixture_core(vec![
            item(
                "game",
                "Shared Game",
                vec![variant("native", "game", Availability::Ready)],
            ),
            tool,
        ]);

        core.go(Route::Library);
        core.focus = 3;
        core.action(&ShellAction::Activate);
        assert_eq!(core.library_items, vec![0]);

        core.focus = 0;
        core.action(&ShellAction::Activate);
        assert_eq!(core.route(), Route::Search);
        core.set_search_query("");
        assert_eq!(core.search_result_ids(), vec!["game", "tool"]);
        core.set_search_query("shared-alias");
        assert_eq!(core.search_result_ids(), vec!["tool"]);
    }

    #[test]
    fn details_hide_raw_provenance_and_show_one_setup_reason_with_runtime_cue() {
        fn visit(node: &Node, labels: &mut Vec<String>) {
            labels.push(node.accessible_label.clone());
            for child in &node.children {
                visit(child, labels);
            }
        }

        let reason = "choose a profile once";
        let mut setup = variant(
            "standard-edition",
            "setup-app",
            Availability::NeedsSetup {
                reason: reason.into(),
            },
        );
        setup.provenance.provider_id = "provider.debug/raw-id".into();
        setup.launch_target.descriptor_path = PathBuf::from("/srv/apps/private/setup.app.toml");
        let mut core = fixture_core(vec![item("setup", "Setup Game", vec![setup])]);
        core.selected_item = Some(0);
        core.go(Route::Details);
        let scene = core
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
        let mut labels = Vec::new();
        visit(scene.root(), &mut labels);
        let joined = labels.join("\n");
        assert!(!joined.contains("provider.debug/raw-id"));
        assert!(!joined.contains("/srv/apps/private"));
        assert_eq!(joined.matches(reason).count(), 2);
        let availability = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "detail-availability-reason")
            .unwrap();
        assert!(
            availability.state.unavailable,
            "runtime supplies the slash cue"
        );
        assert!(
            !scene
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "detail-open")
        );
        assert!(
            !scene
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str().starts_with("detail-fact-")),
            "unavailable variants must not produce installed or offline claims"
        );
    }

    #[test]
    fn details_facts_come_from_a_ready_variant_and_stay_above_the_footer() {
        let mut unavailable = variant(
            "setup",
            "setup-app",
            Availability::NeedsSetup {
                reason: "choose a profile once".into(),
            },
        );
        unavailable.provenance.provider_id = "unavailable-provider".into();
        unavailable.provenance.app_version = Some("0.1".into());
        let mut ready = variant("native", "ready-app", Availability::Ready);
        ready.provenance.provider_id = "ready-provider".into();
        ready.provenance.app_version = Some("2.4".into());
        let mut core = fixture_core(vec![item("mixed", "Mixed Game", vec![unavailable, ready])]);
        core.selected_item = Some(0);
        core.go(Route::Details);

        let scene = core
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
        let root = scene.root();
        let fact = |id: &str| {
            root.children
                .iter()
                .find(|node| node.id.as_str() == id)
                .unwrap()
        };
        assert_eq!(
            fact("detail-fact-developer").accessible_label,
            "Ready provider"
        );
        assert_eq!(
            fact("detail-fact-installed").accessible_label,
            "Version 2.4"
        );
        assert_eq!(fact("detail-fact-offline").accessible_label, "Yes");
        let footer_top = root
            .children
            .iter()
            .find(|node| node.id.as_str() == "prompt-bar")
            .unwrap()
            .bounds
            .y;
        for node in root
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("detail-fact-"))
        {
            assert!(
                node.bounds.y + node.bounds.height <= footer_top,
                "{} crosses the footer",
                node.id.as_str()
            );
        }
    }

    #[test]
    fn details_controls_follow_variant_then_button_focus_order_and_dispatch_by_focus() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![
                variant("native", "game-native", Availability::Ready),
                variant(
                    "cloud",
                    "game-cloud",
                    Availability::NeedsNetwork {
                        reason: "offline".into(),
                    },
                ),
            ],
        )]);
        core.selected_item = Some(0);
        core.go(Route::Details);

        assert_eq!(core.focus(), 0, "the ready variant row is first");
        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(core.focus(), 1, "Play follows the variant rows");
        core.action(&ShellAction::Move(AxisMove::Right));
        assert_eq!(core.focus(), 2, "Pin is beside Play");
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::ToggleFavorite {
                item_id: "game".into(),
                favorite: true,
            }),
            "activating Pin must never launch"
        );
        core.favorite_committed("game", true);
        let scene = core
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
        let pin = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "detail-pin")
            .unwrap();
        assert!(pin.state.focused);
        assert_eq!(pin.action, Some(NodeAction::Activate));
        assert_eq!(pin.accessible_label, "★ Unpin");

        core.action(&ShellAction::Move(AxisMove::Left));
        assert_eq!(core.focus(), 1);
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "game-native".into(),
            }))
        );
    }

    #[test]
    fn details_subtrees_stay_in_surface_and_separate_regions_at_supported_widths() {
        fn intersects(a: Bounds, b: Bounds) -> bool {
            a.x < b.x + b.width
                && a.x + a.width > b.x
                && a.y < b.y + b.height
                && a.y + a.height > b.y
        }

        fn descendants<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
            out.push(node);
            for child in &node.children {
                descendants(child, out);
            }
        }

        let art_item = |with_real_art| {
            let mut catalog_item = item(
                "game",
                "Game",
                vec![variant("native", "game-native", Availability::Ready)],
            );
            if with_real_art {
                catalog_item.presentation.icon_reference = Some("art/game.png".into());
                catalog_item.presentation.icon_decodable = true;
            }
            catalog_item
        };

        for with_real_art in [true, false] {
            let snapshot = CatalogSnapshot {
                revision: 10,
                observed_at_unix_seconds: 0,
                provider_results: vec![],
                items: vec![art_item(with_real_art)],
                user_projection: UserProjection::default(),
            };
            let mut core =
                ShellCore::boot_with_art(&snapshot, &pf_theme::flagship(), false, |_| {
                    with_real_art.then(|| Arc::from(&b"png"[..]))
                });
            core.authority_snapshot(false);
            core.selected_item = Some(0);
            core.go(Route::Details);

            for width in [640.0, 1024.0, 1280.0] {
                let scene = core
                    .scene(
                        SurfaceMetrics {
                            logical_width: width,
                            logical_height: 720.0,
                            scale: 1.0,
                            safe_insets: Default::default(),
                            orientation: pf_scene::Orientation::Landscape,
                        },
                        "",
                    )
                    .unwrap();
                let root = scene.root();
                let cover = root
                    .children
                    .iter()
                    .find(|node| node.id.as_str() == "detail-cover")
                    .unwrap();
                let footer = root
                    .children
                    .iter()
                    .find(|node| node.id.as_str() == "prompts")
                    .unwrap();
                let column: Vec<_> = root
                    .children
                    .iter()
                    .filter(|node| {
                        node.id.as_str().starts_with("detail-")
                            && node.id.as_str() != "detail-cover"
                    })
                    .collect();
                let mut cover_tree = Vec::new();
                descendants(cover, &mut cover_tree);
                let mut column_tree = Vec::new();
                for node in &column {
                    descendants(node, &mut column_tree);
                }
                let mut footer_tree = Vec::new();
                descendants(footer, &mut footer_tree);

                for top_level in root
                    .children
                    .iter()
                    .filter(|node| node.id.as_str().starts_with("detail-"))
                {
                    let mut tree = Vec::new();
                    descendants(top_level, &mut tree);
                    for node in tree {
                        assert!(node.bounds.x >= 0.0, "{}", node.id.as_str());
                        assert!(node.bounds.y >= 0.0, "{}", node.id.as_str());
                        assert!(
                            node.bounds.x + node.bounds.width <= width,
                            "{} overflows width {width}",
                            node.id.as_str()
                        );
                        assert!(
                            node.bounds.y + node.bounds.height <= 720.0,
                            "{} overflows height at width {width}",
                            node.id.as_str()
                        );
                    }
                }

                for (left_name, left, right_name, right) in [
                    ("cover", &cover_tree, "column", &column_tree),
                    ("cover", &cover_tree, "footer", &footer_tree),
                    ("column", &column_tree, "footer", &footer_tree),
                ] {
                    for left_node in left {
                        for right_node in right {
                            assert!(
                                !intersects(left_node.bounds, right_node.bounds),
                                "{left_name} {} {:?} intersects {right_name} {} {:?} at width {width} ({})",
                                left_node.id.as_str(),
                                left_node.bounds,
                                right_node.id.as_str(),
                                right_node.bounds,
                                if with_real_art {
                                    "real art"
                                } else {
                                    "Edition Plate"
                                }
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn library_and_details_scene_styles_are_token_keys_only() {
        fn assert_tokens(node: &Node) {
            assert!(node.style_token.starts_with("--"), "{}", node.style_token);
            for child in &node.children {
                assert_tokens(child);
            }
        }
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.go(Route::Library);
        assert_tokens(core.scene(metrics, "").unwrap().root());
        core.selected_item = Some(0);
        core.go(Route::Details);
        assert_tokens(core.scene(metrics, "").unwrap().root());
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
    fn item_without_variants_renders_as_unavailable() {
        let mut core = fixture_core(vec![item("empty", "Empty Orbit", vec![])]);

        let scene = format!(
            "{:?}",
            core.scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                ""
            )
            .unwrap()
        );

        assert!(scene.contains("Not supported on this device — catalog item has no variants"));
        assert_eq!(core.action(&ShellAction::Activate), None);
    }

    #[test]
    fn details_without_variants_renders_honest_unavailable_reason() {
        let mut core = fixture_core(vec![item("empty", "Empty Orbit", vec![])]);
        core.action(&ShellAction::Custom("Search".into()));
        core.set_search_query("Empty Orbit");
        assert_eq!(core.action(&ShellAction::Activate), None);
        assert_eq!(core.route(), Route::Details);

        let scene = format!(
            "{:?}",
            core.scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                ""
            )
            .unwrap()
        );

        assert!(scene.contains("Not supported on this device — catalog item has no variants"));
        assert!(!scene.contains("No provider"));
        assert!(!scene.contains("No descriptor"));
    }

    #[test]
    fn remembered_pin_does_not_bypass_per_launch_variant_choice() {
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
        assert!(
            !details
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "detail-pinned-variant")
        );
        pinned.go(Route::Home);
        assert_eq!(pinned.action(&ShellAction::Activate), None);
        assert_eq!(pinned.route(), Route::VariantChooser);

        snapshot.items[0].variants[1].availability = Availability::NeedsNetwork {
            reason: "offline".into(),
        };
        let mut fallback = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        fallback.authority_snapshot(false);
        assert_eq!(
            fallback.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "many-native".into()
            }))
        );
    }

    #[test]
    fn chooser_favorite_action_never_persists_a_variant_default() {
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
            Some(Effect::ToggleFavorite {
                item_id: "many".into(),
                favorite: true,
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
        assert!(
            !card
                .children
                .iter()
                .any(|node| node.id.as_str() == "home-card-plate-plate-id")
        );
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
    fn emitted_routes_never_make_structural_groups_actionable() {
        fn assert_no_actionable_group(node: &Node) {
            assert!(
                node.role != Role::Group || node.action.is_none(),
                "structural group {} must not carry an action",
                node.id.as_str()
            );
            for child in &node.children {
                assert_no_actionable_group(child);
            }
        }

        let mut core = fixture_core(vec![item(
            "many",
            "Many Moons",
            vec![
                variant("native", "many-native", Availability::Ready),
                variant("stream", "many-stream", Availability::Ready),
            ],
        )]);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        for route in [
            Route::Home,
            Route::Library,
            Route::Search,
            Route::Details,
            Route::VariantChooser,
            Route::Settings,
            Route::Quick,
        ] {
            core.go(route);
            let scene = core.scene(metrics, "").unwrap();
            assert_no_actionable_group(scene.root());
        }
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
        // Search plus four filter chips precede the deterministic item index space.
        core.focus = 5 + 480;
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
    fn compact_library_emits_only_the_focused_visible_row() {
        let items = (0..12)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);
        core.focus = 5 + 7;
        let metrics = SurfaceMetrics {
            logical_width: 640.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let scene = core.scene(metrics, "").unwrap();
        let geometry = library_geometry(metrics.logical_width);
        let cards = scene
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("library-item-"))
            .collect::<Vec<_>>();
        let toolbar_bottom = scene
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("library-filter-"))
            .map(|node| node.bounds.y + node.bounds.height)
            .fold(0.0_f32, f32::max);

        assert!(cards.iter().all(|card| card.bounds.y >= geometry.card_top));
        assert!(
            cards.iter().all(|card| card.bounds.y >= toolbar_bottom),
            "visible cards must not overlap the filter toolbar"
        );
        assert!(
            cards.len() <= geometry.columns,
            "only one row fits at 640x720"
        );
        assert!(
            cards
                .iter()
                .any(|card| card.id.as_str() == "library-item-item-7" && card.state.focused),
            "the focused card must remain in the emitted row"
        );
    }

    #[test]
    fn desktop_library_emits_every_card_in_the_first_row() {
        let items = (0..12)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);
        core.focus = 5;
        let scene = core
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
        let emitted_ids = scene
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("library-item-"))
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            emitted_ids,
            (0..6)
                .map(|index| format!("library-item-item-{index}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn tall_library_surfaces_fill_the_grid_without_overlapping_the_footer() {
        let items = (0..48)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.go(Route::Library);
        core.focus = 5;

        for (width, height) in [(1280.0, 1080.0), (1024.0, 1200.0)] {
            let metrics = SurfaceMetrics {
                logical_width: width,
                logical_height: height,
                scale: 1.0,
                safe_insets: Default::default(),
                orientation: pf_scene::Orientation::Landscape,
            };
            let scene = core.scene(metrics, "").unwrap();
            let geometry = library_geometry(width);
            let grid_bottom = height - PROMPTS_AREA_HEIGHT;
            let cards = scene
                .root()
                .children
                .iter()
                .filter(|node| node.id.as_str().starts_with("library-item-"))
                .collect::<Vec<_>>();

            assert!(
                cards
                    .iter()
                    .all(|card| card.bounds.y + card.bounds.height <= grid_bottom),
                "cards must remain above the footer at {width}x{height}"
            );

            let row_height = 292.0;
            let mut expected_rows = 1;
            while geometry.card_top + (expected_rows + 1) as f32 * row_height - 18.0 <= grid_bottom
            {
                expected_rows += 1;
            }
            assert_eq!(
                cards.len(),
                expected_rows * geometry.columns,
                "the maximum fitting rows must be emitted at {width}x{height}"
            );
            assert!(
                geometry.card_top + (expected_rows + 1) as f32 * row_height - 18.0 > grid_bottom,
                "one more row must not fit at {width}x{height}"
            );
        }
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
            ("home-shelf-label", Role::Heading),
            ("status-cluster", Role::Text),
        ] {
            assert_eq!(find(scene.root(), id).map(|node| node.role), Some(role));
        }
        for (id, type_role) in [
            ("rooms", TypeRole::Label),
            ("status-cluster", TypeRole::Caption),
            ("route-heading", TypeRole::Eyebrow),
            ("hero-title", TypeRole::Hero),
            ("hero-status", TypeRole::Label),
            ("home-shelf-label", TypeRole::Eyebrow),
            ("prompts", TypeRole::Label),
        ] {
            assert_eq!(
                find(scene.root(), id).map(|node| node.type_role),
                Some(type_role)
            );
        }
        let scroll_region = find(scene.root(), "home-scroll-region").unwrap();
        assert!(
            scroll_region.accessible_label.is_empty(),
            "structural region metadata must not become rasterizable label content"
        );
        assert!(
            !scene
                .root()
                .accessible_label
                .contains("Home content scroll region")
        );
        for id in ["ridge", "tides"] {
            for (part, role) in [("art", Role::Group), ("title", Role::Text)] {
                let node_id = format!("home-card-{part}-{id}");
                assert_eq!(
                    find(scene.root(), &node_id).map(|node| node.role),
                    Some(role),
                    "missing Home card anatomy node {node_id}"
                );
            }
            assert_eq!(
                find(scene.root(), &format!("home-card-plate-{id}")).map(|node| node.type_role),
                Some(TypeRole::Eyebrow)
            );
            assert_eq!(
                find(scene.root(), &format!("home-card-title-{id}"))
                    .map(|node| (node.type_role, node.style_token.as_str())),
                Some((TypeRole::Label, "--color-surface-canvas"))
            );
        }
        assert_eq!(
            find(scene.root(), "home-shelf-label").map(|node| node.accessible_label.as_str()),
            Some("READY NOW · 2")
        );
    }

    #[test]
    fn chrome_text_uses_contrasting_bar_surfaces_in_dusk_and_high_contrast() {
        fn luminance(hex: &str) -> f64 {
            let channel = |offset| {
                let value = u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap();
                let value = f64::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
        }
        fn contrast(a: &str, b: &str) -> f64 {
            let (lighter, darker) = if luminance(a) >= luminance(b) {
                (luminance(a), luminance(b))
            } else {
                (luminance(b), luminance(a))
            };
            (lighter + 0.05) / (darker + 0.05)
        }

        let theme = pf_theme::flagship();
        for (base, floor) in [(Base::Dusk, 4.5), (Base::HighContrast, 7.0)] {
            let text = theme.resolve(base, "--state-rest-text").unwrap();
            let surface = theme.resolve(base, "--color-surface-raised").unwrap();
            assert!(
                contrast(text, surface) >= floor,
                "{base:?} chrome contrast must clear {floor}:1"
            );
        }
    }

    #[test]
    fn fraunces_plate_role_is_reserved_for_edition_monograms() {
        fn visit(node: &Node) {
            if node.type_role == TypeRole::Plate {
                assert!(
                    node.id.as_str().contains("-initial-"),
                    "Fraunces escaped the edition monogram: {}",
                    node.id.as_str()
                );
            }
            for child in &node.children {
                visit(child);
            }
        }

        let scene = fixture_core(vec![item(
            "ridge",
            "Ridgeline",
            vec![variant("native", "ridge", Availability::Ready)],
        )])
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
        visit(scene.root());
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
        assert!(card.children.iter().any(|node| {
            node.accessible_label
                .contains("connect to Wi-Fi to use this edition")
        }));
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
        core.focus = 0;
        let home = core.scene(metrics, "").unwrap();
        let favorite = home
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "item-ridge")
            .unwrap();
        assert!(
            favorite
                .children
                .iter()
                .any(|node| node.id.as_str() == "favorite-pin-ridge")
        );
        for part in ["art", "initial", "plate", "title"] {
            assert!(
                favorite
                    .children
                    .iter()
                    .any(|node| node.id.as_str() == format!("home-card-{part}-ridge"))
            );
        }
        snapshot.user_projection.favorite_item_ids.clear();
        let empty = ShellCore::boot(&snapshot, &pf_theme::flagship(), false)
            .scene(metrics, "")
            .unwrap();
        assert!(!empty.root().children.iter().any(|node| {
            node.children
                .iter()
                .any(|child| child.id.as_str() == "favorite-pin-ridge")
        }));

        core.go(Route::Library);
        core.focus = 2;
        core.action(&ShellAction::Activate);
        assert_eq!(core.library_items, vec![1, 0]);
        core.set_search_query("ridge");
        assert_eq!(core.search_result_ids(), vec!["ridge"]);
        core.set_search_query("tides");
        assert_eq!(core.search_result_ids(), vec!["tides"]);

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
            "detail-cover",
            "detail-provenance",
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

    #[test]
    fn home_rows_scroll_and_clip_without_shelf_overlap() {
        fn intersects(a: Bounds, b: Bounds) -> bool {
            a.x < b.x + b.width
                && a.x + a.width > b.x
                && a.y < b.y + b.height
                && a.y + a.height > b.y
        }

        fn assert_tree_misses(node: &Node, footer: Bounds) {
            assert!(
                !intersects(node.bounds, footer),
                "{} {:?} intersects footer {:?}",
                node.id.as_str(),
                node.bounds,
                footer
            );
            for child in &node.children {
                assert_tree_misses(child, footer);
            }
        }

        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        for item_count in 1..=3 {
            for favorite_mask in 0..(1 << item_count) {
                let all_items = [
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
                    item(
                        "ember",
                        "Ember Garden",
                        vec![variant("native", "ember", Availability::Ready)],
                    ),
                ];
                let items = all_items[..item_count].to_vec();
                let mut favorite_item_ids: Vec<_> = items
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| favorite_mask & (1 << index) != 0)
                    .map(|(_, item)| item.id.clone())
                    .collect();
                favorite_item_ids.sort();
                let snapshot = CatalogSnapshot {
                    revision: 1,
                    observed_at_unix_seconds: 0,
                    provider_results: vec![],
                    items,
                    user_projection: UserProjection {
                        favorite_item_ids,
                        ..UserProjection::default()
                    },
                };
                let mut core = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
                core.authority_snapshot(false);
                let focus_count = core.focus_count();

                for focus in 0..focus_count {
                    core.focus = focus;
                    let scene = core.scene(metrics, "").unwrap();
                    let footer = scene
                        .root()
                        .children
                        .iter()
                        .find(|node| node.id.as_str() == "prompts")
                        .unwrap();
                    for node in scene.root().children.iter().filter(|node| {
                        matches!(
                            node.id.as_str(),
                            "route-heading" | "hero-title" | "hero-status" | "home-shelf-label"
                        ) || node.id.as_str().starts_with("item-")
                    }) {
                        assert_tree_misses(node, footer.bounds);
                    }

                    assert!(scene.root().children.iter().any(|node| node.state.focused));
                    let row: Vec<_> = scene
                        .root()
                        .children
                        .iter()
                        .filter(|node| {
                            let id = node.id.as_str();
                            id == "home-shelf-label" || id.starts_with("item-")
                        })
                        .cloned()
                        .collect();
                    let (row_top, row_bottom) = home_row_vertical_extent(&row);
                    assert!(row_top >= 64.0);
                    assert!(row_bottom <= footer.bounds.y);

                    assert_eq!(
                        scene
                            .root()
                            .children
                            .iter()
                            .filter(|node| node.id.as_str() == "home-scroll-region")
                            .count(),
                        1
                    );
                }
            }
        }

        let snapshot = CatalogSnapshot {
            revision: 1,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            items: vec![item(
                "ridge",
                "Ridgeline",
                vec![variant("native", "ridge", Availability::Ready)],
            )],
            user_projection: UserProjection::default(),
        };
        let empty = ShellCore::boot(&snapshot, &pf_theme::flagship(), false)
            .scene(metrics, "")
            .unwrap();
        assert!(
            empty
                .root()
                .children
                .iter()
                .all(|node| node.id.as_str() != "favorites-label")
        );
    }

    #[test]
    fn detail_and_variant_chooser_rows_stack_without_overlap() {
        fn intersects(a: Bounds, b: Bounds) -> bool {
            a.x < b.x + b.width
                && a.x + a.width > b.x
                && a.y < b.y + b.height
                && a.y + a.height > b.y
        }

        fn assert_no_overlap(scene: &Scene) {
            let nodes: Vec<_> = scene
                .root()
                .children
                .iter()
                .filter(|node| {
                    (node.id.as_str().starts_with("detail-") && node.id.as_str() != "detail-cover")
                        || node.id.as_str().starts_with("chooser-")
                })
                .filter(|node| node.id.as_str() != "chooser-scroll-region")
                .collect();
            for (index, node) in nodes.iter().enumerate() {
                for other in &nodes[index + 1..] {
                    assert!(
                        !intersects(node.bounds, other.bounds),
                        "{} {:?} intersects {} {:?}",
                        node.id.as_str(),
                        node.bounds,
                        other.id.as_str(),
                        other.bounds
                    );
                }
            }
        }

        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        for variant_count in [2, 3, 12] {
            let variants = (0..variant_count)
                .map(|index| {
                    variant(
                        &format!("variant-{index}"),
                        &format!("game-{index}"),
                        Availability::Ready,
                    )
                })
                .collect();
            let mut core = fixture_core(vec![item("game", "Many Moons", variants)]);
            core.selected_item = Some(0);
            core.go(Route::Details);
            assert_no_overlap(&core.scene(metrics, "").unwrap());

            core.go(Route::VariantChooser);
            for focus in 0..variant_count {
                core.focus = focus;
                let scene = core.scene(metrics, "").unwrap();
                assert_no_overlap(&scene);
                assert!(
                    scene.root().children.iter().any(|node| node.state.focused),
                    "chooser focus {focus} of {variant_count} must remain visible"
                );
            }
        }
    }

    #[test]
    fn quiet_console_mockup_cues_and_binding_derived_footers_are_emitted() {
        fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            (node.id.as_str() == id)
                .then_some(node)
                .or_else(|| node.children.iter().find_map(|child| find(child, id)))
        }
        let unavailable = Availability::NeedsSetup {
            reason: "choose a profile".into(),
        };
        let mut core = fixture_core(vec![
            item(
                "ready",
                "Ready Game",
                vec![variant("installed", "ready", Availability::Ready)],
            ),
            item(
                "setup",
                "Setup Game",
                vec![variant("stream", "setup", unavailable)],
            ),
        ]);
        let desktop_bindings = || {
            vec![
                ControlBinding {
                    context: "global".into(),
                    action: "Activate".into(),
                    label: "Activate".into(),
                    binding: "A".into(),
                },
                ControlBinding {
                    context: "global".into(),
                    action: "Back".into(),
                    label: "Back".into(),
                    binding: "B".into(),
                },
                ControlBinding {
                    context: "shell".into(),
                    action: "Quick".into(),
                    label: "Quick".into(),
                    binding: "X".into(),
                },
            ]
        };
        core.set_control_bindings(desktop_bindings());
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        core.go(Route::Library);
        core.focus = 5;
        let library = core.scene(metrics, "wrong caller footer").unwrap();
        assert!(find(library.root(), "library-selected-underline-0").is_some());
        assert!(find(library.root(), "room-library-underline").is_some());
        assert!(find(library.root(), "room-keycap-left-border").is_some());
        assert_eq!(
            find(library.root(), "prompts").map(|node| node.accessible_label.as_str()),
            Some("A Details")
        );
        let library_prompts = find(library.root(), "prompts")
            .unwrap()
            .accessible_label
            .as_str();
        assert!(!library_prompts.contains("SELECT"));
        assert!(!library_prompts.contains('Y'));

        core.selected_item = Some(0);
        core.go(Route::Details);
        let ready = core.scene(metrics, "wrong caller footer").unwrap();
        assert_eq!(
            find(ready.root(), "detail-title").map(|node| node.style_token.as_str()),
            Some("--color-surface-canvas")
        );
        assert!(find(ready.root(), "detail-ways-heading").is_some());
        assert!(
            find(ready.root(), "detail-open").is_some_and(|node| node.accessible_label == "▶ Play")
        );
        assert!(
            find(ready.root(), "prompts")
                .is_some_and(|node| node.accessible_label == "B Back · X Favorite · A Play")
        );

        let mut remapped = desktop_bindings();
        remapped
            .iter_mut()
            .find(|binding| binding.action == "Quick")
            .unwrap()
            .binding = "START".into();
        core.set_control_bindings(remapped);
        let remapped = core.scene(metrics, "wrong caller footer").unwrap();
        assert!(
            find(remapped.root(), "prompts")
                .is_some_and(|node| node.accessible_label.contains("START Favorite"))
        );

        core.set_control_bindings(
            desktop_bindings()
                .into_iter()
                .filter(|binding| binding.action != "Quick")
                .collect(),
        );
        let favorite_unbound = core.scene(metrics, "wrong caller footer").unwrap();
        assert!(
            find(favorite_unbound.root(), "prompts").is_some_and(|node| {
                !node.accessible_label.contains("Favorite")
                    && !node.accessible_label.contains("Unfavorite")
            })
        );

        core.selected_item = Some(1);
        let unavailable = core.scene(metrics, "wrong caller footer").unwrap();
        let labels = format!("{unavailable:?}");
        assert!(labels.contains("⊘ Stream"));
        assert!(labels.contains("choose a profile"));
        assert!(find(unavailable.root(), "detail-open").is_none());
        assert!(find(unavailable.root(), "prompts").is_some_and(|node| {
            !node.accessible_label.contains("Play") && !node.accessible_label.contains("Open")
        }));
    }

    #[test]
    fn emitted_text_on_light_surfaces_has_a_declared_paired_on_color() {
        fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            (node.id.as_str() == id)
                .then_some(node)
                .or_else(|| node.children.iter().find_map(|child| find(child, id)))
        }
        let mut core = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("installed", "ready", Availability::Ready)],
        )]);
        core.load_preferences(&preferences(true), true).unwrap();
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        core.go(Route::Library);
        core.focus = 3;
        let library = core.scene(metrics, "").unwrap();
        assert_eq!(
            find(library.root(), "library-filter-2-label")
                .unwrap()
                .style_token,
            "--color-text-inverse"
        );

        core.selected_item = Some(0);
        core.go(Route::Details);
        let details = core.scene(metrics, "").unwrap();
        for id in [
            "detail-provenance",
            "detail-availability-reason",
            "detail-description",
            "detail-ways-heading",
        ] {
            assert!(
                find(details.root(), id)
                    .unwrap()
                    .style_token
                    .starts_with("--color-text")
            );
        }
        assert_eq!(
            find(details.root(), "detail-variant-0-name")
                .unwrap()
                .style_token,
            "--color-text-inverse"
        );

        core.go(Route::Settings);
        let settings = core.scene(metrics, "").unwrap();
        assert_eq!(
            find(settings.root(), "settings-section-title")
                .unwrap()
                .style_token,
            "--color-text-primary"
        );
        assert_eq!(
            find(settings.root(), "settings-nav-accessibility-label")
                .unwrap()
                .style_token,
            "--color-text-inverse"
        );
    }

    #[test]
    fn chrome_keycaps_emit_border_fill_then_on_surface_label() {
        let scene = core()
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
        let children = &scene.root().children;

        for id in ["room-keycap-left", "room-keycap-right"] {
            let border_id = format!("{id}-border");
            let fill_id = format!("{id}-fill");
            let border_index = children
                .iter()
                .position(|node| node.id.as_str() == border_id)
                .unwrap();
            let fill_index = children
                .iter()
                .position(|node| node.id.as_str() == fill_id)
                .unwrap();
            let label_index = children
                .iter()
                .position(|node| node.id.as_str() == id)
                .unwrap();
            assert!(border_index < fill_index && fill_index < label_index);

            let border = &children[border_index];
            let fill = &children[fill_index];
            let label = &children[label_index];
            assert_eq!(border.style_token, "--color-border-strong");
            assert_eq!(fill.style_token, "--color-surface-raised");
            assert_eq!(label.style_token, "--state-rest-text");
            assert!((fill.bounds.x - border.bounds.x - 2.0).abs() < f32::EPSILON);
            assert!((fill.bounds.y - border.bounds.y - 2.0).abs() < f32::EPSILON);
            assert!((border.bounds.width - fill.bounds.width - 4.0).abs() < f32::EPSILON);
            assert!((border.bounds.height - fill.bounds.height - 4.0).abs() < f32::EPSILON);
            assert!(label.bounds.x >= fill.bounds.x && label.bounds.y >= fill.bounds.y);
            assert!(
                label.bounds.x + label.bounds.width <= fill.bounds.x + fill.bounds.width
                    && label.bounds.y + label.bounds.height <= fill.bounds.y + fill.bounds.height
            );
        }
    }

    #[test]
    fn variant_chooser_bounds_are_responsive_and_rows_remain_focusable() {
        let variants = (0..3)
            .map(|index| {
                variant(
                    &format!("variant-{index}"),
                    &format!("game-{index}"),
                    Availability::Ready,
                )
            })
            .collect();
        let mut core = fixture_core(vec![item("game", "Many Moons", variants)]);
        core.selected_item = Some(0);
        core.go(Route::VariantChooser);

        for (surface_width, expected_left, expected_width) in
            [(640.0, 48.0, 544.0), (1280.0, 360.0, 560.0)]
        {
            let metrics = SurfaceMetrics {
                logical_width: surface_width,
                logical_height: 720.0,
                scale: 1.0,
                safe_insets: Default::default(),
                orientation: pf_scene::Orientation::Landscape,
            };
            for focus in 0..3 {
                core.focus = focus;
                let scene = core.scene(metrics, "").unwrap();
                let chooser_nodes = scene
                    .root()
                    .children
                    .iter()
                    .filter(|node| node.id.as_str().starts_with("chooser-"))
                    .collect::<Vec<_>>();
                assert_eq!(chooser_nodes.len(), 5);
                for node in chooser_nodes {
                    assert!(
                        (node.bounds.x - expected_left).abs() < f32::EPSILON,
                        "{}",
                        node.id.as_str()
                    );
                    assert!(
                        (node.bounds.width - expected_width).abs() < f32::EPSILON,
                        "{}",
                        node.id.as_str()
                    );
                    assert!(node.bounds.width > 0.0, "{}", node.id.as_str());
                    assert!(node.bounds.x >= 0.0, "{}", node.id.as_str());
                    assert!(
                        node.bounds.x + node.bounds.width <= surface_width,
                        "{}",
                        node.id.as_str()
                    );
                    if node.id.as_str().starts_with("chooser-variant-") {
                        assert_eq!(node.role, Role::Button);
                        assert_eq!(node.action, Some(NodeAction::Activate));
                    }
                }
                let focused_id = format!("chooser-variant-{focus}");
                assert!(
                    scene
                        .root()
                        .children
                        .iter()
                        .any(|node| { node.id.as_str() == focused_id && node.state.focused })
                );
            }
        }
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
