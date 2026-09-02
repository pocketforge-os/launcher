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
use pf_render::Rasterizer;
use pf_scene::{
    AlignItems, AxisMove, Bounds, Edges, Elevation, FlexDirection, ImageFit, ImageSource,
    LayoutCache, LayoutStyle, LayoutValue, Node, NodeAction, NodeId, Position, Role, Scene,
    SurfaceMetrics, TextAlign, TypeRole, resolve_layout,
};
use pf_session_authority::{EndPrecision, HistoryEntry};
use pf_theme::{Base, Theme};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

mod design_generated;
mod design_manual;

use design_generated::{
    CARD_ART_HEIGHT, CARD_ART_WIDTH, CHIP_BORDER_WIDTH, CHIP_HEIGHT, CHIP_HORIZONTAL_PADDING,
    COLOR_BORDER_HAIRLINE_TOKEN, COLOR_BORDER_STRONG_TOKEN, COLOR_STATUS_ATTENTION_TOKEN,
    COLOR_STATUS_READY_TOKEN, COLOR_SURFACE_CANVAS_TOKEN, COLOR_SURFACE_RAISED_TOKEN,
    COLOR_SURFACE_SCRIM_TOKEN, COLOR_SURFACE_SUNKEN_TOKEN, COLOR_TEXT_INVERSE_TOKEN,
    COLOR_TEXT_MUTED_TOKEN, COLOR_TEXT_PRIMARY_TOKEN, COLOR_TEXT_SECONDARY_TOKEN,
    KEYCAP_BORDER_WIDTH, KEYCAP_HEIGHT, KEYCAP_MIN_WIDTH, LIB_CARD_ART_HEIGHT, LIB_GRID_TOP,
    LIB_HEAD_TOP, LIB_TOOLBAR_HEIGHT, PROMPTS_AREA_HEIGHT, RADIUS_L, RADIUS_M, RADIUS_PILL,
    RADIUS_S, ROOM_HORIZONTAL_PADDING, ROOM_STRIP_GAP, SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_7,
    STATE_DISABLED_BORDER_TOKEN, STATE_FOCUSED_RING_TOKEN, STATE_FOCUSED_TEXT_TOKEN,
    STATE_REST_SURFACE_TOKEN, STATE_REST_TEXT_TOKEN, STATE_SELECTED_ACCENT_TOKEN,
    STATE_UNAVAILABLE_TEXT_TOKEN, STATE_UNAVAILABLE_VEIL_TOKEN, STATUS_BAR_HEIGHT,
};
use design_manual::{
    CAPTION_GLYPH_ADVANCE, CHIP_COUNT_GAP, LABEL_GLYPH_ADVANCE, SCENE_TRANSPARENT_TOKEN,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceStatus {
    pub battery_percent: u8,
    pub attention_message: Option<String>,
}

pub trait DeviceStatusPort {
    fn status(&self) -> Result<DeviceStatus, String>;
}

const HOME_SHELF_LIMIT: usize = 6;
const HOME_SHELF_GAP: f32 = 47.2;
const LIBRARY_SIDE_MARGIN: f32 = SPACE_7;
const LIBRARY_TOOLBAR_GAP: f32 = SPACE_4;
const COMPACT_LIBRARY_TOOLBAR_TOP: f32 = 180.0;
const LIBRARY_TOOLBAR_ROW_GAP: f32 = 68.0 - CHIP_HEIGHT;
const LIBRARY_SEARCH_MIN_WIDTH: f32 = 320.0;
const CARD_LABEL_GAP: f32 = 12.0;
const CARD_CAPTION_GAP: f32 = 2.0;
const HOME_STACK_BUDGET: f32 = 664.0;
const HOME_STACK_HARD_LIMIT: f32 = 720.0;
const HOME_TOP_SPACER: f32 = 144.0;
const HOME_TOP_SPACER_FLOOR: f32 = 96.0;
const HOME_AIR_SPACER: f32 = 88.0;
const HOME_AIR_SPACER_FLOOR: f32 = 24.0;
// Start-aligned labels are painted into this inset content box by pf-render.
// Keep node sizing expressed in terms of the per-edge paint contract so callers
// cannot accidentally confuse a shaped advance with the containing box width.
const TEXT_NODE_INLINE_INSET: f32 = 6.0;
const ATTENTION_PILL_RIGHT_MARGIN: f32 = 48.0;
const ATTENTION_PILL_TOP: f32 = 77.0;
const ATTENTION_PILL_LABEL_HEIGHT: f32 = 22.0;
const ATTENTION_PILL_VERTICAL_PADDING: f32 = 8.0;
const ATTENTION_PILL_HORIZONTAL_PADDING: f32 = 16.0;
const ATTENTION_PILL_DOT_SIZE: f32 = 6.4;

// The flagship label role is 14 px Manrope semibold. This conservative advance keeps
// scene geometry renderer-independent while reserving enough room for its widest glyphs.
fn label_text_width(text: &str) -> f32 {
    text.chars().count() as f32 * LABEL_GLYPH_ADVANCE
}

fn measured_text_advance(base_advance: f32, text_scale: u16) -> f32 {
    base_advance * f32::from(text_scale) / 100.0
}

fn caption_text_width(text: &str, text_scale: u16) -> f32 {
    measured_text_advance(
        text.chars().count() as f32 * CAPTION_GLYPH_ADVANCE,
        text_scale,
    )
}

// Natural Manrope 14/600 advances for the finite room-strip vocabulary, measured
// with the same Cosmic Text configuration that paints the Label role.
fn room_label_advance(text: &str, text_scale: u16) -> f32 {
    let base_advance = match text {
        "Home" => 43.0,
        "Library" => 50.0,
        "Settings" => 59.0,
        _ => label_text_width(text),
    };
    measured_text_advance(base_advance, text_scale)
}

// Natural Manrope 14/600 advances for the Library prompt verbs. These are measured
// by the same Cosmic Text shaping configuration that paints the Label role. Keeping
// the finite prompt vocabulary exact avoids turning conservative layout reserve into
// invisible trailing space at the right-aligned edge.
fn library_prompt_verb_width(text: &str) -> f32 {
    match text {
        "Search" | "Details" => 52.0,
        "Filter" => 37.0,
        // Home's binding-derived vocabulary is open-ended. Keep its node sized from
        // measured advance plus the renderer's conservative bearing reserve.
        _ => label_text_width(text) + 8.0,
    }
}

fn text_node_box_width(content_advance: f32) -> f32 {
    content_advance + 2.0 * TEXT_NODE_INLINE_INSET
}

fn scaled_text_box_height(base_height: f32, text_scale: u16) -> f32 {
    measured_text_advance(base_height, text_scale)
}

fn chrome_row_bottom(safe_top: f32, text_scale: u16) -> f32 {
    safe_top + STATUS_BAR_HEIGHT.max(16.0 + scaled_text_box_height(32.0, text_scale))
}

fn system_status_group_left(
    surface_width: f32,
    text_scale: u16,
    status_width: Option<f32>,
    has_wifi: bool,
    has_battery: bool,
) -> Option<f32> {
    let mut left = status_width.map(|width| {
        let right = if text_scale == 100 { -16.0 } else { 0.0 };
        surface_width - right - width.max(152.0)
    });
    if has_wifi {
        left = Some(left.map_or(surface_width - 200.0, |value| {
            value.min(surface_width - 200.0)
        }));
    }
    if has_battery {
        left = Some(left.map_or(surface_width - 168.0, |value| {
            value.min(surface_width - 168.0)
        }));
    }
    left
}

#[derive(Clone, Copy, Debug)]
struct HomeVerticalLayout {
    title_y: f32,
    status_y: f32,
    shelf_label_y: f32,
    card_row_y: f32,
    card_height: f32,
    show_card_caption: bool,
}

fn home_vertical_layout(text_scale: u16) -> HomeVerticalLayout {
    let title_height = scaled_text_box_height(72.0, text_scale);
    let status_height = scaled_text_box_height(32.0, text_scale);
    let shelf_label_height = scaled_text_box_height(28.0, text_scale);
    let card_primary_height =
        CARD_ART_HEIGHT + CARD_LABEL_GAP + scaled_text_box_height(34.0, text_scale);
    let card_caption_height = CARD_CAPTION_GAP + scaled_text_box_height(14.0, text_scale);

    let fixed_height = title_height
        + 8.0
        + status_height
        + shelf_label_height
        + 16.0
        + card_primary_height
        + card_caption_height;
    let overflow = (HOME_TOP_SPACER + HOME_AIR_SPACER + fixed_height - HOME_STACK_BUDGET).max(0.0);
    let top_slack = HOME_TOP_SPACER - HOME_TOP_SPACER_FLOOR;
    let air_slack = HOME_AIR_SPACER - HOME_AIR_SPACER_FLOOR;
    let total_slack = top_slack + air_slack;
    let shrink = overflow.min(total_slack);
    let top_spacer = HOME_TOP_SPACER - shrink * top_slack / total_slack;
    let air_spacer = HOME_AIR_SPACER - shrink * air_slack / total_slack;
    let bottom_with_caption = top_spacer + air_spacer + fixed_height;
    let show_card_caption = bottom_with_caption <= HOME_STACK_HARD_LIMIT;
    let card_height = card_primary_height
        + if show_card_caption {
            card_caption_height
        } else {
            0.0
        };
    let title_y = top_spacer;
    let status_y = title_y + title_height + 8.0;
    let shelf_label_y = status_y + status_height + air_spacer;
    let card_row_y = shelf_label_y + shelf_label_height + 16.0;

    HomeVerticalLayout {
        title_y,
        status_y,
        shelf_label_y,
        card_row_y,
        card_height,
        show_card_caption,
    }
}

fn scaled_centered_text_box(base_width: f32, base_height: f32, text_scale: u16) -> (f32, f32) {
    if text_scale == 100 {
        (base_width, base_height)
    } else {
        (
            measured_text_advance(base_width, text_scale),
            scaled_text_box_height(base_height, text_scale),
        )
    }
}

fn scale_aware_single_line(text: &str, width: f32, text_scale: u16) -> String {
    if text_scale == 100 {
        text.to_owned()
    } else {
        ellipsize_to_lines(text, width * 100.0 / f32::from(text_scale), 1)
    }
}

fn settings_value_advance(text: &str, text_scale: u16) -> f32 {
    let base_advance = match text {
        "100%" | "150%" => 34.0,
        "200%" => 37.0,
        "ON" => 20.0,
        "OFF" => 27.0,
        _ => label_text_width(text),
    };
    measured_text_advance(base_advance, text_scale)
}

fn settings_scaled_box_width(base_width: f32, text: &str, text_scale: u16) -> f32 {
    let advance_delta =
        settings_value_advance(text, text_scale) - settings_value_advance(text, 100);
    // Centered text must retain its integral raster phase as the box grows. Round
    // the measured growth to an even delta so both alignment insets gain whole px.
    base_width + (advance_delta / 2.0).ceil() * 2.0
}

fn room_label_box_width(content_advance: f32) -> f32 {
    content_advance + 2.0 * ROOM_HORIZONTAL_PADDING
}

fn room_strip_width(text_scale: u16) -> f32 {
    KEYCAP_MIN_WIDTH * 2.0
        + room_label_box_width(room_label_advance("Home", text_scale))
        + room_label_box_width(room_label_advance("Library", text_scale))
        + room_label_box_width(room_label_advance("Settings", text_scale))
        + ROOM_STRIP_GAP * 4.0
}

fn library_chip_width(label: &str, count: Option<usize>) -> f32 {
    CHIP_HORIZONTAL_PADDING
        + label_text_width(label)
        + 20.0
        + count.map_or(0.0, |value| {
            CHIP_COUNT_GAP + text_node_box_width(label_text_width(&value.to_string()))
        })
        + CHIP_HORIZONTAL_PADDING
}

fn scaled_library_chip_width(label: &str, count: Option<usize>, text_scale: u16) -> f32 {
    if text_scale == 100 {
        return library_chip_width(label, count);
    }

    CHIP_HORIZONTAL_PADDING
        + measured_text_advance(label_text_width(label) + 20.0, text_scale)
        + count.map_or(0.0, |value| {
            CHIP_COUNT_GAP
                + text_node_box_width(measured_text_advance(
                    label_text_width(&value.to_string()),
                    text_scale,
                ))
        })
        + CHIP_HORIZONTAL_PADDING
}

fn ready_variant_label(variant: &Variant) -> String {
    let identity = humanize_identifier(&variant.id);
    match ready_variant_capability(variant) {
        ReadyVariantCapability::Native => format!("{identity} · Installed on this device"),
        ReadyVariantCapability::Stream => format!("{identity} · Stream from your PC"),
        ReadyVariantCapability::Unknown if variant.provenance.runtime_family.is_empty() => identity,
        ReadyVariantCapability::Unknown => format!(
            "{identity} · {}",
            humanize_identifier(&variant.provenance.runtime_family)
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadyVariantCapability {
    Native,
    Stream,
    Unknown,
}

fn ready_variant_capability(variant: &Variant) -> ReadyVariantCapability {
    let runtime = variant.provenance.runtime_family.to_ascii_lowercase();
    let families = runtime.split(['/', '-', '_', '.']);
    if families.clone().any(|part| part == "stream") || runtime.contains("streaming") {
        ReadyVariantCapability::Stream
    } else if families.into_iter().any(|part| part == "native") {
        ReadyVariantCapability::Native
    } else {
        ReadyVariantCapability::Unknown
    }
}

fn ready_variant_capability_cue(variant: &Variant) -> &'static str {
    match ready_variant_capability(variant) {
        ReadyVariantCapability::Native => "Installed",
        ReadyVariantCapability::Stream => "Available over the network",
        ReadyVariantCapability::Unknown => "Source availability unknown",
    }
}

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

    fn compact_toolbar_bottom(self, text_scale: u16) -> f32 {
        let rows = self.toolbar_rows();
        COMPACT_LIBRARY_TOOLBAR_TOP
            + rows as f32 * scaled_text_box_height(CHIP_HEIGHT, text_scale)
            + rows.saturating_sub(1) as f32 * LIBRARY_TOOLBAR_ROW_GAP
    }

    fn scaled_card_top(self, text_scale: u16) -> f32 {
        if self.columns == 6 {
            return self.card_top;
        }
        let original_separation = self.card_top - self.compact_toolbar_bottom(100);
        self.compact_toolbar_bottom(text_scale) + original_separation
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
    let (preferred_columns, toolbar_columns, card_top) = if surface_width >= 1100.0 {
        (6, 4, LIB_GRID_TOP + SPACE_3)
    } else if surface_width >= 760.0 {
        (4, 4, 252.0)
    } else {
        (3, 2, 320.0)
    };
    let card_left = LIBRARY_SIDE_MARGIN;
    let card_width = if preferred_columns == 6 {
        (surface_width - 2.0 * card_left - (preferred_columns - 1) as f32 * SPACE_5)
            / preferred_columns as f32
    } else {
        CARD_ART_WIDTH
    };
    let mut columns = preferred_columns;
    while columns > 1
        && 2.0 * card_left + columns as f32 * card_width + (columns - 1) as f32 * SPACE_4
            > surface_width
    {
        columns -= 1;
    }
    // The desktop mockup fixes the cover width and distributes the remaining content
    // width between columns. Narrower breakpoints retain their existing fluid spacing.
    let card_gap = if columns == 1 {
        0.0
    } else if columns == 6 {
        SPACE_5
    } else {
        ((surface_width - 2.0 * card_left - columns as f32 * card_width) / (columns - 1) as f32)
            .max(SPACE_4)
    };
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
    accessible_label: String,
    action: Option<SettingsRowAction>,
}

#[derive(Clone, Debug)]
struct Item {
    id: String,
    title: String,
    kind: AppKind,
    tags: Vec<String>,
    playtime_fact: Option<String>,
    developer: Option<String>,
    description: Option<String>,
    last_played_fact: Option<String>,
    size_fact: Option<String>,
    art: Option<ImageSource>,
    art_failed: bool,
    variants: Vec<Variant>,
    favorite: bool,
    pinned_variant_id: Option<String>,
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
    battery_percent: Option<u8>,
    attention_message: Option<String>,
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
        Self::boot_with_art(snapshot, theme, reduced_motion, |_, _| None)
    }

    #[must_use]
    pub fn boot_with_art<F>(
        snapshot: &CatalogSnapshot,
        theme: &Theme,
        reduced_motion: bool,
        mut resolve_art: F,
    ) -> Self
    where
        F: FnMut(&pf_catalog::CatalogItem, &str) -> Option<Arc<[u8]>>,
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
                    .and_then(|reference| resolve_art(item, reference))
                    .map(|bytes| {
                        let digest = Sha256::digest(&bytes);
                        ImageSource::new(format!("sha256:{digest:x}"), bytes)
                    });
                Item {
                    id: item.id.clone(),
                    title: item.title.clone(),
                    kind: item.kind.clone(),
                    tags: item.tags.clone(),
                    playtime_fact: item
                        .tags
                        .iter()
                        .find_map(|tag| tag.strip_prefix("playtime:").map(str::to_owned)),
                    developer: item
                        .tags
                        .iter()
                        .find_map(|tag| tag.strip_prefix("developer:").map(str::to_owned)),
                    description: item
                        .tags
                        .iter()
                        .find_map(|tag| tag.strip_prefix("description:").map(str::to_owned)),
                    last_played_fact: item
                        .tags
                        .iter()
                        .find_map(|tag| tag.strip_prefix("last-played:").map(str::to_owned)),
                    size_fact: item
                        .tags
                        .iter()
                        .find_map(|tag| tag.strip_prefix("size:").map(str::to_owned)),
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
            battery_percent: None,
            attention_message: None,
            playtime: HashMap::new(),
            recent_use: HashMap::new(),
        }
    }

    pub fn load_device_status(&mut self, port: &dyn DeviceStatusPort) {
        let (battery_percent, attention) = port.status().map_or((None, None), |status| {
            (
                Some(status.battery_percent.min(100)),
                status.attention_message,
            )
        });
        if self.battery_percent != battery_percent || self.attention_message != attention {
            self.battery_percent = battery_percent;
            self.attention_message = attention;
            self.bump_revision();
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
            Route::Home => self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| matches!(best_availability(item), Availability::Ready))
                .take(HOME_SHELF_LIMIT)
                .nth(self.focus)
                .map(|(index, _)| index),
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
                Route::Home => self
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| matches!(best_availability(item), Availability::Ready))
                    .take(HOME_SHELF_LIMIT)
                    .position(|(item_index, _)| item_index == index),
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
            ShellAction::Custom(name) if matches!(name.as_str(), "Search" | "Search.open") => {
                self.remember_caller();
                self.go(Route::Search);
            }
            ShellAction::Custom(name) if name == "Filter.next" && self.route == Route::Library => {
                self.library_filter = match self.library_filter {
                    LibraryFilter::Recent => LibraryFilter::Alphabetical,
                    LibraryFilter::Alphabetical => LibraryFilter::Games,
                    LibraryFilter::Games => LibraryFilter::EverythingElse,
                    LibraryFilter::EverythingElse => LibraryFilter::Recent,
                };
                self.refresh_library_items();
                self.focus = self.focus.min(self.library_items.len().saturating_add(4));
            }
            ShellAction::Custom(name) if name == "Room.next" => self.next_room(),
            ShellAction::Custom(name) if name == "Room.previous" => self.previous_room(),
            ShellAction::Custom(name) if name == "Quick" => {
                if self.route == Route::Details {
                    if let Some(item) = self.focused_item_index() {
                        return Some(Effect::ToggleFavorite {
                            item_id: self.items[item].id.clone(),
                            favorite: !self.items[item].favorite,
                        });
                    }
                } else {
                    self.go(Route::Quick);
                }
            }
            ShellAction::Custom(name) if name == "Quick.open" => self.go(Route::Quick),
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
                1 => self
                    .active_ready_variant(item)
                    .and_then(|variant| self.launch_variant(item, variant)),
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
            0 => {
                self.selected_item = Some(item);
                self.remember_caller();
                self.go(Route::Details);
                None
            }
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

    fn active_ready_variant(&self, item: usize) -> Option<usize> {
        self.items[item]
            .variants
            .iter()
            .position(|variant| matches!(variant.availability, Availability::Ready))
    }

    fn detail_visible_variants(&self, item: usize) -> Vec<usize> {
        const CAPACITY: usize = 2;
        let variant_count = self.items[item].variants.len();
        let start = self
            .active_ready_variant(item)
            .unwrap_or_default()
            .saturating_sub(CAPACITY - 1)
            .min(variant_count.saturating_sub(CAPACITY));
        (start..(start + CAPACITY).min(variant_count)).collect()
    }

    fn detail_focusable_variants(&self) -> Vec<usize> {
        let Some(item) = self
            .selected_item
            .filter(|&item| self.items.get(item).is_some())
        else {
            return Vec::new();
        };
        self.detail_visible_variants(item)
            .into_iter()
            .filter(|&index| {
                matches!(
                    self.items[item].variants[index].availability,
                    Availability::Ready
                )
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
            Route::Settings => self.go(Route::Home),
            _ => {}
        }
    }
    fn previous_room(&mut self) {
        match self.route {
            Route::Settings => self.go(Route::Library),
            Route::Library => self.go(Route::Home),
            Route::Home => self.go(Route::Settings),
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
            Route::Home => self
                .items
                .iter()
                .filter(|item| matches!(best_availability(item), Availability::Ready))
                .take(HOME_SHELF_LIMIT)
                .count()
                .max(1),
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
            accessible_label: String::new(),
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
                        accessible_label: match &row.effective {
                            PreferenceValue::Bool(value) => {
                                format!("{} · {}", row.label, if *value { "ON" } else { "OFF" })
                            }
                            PreferenceValue::Text(value) => {
                                format!("{} · {value}", row.label)
                            }
                            PreferenceValue::Integer(value) => {
                                format!("{} · {value}", row.label)
                            }
                        },
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
                accessible_label: format!(
                    "Appearance · {}",
                    if self.appearance_day { "Day" } else { "Dusk" }
                ),
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
                        accessible_label: "Recovery".into(),
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
        let mut children = Vec::new();
        let battery_x = w - 168.0;
        let room_width = room_strip_width(self.text_scale);
        let room_left = (w - room_width) / 2.0;
        let room_right = room_left + room_width;
        let has_wifi = self
            .network_state
            .as_ref()
            .is_ok_and(|state| state.connected_ssid.is_some());
        let has_battery = self.battery_percent.is_some();
        let mut status_parts = Vec::new();
        if let Some(percent) = self.battery_percent {
            status_parts.push(percent.to_string());
        }
        if self.authority_unavailable() {
            status_parts.push("!".into());
        }
        if let Ok(state) = &self.time_state {
            let seconds = state
                .wall_clock
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                % 86_400;
            status_parts.push(format!("{}:{:02}", seconds / 3_600, seconds % 3_600 / 60));
        }
        let status_text = (!status_parts.is_empty()).then(|| status_parts.join("     "));
        let status_width = status_text
            .as_ref()
            .map(|text| text_node_box_width(caption_text_width(text, self.text_scale)));
        // Wi-Fi, battery, and status are one right-aligned chrome group. Measure the
        // final, scale-aware extent used by the layout seam and admit every available
        // member together only when the complete group clears the room strip.
        let status_group_fits =
            system_status_group_left(w, self.text_scale, status_width, has_wifi, has_battery)
                .is_some_and(|left| left >= room_right + ROOM_STRIP_GAP);
        if status_group_fits && has_wifi {
            children.push(
                node(
                    "wifi-glyph",
                    Role::Group,
                    "Wi-Fi connected",
                    w - 200.0,
                    22.0,
                    20.0,
                    20.0,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_image(wifi_glyph_source(), ImageFit::Contain),
            );
        }
        if let Some(battery_percent) = self.battery_percent.filter(|_| status_group_fits) {
            children.extend([
                node(
                    "battery-outline",
                    Role::Group,
                    "Battery",
                    battery_x,
                    24.0,
                    24.0,
                    14.0,
                    COLOR_BORDER_STRONG_TOKEN,
                ),
                node(
                    "battery-cavity",
                    Role::Group,
                    "",
                    battery_x + 2.0,
                    26.0,
                    18.0,
                    10.0,
                    COLOR_SURFACE_RAISED_TOKEN,
                ),
                node(
                    "battery-level",
                    Role::Group,
                    "",
                    battery_x + 3.0,
                    27.0,
                    16.0 * f32::from(battery_percent) / 100.0,
                    8.0,
                    COLOR_TEXT_SECONDARY_TOKEN,
                ),
                node(
                    "battery-terminal",
                    Role::Group,
                    "",
                    battery_x + 24.0,
                    28.0,
                    2.0,
                    6.0,
                    COLOR_BORDER_STRONG_TOKEN,
                ),
            ]);
        }
        if status_group_fits {
            if let (Some(status_text), Some(status_width)) = (status_text, status_width) {
                let status_left = battery_x + 32.0;
                let mut status = node(
                    "status-cluster",
                    Role::Text,
                    &status_text,
                    status_left,
                    16.0,
                    status_width,
                    scaled_text_box_height(32.0, self.text_scale),
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Caption);
                if self.text_scale > 100 {
                    status = status.with_ink_token(COLOR_TEXT_PRIMARY_TOKEN);
                }
                children.push(status);
            }
        }
        let mut room_nodes = Vec::new();
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
                room_nodes.push(node(
                    &format!("{id}-border"),
                    Role::Group,
                    "",
                    x - 4.0,
                    16.0,
                    width + 8.0,
                    24.0,
                    COLOR_BORDER_STRONG_TOKEN,
                ));
                room_nodes.push(node(
                    &format!("{id}-fill"),
                    Role::Group,
                    "",
                    x - 3.0,
                    17.0,
                    width + 6.0,
                    22.0,
                    COLOR_SURFACE_RAISED_TOKEN,
                ));
                room_nodes.push(
                    node(
                        id,
                        Role::Text,
                        label,
                        x,
                        17.0,
                        width,
                        22.0,
                        COLOR_SURFACE_RAISED_TOKEN,
                    )
                    .with_type_role(TypeRole::Caption),
                );
            } else {
                room_nodes.push(
                    node(
                        id,
                        Role::Text,
                        label,
                        x,
                        16.0,
                        width,
                        32.0,
                        SCENE_TRANSPARENT_TOKEN,
                    )
                    .with_type_role(TypeRole::Label),
                );
                if selected {
                    room_nodes.push(node(
                        &format!("{id}-underline"),
                        Role::Group,
                        "",
                        x,
                        49.0,
                        width,
                        3.0,
                        STATE_SELECTED_ACCENT_TOKEN,
                    ));
                }
            }
        }
        let mut rooms = node(
            "rooms",
            Role::Group,
            "",
            room_left,
            12.0,
            424.0,
            40.0,
            SCENE_TRANSPARENT_TOKEN,
        )
        .with_type_role(TypeRole::Label);
        rooms = rooms_layout(rooms, room_nodes, w, self.text_scale);
        children.push(rooms);
        if let Some(status) = self.session_status() {
            children.push(node(
                "session-status",
                Role::Text,
                status,
                48.0,
                266.0,
                520.0,
                32.0,
                COLOR_SURFACE_CANVAS_TOKEN,
            ));
        }
        match self.presentation {
            Presentation::FirstRun => self.first_run_nodes(&mut children, w),
            Presentation::Crash => self.crash_nodes(&mut children, w, h),
            _ if self.route == Route::Quick => self.quick_nodes(&mut children, w, h),
            _ => self.route_nodes(&mut children, metrics),
        }
        let supplied_footer = footer.to_owned();
        let footer = match self.route {
            Route::Home => self.focused_item_index().map_or_else(String::new, |item| {
                let mut prompts = self
                    .binding_prompt("Search.open", "Search")
                    .into_iter()
                    .collect::<Vec<_>>();
                if let Some(prompt) = self.binding_prompt("Quick", "Quick") {
                    prompts.push(prompt);
                }
                if let Some(prompt) = self.binding_prompt(
                    "Activate",
                    if self.ready_variants(item).is_empty() {
                        "Details"
                    } else {
                        "Open"
                    },
                ) {
                    prompts.push(prompt);
                }
                let global_prompts = supplied_footer
                    .split_once("     ")
                    .map_or(supplied_footer.as_str(), |(_, global)| global);
                if !global_prompts.is_empty() {
                    prompts.push(global_prompts.to_owned());
                }
                prompts.join(" · ")
            }),
            Route::Library => {
                let mut prompts = self
                    .binding_prompt("Search.open", "Search")
                    .into_iter()
                    .collect::<Vec<_>>();
                if let Some(prompt) = self.binding_prompt("Filter.next", "Filter") {
                    prompts.push(prompt);
                }
                if self.focus >= 5
                    && let Some(prompt) = self.binding_prompt("Activate", "Details")
                {
                    prompts.push(prompt);
                }
                prompts.join("     ")
            }
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
            SCENE_TRANSPARENT_TOKEN,
        ));
        let prompt_height = scaled_text_box_height(32.0, self.text_scale);
        let prompt_top = h - PROMPTS_AREA_HEIGHT.max(prompt_height);
        let prompt_label = if matches!(self.route, Route::Search | Route::Details) {
            ""
        } else {
            &footer
        };
        let mut prompt_node = node(
            "prompts",
            if matches!(
                self.route,
                Route::Home | Route::Library | Route::Details | Route::Quick | Route::Search
            ) {
                Role::Group
            } else {
                Role::Text
            },
            prompt_label,
            if self.route == Route::Home {
                w - 660.0
            } else {
                w - 600.0
            },
            prompt_top,
            if self.route == Route::Home {
                612.0
            } else {
                552.0
            },
            prompt_height,
            SCENE_TRANSPARENT_TOKEN,
        )
        .with_type_role(TypeRole::Label);
        if self.route == Route::Home {
            prompt_node.children = home_prompt_nodes(&footer, w, h, self.text_scale);
        } else if matches!(
            self.route,
            Route::Library | Route::Details | Route::Quick | Route::Search
        ) {
            prompt_node.children = right_aligned_prompt_nodes(&footer, w, h, self.text_scale);
        }
        children.push(prompt_node);
        wrap_system_layout(&mut children, w, self.text_scale);
        let radius_scale = f32::from(self.text_scale) / 100.0;
        for child in &mut children {
            add_explicit_action_name(child, self.text_scale);
        }
        if self.route == Route::Library {
            place_library_fade_below_footer(&mut children);
        }
        let focus_id = children
            .iter()
            .find_map(focused_node_id)
            .map_or("quiet-console", |n| n.id.as_str())
            .to_owned();
        let mut root = Node::new(
            NodeId::new("quiet-console").unwrap(),
            Role::Group,
            "",
            Bounds::new(0.0, 0.0, w, h),
            COLOR_SURFACE_CANVAS_TOKEN,
        )
        .with_children(children);
        #[cfg(test)]
        let semantics_before = semantic_snapshot(&root);
        if self.route == Route::Home {
            resolve_layout(
                &mut root,
                metrics,
                f32::from(self.text_scale) / 100.0,
                &Rasterizer::new(),
                &mut LayoutCache::default(),
            );
        } else {
            for child in &mut root.children {
                if matches!(
                    child.id.as_str(),
                    "rooms-layout-anchor" | "system-status-layout-anchor"
                ) {
                    resolve_layout(
                        child,
                        metrics,
                        f32::from(self.text_scale) / 100.0,
                        &Rasterizer::new(),
                        &mut LayoutCache::default(),
                    );
                }
            }
        }
        #[cfg(test)]
        assert_eq!(semantic_snapshot(&root), semantics_before);
        apply_quiet_console_radius(&mut root, radius_scale);
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

    fn route_nodes(&self, out: &mut Vec<Node>, metrics: SurfaceMetrics) {
        let (w, h) = (metrics.logical_width, metrics.logical_height);
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
        if !matches!(
            self.route,
            Route::Settings | Route::Library | Route::Details
        ) {
            out.push(
                node(
                    "route-heading",
                    Role::Heading,
                    heading,
                    48.0,
                    112.0,
                    500.0,
                    48.0,
                    COLOR_SURFACE_CANVAS_TOKEN,
                )
                .with_type_role(TypeRole::Eyebrow),
            );
        }
        if self.route == Route::Home {
            let mut heading = out
                .pop()
                .expect("Home route heading was just added")
                .with_ink_token(COLOR_TEXT_MUTED_TOKEN);
            heading.style_token = SCENE_TRANSPARENT_TOKEN.into();
            let focused = self
                .focused_item_index()
                .and_then(|index| self.items.get(index));
            let hero_status = focused.map_or_else(
                || "⊘ Unavailable · Game".to_owned(),
                |item| {
                    let kind = sentence_kind(&item.kind);
                    match best_availability(item) {
                        Availability::Ready
                            if matches!(self.presentation, Presentation::Starting) =>
                        {
                            let cue = item
                                .variants
                                .iter()
                                .find(|variant| matches!(variant.availability, Availability::Ready))
                                .map_or(
                                    "Source availability unknown",
                                    ready_variant_capability_cue,
                                );
                            format!("● Starting · {kind} · {cue}")
                        }
                        Availability::Ready => {
                            let cue = item
                                .variants
                                .iter()
                                .find(|variant| matches!(variant.availability, Availability::Ready))
                                .map_or(
                                    "Source availability unknown",
                                    ready_variant_capability_cue,
                                );
                            format!("● Ready · {kind} · {cue}")
                        }
                        Availability::NeedsSetup { .. } => format!("⊘ Needs setup · {kind}"),
                        Availability::NeedsNetwork { .. } => {
                            format!("⊘ Network required · {kind}")
                        }
                        Availability::UnsupportedCapability { .. }
                        | Availability::IncompatibleRuntime { .. } => {
                            format!("⊘ Unavailable · {kind}")
                        }
                    }
                },
            );
            let hero_status = format!(
                "{}{}",
                hero_status,
                focused
                    .and_then(|item| item.playtime_fact.as_deref())
                    .map_or(String::new(), |fact| format!(" · {fact}"))
            );
            let vertical = home_vertical_layout(self.text_scale);
            let hero_title_height = scaled_text_box_height(72.0, self.text_scale);
            let hero_status_width = if self.text_scale == 100 {
                480.0
            } else {
                text_node_box_width(measured_text_advance(
                    label_text_width(&hero_status),
                    self.text_scale,
                ))
                .min(w - 96.0)
            };
            let hero_status_height = scaled_text_box_height(32.0, self.text_scale);
            // At 150% the responsive hero stack rises by 24 px. Keep its eyebrow
            // immediately above the title; at 200% the chrome consumes that slot,
            // so omit the decorative eyebrow rather than overlap either region.
            if self.text_scale == 150 {
                heading.bounds.y = vertical.title_y - heading.bounds.height - 8.0;
            }
            let mut content = vec![node(
                    "hero-wash",
                    Role::Group,
                    "Ridgeline aura: rgba(201,111,87,0.5) to transparent 68%; rgba(58,43,78,0.65) to transparent 70%; layer opacity 0.55",
                    0.0,
                    0.0,
                    w,
                    344.0,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_image(hero_wash_source(), ImageFit::Cover)
                // The wash is the ambient image beneath the whole hero composition,
                // not foreign foreground content. Keep that role explicit in the
                // scene graph so paint-order guards never have to infer it by id.
                .with_ink_token("--scene-underlay-role")];
            if self.text_scale < 200 {
                content.push(heading);
            }
            content.extend([
                node(
                    "hero-title",
                    Role::Heading,
                    focused.map_or("Nothing ready", |item| item.title.as_str()),
                    48.0,
                    vertical.title_y,
                    w - 96.0,
                    hero_title_height,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Hero)
                .with_line_height(1.04)
                .with_ink_token(COLOR_TEXT_PRIMARY_TOKEN),
                node(
                    "hero-status",
                    Role::Text,
                    &hero_status,
                    48.0,
                    vertical.status_y,
                    hero_status_width,
                    hero_status_height,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Label)
                .with_ink_token(COLOR_STATUS_READY_TOKEN),
            ]);
            let attention = self.attention_message.as_deref().or_else(|| {
                (self.presentation == Presentation::ForcedClose)
                    .then_some("The previous game didn't close cleanly")
            });
            if let Some(message) = attention {
                // Caption glyph shaping needs the same 22px control box used by the
                // chrome keycaps; the CSS padding remains outside this line box.
                let rem_scale = f32::from(self.text_scale) / 100.0;
                let pill_label_height = ATTENTION_PILL_LABEL_HEIGHT * rem_scale;
                let pill_height = pill_label_height + 2.0 * ATTENTION_PILL_VERTICAL_PADDING;
                let dot_size = ATTENTION_PILL_DOT_SIZE * rem_scale;
                // Start-aligned text paints after the renderer's inline inset. Compensate
                // the layout gap so the visible dot-to-glyph gap, not the control-box gap,
                // resolves to space-2 at every text scale.
                let pill_gap = SPACE_2 * rem_scale - TEXT_NODE_INLINE_INSET;
                let mut pill_border = node(
                    "attention-pill-border",
                    Role::Group,
                    "",
                    0.0,
                    ATTENTION_PILL_TOP,
                    0.0,
                    pill_height,
                    COLOR_BORDER_HAIRLINE_TOKEN,
                );
                pill_border.layout = Some(LayoutStyle {
                    position: Position::Absolute,
                    align_items: Some(AlignItems::Center),
                    gap: (LayoutValue::Px(0.0), LayoutValue::Px(pill_gap)),
                    padding: px_edges(
                        ATTENTION_PILL_VERTICAL_PADDING,
                        ATTENTION_PILL_HORIZONTAL_PADDING,
                        ATTENTION_PILL_VERTICAL_PADDING,
                        ATTENTION_PILL_HORIZONTAL_PADDING,
                    ),
                    inset: Edges {
                        top: LayoutValue::Px(ATTENTION_PILL_TOP),
                        right: LayoutValue::Px(ATTENTION_PILL_RIGHT_MARGIN),
                        bottom: LayoutValue::Auto,
                        left: LayoutValue::Auto,
                    },
                    height: LayoutValue::Px(pill_height),
                    ..LayoutStyle::default()
                });
                let mut pill_fill = node(
                    "attention-pill",
                    Role::Group,
                    "",
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    COLOR_SURFACE_RAISED_TOKEN,
                );
                pill_fill.layout = Some(LayoutStyle {
                    position: Position::Absolute,
                    inset: px_edges(1.0, 1.0, 1.0, 1.0),
                    ..LayoutStyle::default()
                });
                let mut dot = node(
                    "attention-dot",
                    Role::Group,
                    "",
                    0.0,
                    0.0,
                    dot_size,
                    dot_size,
                    COLOR_STATUS_ATTENTION_TOKEN,
                );
                dot.layout = Some(fixed_layout(dot_size, dot_size));
                let mut text = node(
                    "attention",
                    Role::Text,
                    message,
                    0.0,
                    0.0,
                    0.0,
                    pill_label_height,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Caption)
                .with_ink_token(COLOR_TEXT_SECONDARY_TOKEN);
                text.layout = Some(LayoutStyle {
                    height: LayoutValue::Px(pill_label_height),
                    // Production shaping owns intrinsic measurement. Preserve the legacy
                    // caption reserve as a minimum so the seam cannot move the anchored
                    // capsule's left cap by a subpixel on this conversion.
                    min_width: LayoutValue::Px(caption_text_width(message, self.text_scale)),
                    flex_shrink: 0.0,
                    ..LayoutStyle::default()
                });
                pill_border.children = vec![pill_fill, dot, text];
                let mut anchor = node(
                    "attention-layout-anchor",
                    Role::Group,
                    "",
                    0.0,
                    0.0,
                    w,
                    STATUS_BAR_HEIGHT,
                    SCENE_TRANSPARENT_TOKEN,
                );
                anchor.layout = Some(fixed_layout(w, STATUS_BAR_HEIGHT));
                anchor.children.push(pill_border);
                content.push(anchor);
            }
            let ready_items = self
                .items
                .iter()
                .filter(|item| matches!(best_availability(item), Availability::Ready))
                .take(HOME_SHELF_LIMIT)
                .collect::<Vec<_>>();
            let ready_count = ready_items.len();
            let shelf_label_height = scaled_text_box_height(28.0, self.text_scale);
            content.push(
                node(
                    "home-shelf-label",
                    Role::Heading,
                    &format!("READY NOW · {ready_count}"),
                    48.0,
                    vertical.shelf_label_y,
                    220.0,
                    shelf_label_height,
                    COLOR_SURFACE_CANVAS_TOKEN,
                )
                .with_type_role(TypeRole::Eyebrow),
            );
            let shelf_count = ready_items.len();
            let card_width = CARD_ART_WIDTH;
            let horizontal_margin = 48.0;
            let available = (w - horizontal_margin * 2.0).max(0.0);
            let mut visible_count = 1;
            let mut occupied = card_width;
            while visible_count < shelf_count
                && horizontal_margin * 2.0 + occupied + HOME_SHELF_GAP + card_width <= w
            {
                visible_count += 1;
                occupied += HOME_SHELF_GAP + card_width;
            }
            let first_visible = self
                .focus
                .saturating_sub(visible_count - 1)
                .min(shelf_count.saturating_sub(visible_count));
            let gap = if visible_count > 1 {
                (available - card_width * visible_count as f32) / (visible_count - 1) as f32
            } else {
                0.0
            };
            for (i, item) in ready_items
                .into_iter()
                .skip(first_visible)
                .take(visible_count)
                .enumerate()
            {
                let availability = best_availability(item);
                let x = horizontal_margin + i as f32 * (card_width + gap);
                let mut n = node(
                    &format!("item-{}", item.id),
                    Role::ListItem,
                    &item.title,
                    x,
                    vertical.card_row_y,
                    card_width,
                    vertical.card_height,
                    COLOR_SURFACE_CANVAS_TOKEN,
                );
                n.action = Some(NodeAction::Activate);
                n.state.focused = i + first_visible == self.focus;
                n.children = art_nodes(
                    item,
                    "home-card",
                    x,
                    vertical.card_row_y,
                    card_width,
                    CARD_ART_HEIGHT,
                    i == self.focus,
                    self.text_scale,
                );
                add_unavailable_card_cues(
                    &mut n.children,
                    item,
                    availability,
                    "home-card",
                    x,
                    vertical.card_row_y,
                    card_width,
                    CARD_ART_HEIGHT,
                    Some(h - PROMPTS_AREA_HEIGHT),
                    self.text_scale,
                    vertical.show_card_caption,
                );
                if item.favorite {
                    let (pin_width, pin_height) =
                        scaled_centered_text_box(20.0, 20.0, self.text_scale);
                    let pin_center_x = x + card_width - 18.0;
                    let pin_center_y = vertical.card_row_y + 20.0;
                    n.children.push(
                        node(
                            &format!("favorite-pin-{}", item.id),
                            Role::Text,
                            "★",
                            pin_center_x - pin_width / 2.0,
                            pin_center_y - pin_height / 2.0,
                            pin_width,
                            pin_height,
                            COLOR_SURFACE_SCRIM_TOKEN,
                        )
                        .with_type_role(TypeRole::Caption),
                    );
                }
                content.push(n);
            }
            // The chrome row can grow past its nominal 64 px anchor when status text
            // is enlarged. Start the opaque Home content region at that derived
            // bottom so it cannot repaint scaled chrome; this is identical at 100%.
            let chrome_bottom = chrome_row_bottom(metrics.safe_insets.top, self.text_scale);
            out.push(Node::new(
                NodeId::new("home-scroll-region").unwrap(),
                Role::Group,
                "",
                Bounds::new(
                    0.0,
                    chrome_bottom,
                    w,
                    h - chrome_bottom - PROMPTS_AREA_HEIGHT,
                ),
                COLOR_SURFACE_CANVAS_TOKEN,
            ));
            out.extend(content);
        } else if self.route == Route::Library {
            let geometry = library_geometry(w);
            // The chrome row is laid out inside the safe area and grows with status
            // text. Derive every Library row from that scaled edge; the subtractions
            // preserve the existing 100%/zero-inset geometry exactly.
            let chrome_bottom = chrome_row_bottom(metrics.safe_insets.top, self.text_scale);
            let library_head_top = chrome_bottom + (LIB_HEAD_TOP - STATUS_BAR_HEIGHT);
            let compact_toolbar_top =
                chrome_bottom + (COMPACT_LIBRARY_TOOLBAR_TOP - STATUS_BAR_HEIGHT);
            let games = self
                .items
                .iter()
                .filter(|item| matches!(item.kind, AppKind::Game))
                .count();
            let other = self.items.len() - games;
            let compact_toolbar = geometry.columns < 6;
            let filters = [
                ("Recent".to_owned(), None, LibraryFilter::Recent),
                ("A–Z".to_owned(), None, LibraryFilter::Alphabetical),
                ("Games".to_owned(), Some(games), LibraryFilter::Games),
                (
                    "Everything else".to_owned(),
                    Some(other),
                    LibraryFilter::EverythingElse,
                ),
            ];
            let chip_widths = filters
                .iter()
                .map(|(label, count, _)| scaled_library_chip_width(label, *count, self.text_scale))
                .collect::<Vec<_>>();
            let required_toolbar_width = chip_widths.iter().sum::<f32>()
                + (geometry.toolbar_columns - 1) as f32 * LIBRARY_TOOLBAR_GAP;
            let search_width = if compact_toolbar {
                w - 2.0 * LIBRARY_SIDE_MARGIN
            } else {
                (w - 2.0 * LIBRARY_SIDE_MARGIN - LIBRARY_TOOLBAR_GAP - required_toolbar_width)
                    .max(LIBRARY_SEARCH_MIN_WIDTH)
            };
            let search_height = scaled_text_box_height(LIB_TOOLBAR_HEIGHT, self.text_scale);
            let mut search = node(
                "library-search",
                Role::Button,
                &format!("⌕  Search {} titles", self.items.len()),
                LIBRARY_SIDE_MARGIN,
                library_head_top,
                search_width,
                search_height,
                STATE_REST_SURFACE_TOKEN,
            );
            search.state.focused = self.focus == 0;
            search.action = Some(NodeAction::Activate);
            search.children.push(
                node(
                    "library-search-placeholder",
                    Role::Text,
                    &format!("⌕  Search {} titles", self.items.len()),
                    LIBRARY_SIDE_MARGIN + SPACE_4,
                    library_head_top + 8.0,
                    search_width - 32.0,
                    scaled_text_box_height(28.0, self.text_scale),
                    STATE_REST_SURFACE_TOKEN,
                )
                .with_type_role(TypeRole::Label),
            );
            out.push(search);
            for (index, (full_label, count, filter)) in filters.into_iter().enumerate() {
                let toolbar_left = if compact_toolbar {
                    LIBRARY_SIDE_MARGIN
                } else {
                    LIBRARY_SIDE_MARGIN + search_width + LIBRARY_TOOLBAR_GAP
                };
                let toolbar_top = if compact_toolbar {
                    compact_toolbar_top
                } else {
                    library_head_top
                };
                let toolbar_width = if compact_toolbar {
                    w - 2.0 * LIBRARY_SIDE_MARGIN
                } else {
                    w - toolbar_left - LIBRARY_SIDE_MARGIN
                };
                let chip_width = if compact_toolbar {
                    (toolbar_width - (geometry.toolbar_columns - 1) as f32 * geometry.card_gap)
                        / geometry.toolbar_columns as f32
                } else {
                    chip_widths[index]
                };
                let chip_column = index % geometry.toolbar_columns;
                let chip_row = index / geometry.toolbar_columns;
                let chip_x = if compact_toolbar {
                    toolbar_left + chip_column as f32 * (chip_width + geometry.card_gap)
                } else {
                    toolbar_left
                        + chip_widths[..index].iter().sum::<f32>()
                        + index as f32 * LIBRARY_TOOLBAR_GAP
                };
                let focused = self.focus == index + 1;
                let active = self.library_filter == filter;
                let chip_height = scaled_text_box_height(CHIP_HEIGHT, self.text_scale);
                let chip_row_gap = LIBRARY_TOOLBAR_ROW_GAP;
                let chip_padding = if compact_toolbar && self.text_scale == 200 {
                    0.0
                } else {
                    CHIP_HORIZONTAL_PADDING
                };
                let count_width = count.map_or(0.0, |value| {
                    measured_text_advance(label_text_width(&value.to_string()), self.text_scale)
                });
                let count_box_width = text_node_box_width(count_width);
                let available_label_width = (chip_width
                    - 2.0 * chip_padding
                    - count.map_or(0.0, |_| CHIP_COUNT_GAP + count_box_width))
                .max(0.0);
                let label_width = if self.text_scale == 100 {
                    label_text_width(&full_label) + 20.0
                } else {
                    available_label_width
                };
                let painted_label =
                    scale_aware_single_line(&full_label, available_label_width, self.text_scale);
                let mut chip = node(
                    &format!("library-filter-{index}"),
                    Role::Button,
                    &count.map_or_else(
                        || full_label.clone(),
                        |count| format!("{full_label} · {count}"),
                    ),
                    chip_x,
                    toolbar_top + chip_row as f32 * (chip_height + chip_row_gap),
                    chip_width,
                    chip_height,
                    STATE_REST_SURFACE_TOKEN,
                );
                chip.state.focused = focused;
                chip.state.selected = active;
                chip.action = Some(NodeAction::Activate);
                if active {
                    for (edge, x, y, width, height) in [
                        (
                            "top",
                            chip.bounds.x,
                            chip.bounds.y,
                            chip.bounds.width,
                            CHIP_BORDER_WIDTH,
                        ),
                        (
                            "right",
                            chip.bounds.x + chip.bounds.width - CHIP_BORDER_WIDTH,
                            chip.bounds.y,
                            CHIP_BORDER_WIDTH,
                            chip.bounds.height,
                        ),
                        (
                            "bottom",
                            chip.bounds.x,
                            chip.bounds.y + chip.bounds.height - CHIP_BORDER_WIDTH,
                            chip.bounds.width,
                            CHIP_BORDER_WIDTH,
                        ),
                        (
                            "left",
                            chip.bounds.x,
                            chip.bounds.y,
                            CHIP_BORDER_WIDTH,
                            chip.bounds.height,
                        ),
                    ] {
                        chip.children.push(node(
                            &format!("library-filter-{index}-border-{edge}"),
                            Role::Group,
                            "",
                            x,
                            y,
                            width,
                            height,
                            COLOR_BORDER_STRONG_TOKEN,
                        ));
                    }
                }
                chip.children.push({
                    let mut label_node = node(
                        &format!("library-filter-{index}-label"),
                        Role::Text,
                        &painted_label,
                        chip.bounds.x + chip_padding,
                        chip.bounds.y + 5.0,
                        label_width,
                        if self.text_scale == 100 {
                            26.0
                        } else {
                            scaled_text_box_height(28.0, self.text_scale)
                        },
                        chip.style_token.as_str(),
                    )
                    .with_type_role(TypeRole::Label);
                    label_node.state.focused = focused;
                    label_node
                });
                if let Some(count) = count {
                    chip.children.push({
                        let mut count_node = node(
                            &format!("library-filter-{index}-count"),
                            Role::Text,
                            &count.to_string(),
                            chip.bounds.x + chip_width - chip_padding - count_box_width,
                            chip.bounds.y + 5.0,
                            count_box_width,
                            if self.text_scale == 100 {
                                26.0
                            } else {
                                scaled_text_box_height(28.0, self.text_scale)
                            },
                            chip.style_token.as_str(),
                        )
                        .with_type_role(TypeRole::Label);
                        count_node.state.focused = focused;
                        count_node
                    });
                }
                out.push(chip);
                if active {
                    out.push(node(
                        &format!("library-selected-underline-{index}"),
                        Role::Group,
                        "",
                        chip_x + CHIP_BORDER_WIDTH,
                        toolbar_top + chip_row as f32 * (chip_height + chip_row_gap) + chip_height
                            - 3.0,
                        chip_width - 2.0 * CHIP_BORDER_WIDTH,
                        3.0,
                        STATE_SELECTED_ACCENT_TOKEN,
                    ));
                }
            }
            let library_title_height = scaled_text_box_height(34.0, self.text_scale);
            let library_cue_slot_height = if self.text_scale == 100 {
                0.0
            } else {
                scaled_text_box_height(28.0, self.text_scale) + CARD_CAPTION_GAP
            };
            let card_top =
                chrome_bottom + (geometry.scaled_card_top(self.text_scale) - STATUS_BAR_HEIGHT);
            // Compact enlarged-text cells have less room below the derived toolbar.
            // Keep the complete label chain above the prompt bar by yielding image
            // height; the default cell retains the design-token art height exactly.
            let library_card_art_height = LIB_CARD_ART_HEIGHT.min(
                (h - PROMPTS_AREA_HEIGHT
                    - card_top
                    - CARD_LABEL_GAP
                    - library_cue_slot_height
                    - library_title_height)
                    .max(0.0),
            );
            let row_height = library_card_art_height
                + CARD_LABEL_GAP
                + library_cue_slot_height
                + library_title_height
                + SPACE_5;
            let mut visible_rows: usize = 1;
            let card_content_height = library_card_art_height
                + CARD_LABEL_GAP
                + library_cue_slot_height
                + library_title_height;
            while if geometry.columns == 6
                && self.text_scale == 100
                && metrics.safe_insets.top == 0.0
            {
                // Preserve the desktop mockup's zero-inset two-row crop exactly.
                card_top + visible_rows as f32 * row_height < h - PROMPTS_AREA_HEIGHT
            } else {
                // Once chrome consumes a top inset, admit only rows whose complete
                // derived content remains above the fixed prompt area.
                card_top + visible_rows as f32 * row_height + card_content_height
                    <= h - PROMPTS_AREA_HEIGHT
            } {
                visible_rows += 1;
            }
            let focused_row = self.focus.saturating_sub(5) / geometry.columns;
            let first_visible_row = focused_row.saturating_sub(visible_rows.saturating_sub(1));
            for (i, &item_index) in self.library_items.iter().enumerate() {
                let column = i % geometry.columns;
                let row = i / geometry.columns;
                if row < first_visible_row || row >= first_visible_row + visible_rows {
                    continue;
                }
                let item = &self.items[item_index];
                let availability = best_availability(item);
                let card_y = card_top + (row as f32 - first_visible_row as f32) * row_height;
                let mut card = node(
                    &format!("library-item-{}", item.id),
                    Role::ListItem,
                    &item.title,
                    geometry.card_left + column as f32 * (geometry.card_width + geometry.card_gap),
                    card_y,
                    geometry.card_width,
                    card_content_height,
                    COLOR_SURFACE_CANVAS_TOKEN,
                );
                card.state.focused = self.focus == i + 5;
                // Availability is painted explicitly as an art veil plus badge so the status
                // ink itself remains legible instead of being dimmed with the cover.
                card.state.unavailable = false;
                card.action = Some(NodeAction::Activate);
                card.children = art_nodes(
                    item,
                    "library-card",
                    card.bounds.x,
                    card.bounds.y,
                    geometry.card_width,
                    library_card_art_height,
                    self.focus == i + 5,
                    self.text_scale,
                );
                add_unavailable_card_cues(
                    &mut card.children,
                    item,
                    availability,
                    "library-card",
                    card.bounds.x,
                    card.bounds.y,
                    geometry.card_width,
                    library_card_art_height,
                    None,
                    self.text_scale,
                    true,
                );
                card.children.retain(|child| {
                    !child.id.as_str().contains("-title-")
                        && !child.id.as_str().contains("-label-mask-")
                });
                if self.focus == i + 5
                    && let Some(art) = card
                        .children
                        .iter_mut()
                        .find(|child| child.id.as_str() == format!("library-card-art-{}", item.id))
                {
                    art.style_token = STATE_FOCUSED_RING_TOKEN.into();
                    art.state.focused = true;
                }
                if item.favorite {
                    let (pin_width, pin_height) =
                        scaled_centered_text_box(20.0, 20.0, self.text_scale);
                    let pin_center_x = card.bounds.x + geometry.card_width - 18.0;
                    let pin_center_y = card.bounds.y + 18.0;
                    card.children.push(
                        node(
                            &format!("favorite-pin-{}", item.id),
                            Role::Text,
                            "★",
                            pin_center_x - pin_width / 2.0,
                            pin_center_y - pin_height / 2.0,
                            pin_width,
                            pin_height,
                            COLOR_SURFACE_SCRIM_TOKEN,
                        )
                        .with_type_role(TypeRole::Caption),
                    );
                }
                let title_y = card.bounds.y
                    + library_card_art_height
                    + CARD_LABEL_GAP
                    + library_cue_slot_height;
                card.children.push(
                    node(
                        &format!("library-title-{}", item.id),
                        Role::Text,
                        &scale_aware_single_line(&item.title, geometry.card_width, self.text_scale),
                        card.bounds.x,
                        title_y,
                        geometry.card_width,
                        library_title_height,
                        COLOR_SURFACE_CANVAS_TOKEN,
                    )
                    .with_type_role(TypeRole::Label),
                );
                out.push(card);
            }
            out.push(
                node(
                    "library-grid-footer-fade",
                    Role::Group,
                    "",
                    0.0,
                    h - 96.0,
                    w,
                    96.0,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_image(library_footer_fade_source(w), ImageFit::Cover)
                // Explicit scene-level declaration: this gradient intentionally
                // overlays the final row of library art and labels.
                .with_ink_token("--scene-overlay-role"),
            );
        } else if self.route == Route::Search {
            let scale = f32::from(self.text_scale) / 100.0;
            let column_width = (w - 2.0 * SPACE_7).min(800.0);
            let column_left = (w - column_width) / 2.0;
            let search_top = chrome_row_bottom(metrics.safe_insets.top, self.text_scale) + SPACE_5;
            let search_height = 52.0 * scale;
            let mut search_box = node(
                "search-query",
                Role::Text,
                &format!("{}│", self.search_query),
                column_left,
                search_top,
                column_width,
                search_height,
                COLOR_SURFACE_RAISED_TOKEN,
            )
            .with_type_role(TypeRole::Label)
            .with_corner_radius(RADIUS_M * scale)
            .with_elevation(Elevation::Elev2);
            search_box.state.focused = self.search_results.is_empty();
            if search_box.state.focused {
                search_box.border_token = Some(STATE_FOCUSED_RING_TOKEN.into());
                search_box.border_width = 2.0;
                search_box.action = Some(NodeAction::Custom("Search".into()));
            } else {
                search_box.border_token = Some(COLOR_BORDER_HAIRLINE_TOKEN.into());
                search_box.border_width = 1.0;
            }
            out.push(search_box);
            let hint_top = search_top + search_height + SPACE_2 * scale;
            let hint_height = scaled_text_box_height(24.0, self.text_scale);
            out.push(
                node(
                    "search-hint",
                    Role::Text,
                    "SEARCH · Titles and tags · Back returns to where you were",
                    column_left,
                    hint_top,
                    column_width,
                    hint_height,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Caption)
                .with_ink_token(COLOR_TEXT_MUTED_TOKEN),
            );
            if self.search_results.is_empty() {
                out.push(node(
                    "search-empty",
                    Role::Text,
                    if self.items.is_empty() {
                        "Your shelf is empty — nothing to search yet."
                    } else {
                        "Nothing matches — check the spelling, or browse the Library."
                    },
                    column_left,
                    hint_top + hint_height + SPACE_4 * scale,
                    column_width,
                    70.0,
                    COLOR_TEXT_SECONDARY_TOKEN,
                ));
            }
            let rows_top = hint_top + hint_height + SPACE_4 * scale;
            let rows_bottom = h
                - PROMPTS_AREA_HEIGHT.max(scaled_text_box_height(32.0, self.text_scale))
                - SPACE_3;
            let row_height = 44.0 * scale;
            let row_gap = SPACE_3 * scale;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let capacity = (((rows_bottom - rows_top + row_gap) / (row_height + row_gap))
                .floor()
                .max(1.0)) as usize;
            let first = self
                .focus
                .saturating_sub(capacity.saturating_sub(1))
                .min(self.search_results.len().saturating_sub(capacity));
            let mut results_region = node(
                "search-results-scroll-region",
                Role::Group,
                "Search results",
                column_left,
                rows_top,
                column_width,
                (rows_bottom - rows_top).max(0.0),
                SCENE_TRANSPARENT_TOKEN,
            );
            for (result, &item_index) in self
                .search_results
                .iter()
                .enumerate()
                .skip(first)
                .take(capacity)
            {
                let item = &self.items[item_index];
                let availability = best_availability(item);
                let result_top = rows_top + (result - first) as f32 * (row_height + row_gap);
                let title = item.title.as_str();
                let caption = format!(
                    " · {} · {}",
                    kind_text(&item.kind),
                    availability_text(availability, &self.presentation)
                );
                let text_left = column_left + SPACE_4 * scale;
                let content_width = (column_width - 2.0 * SPACE_4 * scale).max(0.0);
                let inter_gap = SPACE_2 * scale;
                let title_floor = 72.0 * scale;
                let caption_natural_width = caption_text_width(&caption, self.text_scale)
                    + 2.0 * TEXT_NODE_INLINE_INSET * scale;
                let caption_reserve =
                    caption_natural_width.min((content_width - inter_gap - title_floor).max(0.0));
                // Keep the established 42% column at roomy widths, but reserve the
                // caption before allowing long titles to grow. The gap lives at the
                // end of the title column so existing wide evidence keeps its geometry.
                let title_column_width = (column_width * 0.42)
                    .max(
                        measured_text_advance(label_text_width(title), self.text_scale)
                            + 2.0 * TEXT_NODE_INLINE_INSET * scale
                            + inter_gap,
                    )
                    .min((content_width - caption_reserve).max(0.0));
                let title_paint_width = (title_column_width - inter_gap).max(0.0);
                let caption_width =
                    (column_left + column_width - text_left - title_column_width).max(0.0);
                let painted_title = ellipsize_to_lines(
                    title,
                    title_paint_width * 100.0 / f32::from(self.text_scale),
                    1,
                );
                let painted_caption = ellipsize_to_lines(
                    &caption,
                    caption_width * 100.0 / f32::from(self.text_scale),
                    1,
                );
                let mut row = node(
                    &format!("search-result-{}", item.id),
                    Role::Button,
                    &format!("{title}{caption}"),
                    column_left,
                    result_top,
                    column_width,
                    row_height,
                    STATE_REST_SURFACE_TOKEN,
                )
                .with_type_role(TypeRole::Label)
                .with_corner_radius(RADIUS_M * scale)
                .with_ink_token(SCENE_TRANSPARENT_TOKEN)
                .with_children(vec![
                    node(
                        &format!("search-result-{}-title", item.id),
                        Role::Text,
                        &painted_title,
                        text_left,
                        result_top,
                        title_paint_width,
                        row_height,
                        SCENE_TRANSPARENT_TOKEN,
                    )
                    .with_type_role(TypeRole::Label)
                    .with_ink_token(COLOR_TEXT_PRIMARY_TOKEN),
                    node(
                        &format!("search-result-{}-caption", item.id),
                        Role::Text,
                        &painted_caption,
                        text_left + title_column_width,
                        result_top,
                        caption_width,
                        row_height,
                        SCENE_TRANSPARENT_TOKEN,
                    )
                    .with_type_role(TypeRole::Caption)
                    .with_ink_token(COLOR_TEXT_MUTED_TOKEN),
                ]);
                row.state.focused = self.focus == result;
                if row.state.focused {
                    row.border_token = Some(STATE_FOCUSED_RING_TOKEN.into());
                    row.border_width = 2.0;
                }
                row.action = Some(NodeAction::Activate);
                results_region.children.push(row);
            }
            out.push(results_region);
        } else if matches!(self.route, Route::Details | Route::VariantChooser) {
            let Some(item_index) = self.selected_item else {
                return;
            };
            let item = &self.items[item_index];
            let first_variant = item.variants.first();
            let provenance = first_variant.map_or_else(
                || format!("{} · Source unavailable", sentence_kind(&item.kind)),
                |variant| detail_provenance_text(&item.kind, variant),
            );
            let compact = library_geometry(w).columns < 6;
            let detail_wrap_top = 96.0;
            let cover_left = 48.0;
            let cover_top = detail_wrap_top;
            let cover_width = 320.0;
            let cover_height = 428.0;
            let detail_column_left = cover_left + cover_width + 48.0;
            let detail_column_width = w - detail_column_left - 48.0;
            let provenance_height = scaled_text_box_height(30.0, self.text_scale);
            let provenance_top = detail_wrap_top;
            out.push(
                node(
                    "detail-provenance",
                    Role::Text,
                    &provenance,
                    detail_column_left,
                    provenance_top,
                    detail_column_width,
                    provenance_height,
                    COLOR_SURFACE_CANVAS_TOKEN,
                )
                .with_type_role(TypeRole::Caption)
                .with_ink_token(COLOR_TEXT_MUTED_TOKEN),
            );
            let title_top = provenance_top + provenance_height + 6.0;
            let title_height = scaled_text_box_height(66.0, self.text_scale);
            out.push(
                node(
                    "detail-title",
                    Role::Heading,
                    &item.title,
                    detail_column_left,
                    title_top,
                    detail_column_width,
                    title_height,
                    COLOR_SURFACE_CANVAS_TOKEN,
                )
                .with_type_role(TypeRole::Hero)
                .with_line_height(1.04)
                .with_ink_token(COLOR_TEXT_PRIMARY_TOKEN),
            );
            let mut cover = node(
                "detail-cover",
                Role::Group,
                &format!("Cover for {}", item.title),
                cover_left,
                cover_top,
                cover_width,
                cover_height,
                COLOR_SURFACE_RAISED_TOKEN,
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
                self.text_scale,
            );
            out.push(cover);
            let detail_availability = best_availability(item);
            let availability = if matches!(detail_availability, Availability::Ready) {
                "● Ready".to_owned()
            } else {
                format!(
                    "⊘ {}",
                    availability_text(detail_availability, &self.presentation)
                )
            };
            let availability = if matches!(detail_availability, Availability::Ready) {
                [item.last_played_fact.as_deref(), item.size_fact.as_deref()]
                    .into_iter()
                    .flatten()
                    .fold(availability, |status, fact| format!("{status} · {fact}"))
            } else {
                availability
            };
            let availability_top = title_top + title_height + 6.0;
            let availability_height = scaled_text_box_height(30.0, self.text_scale);
            let mut availability_node = node(
                "detail-availability-reason",
                Role::Text,
                &availability,
                detail_column_left,
                availability_top,
                detail_column_width,
                availability_height,
                COLOR_SURFACE_CANVAS_TOKEN,
            );
            availability_node.state.unavailable =
                !matches!(detail_availability, Availability::Ready);
            out.push(availability_node);
            let description_top = availability_top + availability_height + 4.0;
            let description_height = item
                .description
                .as_ref()
                .map(|_| scaled_text_box_height(50.0, self.text_scale));
            if let Some(description) = item.description.as_deref() {
                out.push(
                    declared_multiline(node(
                        "detail-description",
                        Role::Text,
                        description,
                        detail_column_left,
                        description_top,
                        detail_column_width,
                        description_height.unwrap(),
                        COLOR_SURFACE_CANVAS_TOKEN,
                    ))
                    .with_line_height(1.55)
                    .with_ink_token(COLOR_TEXT_SECONDARY_TOKEN),
                );
            }
            let ways_heading_top = description_top + description_height.unwrap_or_default();
            let ways_heading_height = scaled_text_box_height(28.0, self.text_scale);
            out.push(
                node(
                    "detail-ways-heading",
                    Role::Heading,
                    "WAYS TO PLAY",
                    detail_column_left,
                    ways_heading_top,
                    detail_column_width,
                    ways_heading_height,
                    COLOR_SURFACE_CANVAS_TOKEN,
                )
                .with_type_role(TypeRole::Eyebrow),
            );
            let ready = self.ready_variants(item_index);
            let active_ready_variant = self.active_ready_variant(item_index);
            let variant_row_height = 66.0;
            let variant_row_gap = 7.0;
            let variant_rows_top = ways_heading_top + ways_heading_height + 4.0;
            let visible_detail_variants = self.detail_visible_variants(item_index);
            if self.route == Route::Details {
                for (display_index, &variant_index) in visible_detail_variants.iter().enumerate() {
                    let variant = &item.variants[variant_index];
                    let (variant_name, variant_sub) =
                        if matches!(variant.availability, Availability::Ready) {
                            let capability_copy = match ready_variant_capability(variant) {
                                ReadyVariantCapability::Native => " · works offline",
                                ReadyVariantCapability::Stream
                                | ReadyVariantCapability::Unknown => "",
                            };
                            (
                                ready_variant_label(variant),
                                format!(
                                    "{}{}{}",
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
                                    },
                                    capability_copy,
                                ),
                            )
                        } else {
                            (
                                format!("⊘ {}", humanize_identifier(&variant.id)),
                                availability_text(&variant.availability, &self.presentation),
                            )
                        };
                    let variant_focus = visible_detail_variants[..=display_index]
                        .iter()
                        .filter(|&&index| {
                            matches!(item.variants[index].availability, Availability::Ready)
                        })
                        .count()
                        .checked_sub(1);
                    let focused = matches!(variant.availability, Availability::Ready)
                        && variant_focus == Some(self.focus);
                    let variant_accessible_label =
                        if matches!(variant.availability, Availability::Ready) {
                            format!("{variant_name} · {variant_sub}")
                        } else {
                            variant_name.clone()
                        };
                    let mut variant_node = node(
                        &format!("detail-variant-{variant_index}"),
                        if matches!(variant.availability, Availability::Ready) {
                            Role::Button
                        } else {
                            Role::Text
                        },
                        &variant_accessible_label,
                        detail_column_left,
                        variant_rows_top
                            + display_index as f32 * (variant_row_height + variant_row_gap),
                        detail_column_width,
                        variant_row_height,
                        STATE_REST_SURFACE_TOKEN,
                    );
                    variant_node.state.focused = focused;
                    variant_node.state.unavailable =
                        !matches!(variant.availability, Availability::Ready);
                    let selected = Some(variant_index) == active_ready_variant;
                    variant_node.state.selected = selected;
                    variant_node = variant_node.with_border(
                        if focused || selected {
                            STATE_SELECTED_ACCENT_TOKEN
                        } else {
                            COLOR_BORDER_HAIRLINE_TOKEN
                        },
                        1.0,
                    );
                    let text_token = STATE_REST_SURFACE_TOKEN;
                    variant_node.children = vec![
                        node(
                            &format!("detail-variant-{variant_index}-name"),
                            Role::Text,
                            &variant_name,
                            detail_column_left + 16.0,
                            variant_node.bounds.y + 6.0,
                            detail_column_width - 64.0,
                            26.0,
                            text_token,
                        ),
                        node(
                            &format!("detail-variant-{variant_index}-sub"),
                            Role::Text,
                            &variant_sub,
                            detail_column_left + 16.0,
                            variant_node.bounds.y + 34.0,
                            detail_column_width - 64.0,
                            24.0,
                            STATE_REST_SURFACE_TOKEN,
                        ),
                    ];
                    if variant_node.state.selected {
                        variant_node.children.push(
                            node(
                                &format!("detail-variant-{variant_index}-selection-mark"),
                                Role::Text,
                                "✓",
                                detail_column_left + detail_column_width - 40.0,
                                variant_node.bounds.y + 19.0,
                                24.0,
                                28.0,
                                STATE_REST_SURFACE_TOKEN,
                            )
                            .with_text_align(TextAlign::Center)
                            .with_ink_token(STATE_SELECTED_ACCENT_TOKEN),
                        );
                    }
                    for label in &mut variant_node.children {
                        label.state.focused = focused;
                    }
                    if matches!(variant.availability, Availability::Ready) {
                        variant_node.action = Some(NodeAction::Activate);
                    }
                    out.push(variant_node);
                }
                if item.variants.len() > visible_detail_variants.len() {
                    out.push(node(
                        "detail-variant-fold",
                        Role::Text,
                        &format!(
                            "+{} more ways to play",
                            item.variants.len() - visible_detail_variants.len()
                        ),
                        detail_column_left,
                        variant_rows_top
                            + visible_detail_variants.len() as f32
                                * (variant_row_height + variant_row_gap),
                        detail_column_width,
                        28.0,
                        COLOR_TEXT_MUTED_TOKEN,
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
                    COLOR_SURFACE_CANVAS_TOKEN,
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
                    COLOR_SURFACE_CANVAS_TOKEN,
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
                            STATE_FOCUSED_RING_TOKEN
                        } else {
                            STATE_REST_SURFACE_TOKEN
                        },
                    );
                    row.state.focused = self.focus == choice;
                    row.action = Some(NodeAction::Activate);
                    out.push(row);
                }
            } else {
                let actions_bottom = if let Some(_ready_variant) = ready.first() {
                    let variants_bottom = variant_rows_top
                        + visible_detail_variants.len() as f32
                            * (variant_row_height + variant_row_gap)
                        + if item.variants.len() > visible_detail_variants.len() {
                            35.0
                        } else {
                            0.0
                        };
                    let button_gap = 12.0;
                    let stack_buttons = compact && detail_column_width < 336.0;
                    let buttons_top = variants_bottom.max(detail_wrap_top + 334.0);
                    let play_focus = self.detail_play_focus().unwrap_or(0);
                    let open_label = if ready.len() == 1 {
                        "▶ Play"
                    } else {
                        "Choose how to play"
                    };
                    let open_width =
                        (measured_text_advance(label_text_width(open_label), self.text_scale)
                            + 48.0)
                            .min(detail_column_width);
                    let mut open = node(
                        "detail-open",
                        Role::Button,
                        open_label,
                        detail_column_left,
                        buttons_top,
                        open_width,
                        54.0,
                        STATE_SELECTED_ACCENT_TOKEN,
                    );
                    open.state.focused = self.focus == play_focus;
                    open.action = Some(NodeAction::Activate);
                    let mut open_label_node = node(
                        "detail-open-label",
                        Role::Text,
                        &open.accessible_label,
                        open.bounds.x + 16.0,
                        open.bounds.y + 13.0,
                        open.bounds.width - 32.0,
                        28.0,
                        STATE_SELECTED_ACCENT_TOKEN,
                    )
                    .with_type_role(TypeRole::Label)
                    .with_ink_token(COLOR_TEXT_INVERSE_TOKEN);
                    open_label_node.state.focused = open.state.focused;
                    open.children.push(open_label_node);
                    out.push(open);
                    let pin_label = if item.favorite {
                        "★ Unpin"
                    } else {
                        "★ Pin to favorites"
                    };
                    let pin_width =
                        (measured_text_advance(label_text_width(pin_label), self.text_scale)
                            + 48.0)
                            .min(detail_column_width);
                    let mut pin = node(
                        "detail-pin",
                        Role::Button,
                        pin_label,
                        if stack_buttons {
                            detail_column_left
                        } else {
                            detail_column_left + open_width + button_gap
                        },
                        if stack_buttons {
                            buttons_top + 54.0 + button_gap
                        } else {
                            buttons_top
                        },
                        pin_width,
                        54.0,
                        if self.focus == self.detail_pin_focus() {
                            STATE_FOCUSED_RING_TOKEN
                        } else {
                            STATE_REST_SURFACE_TOKEN
                        },
                    );
                    pin.state.focused = self.focus == self.detail_pin_focus();
                    pin.action = Some(NodeAction::Activate);
                    pin.children.push(
                        node(
                            "detail-pin-label",
                            Role::Text,
                            pin_label,
                            pin.bounds.x + 16.0,
                            pin.bounds.y + 13.0,
                            pin.bounds.width - 32.0,
                            28.0,
                            STATE_REST_SURFACE_TOKEN,
                        )
                        .with_type_role(TypeRole::Label),
                    );
                    out.push(pin);
                    buttons_top + if stack_buttons { 124.0 } else { 54.0 }
                } else {
                    let mut unavailable = node(
                        "detail-unavailable",
                        Role::Text,
                        "No launch action is available",
                        detail_column_left,
                        detail_wrap_top + 318.0,
                        detail_column_width,
                        60.0,
                        STATE_UNAVAILABLE_TEXT_TOKEN,
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
                        detail_wrap_top + 388.0,
                        detail_column_width,
                        54.0,
                        STATE_FOCUSED_RING_TOKEN,
                    );
                    pin.state.focused = true;
                    pin.action = Some(NodeAction::Activate);
                    pin.children.push(
                        node(
                            "detail-pin-label",
                            Role::Text,
                            &pin.accessible_label,
                            pin.bounds.x + 16.0,
                            pin.bounds.y + 13.0,
                            pin.bounds.width - 32.0,
                            28.0,
                            STATE_REST_SURFACE_TOKEN,
                        )
                        .with_type_role(TypeRole::Label),
                    );
                    out.push(pin);
                    detail_wrap_top + 442.0
                };
                let block_gap = 16.0;
                let block_height = 54.0;
                let mut flow_top = actions_bottom + block_gap;
                let footer_top = h - PROMPTS_AREA_HEIGHT;
                if let Some(variant) = ready
                    .first()
                    .map(|&variant_index| &item.variants[variant_index])
                    .filter(|_| !compact && flow_top + block_height <= footer_top)
                {
                    let mut facts = vec![(
                        "developer",
                        "DEVELOPER",
                        item.developer.clone().unwrap_or_else(|| {
                            humanize_identifier(&variant.provenance.provider_id)
                        }),
                    )];
                    if ready_variant_capability(variant) == ReadyVariantCapability::Native {
                        facts.push((
                            "installed",
                            "INSTALLED",
                            variant.provenance.app_version.as_deref().map_or_else(
                                || "Current version".to_owned(),
                                |version| format!("Version {version}"),
                            ),
                        ));
                    }
                    if let Some(playtime) = self.playtime.get(&item.id).copied() {
                        facts.push(("time-played", "TIME PLAYED", format_playtime(playtime)));
                    }
                    if ready_variant_capability(variant) == ReadyVariantCapability::Native {
                        facts.push(("offline", "WORKS OFFLINE", "Yes".to_owned()));
                    }
                    out.push(node(
                        "detail-facts-top-border",
                        Role::Group,
                        "",
                        detail_column_left,
                        flow_top,
                        detail_column_width,
                        1.0,
                        COLOR_BORDER_HAIRLINE_TOKEN,
                    ));
                    flow_top += 16.0;
                    let fact_width = detail_column_width / facts.len() as f32;
                    for (column, (id, eyebrow, value)) in facts.into_iter().enumerate() {
                        let left = detail_column_left + column as f32 * fact_width;
                        let value_id = if id == "time-played" {
                            "detail-playtime".to_owned()
                        } else {
                            format!("detail-fact-{id}")
                        };
                        out.push(
                            node(
                                &format!("detail-fact-{id}-heading"),
                                Role::Heading,
                                eyebrow,
                                left,
                                flow_top,
                                fact_width - 8.0,
                                22.0,
                                COLOR_SURFACE_CANVAS_TOKEN,
                            )
                            .with_type_role(TypeRole::Eyebrow),
                        );
                        out.push(node(
                            &value_id,
                            Role::Text,
                            &value,
                            left,
                            flow_top + 26.0,
                            fact_width - 8.0,
                            28.0,
                            COLOR_SURFACE_CANVAS_TOKEN,
                        ));
                    }
                } else if let Some(playtime) = self.playtime.get(&item.id).copied()
                    && flow_top + block_height <= footer_top
                {
                    out.push(node(
                        "detail-facts-top-border",
                        Role::Group,
                        "",
                        detail_column_left,
                        flow_top,
                        detail_column_width,
                        1.0,
                        COLOR_BORDER_HAIRLINE_TOKEN,
                    ));
                    out.push(
                        node(
                            "detail-time-played-heading",
                            Role::Heading,
                            "TIME PLAYED",
                            detail_column_left,
                            flow_top + 16.0,
                            detail_column_width,
                            22.0,
                            COLOR_SURFACE_CANVAS_TOKEN,
                        )
                        .with_type_role(TypeRole::Eyebrow),
                    );
                    out.push(node(
                        "detail-playtime",
                        Role::Text,
                        &format_playtime(playtime),
                        detail_column_left,
                        flow_top + 42.0,
                        detail_column_width,
                        28.0,
                        COLOR_SURFACE_CANVAS_TOKEN,
                    ));
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
                        STATE_FOCUSED_RING_TOKEN
                    } else {
                        STATE_REST_SURFACE_TOKEN
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
        let nav_scale_delta = f32::from(self.text_scale) / 100.0 - 1.0;
        let scaled_nav_width = nav_width + 72.0 * nav_scale_delta;
        let rooms = self.settings_rooms();
        if !portrait || !self.settings_in_rows {
            let nav_left = 32.0;
            let nav_top = 168.0;
            let nav_label_height = 30.0 + 16.0 * nav_scale_delta;
            let nav_label_inset = 12.0 - 8.0 * nav_scale_delta;
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
                    name,
                    nav_left,
                    nav_top + index as f32 * 62.0,
                    scaled_nav_width - 32.0,
                    50.0,
                    if focused {
                        STATE_REST_SURFACE_TOKEN
                    } else {
                        COLOR_SURFACE_RAISED_TOKEN
                    },
                );
                nav.state.focused = focused;
                nav.state.selected = selected && !focused;
                nav.action = Some(NodeAction::Activate);
                let mut nav_label = node(
                    &format!("settings-nav-{}-label", name.to_ascii_lowercase()),
                    Role::Text,
                    &format!("{} {name}", if selected { "▌" } else { " " }),
                    nav.bounds.x + nav_label_inset,
                    nav.bounds.y + (nav.bounds.height - nav_label_height) / 2.0,
                    nav.bounds.width - 2.0 * nav_label_inset,
                    nav_label_height,
                    if focused {
                        STATE_REST_SURFACE_TOKEN
                    } else {
                        COLOR_SURFACE_RAISED_TOKEN
                    },
                );
                nav_label.state.focused = focused;
                nav.children.push(nav_label);
                out.push(nav);
            }
            if portrait && !self.settings_in_rows {
                return;
            }
        }

        let content_left = if portrait {
            32.0
        } else {
            scaled_nav_width + 56.0
        };
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
            COLOR_SURFACE_CANVAS_TOKEN,
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
                COLOR_SURFACE_CANVAS_TOKEN,
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
            COLOR_SURFACE_CANVAS_TOKEN,
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
                &row.accessible_label,
                content_left,
                rows_top + (index - first) as f32 * (row_height + row_gap),
                content_width,
                row_height,
                if interactive {
                    STATE_REST_SURFACE_TOKEN
                } else {
                    STATE_DISABLED_BORDER_TOKEN
                },
            );
            // Value editing is represented by the value affordance, never by
            // painting a focused surface across the whole row.
            scene_row.state.focused = false;
            if interactive {
                scene_row.action = Some(NodeAction::Activate);
            }
            let row_surface = if interactive {
                STATE_REST_SURFACE_TOKEN
            } else {
                STATE_DISABLED_BORDER_TOKEN
            };
            let lines = row.label.lines().collect::<Vec<_>>();
            let row_line_height = scaled_text_box_height(24.0, self.text_scale);
            let row_line_top = 7.0 * scale;
            let row_line_step = 25.0 * scale;
            let mut fills = Vec::new();
            let mut text = Vec::new();
            for (line_index, line) in lines.iter().take(2).enumerate() {
                let mut label = node(
                    &format!("settings-row-{}-line-{line_index}", row.id),
                    Role::Text,
                    line,
                    content_left + 16.0,
                    scene_row.bounds.y + row_line_top + line_index as f32 * row_line_step,
                    content_width - 150.0,
                    row_line_height,
                    row_surface,
                );
                // Single-line source/status notices are prose, not compact row
                // labels. At enlarged text scales they intentionally wrap within
                // the row and carry the scene's explicit multiline declaration.
                if !interactive && lines.len() == 1 && self.text_scale > 100 {
                    label.bounds.height = row_height - 2.0 * row_line_top;
                    label = declared_multiline(label);
                } else if line_index > 0 && self.text_scale > 100 {
                    // Settings descriptions are prose and may wrap as their effective
                    // font grows. Give the declared multiline node the remainder of
                    // its scaled row instead of clipping it to one nominal line box.
                    label.bounds.height =
                        row_height - row_line_top - line_index as f32 * row_line_step;
                    label = declared_multiline(label);
                }
                label = label.with_ink_token(if line_index == 0 {
                    COLOR_TEXT_PRIMARY_TOKEN
                } else {
                    COLOR_TEXT_SECONDARY_TOKEN
                });
                text.push(label);
            }
            if row.id == "accessibility-textScale" {
                let selected_value = lines
                    .last()
                    .and_then(|line| line.rsplit_once(" · "))
                    .map_or("100%", |(_, effective)| effective);
                let value_widths = ["100%", "150%", "200%"]
                    .map(|value| settings_scaled_box_width(56.0, value, self.text_scale));
                let segment_widths = value_widths.map(|width| (width + 16.0).max(72.0));
                let control_width = segment_widths.iter().sum::<f32>();
                let control_left = content_left + content_width - 24.0 - control_width;
                let control_top = scene_row.bounds.y + 20.0 * scale;
                let control_height = scaled_text_box_height(34.0, self.text_scale);
                fills.push(node(
                    "settings-text-scale-segmented-control",
                    Role::Group,
                    "",
                    control_left,
                    control_top,
                    control_width,
                    control_height,
                    SCENE_TRANSPARENT_TOKEN,
                ));
                for (segment, value) in ["100%", "150%", "200%"].into_iter().enumerate() {
                    let selected = selected_value == value;
                    let x = control_left + segment_widths[..segment].iter().sum::<f32>();
                    let segment_width = segment_widths[segment];
                    let value_width = value_widths[segment];
                    let mut chip = node(
                        &format!("settings-text-scale-chip-{value}"),
                        Role::Group,
                        "",
                        x + 2.0,
                        control_top,
                        segment_width - 4.0,
                        control_height,
                        if selected {
                            STATE_SELECTED_ACCENT_TOKEN
                        } else {
                            STATE_REST_SURFACE_TOKEN
                        },
                    )
                    .with_corner_radius(RADIUS_S * scale)
                    .with_border(
                        if focused && selected {
                            STATE_FOCUSED_RING_TOKEN
                        } else {
                            COLOR_BORDER_HAIRLINE_TOKEN
                        },
                        1.0,
                    );
                    chip.state.selected = selected;
                    fills.push(chip);
                    let mut value_node = node(
                        &format!("settings-text-scale-value-{value}"),
                        Role::Text,
                        value,
                        x + (segment_width - value_width) / 2.0,
                        scene_row.bounds.y + 24.0 * scale,
                        value_width,
                        scaled_text_box_height(26.0, self.text_scale),
                        if selected {
                            STATE_SELECTED_ACCENT_TOKEN
                        } else {
                            STATE_REST_SURFACE_TOKEN
                        },
                    )
                    .with_ink_token(if selected {
                        COLOR_TEXT_INVERSE_TOKEN
                    } else {
                        COLOR_TEXT_PRIMARY_TOKEN
                    });
                    value_node.state.focused = false;
                    text.push(value_node);
                }
            } else if row.id.starts_with("accessibility-")
                && lines
                    .last()
                    .is_some_and(|line| line.starts_with("ON") || line.starts_with("OFF"))
            {
                let on = lines.last().is_some_and(|line| line.starts_with("ON"));
                let state = if on { "ON" } else { "OFF" };
                let state_width = settings_scaled_box_width(44.0, state, self.text_scale);
                let toggle_gap = 8.0 * scale;
                let track_width = 58.0 * scale;
                let track_height = 28.0 * scale;
                let knob_size = 20.0 * scale;
                let knob_inset = 4.0 * scale;
                let control_right = content_left + content_width - 26.0;
                let control_left = control_right - state_width - toggle_gap - track_width;
                let group_center_y = scene_row.bounds.y + scene_row.bounds.height / 2.0;
                let state_height = if self.text_scale == 100 {
                    // Extending the transparent bottom of the text box preserves
                    // the approved raster while bringing its scene centerline
                    // within 1 px of the established track and knob offsets.
                    28.0
                } else {
                    scaled_text_box_height(26.0, self.text_scale)
                };
                let state_top = if self.text_scale == 100 {
                    scene_row.bounds.y + 24.0
                } else {
                    group_center_y - state_height / 2.0
                };
                let track_top = if self.text_scale == 100 {
                    scene_row.bounds.y + 25.0
                } else {
                    group_center_y - track_height / 2.0
                };
                let knob_top = if self.text_scale == 100 {
                    scene_row.bounds.y + 29.0
                } else {
                    group_center_y - knob_size / 2.0
                };
                let mut state_node = node(
                    &format!("settings-toggle-{}-state", row.id),
                    Role::Text,
                    state,
                    control_left,
                    state_top,
                    state_width,
                    state_height,
                    row_surface,
                );
                state_node.state.focused = focused;
                text.push(state_node);
                let track_left = control_left + state_width + toggle_gap;
                let track_token = if on {
                    STATE_SELECTED_ACCENT_TOKEN
                } else {
                    COLOR_SURFACE_SUNKEN_TOKEN
                };
                fills.push(node(
                    &format!("settings-toggle-{}-track", row.id),
                    Role::Group,
                    "",
                    track_left,
                    track_top,
                    track_width,
                    track_height,
                    track_token,
                ));
                let knob_left = control_left
                    + state_width
                    + toggle_gap
                    + if on {
                        track_width - knob_inset - knob_size
                    } else {
                        knob_inset
                    };
                let knob_token = if on {
                    COLOR_TEXT_INVERSE_TOKEN
                } else {
                    COLOR_TEXT_PRIMARY_TOKEN
                };
                fills.push(node(
                    &format!("settings-toggle-{}-knob", row.id),
                    Role::Group,
                    "",
                    knob_left,
                    knob_top,
                    knob_size,
                    knob_size,
                    knob_token,
                ));
            } else if let Some(control) = lines.get(2) {
                let appearance = row.id == "display-appearance";
                let control_width = if appearance {
                    text_node_box_width(settings_value_advance(control, self.text_scale))
                        .max(104.0 * scale)
                } else {
                    104.0 * scale
                };
                let control_height = if appearance {
                    scaled_text_box_height(34.0, self.text_scale)
                } else {
                    scaled_text_box_height(26.0, self.text_scale) + 8.0 * (scale - 1.0)
                };
                let control_top = if appearance {
                    scene_row.bounds.y + (scene_row.bounds.height - control_height) / 2.0
                } else {
                    scene_row.bounds.y + 24.0 * scale
                };
                let mut control_node = node(
                    &format!("settings-row-{}-control", row.id),
                    Role::Text,
                    control,
                    content_left + content_width - 16.0 - control_width,
                    control_top,
                    control_width,
                    control_height,
                    row_surface,
                );
                control_node.state.focused = focused;
                text.push(control_node);
            }
            scene_row.children.extend(fills);
            scene_row.children.extend(text);
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
                STATE_REST_TEXT_TOKEN,
            ));
            let mut fields = node(
                "manual-time-fields",
                Role::Button,
                &self.manual_time_picker.label(),
                48.0,
                180.0,
                w - 96.0,
                74.0,
                STATE_FOCUSED_RING_TOKEN,
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
                COLOR_TEXT_SECONDARY_TOKEN,
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
                STATE_FOCUSED_RING_TOKEN,
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
                COLOR_TEXT_SECONDARY_TOKEN,
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
                    STATE_REST_SURFACE_TOKEN
                } else if i == self.focus {
                    STATE_FOCUSED_RING_TOKEN
                } else {
                    STATE_REST_SURFACE_TOKEN
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
                COLOR_TEXT_SECONDARY_TOKEN,
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
                        STATE_FOCUSED_RING_TOKEN
                    } else {
                        STATE_REST_SURFACE_TOKEN
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
                    COLOR_TEXT_SECONDARY_TOKEN,
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
                    COLOR_TEXT_SECONDARY_TOKEN,
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
                    COLOR_TEXT_SECONDARY_TOKEN,
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
                    COLOR_TEXT_SECONDARY_TOKEN,
                ));
            }
        }
    }

    fn first_run_nodes(&self, out: &mut Vec<Node>, w: f32) {
        out.push(node(
            "first-run-panel",
            Role::Group,
            "",
            w / 2.0 - 364.0,
            32.0,
            728.0,
            584.0,
            COLOR_SURFACE_SCRIM_TOKEN,
        ));
        out.push(node(
            "first-run-title",
            Role::Heading,
            "FIRST RUN · Make it comfortable",
            w / 2.0 - 340.0,
            54.0,
            680.0,
            56.0,
            COLOR_SURFACE_SCRIM_TOKEN,
        ));
        out.push(declared_multiline(node(
            "first-run-copy",
            Role::Text,
            "All of this lives in Settings → Accessibility and can change any time.",
            w / 2.0 - 340.0,
            112.0,
            680.0,
            scaled_text_box_height(48.0, self.text_scale),
            COLOR_SURFACE_SCRIM_TOKEN,
        )));
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
                    STATE_FOCUSED_RING_TOKEN
                } else {
                    STATE_REST_SURFACE_TOKEN
                },
            );
            n.state.focused = i == self.focus;
            n.action = Some(NodeAction::Activate);
            out.push(n);
        }
        out.push(declared_multiline(node(
            "safe-return-teach",
            Role::Text,
            &format!("{} returns you here.", self.safe_return_binding),
            w / 2.0 - 340.0,
            470.0,
            680.0,
            scaled_text_box_height(48.0, self.text_scale),
            COLOR_SURFACE_CANVAS_TOKEN,
        )));
        let mut continue_node = node(
            "continue",
            Role::Button,
            "Continue · START",
            w / 2.0 - 340.0,
            540.0,
            680.0,
            54.0,
            if self.focus == rows.len() {
                STATE_FOCUSED_RING_TOKEN
            } else {
                STATE_REST_SURFACE_TOKEN
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
                STATE_REST_TEXT_TOKEN,
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
                        STATE_FOCUSED_RING_TOKEN
                    } else {
                        STATE_REST_SURFACE_TOKEN
                    },
                );
                button.state.focused = self.focus == index;
                button.action = Some(NodeAction::Activate);
                out.push(button);
            }
            return;
        }
        let scale = f32::from(self.text_scale) / 100.0;
        let panel_width = (360.0 * scale).min(w - 2.0 * SPACE_5);
        let panel_left = w - SPACE_5 - panel_width;
        let panel_top = chrome_row_bottom(0.0, self.text_scale) + SPACE_5;
        let panel_bottom = h - PROMPTS_AREA_HEIGHT - SPACE_5;
        let panel_height = panel_bottom - panel_top;
        let content_left = panel_left + SPACE_5;
        let content_width = panel_width - 2.0 * SPACE_5;
        let row_height = scaled_text_box_height(40.0, self.text_scale).min(52.0);
        let preferred_gap = SPACE_3;
        let mut panel = node(
            "quick-panel-surface",
            Role::Group,
            "Quick actions",
            panel_left,
            panel_top,
            panel_width,
            panel_height,
            COLOR_SURFACE_RAISED_TOKEN,
        )
        .with_border(COLOR_BORDER_HAIRLINE_TOKEN, 1.0)
        .with_elevation(Elevation::Elev2);
        panel.state.expanded = true;
        out.push(panel);

        let mut push_row = |id: &str, index: usize, label: &str, y: f32, enabled: bool| {
            let focused = index == self.focus;
            let mut row = node(
                id,
                Role::Button,
                label,
                content_left,
                y,
                content_width,
                row_height,
                STATE_REST_SURFACE_TOKEN,
            )
            .with_border(
                if focused {
                    STATE_FOCUSED_RING_TOKEN
                } else {
                    COLOR_BORDER_HAIRLINE_TOKEN
                },
                if focused { 2.0 } else { 1.0 },
            );
            row.state.focused = focused;
            row.state.disabled = !enabled;
            if enabled {
                row.action = Some(NodeAction::Activate);
            }
            let label_height = scaled_text_box_height(24.0, self.text_scale).min(row_height);
            row.children.push(
                node(
                    &format!("{id}-label"),
                    Role::Text,
                    label,
                    content_left + SPACE_4,
                    y + (row_height - label_height) / 2.0,
                    content_width - 2.0 * SPACE_4,
                    label_height,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Label)
                .with_ink_token(if focused {
                    STATE_FOCUSED_TEXT_TOKEN
                } else {
                    STATE_REST_TEXT_TOKEN
                }),
            );
            out.push(row);
        };

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

        // Quick is a fixed-height transient sheet. Preserve its preferred rhythm when
        // possible, then yield spacing uniformly before dropping reassurance copy.
        let status_height = self
            .power_status
            .as_ref()
            .map(|_| scaled_text_box_height(32.0, self.text_scale));
        let truth_height = scaled_text_box_height(40.0, self.text_scale);
        let row_count = rows.len() as f32 + 3.0; // two contextual rows + screenshot
        let gap_count = rows.len() as f32 + 4.0 + f32::from(status_height.is_some());
        let fixed_height = row_count * row_height + status_height.unwrap_or(0.0) + truth_height;
        let stack_top = panel_top + SPACE_5;
        let budget_bottom = panel_bottom - SPACE_5;
        let preferred_bottom = stack_top + fixed_height + gap_count * preferred_gap;
        let gap_floor = preferred_gap / 2.0;
        let gap = (preferred_gap - (preferred_bottom - budget_bottom).max(0.0) / gap_count)
            .max(gap_floor);
        let show_truth = stack_top + fixed_height + gap_count * gap <= budget_bottom;

        // Intentionally no title: §4.2/§4.7 makes the first contextual action the top edge.
        for (i, label) in ["Open focused item", "Browse the library"]
            .iter()
            .enumerate()
        {
            push_row(
                &format!("quick-{i}"),
                i,
                label,
                panel_top + SPACE_5 + i as f32 * (row_height + gap),
                true,
            );
        }
        out.push(node(
            "quick-section-divider",
            Role::Group,
            "",
            content_left,
            panel_top + SPACE_5 + 2.0 * (row_height + gap),
            content_width,
            1.0,
            COLOR_BORDER_HAIRLINE_TOKEN,
        ));
        let system_top = panel_top + SPACE_5 + 2.0 * (row_height + gap) + gap;
        let mut system_position = 0_usize;
        for (index, id, label) in rows {
            let enabled = match index {
                2 => self.supports_power(PowerAction::PowerOff),
                3 => self.supports_power(PowerAction::Restart),
                _ => true,
            };
            let y = system_top + system_position as f32 * (row_height + gap);
            let mut row = node(
                &format!("quick-power-{id}"),
                Role::Button,
                label,
                content_left,
                y,
                content_width,
                row_height,
                STATE_REST_SURFACE_TOKEN,
            )
            .with_border(
                if index == self.focus {
                    STATE_FOCUSED_RING_TOKEN
                } else {
                    COLOR_BORDER_HAIRLINE_TOKEN
                },
                if index == self.focus { 2.0 } else { 1.0 },
            );
            row.state.focused = index == self.focus;
            row.state.disabled = !enabled;
            if enabled {
                row.action = Some(NodeAction::Activate);
            }
            let (primary, value) = label.split_once(" · ").unwrap_or((label, ""));
            let label_height = scaled_text_box_height(24.0, self.text_scale).min(row_height);
            let mut label_node = node(
                &format!("quick-power-{id}-label"),
                Role::Text,
                primary,
                content_left + SPACE_4,
                y + (row_height - label_height) / 2.0,
                content_width - 2.0 * SPACE_4,
                label_height,
                SCENE_TRANSPARENT_TOKEN,
            )
            .with_type_role(TypeRole::Label)
            .with_ink_token(if !enabled {
                STATE_UNAVAILABLE_TEXT_TOKEN
            } else if index == self.focus {
                STATE_FOCUSED_TEXT_TOKEN
            } else {
                STATE_REST_TEXT_TOKEN
            });
            label_node.state.disabled = !enabled;
            row.children.push(label_node);
            if !value.is_empty() {
                let value_width = text_node_box_width(caption_text_width(value, self.text_scale));
                let mut value_node = node(
                    &format!("quick-power-{id}-value"),
                    Role::Text,
                    value,
                    content_left + content_width - SPACE_4 - value_width,
                    y + (row_height - label_height) / 2.0,
                    value_width,
                    label_height,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Caption)
                .with_ink_token(if enabled {
                    COLOR_TEXT_MUTED_TOKEN
                } else {
                    STATE_UNAVAILABLE_TEXT_TOKEN
                });
                value_node.state.disabled = !enabled;
                row.children.push(value_node);
            }
            out.push(row);
            system_position += 1;
        }
        let screenshot_index = self.screenshot_row();
        let mut screenshot = node(
            "quick-capture-screenshot",
            Role::Button,
            "Capture screenshot",
            content_left,
            system_top + system_position as f32 * (row_height + gap),
            content_width,
            row_height,
            STATE_REST_SURFACE_TOKEN,
        )
        .with_border(
            if screenshot_index == self.focus {
                STATE_FOCUSED_RING_TOKEN
            } else {
                COLOR_BORDER_HAIRLINE_TOKEN
            },
            if screenshot_index == self.focus {
                2.0
            } else {
                1.0
            },
        );
        screenshot.state.focused = screenshot_index == self.focus;
        screenshot.action = Some(NodeAction::Activate);
        let screenshot_y = system_top + system_position as f32 * (row_height + gap);
        let label_height = scaled_text_box_height(24.0, self.text_scale).min(row_height);
        screenshot.children.push(
            node(
                "quick-capture-screenshot-label",
                Role::Text,
                "Capture screenshot",
                content_left + SPACE_4,
                screenshot_y + (row_height - label_height) / 2.0,
                content_width - 2.0 * SPACE_4,
                label_height,
                SCENE_TRANSPARENT_TOKEN,
            )
            .with_type_role(TypeRole::Label)
            .with_ink_token(if screenshot_index == self.focus {
                STATE_FOCUSED_TEXT_TOKEN
            } else {
                STATE_REST_TEXT_TOKEN
            }),
        );
        out.push(screenshot);
        let mut note_y = screenshot_y + row_height + gap;
        if let Some(status) = &self.power_status {
            let status_height = status_height.expect("power status height must be measured");
            out.push(node(
                "quick-power-status",
                Role::Text,
                status,
                content_left,
                note_y,
                content_width,
                status_height,
                COLOR_STATUS_ATTENTION_TOKEN,
            ));
            note_y += status_height + gap;
        }
        if show_truth {
            out.push(declared_multiline(
                node(
                    "quick-truth",
                    Role::Text,
                    "Nothing is running now. Quick shows only what applies right here.",
                    content_left,
                    note_y,
                    content_width,
                    truth_height,
                    SCENE_TRANSPARENT_TOKEN,
                )
                .with_type_role(TypeRole::Caption)
                .with_ink_token(COLOR_TEXT_MUTED_TOKEN),
            ));
        }
    }
    fn crash_nodes(&self, out: &mut Vec<Node>, w: f32, _h: f32) {
        out.push(node(
            "receipt-panel",
            Role::Group,
            "",
            152.0,
            72.0,
            w - 304.0,
            552.0,
            COLOR_SURFACE_RAISED_TOKEN,
        ));
        out.push(node(
            "crash-eyebrow",
            Role::Text,
            "⚠ Closed unexpectedly",
            180.0,
            100.0,
            w - 360.0,
            40.0,
            COLOR_STATUS_ATTENTION_TOKEN,
        ));
        out.push(node(
            "crash-title",
            Role::Heading,
            &self.active_title,
            180.0,
            150.0,
            w - 360.0,
            54.0,
            STATE_REST_TEXT_TOKEN,
        ));
        out.push(node("crash-copy", Role::Text, &format!("{} stopped on its own and the shelf took the screen back. Nothing else was affected, and it's ready to open again.", self.active_title), 180.0, 220.0, w - 360.0, 70.0, COLOR_TEXT_SECONDARY_TOKEN));
        out.push(node(
            "crash-facts",
            Role::Text,
            &format!("Session · Ended · What happened · {}", self.crash_summary),
            180.0,
            310.0,
            w - 360.0,
            50.0,
            COLOR_STATUS_ATTENTION_TOKEN,
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
            COLOR_TEXT_SECONDARY_TOKEN,
        ));
        out.push(node("crash-honesty", Role::Text, "This record stays on the device — there's nowhere it gets sent, so there's no Report button to press.", 180.0, 420.0, w - 360.0, 60.0, COLOR_TEXT_SECONDARY_TOKEN));
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
                    STATE_FOCUSED_RING_TOKEN
                } else {
                    STATE_REST_SURFACE_TOKEN
                },
            );
            n.state.focused = i == self.focus;
            n.action = Some(NodeAction::Activate);
            out.push(n);
        }
    }
}

#[allow(dead_code)]
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
    text_scale: u16,
) -> Vec<Node> {
    type ScenicLayer = (&'static str, f32, f32, f32, f32);
    // Catalog providers namespace item identities (for example,
    // `installed-applications:ridgeline`).  Art direction belongs to the app identity,
    // not the provider projection, so fixture and production catalog paths resolve the
    // same authored composition.
    let composition_id = id.rsplit(':').next().unwrap_or(id);
    let hash = composition_id
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
        });
    let (motif, scenic_layers): (String, Vec<ScenicLayer>) = match composition_id {
        "ridgeline" => (
            "    ●".into(),
            vec![
                ("--deco-plate-b-bg", 0.00, 0.48, 1.00, 0.52),
                (COLOR_SURFACE_SCRIM_TOKEN, 0.00, 0.64, 0.72, 0.36),
                (COLOR_SURFACE_RAISED_TOKEN, 0.36, 0.72, 0.64, 0.28),
            ],
        ),
        "hollow-tides" => (
            "≈  ◒  ≈".into(),
            vec![
                ("--deco-plate-e-bg", 0.00, 0.52, 1.00, 0.48),
                (COLOR_SURFACE_SCRIM_TOKEN, 0.00, 0.72, 1.00, 0.28),
                (COLOR_SURFACE_RAISED_TOKEN, 0.42, 0.63, 0.24, 0.07),
                (COLOR_SURFACE_RAISED_TOKEN, 0.53, 0.55, 0.03, 0.16),
            ],
        ),
        "sunwake" => (
            "  ☼\n⌁   ⌁".into(),
            vec![
                ("--deco-plate-a-bg", 0.00, 0.58, 1.00, 0.42),
                (COLOR_SURFACE_RAISED_TOKEN, 0.00, 0.78, 1.00, 0.22),
            ],
        ),
        "glass-harbor" => (
            String::new(),
            vec![
                ("--deco-plate-f-bg", 0.00, 0.46, 1.00, 0.54),
                (COLOR_SURFACE_SCRIM_TOKEN, 0.00, 0.68, 1.00, 0.32),
                (COLOR_SURFACE_RAISED_TOKEN, 0.13, 0.48, 0.08, 0.30),
                (COLOR_SURFACE_RAISED_TOKEN, 0.76, 0.42, 0.07, 0.36),
                ("--deco-plate-c-bg", 0.44, 0.58, 0.13, 0.10),
            ],
        ),
        "lantern-vale" => (
            "·  ✦  ·\n  ·  ·".into(),
            vec![
                (COLOR_SURFACE_SCRIM_TOKEN, 0.00, 0.56, 1.00, 0.44),
                ("--deco-plate-d-bg", 0.38, 0.30, 0.24, 0.42),
                (COLOR_SURFACE_RAISED_TOKEN, 0.46, 0.70, 0.08, 0.30),
            ],
        ),
        "paper-comet" => (
            "✦  ·  ·\n  ╲".into(),
            vec![
                ("--deco-plate-c-bg", 0.00, 0.62, 1.00, 0.38),
                (COLOR_SURFACE_RAISED_TOKEN, 0.00, 0.82, 1.00, 0.18),
                (COLOR_SURFACE_SCRIM_TOKEN, 0.56, 0.22, 0.08, 0.54),
                ("--deco-plate-a-bg", 0.18, 0.47, 0.30, 0.06),
            ],
        ),
        _ => {
            let hash_bytes = hash.to_le_bytes();
            let left = f32::from(hash_bytes[1]) / 510.0;
            let top = 0.18 + f32::from(hash_bytes[2]) / 850.0;
            let width = 0.18 + f32::from(hash_bytes[3]) / 640.0;
            (
                format!(
                    "·  {}  ·",
                    ['◆', '●', '✦', '◒'][usize::from(hash_bytes[0] >> 4) & 3]
                ),
                vec![
                    (COLOR_SURFACE_SCRIM_TOKEN, 0.0, 0.68, 1.0, 0.32),
                    (COLOR_SURFACE_RAISED_TOKEN, left, top, width, 0.16),
                ],
            )
        }
    };
    let token = match composition_id {
        "ridgeline" => "--deco-plate-a-bg",
        "hollow-tides" => "--deco-plate-b-bg",
        "sunwake" => "--deco-plate-c-bg",
        "glass-harbor" => "--deco-plate-d-bg",
        "lantern-vale" => "--deco-plate-e-bg",
        "paper-comet" => "--deco-plate-f-bg",
        _ => [
            "--deco-plate-a-bg",
            "--deco-plate-b-bg",
            "--deco-plate-c-bg",
            "--deco-plate-d-bg",
            "--deco-plate-e-bg",
            "--deco-plate-f-bg",
        ][(hash % 6) as usize],
    };
    let home = context == "home-card";
    let favorite = context == "favorite-card";
    let detail = context == "detail-art";
    let kind_y = if favorite {
        y + 8.0
    } else if detail {
        y + art_height - 68.0
    } else {
        y + 142.0
    };
    let label_y = if home {
        y + 166.0
    } else if favorite {
        y + 32.0
    } else if detail {
        y + art_height - 36.0
    } else {
        y + 176.0
    };
    let title = if home {
        scale_aware_single_line(title, width, text_scale)
    } else {
        title.to_owned()
    };
    let title_height = if home {
        scaled_text_box_height(28.0, text_scale)
    } else {
        28.0
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
    ];
    let scene_height = if detail {
        art_height
    } else if home {
        158.0
    } else if favorite {
        64.0
    } else {
        136.0
    };
    nodes.extend(scenic_layers.iter().enumerate().map(
        |(index, (layer_token, left, top, layer_width, layer_height))| {
            node(
                &format!("{context}-scene-{index}-{id}"),
                Role::Group,
                "",
                art_x + art_width * left,
                (if favorite { y + 4.0 } else { y }) + scene_height * top,
                art_width * layer_width,
                scene_height * layer_height,
                layer_token,
            )
        },
    ));
    if !motif.is_empty() {
        nodes.push(node(
            &format!("{context}-motif-{id}"),
            Role::Text,
            &motif,
            art_x,
            if favorite { y + 4.0 } else { y },
            art_width,
            if favorite { 28.0 } else { 60.0 },
            COLOR_SURFACE_CANVAS_TOKEN,
        ));
    }
    if let Some(edition) = edition.filter(|_| !home) {
        nodes.push(
            node(
                &format!("{context}-plate-{id}"),
                Role::Text,
                edition,
                if favorite { x + 68.0 } else { x + 12.0 },
                kind_y,
                if favorite { width - 72.0 } else { width - 24.0 },
                if favorite { 20.0 } else { 24.0 },
                COLOR_SURFACE_RAISED_TOKEN,
            )
            .with_type_role(TypeRole::Eyebrow),
        );
    }
    if detail {
        nodes.push(node(
            &format!("{context}-title-scrim-{id}"),
            Role::Group,
            "",
            x,
            label_y,
            width,
            28.0,
            COLOR_SURFACE_SCRIM_TOKEN,
        ));
    }
    nodes.push(
        node(
            &format!("{context}-title-{id}"),
            Role::Text,
            &title,
            if favorite { x + 68.0 } else { x },
            label_y,
            if favorite { width - 72.0 } else { width },
            if favorite { 32.0 } else { title_height },
            if detail {
                COLOR_SURFACE_SCRIM_TOKEN
            } else if context == "home-card" {
                COLOR_SURFACE_CANVAS_TOKEN
            } else if focused {
                STATE_FOCUSED_TEXT_TOKEN
            } else {
                COLOR_TEXT_SECONDARY_TOKEN
            },
        )
        .with_type_role(TypeRole::Label),
    );
    nodes
}

fn plate_art_nodes(
    item: &Item,
    context: &str,
    x: f32,
    y: f32,
    width: f32,
    art_height: f32,
    text_scale: u16,
) -> Vec<Node> {
    let identity = item.id.rsplit(':').next().unwrap_or(&item.id);
    let hash = identity
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
        });
    let favorite = context == "favorite-card";
    let detail = context == "detail-art";
    let art_x = if favorite { x + 4.0 } else { x };
    let art_y = if favorite { y + 4.0 } else { y };
    let art_width = if favorite { 56.0 } else { width };
    let kind = item
        .tags
        .iter()
        .find_map(|tag| tag.strip_prefix("kind-label:"))
        .unwrap_or_else(|| sentence_kind(&item.kind))
        .to_uppercase();
    let initial = item.title.chars().next().unwrap_or('?').to_string();
    let (plate_name, plate_bytes, plate_ink): (&str, &'static [u8], &str) = match identity {
        "steam-link" => (
            "plate-a",
            include_bytes!("../../pf-shell/fixtures/art/plate-a.png"),
            "--deco-plate-a-fg",
        ),
        "tidelines" => (
            "plate-d",
            include_bytes!("../../pf-shell/fixtures/art/plate-d.png"),
            "--deco-plate-d-fg",
        ),
        "button-tester" => (
            "plate-c",
            include_bytes!("../../pf-shell/fixtures/art/plate-c.png"),
            "--deco-plate-c-fg",
        ),
        _ => [
            (
                "plate-a",
                &include_bytes!("../../pf-shell/fixtures/art/plate-a.png")[..],
                "--deco-plate-a-fg",
            ),
            (
                "plate-d",
                &include_bytes!("../../pf-shell/fixtures/art/plate-d.png")[..],
                "--deco-plate-d-fg",
            ),
            (
                "plate-c",
                &include_bytes!("../../pf-shell/fixtures/art/plate-c.png")[..],
                "--deco-plate-c-fg",
            ),
        ][(hash % 3) as usize],
    };
    let mut art = node(
        &format!("{context}-art-{}", item.id),
        Role::Group,
        &format!("{} {plate_name} identity plate", item.title),
        art_x,
        art_y,
        art_width,
        art_height,
        COLOR_SURFACE_RAISED_TOKEN,
    );
    art = art.with_image(
        ImageSource::new(format!("fixture-art:{plate_name}.png"), plate_bytes),
        ImageFit::Cover,
    );
    let mut nodes = vec![art];
    let home = context == "home-card";
    let scale_aware_card = !favorite && !detail;
    let stack_scale = if scale_aware_card {
        f32::from(text_scale) / 100.0
    } else {
        1.0
    };
    let initial_width = (72.0 * stack_scale).min(art_width);
    let initial_height = 56.0 * stack_scale;
    let kind_width = (88.0 * stack_scale).min(art_width);
    let kind_height = 24.0 * stack_scale;
    let stack_gap = 8.0 * stack_scale;
    let stack_top = art_y + (art_height - initial_height - stack_gap - kind_height) / 2.0;
    nodes.push(
        node(
            &format!("{context}-initial-plate-{}", item.id),
            Role::Text,
            &initial,
            art_x + (art_width - initial_width) / 2.0,
            stack_top,
            initial_width,
            initial_height,
            SCENE_TRANSPARENT_TOKEN,
        )
        .with_type_role(TypeRole::Plate)
        .with_ink_token(plate_ink)
        .with_text_align(TextAlign::Center),
    );
    nodes.push(
        node(
            &format!("{context}-plate-kind-{}", item.id),
            Role::Text,
            &kind,
            art_x + (art_width - kind_width) / 2.0,
            stack_top + initial_height + stack_gap,
            kind_width,
            kind_height,
            SCENE_TRANSPARENT_TOKEN,
        )
        .with_type_role(TypeRole::Eyebrow)
        .with_ink_token(plate_ink)
        .with_text_align(TextAlign::Center),
    );
    if !detail {
        let title = if home {
            scale_aware_single_line(&item.title, width, text_scale)
        } else {
            item.title.clone()
        };
        let title_height = if home {
            scaled_text_box_height(28.0, text_scale)
        } else if favorite {
            32.0
        } else {
            28.0
        };
        nodes.push(
            node(
                &format!("{context}-title-{}", item.id),
                Role::Text,
                &title,
                if favorite { x + 68.0 } else { x },
                if favorite {
                    y + 32.0
                } else {
                    y + art_height + CARD_LABEL_GAP
                },
                if favorite { width - 72.0 } else { width },
                title_height,
                COLOR_SURFACE_CANVAS_TOKEN,
            )
            .with_type_role(TypeRole::Label),
        );
    }
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
    text_scale: u16,
) -> Vec<Node> {
    if let Some(art) = item.art.as_ref().filter(|_| !item.art_failed) {
        let home = context == "home-card";
        let favorite = context == "favorite-card";
        let detail = context == "detail-art";
        let label_y = if home {
            y + art_height + CARD_LABEL_GAP
        } else if favorite {
            y + 32.0
        } else {
            y + art_height + CARD_LABEL_GAP
        };
        let title = if home {
            scale_aware_single_line(&item.title, width, text_scale)
        } else {
            item.title.clone()
        };
        let title_height = if home {
            scaled_text_box_height(28.0, text_scale)
        } else if favorite {
            32.0
        } else {
            28.0
        };
        let mut image = node(
            &format!("{context}-art-{}", item.id),
            Role::Group,
            &format!("{} cover art", item.title),
            if favorite { x + 4.0 } else { x },
            if favorite { y + 4.0 } else { y },
            if favorite { 56.0 } else { width },
            art_height,
            STATE_REST_SURFACE_TOKEN,
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
                &title,
                if favorite { x + 68.0 } else { x },
                label_y,
                if favorite { width - 72.0 } else { width },
                title_height,
                COLOR_SURFACE_CANVAS_TOKEN,
            )
            .with_type_role(TypeRole::Label),
        ];
    }
    let _ = focused;
    plate_art_nodes(item, context, x, y, width, art_height, text_scale)
}

fn add_unavailable_card_cues(
    nodes: &mut Vec<Node>,
    item: &Item,
    availability: &Availability,
    context: &str,
    x: f32,
    y: f32,
    width: f32,
    art_height: f32,
    footer_top: Option<f32>,
    text_scale: u16,
    show_reason: bool,
) {
    let home = context == "home-card";
    let scale_aware_card = context != "favorite-card" && context != "detail-art";
    let library_cue_slot_height = if scale_aware_card && !home && text_scale != 100 {
        scaled_text_box_height(28.0, text_scale) + CARD_CAPTION_GAP
    } else {
        0.0
    };
    let title_y = y + art_height + CARD_LABEL_GAP + library_cue_slot_height;
    let title_height = if scale_aware_card {
        scaled_text_box_height(28.0, text_scale)
    } else {
        28.0
    };
    let reason_y = title_y + title_height + CARD_CAPTION_GAP;
    let cue_box = |text: &str, base_width: f32, base_height: f32| {
        if scale_aware_card && text_scale != 100 {
            (
                measured_text_advance(base_width, text_scale)
                    .max(text_node_box_width(caption_text_width(text, text_scale)))
                    .min(width - 20.0),
                scaled_text_box_height(base_height, text_scale),
            )
        } else {
            (base_width, base_height)
        }
    };
    let badge_top = |badge_height: f32| {
        if home {
            y + art_height - 46.0
        } else if scale_aware_card && text_scale != 100 {
            y + art_height + CARD_LABEL_GAP
        } else {
            y + art_height - 18.0 - badge_height
        }
    };
    if matches!(availability, Availability::Ready)
        && best_variant(item).is_some_and(|variant| {
            variant
                .requirements
                .iter()
                .any(|requirement| !requirement.optional && requirement.capability == "network")
        })
    {
        let badge = scale_aware_single_line("⊘ Network", width - 20.0, text_scale);
        let (badge_width, badge_height) = cue_box(&badge, 92.0, 28.0);
        nodes.push(
            node(
                &format!("{context}-badge-{}", item.id),
                Role::Text,
                &badge,
                x + 10.0,
                badge_top(badge_height),
                badge_width,
                badge_height,
                COLOR_SURFACE_CANVAS_TOKEN,
            )
            .with_type_role(TypeRole::Caption),
        );
        if show_reason {
            let reason = scale_aware_single_line("⊘ Network required", width, text_scale);
            let (_, reason_height) = cue_box(&reason, width, 20.0);
            nodes.push(
                node(
                    &format!("{context}-reason-{}", item.id),
                    Role::Text,
                    &reason,
                    x,
                    reason_y,
                    width,
                    reason_height,
                    COLOR_SURFACE_CANVAS_TOKEN,
                )
                .with_type_role(TypeRole::Caption),
            );
        }
    }
    if matches!(availability, Availability::IncompatibleRuntime { .. }) {
        let badge = scale_aware_single_line("◉ Update", width - 20.0, text_scale);
        let (badge_width, badge_height) = cue_box(&badge, 84.0, 28.0);
        nodes.push(
            node(
                &format!("{context}-badge-{}", item.id),
                Role::Text,
                &badge,
                x + 10.0,
                badge_top(badge_height),
                badge_width,
                badge_height,
                COLOR_SURFACE_CANVAS_TOKEN,
            )
            .with_type_role(TypeRole::Caption),
        );
        return;
    }
    if matches!(availability, Availability::Ready) {
        return;
    }
    let badge = match availability {
        Availability::NeedsNetwork { .. } => "Network",
        Availability::NeedsSetup { .. } => "Setup",
        Availability::IncompatibleRuntime { .. } => "Update",
        Availability::UnsupportedCapability { .. } => "Unavailable",
        Availability::Ready => return,
    };
    let badge = scale_aware_single_line(&format!("⊘ {badge}"), width - 20.0, text_scale);
    let (badge_width, badge_height) = cue_box(&badge, 84.0, 28.0);
    // Illustrated covers receive a dimming veil. Identity plates already encode their
    // unavailable state with the art badge; veiling the plate would erase its mono/kind identity.
    if item.art.is_some() && !item.art_failed {
        nodes.push(node(
            &format!("{context}-veil-{}", item.id),
            Role::Group,
            "",
            x + 8.0,
            y,
            width - 16.0,
            art_height,
            STATE_UNAVAILABLE_VEIL_TOKEN,
        ));
    }
    nodes.push(
        node(
            &format!("{context}-badge-{}", item.id),
            Role::Text,
            &badge,
            x + 10.0,
            badge_top(badge_height),
            badge_width,
            badge_height,
            COLOR_SURFACE_CANVAS_TOKEN,
        )
        .with_type_role(TypeRole::Caption),
    );
    if !show_reason {
        return;
    }
    let full_reason = format!(
        "⊘ {}",
        availability_text(availability, &Presentation::Ready)
    );
    let max_lines = footer_top.map(|footer_top| {
        let available_height = footer_top - reason_y;
        let mut lines = 1;
        while 8.0 + 20.0 * (lines + 1) as f32 <= available_height {
            lines += 1;
        }
        lines
    });
    let reason = max_lines.map_or_else(
        || full_reason.clone(),
        |max_lines| ellipsize_to_lines(&full_reason, width, max_lines),
    );
    let reason = if scale_aware_card {
        scale_aware_single_line(&reason, width, text_scale)
    } else {
        reason
    };
    let reason_lines = (label_text_width(&reason) / width).ceil().max(1.0);
    let reason_height = if scale_aware_card {
        scaled_text_box_height(20.0, text_scale)
    } else {
        8.0 + 20.0 * reason_lines
    };
    nodes.push(
        node(
            &format!("{context}-reason-{}", item.id),
            Role::Text,
            &reason,
            x,
            reason_y,
            width,
            reason_height,
            COLOR_SURFACE_CANVAS_TOKEN,
        )
        .with_type_role(TypeRole::Caption),
    );
}

fn ellipsize_to_lines(text: &str, width: f32, max_lines: usize) -> String {
    let max_width = width * max_lines as f32;
    if label_text_width(text) <= max_width {
        return text.to_owned();
    }

    let mut truncated = String::new();
    for character in text.chars() {
        truncated.push(character);
        if label_text_width(&truncated) + 8.0 > max_width {
            truncated.pop();
            break;
        }
    }
    if let Some(word_end) = truncated.rfind(char::is_whitespace) {
        truncated.truncate(word_end);
    }
    truncated.push('…');
    truncated
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

fn detail_provenance_text(kind: &AppKind, variant: &Variant) -> String {
    let provider = humanize_identifier(&variant.provenance.provider_id);
    let manifest = variant
        .launch_target
        .descriptor_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(|| "Manifest".to_owned(), humanize_identifier);
    format!("{} · {provider} · {manifest}", sentence_kind(kind))
}

fn best_availability(item: &Item) -> &Availability {
    static NO_VARIANTS: OnceLock<Availability> = OnceLock::new();

    best_variant(item).map_or_else(
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

fn best_variant(item: &Item) -> Option<&Variant> {
    item.variants
        .iter()
        .find(|variant| matches!(variant.availability, Availability::Ready))
        .or_else(|| item.variants.first())
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
    let colon_chain = value.contains(':');
    let identity = value
        .split(':')
        .nth(usize::from(colon_chain))
        .unwrap_or(value);
    let words = identity
        .split(['-', '_', '.', '/'])
        .filter(|word| !word.is_empty())
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    if colon_chain {
        words
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let mut chars = words.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
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

/// Declares that wrapping is intentional for this absolute-positioned text node.
///
/// Absolute nodes never consume layout constraints, so infinity is a pixel-neutral
/// scene marker that the blanket raster guard can distinguish from accidental wrap.
fn declared_multiline(mut node: Node) -> Node {
    debug_assert!(node.layout.is_none());
    node.bounds.max_height = Some(f32::INFINITY);
    node
}

#[cfg(test)]
fn semantic_snapshot(node: &Node) -> Vec<(String, Role, String, Option<NodeAction>, bool)> {
    fn collect(node: &Node, out: &mut Vec<(String, Role, String, Option<NodeAction>, bool)>) {
        out.push((
            node.id.as_str().to_owned(),
            node.role,
            node.accessible_label.clone(),
            node.action.clone(),
            node.state.focused,
        ));
        for child in &node.children {
            collect(child, out);
        }
    }
    let mut snapshot = Vec::new();
    collect(node, &mut snapshot);
    snapshot
}

fn px_edges(top: f32, right: f32, bottom: f32, left: f32) -> Edges<LayoutValue> {
    Edges {
        top: LayoutValue::Px(top),
        right: LayoutValue::Px(right),
        bottom: LayoutValue::Px(bottom),
        left: LayoutValue::Px(left),
    }
}

fn fixed_layout(width: f32, height: f32) -> LayoutStyle {
    LayoutStyle {
        flex_shrink: 0.0,
        width: LayoutValue::Px(width),
        height: LayoutValue::Px(height),
        ..LayoutStyle::default()
    }
}

fn wrap_system_layout(nodes: &mut Vec<Node>, surface_width: f32, text_scale: u16) {
    let system_id = |id: &str| {
        matches!(
            id,
            "wifi-glyph"
                | "battery-outline"
                | "battery-cavity"
                | "battery-level"
                | "battery-terminal"
                | "status-cluster"
        )
    };
    let insertion = nodes
        .iter()
        .position(|node| system_id(node.id.as_str()))
        .unwrap_or(0);
    let mut system_nodes = Vec::new();
    nodes.retain(|node| {
        if system_id(node.id.as_str()) {
            system_nodes.push(node.clone());
            false
        } else {
            true
        }
    });
    if system_nodes.is_empty() {
        return;
    }
    for node in &mut system_nodes {
        let (right, top, width, height) = match node.id.as_str() {
            "wifi-glyph" => (180.0, 22.0, 20.0, 20.0),
            "battery-outline" => (144.0, 24.0, 24.0, 14.0),
            "battery-cavity" => (148.0, 26.0, 18.0, 10.0),
            "battery-level" => (165.0 - node.bounds.width, 27.0, node.bounds.width, 8.0),
            "battery-terminal" => (142.0, 28.0, 2.0, 6.0),
            "status-cluster" => (
                if text_scale == 100 { -16.0 } else { 0.0 },
                16.0,
                node.bounds.width.max(152.0),
                node.bounds.height,
            ),
            _ => continue,
        };
        node.layout = Some(LayoutStyle {
            position: Position::Absolute,
            inset: Edges {
                top: LayoutValue::Px(top),
                right: LayoutValue::Px(right),
                bottom: LayoutValue::Auto,
                left: LayoutValue::Auto,
            },
            ..fixed_layout(width, height)
        });
    }
    let mut cluster = node(
        "system-status-layout-anchor",
        Role::Group,
        "",
        0.0,
        0.0,
        surface_width,
        STATUS_BAR_HEIGHT,
        SCENE_TRANSPARENT_TOKEN,
    );
    cluster.layout = Some(fixed_layout(surface_width, STATUS_BAR_HEIGHT));
    cluster.children = system_nodes;
    nodes.insert(insertion, cluster);
}

fn rooms_layout(
    mut rooms: Node,
    mut nodes: Vec<Node>,
    surface_width: f32,
    text_scale: u16,
) -> Node {
    fn take(nodes: &mut Vec<Node>, expected: &str) -> Node {
        let index = nodes
            .iter()
            .position(|node| node.id.as_str() == expected)
            .unwrap_or_else(|| panic!("complete rooms subtree must contain {expected}"));
        nodes.remove(index)
    }

    let scale_delta = f32::from(text_scale) / 100.0 - 1.0;
    let keycap_height = KEYCAP_HEIGHT + 8.0 * scale_delta;
    let room_height = 32.0 + 12.0 * scale_delta;
    let keycap = |mut border: Node, mut fill: Node, mut label: Node| {
        border.layout = Some(LayoutStyle {
            ..fixed_layout(KEYCAP_MIN_WIDTH, keycap_height)
        });
        fill.layout = Some(LayoutStyle {
            position: Position::Absolute,
            inset: px_edges(
                KEYCAP_BORDER_WIDTH,
                KEYCAP_BORDER_WIDTH,
                KEYCAP_BORDER_WIDTH,
                KEYCAP_BORDER_WIDTH,
            ),
            ..LayoutStyle::default()
        });
        label.layout = Some(LayoutStyle {
            position: Position::Absolute,
            inset: px_edges(
                KEYCAP_BORDER_WIDTH,
                KEYCAP_BORDER_WIDTH,
                KEYCAP_BORDER_WIDTH,
                KEYCAP_BORDER_WIDTH,
            ),
            ..LayoutStyle::default()
        });
        label.text_align = TextAlign::Center;
        border.children = vec![fill, label];
        border
    };
    let room = |mut label: Node, underline: Option<Node>| {
        let advance = room_label_advance(&label.accessible_label, text_scale);
        let width = room_label_box_width(advance);
        label.layout = Some(LayoutStyle {
            ..fixed_layout(width, room_height)
        });
        // Center alignment paints into the full node width. Start alignment reserves
        // the renderer's larger inline inset, which is not part of the room CSS box.
        label.text_align = TextAlign::Center;
        if let Some(mut underline) = underline {
            underline.layout = Some(LayoutStyle {
                position: Position::Absolute,
                inset: Edges {
                    top: LayoutValue::Px(room_height + 1.0),
                    bottom: LayoutValue::Auto,
                    left: LayoutValue::Px(ROOM_HORIZONTAL_PADDING),
                    right: LayoutValue::Auto,
                },
                width: LayoutValue::Px(advance),
                height: LayoutValue::Px(3.0),
                ..LayoutStyle::default()
            });
            label.children.push(underline);
        }
        label
    };

    let left = keycap(
        take(&mut nodes, "room-keycap-left-border"),
        take(&mut nodes, "room-keycap-left-fill"),
        take(&mut nodes, "room-keycap-left"),
    );
    let home = take(&mut nodes, "room-home");
    let home_underline = nodes
        .iter()
        .any(|node| node.id.as_str() == "room-home-underline")
        .then(|| take(&mut nodes, "room-home-underline"));
    let home = room(home, home_underline);
    let library = take(&mut nodes, "room-library");
    let library_underline = nodes
        .iter()
        .any(|node| node.id.as_str() == "room-library-underline")
        .then(|| take(&mut nodes, "room-library-underline"));
    let library = room(library, library_underline);
    let settings = take(&mut nodes, "room-settings");
    let settings_underline = nodes
        .iter()
        .any(|node| node.id.as_str() == "room-settings-underline")
        .then(|| take(&mut nodes, "room-settings-underline"));
    let settings = room(settings, settings_underline);
    let right = keycap(
        take(&mut nodes, "room-keycap-right-border"),
        take(&mut nodes, "room-keycap-right-fill"),
        take(&mut nodes, "room-keycap-right"),
    );
    assert!(nodes.is_empty(), "rooms subtree contains unexpected nodes");

    let rooms_width = room_strip_width(text_scale);
    rooms.layout = Some(LayoutStyle {
        position: Position::Absolute,
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        gap: (
            LayoutValue::Px(ROOM_STRIP_GAP),
            LayoutValue::Px(ROOM_STRIP_GAP),
        ),
        inset: Edges {
            top: LayoutValue::Px(0.0),
            left: LayoutValue::Pct(0.5),
            ..Edges::default()
        },
        margin: px_edges(0.0, 0.0, 0.0, -rooms_width / 2.0),
        width: LayoutValue::Px(rooms_width),
        height: LayoutValue::Px(STATUS_BAR_HEIGHT),
        ..LayoutStyle::default()
    });
    rooms.children = vec![left, home, library, settings, right];
    let mut anchor = node(
        "rooms-layout-anchor",
        Role::Group,
        "",
        0.0,
        0.0,
        surface_width,
        STATUS_BAR_HEIGHT,
        SCENE_TRANSPARENT_TOKEN,
    );
    anchor.layout = Some(fixed_layout(surface_width, STATUS_BAR_HEIGHT));
    anchor.children.push(rooms);
    anchor
}

fn home_prompt_nodes(
    footer: &str,
    surface_width: f32,
    surface_height: f32,
    text_scale: u16,
) -> Vec<Node> {
    fn binding_width(binding: &str, scale: f32) -> f32 {
        let measured = binding.chars().count() as f32 * CAPTION_GLYPH_ADVANCE + 9.6;
        let delta = (measured - KEYCAP_MIN_WIDTH).max(0.0).ceil();
        (KEYCAP_MIN_WIDTH + (delta / 2.0).ceil() * 2.0) * scale
            + if scale > 1.0 { 1.0 } else { 0.0 }
    }

    fn outline(
        prefix: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        wide: bool,
        scale: f32,
    ) -> Node {
        let radius = if wide { RADIUS_S * scale } else { height / 2.0 };
        node(
            &format!("{prefix}-border"),
            Role::Group,
            "",
            x,
            y,
            width,
            height,
            SCENE_TRANSPARENT_TOKEN,
        )
        .with_corner_radius(radius)
        .with_border(COLOR_BORDER_STRONG_TOKEN, KEYCAP_BORDER_WIDTH)
    }

    fn translate(node: &mut Node, dx: f32, dy: f32) {
        node.bounds.x += dx;
        node.bounds.y += dy;
        for child in &mut node.children {
            translate(child, dx, dy);
        }
    }

    let scale = f32::from(text_scale) / 100.0;
    let keycap_height = KEYCAP_HEIGHT * scale;
    let row_height = scaled_text_box_height(32.0, text_scale);
    let prompt_area_height = PROMPTS_AREA_HEIGHT.max(row_height);
    let y = surface_height - prompt_area_height + (prompt_area_height - keycap_height) / 2.0;
    let mut x = 0.0;
    let mut nodes = Vec::new();
    for (index, prompt) in footer.split(" · ").enumerate() {
        let Some((binding, verb)) = prompt.split_once(' ') else {
            continue;
        };
        let verb = verb.trim_start();
        if index > 0 {
            x += SPACE_5 * scale;
        }
        let binding_width = binding_width(binding, scale);
        let prefix = format!("home-prompt-keycap-{index}");
        nodes.push(outline(
            &prefix,
            x,
            y,
            binding_width,
            keycap_height,
            binding.chars().count() > 1,
            scale,
        ));
        nodes.push(
            node(
                &prefix,
                Role::Text,
                binding,
                x,
                y,
                binding_width,
                keycap_height,
                SCENE_TRANSPARENT_TOKEN,
            )
            .with_type_role(TypeRole::Caption)
            .with_text_align(TextAlign::Center)
            .with_ink_token(COLOR_TEXT_SECONDARY_TOKEN),
        );
        x += binding_width + SPACE_2 * scale;
        let verb_width = text_node_box_width(library_prompt_verb_width(verb)) * scale;
        nodes.push(
            node(
                &format!("home-prompt-verb-{index}"),
                Role::Text,
                verb,
                x,
                y,
                verb_width,
                keycap_height,
                SCENE_TRANSPARENT_TOKEN,
            )
            .with_type_role(TypeRole::Label)
            .with_ink_token(COLOR_TEXT_SECONDARY_TOKEN),
        );
        x += verb_width;
    }
    let offset = surface_width - SPACE_7 * scale - x;
    for node in &mut nodes {
        translate(node, offset, 0.0);
    }
    nodes
}

fn right_aligned_prompt_nodes(
    footer: &str,
    surface_width: f32,
    surface_height: f32,
    text_scale: u16,
) -> Vec<Node> {
    fn translate(node: &mut Node, dx: f32, dy: f32) {
        node.bounds.x += dx;
        node.bounds.y += dy;
        for child in &mut node.children {
            translate(child, dx, dy);
        }
    }
    let normalized = footer.replace("     ", " · ");
    let scale = f32::from(text_scale) / 100.0;
    let keycap_height = KEYCAP_HEIGHT * scale;
    let prompt_area_height = PROMPTS_AREA_HEIGHT.max(scaled_text_box_height(32.0, text_scale));
    let mut nodes = home_prompt_nodes(&normalized, surface_width, surface_height, text_scale);
    let prompt_top = surface_height - prompt_area_height;
    let centered_y = prompt_top + (prompt_area_height - keycap_height) / 2.0;
    for node in &mut nodes {
        let id = node.id.as_str();
        let suffix = id
            .strip_prefix("home-prompt-keycap-")
            .or_else(|| id.strip_prefix("home-prompt-verb-"));
        let Some(index) = suffix
            .and_then(|suffix| suffix.split('-').next())
            .and_then(|index| index.parse::<usize>().ok())
        else {
            continue;
        };
        translate(
            node,
            -(index as f32 * SPACE_2 * scale),
            centered_y - node.bounds.y,
        );
    }
    let prompt_count = nodes
        .iter()
        .filter_map(|node| {
            node.id
                .as_str()
                .strip_prefix("home-prompt-verb-")
                .and_then(|index| index.parse::<usize>().ok())
        })
        .max()
        .map_or(0, |index| index + 1);
    let mut x = nodes
        .iter()
        .map(|node| node.bounds.x)
        .fold(f32::INFINITY, f32::min);
    for index in 0..prompt_count {
        let prefix = format!("home-prompt-keycap-{index}");
        let keycap_x = nodes
            .iter()
            .find(|node| node.id.as_str() == prefix)
            .map(|node| node.bounds.x)
            .expect("prompt keycap text");
        let keycap_width = nodes
            .iter()
            .find(|node| node.id.as_str() == format!("{prefix}-border"))
            .map(|node| node.bounds.width)
            .expect("prompt keycap border");
        for node in nodes.iter_mut().filter(|node| {
            node.id.as_str() == prefix || node.id.as_str() == format!("{prefix}-border")
        }) {
            translate(node, x - keycap_x, 0.0);
        }
        x += keycap_width + SPACE_2 * scale;

        let verb = nodes
            .iter_mut()
            .find(|node| node.id.as_str() == format!("home-prompt-verb-{index}"))
            .expect("prompt verb");
        verb.bounds.x = x;
        verb.bounds.width =
            text_node_box_width(library_prompt_verb_width(&verb.accessible_label)) * scale;
        x += verb.bounds.width + SPACE_5 * scale;
    }
    let right = nodes
        .iter()
        .map(|node| node.bounds.x + node.bounds.width)
        .fold(0.0_f32, f32::max);
    let last_verb_ink_inset = TEXT_NODE_INLINE_INSET
        + nodes
            .iter()
            .filter(|node| node.id.as_str().starts_with("home-prompt-verb-"))
            .max_by_key(|node| node.id.as_str())
            .map_or(0.0, |node| match node.accessible_label.as_str() {
                // Swash leaves this much of the shaped advance unpainted after the final
                // `s`; compensate the ink bearing, not the semantic node boundary.
                "Details" => 7.0,
                _ => 0.0,
            });
    let offset = surface_width - LIBRARY_SIDE_MARGIN * scale + last_verb_ink_inset * scale - right;
    for node in &mut nodes {
        translate(node, offset, 0.0);
    }
    nodes
}

fn encoded_png(width: u32, height: u32, rgba: &[u8]) -> Arc<[u8]> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut bytes), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .expect("in-memory PNG header")
            .write_image_data(rgba)
            .expect("in-memory PNG pixels");
    }
    bytes.into()
}

fn wifi_glyph_source() -> ImageSource {
    static WIFI: OnceLock<Arc<[u8]>> = OnceLock::new();
    let bytes = WIFI.get_or_init(|| {
        let mut rgba = vec![0_u8; 24 * 24 * 4];
        for y in 0..24 {
            for x in 0..24 {
                let dx = f32::from(u16::try_from(x).unwrap()) + 0.5 - 12.0;
                let dy = 19.0 - (f32::from(u16::try_from(y).unwrap()) + 0.5);
                let radius = dx.hypot(dy);
                let in_upper_fan = dy > 0.0 && dx.abs() <= dy * 1.45;
                let painted = radius <= 1.8
                    || in_upper_fan
                        && ((4.3..=6.3).contains(&radius) || (8.0..=10.2).contains(&radius));
                if painted {
                    let offset = (y * 24 + x) * 4;
                    rgba[offset..offset + 4].copy_from_slice(&[0xc9, 0xc2, 0xb4, 0xff]);
                }
            }
        }
        encoded_png(24, 24, &rgba)
    });
    ImageSource::new("quiet-console:g-wifi.svg-path", bytes.clone())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn library_footer_fade_source(surface_width: f32) -> ImageSource {
    static FADES: OnceLock<Mutex<HashMap<u32, Arc<[u8]>>>> = OnceLock::new();
    let width = surface_width.round().max(1.0) as u32;
    let bytes = FADES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("library fade cache")
        .entry(width)
        .or_insert_with(|| {
            let width = usize::try_from(width).unwrap();
            const HEIGHT: usize = 96;
            let mut rgba = vec![0_u8; width * HEIGHT * 4];
            for y in 0..HEIGHT {
                let alpha = u8::try_from((y + 1) * 255 / HEIGHT).unwrap();
                for x in 0..width {
                    let offset = (y * width + x) * 4;
                    rgba[offset..offset + 4].copy_from_slice(&[0x17, 0x15, 0x12, alpha]);
                }
            }
            encoded_png(
                u32::try_from(width).unwrap(),
                u32::try_from(HEIGHT).unwrap(),
                &rgba,
            )
        })
        .clone();
    let id = if width == 1280 {
        "quiet-console:library-footer-fade".to_owned()
    } else {
        format!("quiet-console:library-footer-fade-{width}")
    };
    ImageSource::new(id, bytes)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn hero_wash_source() -> ImageSource {
    static WASH: OnceLock<Arc<[u8]>> = OnceLock::new();
    let bytes = WASH.get_or_init(|| {
        const WIDTH: usize = 1280;
        const HEIGHT: usize = 344;
        let mut rgba = vec![0_u8; WIDTH * HEIGHT * 4];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                // Exact Ridgeline aura declarations from assets/auras.css. CSS paints the
                // first radial gradient over the second, then .deco applies opacity 0.55.
                let global_y = y as f32;
                let sample = |center_x: f32,
                              center_y: f32,
                              radius_x: f32,
                              radius_y: f32,
                              stop: f32,
                              color: [f32; 4]| {
                    let dx = (x as f32 + 0.5 - center_x) / radius_x;
                    let dy = (global_y + 0.5 - center_y) / radius_y;
                    let distance = dx.hypot(dy);
                    let fade = (1.0 - distance / stop).clamp(0.0, 1.0);
                    [color[0], color[1], color[2], color[3] * fade]
                };
                let bottom = sample(
                    1280.0 * 0.12,
                    720.0 * 0.92,
                    900.0,
                    520.0,
                    0.70,
                    [58.0, 43.0, 78.0, 0.65],
                );
                let top = sample(
                    1280.0 * 0.78,
                    720.0 * 0.08,
                    720.0,
                    420.0,
                    0.68,
                    [201.0, 111.0, 87.0, 0.5],
                );
                let alpha = top[3] + bottom[3] * (1.0 - top[3]);
                let rgb = if alpha > 0.0 {
                    [0, 1, 2].map(|channel| {
                        (top[channel] * top[3] + bottom[channel] * bottom[3] * (1.0 - top[3]))
                            / alpha
                    })
                } else {
                    [0.0; 3]
                };
                let offset = (y * WIDTH + x) * 4;
                let dither = [[-0.75, 0.25], [0.75, -0.25]][y % 2][x % 2];
                rgba[offset..offset + 4].copy_from_slice(&[
                    (rgb[0] + dither).round().clamp(0.0, 255.0) as u8,
                    (rgb[1] + dither).round().clamp(0.0, 255.0) as u8,
                    (rgb[2] + dither).round().clamp(0.0, 255.0) as u8,
                    (alpha * 0.55 * 255.0).round() as u8,
                ]);
            }
        }
        encoded_png(
            u32::try_from(WIDTH).unwrap(),
            u32::try_from(HEIGHT).unwrap(),
            &rgba,
        )
    });
    ImageSource::new(
        "quiet-console:aura-ridgeline:201-111-87@0.5/68%;58-43-78@0.65/70%;opacity=0.55",
        bytes.clone(),
    )
}

fn add_explicit_action_name(action_node: &mut Node, text_scale: u16) {
    fn contains(outer: Bounds, inner: Bounds) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.width <= outer.x + outer.width
            && inner.y + inner.height <= outer.y + outer.height
    }
    fn has_name(node: &Node, bounds: Bounds) -> bool {
        node.children.iter().any(|child| {
            (matches!(child.role, Role::Text | Role::Heading)
                && !child.accessible_label.trim().is_empty()
                && contains(bounds, child.bounds))
                || has_name(child, bounds)
        })
    }

    if action_node.action.is_some()
        && !action_node.accessible_label.trim().is_empty()
        && !has_name(action_node, action_node.bounds)
    {
        let label_height = scaled_text_box_height(28.0, text_scale)
            .min(action_node.bounds.height)
            .max(1.0);
        let bottom_inset = scaled_text_box_height(6.0, text_scale);
        let mut label = node(
            &format!("action-name-{}", action_node.id.as_str()),
            Role::Text,
            &action_node.accessible_label,
            action_node.bounds.x,
            action_node.bounds.y
                + (action_node.bounds.height - label_height - bottom_inset).max(0.0),
            action_node.bounds.width.max(1.0),
            label_height,
            COLOR_SURFACE_CANVAS_TOKEN,
        )
        .with_type_role(TypeRole::Label);
        label.state = action_node.state;
        action_node.children.push(label);
    }
    for child in &mut action_node.children {
        add_explicit_action_name(child, text_scale);
    }
}

fn place_library_fade_below_footer(children: &mut Vec<Node>) {
    let Some(fade_index) = children
        .iter()
        .position(|node| node.id.as_str() == "library-grid-footer-fade")
    else {
        return;
    };
    let fade = children.remove(fade_index);
    let prompt_bar_index = children
        .iter()
        .position(|node| node.id.as_str() == "prompt-bar")
        .unwrap_or(children.len());
    children.insert(prompt_bar_index, fade);
}

fn apply_quiet_console_radius(node: &mut Node, scale: f32) {
    let id = node.id.as_str();
    let prompt_keycap = id
        .strip_prefix("home-prompt-keycap-")
        .and_then(|suffix| suffix.strip_suffix("-border").or(Some(suffix)))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        });
    let numeric_suffix = |prefix: &str| {
        id.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    };
    let radius = if prompt_keycap {
        Some(if node.bounds.width > KEYCAP_MIN_WIDTH {
            RADIUS_S
        } else {
            RADIUS_PILL
        })
    } else if id.starts_with("room-keycap-")
        || id.contains("keycap")
            && (id.to_ascii_lowercase().contains("select")
                || id.to_ascii_lowercase().contains("start"))
    {
        Some(RADIUS_S)
    } else if id == "attention"
        || id == "attention-pill"
        || id == "attention-pill-border"
        || id == "attention-dot"
        || id.contains("status-dot")
        || id.contains("-pip")
        || id.starts_with("favorite-pin-")
        || id.contains("current-indicator")
        || id.starts_with("room-") && id.ends_with("-underline")
        || id.starts_with("settings-toggle-") && (id.ends_with("-track") || id.ends_with("-knob"))
    {
        Some(RADIUS_PILL)
    } else if id.contains("plate-frame-") || id.contains("press-marker") {
        Some(RADIUS_S)
    } else if id == "detail-cover"
        || id.starts_with("detail-art-")
        || id == "first-run-panel"
        || id == "receipt-panel"
        || id == "quick-panel-surface"
    {
        Some(RADIUS_L)
    } else if id.starts_with("item-")
        || id.starts_with("library-item-")
        || id.starts_with("home-card-label-mask-")
        || id.starts_with("home-card-art-")
        || id.starts_with("home-card-plate-")
        || id.starts_with("home-card-veil-")
        || id.starts_with("library-card-label-mask-")
        || id.starts_with("library-card-art-")
        || id.starts_with("library-card-plate-")
        || id.starts_with("library-card-veil-")
        || id == "library-search"
        || numeric_suffix("library-filter-")
        || numeric_suffix("detail-variant-")
        || id.starts_with("chooser-") && id != "chooser-note" && id != "chooser-scroll-region"
        || id.starts_with("settings-nav-") && !id.ends_with("-label")
        || id.starts_with("settings-row-") && !id.contains("-line-") && !id.ends_with("-control")
        || id.starts_with("quick-") && (node.action.is_some() || id == "quick-capture-screenshot")
        || id == "settings-text-scale-segmented-control"
        || id.starts_with("comfort-")
    {
        Some(RADIUS_M)
    } else {
        None
    };
    if let Some(radius) = radius {
        node.corner_radius = (radius * scale).min(node.bounds.width.min(node.bounds.height) / 2.0);
    }
    for child in &mut node.children {
        apply_quiet_console_radius(child, scale);
    }
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
        AppKind, AppManifestRef, Presentation as CP, Provenance, Requirement, UserProjection,
        Variant,
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
            assert!(scene.contains(STATE_DISABLED_BORDER_TOKEN));
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
    fn text_scale_edit_mode_preserves_row_ink_and_styles_value_chips() {
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

            let resting = settings_scene(&core);
            let resting_title = find(
                resting.root(),
                "settings-row-accessibility-textScale-line-0",
            )
            .unwrap();
            let resting_caption = find(
                resting.root(),
                "settings-row-accessibility-textScale-line-1",
            )
            .unwrap();
            assert_eq!(
                resting_title.ink_token.as_deref(),
                Some(COLOR_TEXT_PRIMARY_TOKEN)
            );
            assert_eq!(
                resting_caption.ink_token.as_deref(),
                Some(COLOR_TEXT_SECONDARY_TOKEN)
            );

            core.enter_settings_rows();
            core.focus = core
                .settings_scene_rows()
                .iter()
                .position(|row| row.id == "accessibility-textScale")
                .unwrap();
            let scene = settings_scene(&core);
            let title = find(scene.root(), "settings-row-accessibility-textScale-line-0").unwrap();
            let caption =
                find(scene.root(), "settings-row-accessibility-textScale-line-1").unwrap();
            assert_eq!(title.style_token, resting_title.style_token);
            assert_eq!(title.ink_token, resting_title.ink_token);
            assert_eq!(caption.style_token, resting_caption.style_token);
            assert_eq!(caption.ink_token, resting_caption.ink_token);
            assert!(find(scene.root(), "settings-text-scale-segmented-control").is_some());
            for value in ["100%", "150%", "200%"] {
                let chip =
                    find(scene.root(), &format!("settings-text-scale-chip-{value}")).unwrap();
                assert_eq!(
                    chip.style_token,
                    if value == effective {
                        STATE_SELECTED_ACCENT_TOKEN
                    } else {
                        STATE_REST_SURFACE_TOKEN
                    }
                );
                assert_eq!(
                    chip.border_token.as_deref(),
                    Some(if value == effective {
                        STATE_FOCUSED_RING_TOKEN
                    } else {
                        COLOR_BORDER_HAIRLINE_TOKEN
                    })
                );
                assert_eq!(chip.state.selected, value == effective);
                let value_text =
                    find(scene.root(), &format!("settings-text-scale-value-{value}")).unwrap();
                assert_eq!(
                    value_text.ink_token.as_deref(),
                    Some(if value == effective {
                        COLOR_TEXT_INVERSE_TOKEN
                    } else {
                        COLOR_TEXT_PRIMARY_TOKEN
                    })
                );
            }
        }
    }

    #[test]
    fn settings_toggle_state_and_disabled_remap_value_are_not_clipped() {
        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        core.go(Route::Settings);
        let scene = settings_scene(&core);
        let off = node_by_id(
            scene.root(),
            "settings-toggle-accessibility-highContrast-state",
        )
        .unwrap();
        assert_eq!(off.accessible_label, "OFF");
        assert!(off.bounds.width >= label_text_width("OFF") + 12.0);

        core.settings_room = SettingsRoom::Controls;
        let controls = settings_scene(&core);
        let dash = node_by_id(controls.root(), "settings-row-controls-remap-control").unwrap();
        let row = node_by_id(controls.root(), "settings-row-controls-remap").unwrap();
        assert_eq!(dash.accessible_label, "—");
        assert!(dash.bounds.x + dash.bounds.width <= row.bounds.x + row.bounds.width);
        assert!(dash.bounds.x >= row.bounds.x + row.bounds.width - 120.0);
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
        assert!(bottom.contains(STATE_DISABLED_BORDER_TOKEN));
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
                .resolve(Base::Dusk, COLOR_SURFACE_CANVAS_TOKEN)
                .unwrap(),
            pf_theme::flagship()
                .resolve(Base::HighContrast, COLOR_SURFACE_CANVAS_TOKEN)
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
        assert_eq!(diagnostic.style_token, COLOR_TEXT_SECONDARY_TOKEN);
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
        struct FixtureStatus;
        impl DeviceStatusPort for FixtureStatus {
            fn status(&self) -> Result<DeviceStatus, String> {
                Ok(DeviceStatus {
                    battery_percent: 82,
                    attention_message: None,
                })
            }
        }
        let snapshot = CatalogSnapshot {
            revision: 10,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            items,
            user_projection: UserProjection::default(),
        };
        let mut core = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        core.load_device_status(&FixtureStatus);
        core
    }

    #[test]
    fn quick_is_contextual_between_home_and_details() {
        let mut home = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        assert_eq!(home.action(&ShellAction::Custom("Quick".into())), None);
        assert_eq!(home.route(), Route::Quick);

        let mut details = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        details.selected_item = Some(0);
        details.go(Route::Details);
        assert_eq!(
            details.action(&ShellAction::Custom("Quick".into())),
            Some(Effect::ToggleFavorite {
                item_id: "ready".into(),
                favorite: true,
            })
        );
        assert_eq!(details.route(), Route::Details);

        assert_eq!(
            details.action(&ShellAction::Custom("Quick.open".into())),
            None
        );
        assert_eq!(details.route(), Route::Quick);
    }

    #[test]
    fn unavailable_device_status_and_time_are_absent_from_chrome() {
        struct Unavailable;
        impl DeviceStatusPort for Unavailable {
            fn status(&self) -> Result<DeviceStatus, String> {
                Err("unavailable".into())
            }
        }
        let mut core = fixture_core(vec![]);
        core.load_device_status(&Unavailable);
        let scene = settings_scene(&core);
        for id in [
            "battery-outline",
            "battery-cavity",
            "battery-level",
            "battery-terminal",
            "status-cluster",
            "attention-pill",
        ] {
            assert!(node_by_id(scene.root(), id).is_none(), "unexpected {id}");
        }
        assert!(node_by_id(scene.root(), "status-bar").is_none());
    }

    #[test]
    fn hero_wash_starts_at_top_and_has_local_dither_variance() {
        let core = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        let scene = settings_scene(&core);
        let wash = node_by_id(scene.root(), "hero-wash").unwrap();
        assert!(wash.bounds.y.abs() < f32::EPSILON);
        let pf_scene::NodeContent::Image { source, .. } = &wash.content else {
            panic!("wash must be an image");
        };
        let decoder = png::Decoder::new(Cursor::new(source.bytes.as_ref()));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels).unwrap();
        let stride = info.width as usize * 4;
        let mut values = std::collections::HashSet::new();
        for y in 120..136 {
            for x in 600..616 {
                values.insert(pixels[y * stride + x * 4]);
            }
        }
        assert!(
            values.len() > 1,
            "mid-gradient tile must not be a flat band"
        );
    }

    #[test]
    fn narrow_home_keeps_every_painted_card_inside_the_surface() {
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
        let scene = fixture_core(items)
            .scene(
                SurfaceMetrics {
                    logical_width: 480.0,
                    logical_height: 800.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Portrait,
                },
                "",
            )
            .unwrap();

        for card in scene
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("item-"))
        {
            assert!(
                card.bounds.x >= 0.0,
                "{} starts off-screen",
                card.id.as_str()
            );
            assert!(
                card.bounds.x + card.bounds.width <= 480.0,
                "{} ends off-screen at {}",
                card.id.as_str(),
                card.bounds.x + card.bounds.width
            );
        }
    }

    #[test]
    fn narrow_library_keeps_every_painted_card_inside_the_surface() {
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
        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 480.0,
                    logical_height: 800.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Portrait,
                },
                "",
            )
            .unwrap();

        for card in scene
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("library-item-"))
        {
            assert!(
                card.bounds.x >= 0.0,
                "{} starts off-screen",
                card.id.as_str()
            );
            assert!(
                card.bounds.x + card.bounds.width <= 480.0,
                "{} ends off-screen at {}",
                card.id.as_str(),
                card.bounds.x + card.bounds.width
            );
        }
    }

    #[test]
    fn ready_now_skips_unavailable_catalog_entries_but_library_keeps_their_cues() {
        let unavailable = item(
            "offline",
            "Offline Stream",
            vec![variant(
                "stream",
                "offline-stream",
                Availability::NeedsNetwork {
                    reason: "connect to Wi-Fi".into(),
                },
            )],
        );
        let mut items = vec![unavailable];
        items.extend((0..6).map(|index| {
            item(
                &format!("ready-{index}"),
                &format!("Ready {index}"),
                vec![variant(
                    "native",
                    &format!("ready-app-{index}"),
                    Availability::Ready,
                )],
            )
        }));
        let mut core = fixture_core(items);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        let home = core.scene(metrics, "").unwrap();
        assert_eq!(
            node_by_id(home.root(), "home-shelf-label").map(|node| node.accessible_label.as_str()),
            Some("READY NOW · 6")
        );
        assert!(node_by_id(home.root(), "item-offline").is_none());
        for index in 0..6 {
            assert!(node_by_id(home.root(), &format!("item-ready-{index}")).is_some());
        }

        core.go(Route::Library);
        let library = core.scene(metrics, "").unwrap();
        assert!(node_by_id(library.root(), "library-item-offline").is_some());
        assert!(node_by_id(library.root(), "library-card-badge-offline").is_some());
        assert!(node_by_id(library.root(), "library-card-reason-offline").is_some());
    }

    fn node_by_id<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
        (node.id.as_str() == id)
            .then_some(node)
            .or_else(|| node.children.iter().find_map(|child| node_by_id(child, id)))
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

        for (width, columns) in [(480.0, 2), (640.0, 3), (800.0, 4), (1280.0, 6)] {
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

            core.focus = 5;
            core.action(&ShellAction::Move(AxisMove::Right));
            core.action(&ShellAction::Move(AxisMove::Left));
            assert_eq!(core.focus, 5, "left/right inverse at {width}px");

            core.action(&ShellAction::Move(AxisMove::Down));
            assert_eq!(core.focus, 5 + columns, "down at {width}px");
            core.action(&ShellAction::Move(AxisMove::Up));
            assert_eq!(core.focus, 5, "up/down inverse at {width}px");
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
    fn widest_library_chip_label_fits_at_desktop_breakpoints() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.go(Route::Library);

        for width in [1100.0, 1280.0] {
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
            let label = node_by_id(scene.root(), "library-filter-3-label").unwrap();
            assert_eq!(label.accessible_label, "Everything else");
            assert!(
                label.bounds.width >= label_text_width(&label.accessible_label),
                "Everything else needs {}px but receives {}px at {width}px",
                label_text_width(&label.accessible_label),
                label.bounds.width
            );
        }
    }

    #[test]
    fn compact_scaled_library_filter_keeps_full_accessible_name_when_paint_is_ellipsized() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.text_scale = 200;
        core.go(Route::Library);

        let scene = core
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
        let chip = node_by_id(scene.root(), "library-filter-3").unwrap();
        let painted_label = node_by_id(chip, "library-filter-3-label").unwrap();

        assert_eq!(chip.accessible_label, "Everything else · 0");
        assert_eq!(painted_label.accessible_label, "Everything…");
    }

    #[test]
    fn narrow_scaled_search_row_caps_title_and_preserves_inline_caption() {
        let title = "An Extraordinary Ridgeline Adventure With A Deliberately Long Title";
        let mut core = fixture_core(vec![item(
            "long-search-result",
            title,
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.text_scale = 200;
        core.go(Route::Search);
        core.set_search_query("extraordinary");

        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 640.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "A Open     PF Safe Return",
            )
            .unwrap();
        let row = node_by_id(scene.root(), "search-result-long-search-result").unwrap();
        let painted_title = node_by_id(row, "search-result-long-search-result-title").unwrap();
        let caption = node_by_id(row, "search-result-long-search-result-caption").unwrap();

        assert_eq!(row.accessible_label, format!("{title} · GAME · Ready"));
        assert!(painted_title.accessible_label.ends_with('…'));
        assert!(painted_title.bounds.x >= row.bounds.x);
        assert!(painted_title.bounds.x + painted_title.bounds.width <= caption.bounds.x);
        assert!(caption.bounds.x >= painted_title.bounds.x + painted_title.bounds.width);
        assert!(caption.bounds.x + caption.bounds.width <= row.bounds.x + row.bounds.width);
        assert!(caption.bounds.width >= caption_text_width(" · GAME · Ready", 200));
    }

    #[test]
    fn search_has_one_semantic_focus_owner_and_grouped_prompts() {
        fn focused_nodes<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
            if node.state.focused {
                out.push(node);
            }
            for child in &node.children {
                focused_nodes(child, out);
            }
        }

        let mut core = fixture_core(vec![item(
            "search-result",
            "Ridgeline",
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.go(Route::Search);
        core.set_search_query("ridge");
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        let populated = core.scene(metrics, "A Open     PF Safe Return").unwrap();
        let mut focused = Vec::new();
        focused_nodes(populated.root(), &mut focused);
        assert_eq!(focused.len(), 1);
        assert_eq!(focused[0].id.as_str(), "search-result-search-result");
        assert_eq!(
            populated.focused().map(NodeId::as_str),
            Some("search-result-search-result")
        );
        let query = node_by_id(populated.root(), "search-query").unwrap();
        assert!(!query.state.focused);
        assert_eq!(
            query.border_token.as_deref(),
            Some(COLOR_BORDER_HAIRLINE_TOKEN)
        );
        let results_region = node_by_id(populated.root(), "search-results-scroll-region").unwrap();
        assert_eq!(results_region.children.len(), 1);
        assert_eq!(
            results_region.children[0].id.as_str(),
            "search-result-search-result"
        );
        assert!(
            populated
                .root()
                .children
                .iter()
                .all(|node| { !node.id.as_str().starts_with("search-result-") })
        );
        let prompts = node_by_id(populated.root(), "prompts").unwrap();
        assert_eq!(prompts.role, Role::Group);
        assert!(prompts.accessible_label.is_empty());
        assert!(!prompts.children.is_empty());

        core.set_search_query("no match");
        let empty = core.scene(metrics, "A Open     PF Safe Return").unwrap();
        let mut focused = Vec::new();
        focused_nodes(empty.root(), &mut focused);
        assert_eq!(focused.len(), 1);
        assert_eq!(focused[0].id.as_str(), "search-query");
        assert_eq!(empty.focused().map(NodeId::as_str), Some("search-query"));
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
    fn library_never_paints_a_full_width_fold_band() {
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
        assert!(!has_fold(&scene_for_count(24)));
    }

    #[test]
    fn library_has_no_empty_painted_grid_container() {
        let mut core = fixture_core(vec![item(
            "item-0",
            "Item 0",
            vec![variant("native", "app-0", Availability::Ready)],
        )]);
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

        assert!(
            scene
                .root()
                .children
                .iter()
                .all(|node| node.id.as_str() != "library-grid-scroll"),
            "the Library grid must not add an empty full-width painted strip"
        );
    }

    #[test]
    fn second_library_row_keeps_every_card_child_inside_its_card() {
        let items = (0..7)
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
        let card = node_by_id(scene.root(), "library-item-item-6").unwrap();

        assert!(
            node_by_id(card, "library-card-art-item-6").is_some(),
            "missing library-card-art-item-6"
        );
        for required in [
            "library-card-initial-plate-item-6",
            "library-card-plate-kind-item-6",
        ] {
            assert!(node_by_id(card, required).is_some(), "missing {required}");
        }
        assert!(card.children.iter().all(|child| {
            child.bounds.x >= card.bounds.x
                && child.bounds.y >= card.bounds.y
                && child.bounds.x + child.bounds.width <= card.bounds.x + card.bounds.width
                && child.bounds.y + child.bounds.height <= card.bounds.y + card.bounds.height
        }));
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
    fn library_filter_count_respects_chip_trailing_padding() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
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
        let chip = node_by_id(scene.root(), "library-filter-2").unwrap();
        let count_node = node_by_id(chip, "library-filter-2-count").unwrap();

        let count_right = count_node.bounds.x + count_node.bounds.width;
        let padded_chip_right = chip.bounds.x + chip.bounds.width - CHIP_HORIZONTAL_PADDING;
        assert!(
            (count_right - padded_chip_right).abs() < f32::EPSILON,
            "library count must end at the generated chip trailing padding: {count_right} != {padded_chip_right}"
        );
    }

    #[test]
    fn countless_library_filter_respects_chip_trailing_padding() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
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
        let chip = node_by_id(scene.root(), "library-filter-0").unwrap();
        let label_node = node_by_id(chip, "library-filter-0-label").unwrap();

        let label_right = label_node.bounds.x + label_node.bounds.width;
        let padded_chip_right = chip.bounds.x + chip.bounds.width - CHIP_HORIZONTAL_PADDING;
        assert!(
            (label_right - padded_chip_right).abs() < f32::EPSILON,
            "countless library label must end at the generated chip trailing padding: {label_right} != {padded_chip_right}"
        );
    }

    #[test]
    fn counted_library_filter_preserves_label_count_gap() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
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
        let chip = node_by_id(scene.root(), "library-filter-2").unwrap();
        let label_node = node_by_id(chip, "library-filter-2-label").unwrap();
        let count_node = node_by_id(chip, "library-filter-2-count").unwrap();

        let label_right = label_node.bounds.x + label_node.bounds.width;
        let gap = count_node.bounds.x - label_right;
        assert!(
            gap >= CHIP_COUNT_GAP,
            "counted library label/count gap must be at least {CHIP_COUNT_GAP}px, got {gap}px"
        );
    }

    #[test]
    fn artless_library_cards_keep_identity_plate_label_and_optional_reason() {
        let unavailable = item(
            "setup",
            "Setup Game",
            vec![variant(
                "stream",
                "setup-game",
                Availability::NeedsSetup {
                    reason: "choose a profile".into(),
                },
            )],
        );
        let mut core = fixture_core(vec![unavailable]);
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
        let card = node_by_id(scene.root(), "library-item-setup").unwrap();
        assert_eq!(card.style_token, COLOR_SURFACE_CANVAS_TOKEN);
        assert!(
            (card.bounds.height - (LIB_CARD_ART_HEIGHT + CARD_LABEL_GAP + 34.0)).abs()
                < f32::EPSILON
        );
        assert!(node_by_id(card, "library-card-art-setup").is_some());
        for required in [
            "library-title-setup",
            "library-card-reason-setup",
            "library-card-initial-plate-setup",
            "library-card-plate-kind-setup",
        ] {
            assert!(node_by_id(card, required).is_some(), "missing {required}");
        }
        assert_eq!(
            node_by_id(card, "library-card-reason-setup")
                .unwrap()
                .accessible_label,
            "⊘ Finish setup — choose a profile"
        );
    }

    #[test]
    fn identity_plate_centers_its_initial_and_kind_stack_without_an_inner_box() {
        let mut plate = item(
            "steam-link",
            "Steam Link",
            vec![variant("stream", "steam-link", Availability::Ready)],
        );
        plate.kind = AppKind::Stream;
        plate.tags.push("kind-label:Stream".into());
        let core = fixture_core(vec![plate]);
        let nodes = plate_art_nodes(
            &core.items[0],
            "home-card",
            48.0,
            388.0,
            CARD_ART_WIDTH,
            CARD_ART_HEIGHT,
            100,
        );
        let art = nodes
            .iter()
            .find(|node| node.id.as_str() == "home-card-art-steam-link")
            .unwrap();
        let initial = nodes
            .iter()
            .find(|node| node.id.as_str() == "home-card-initial-plate-steam-link")
            .unwrap();
        let kind = nodes
            .iter()
            .find(|node| node.id.as_str() == "home-card-plate-kind-steam-link")
            .unwrap();
        let art_center_x = art.bounds.x + art.bounds.width / 2.0;
        let art_center_y = art.bounds.y + art.bounds.height / 2.0;
        let stack_center_y = (initial.bounds.y + kind.bounds.y + kind.bounds.height) / 2.0;

        assert!(((initial.bounds.x + initial.bounds.width / 2.0) - art_center_x).abs() <= 1.0);
        assert!(((kind.bounds.x + kind.bounds.width / 2.0) - art_center_x).abs() <= 1.0);
        assert!((stack_center_y - art_center_y).abs() <= 1.0);
        assert!((kind.bounds.y - (initial.bounds.y + initial.bounds.height) - 8.0).abs() <= 1.0);
        assert_eq!(initial.text_align, TextAlign::Center);
        assert_eq!(kind.text_align, TextAlign::Center);
        assert_eq!(initial.style_token, SCENE_TRANSPARENT_TOKEN);
        assert_eq!(kind.style_token, SCENE_TRANSPARENT_TOKEN);
        assert_eq!(initial.ink_token.as_deref(), Some("--deco-plate-a-fg"));
        assert_eq!(kind.ink_token.as_deref(), Some("--deco-plate-a-fg"));
        assert!(matches!(art.content, pf_scene::NodeContent::Image { .. }));
        assert!(nodes.iter().all(|node| {
            let id = node.id.as_str();
            !id.contains("plate-chip")
                && !id.contains("plate-box")
                && !id.contains("plate-motif")
                && !id.contains("plate-frame")
        }));
    }

    #[test]
    fn ready_network_cues_follow_the_selected_variants_requirement_not_item_identity() {
        let mut differently_named = variant("stream", "moonlight", Availability::Ready);
        differently_named.requirements.push(Requirement {
            capability: "network".into(),
            optional: false,
        });
        let ordinary = variant("native", "ordinary", Availability::Ready);
        let mut steam_link = variant("stream", "steam-link", Availability::Ready);
        steam_link.requirements.push(Requirement {
            capability: "network".into(),
            optional: false,
        });
        let core = fixture_core(vec![
            item("moonlight", "Moonlight", vec![differently_named]),
            item("ordinary", "Ordinary", vec![ordinary]),
            item("steam-link", "Steam Link", vec![steam_link]),
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
                "",
            )
            .unwrap();

        for id in ["moonlight", "steam-link"] {
            let card = node_by_id(scene.root(), &format!("item-{id}")).unwrap();
            assert_eq!(
                node_by_id(card, &format!("home-card-badge-{id}"))
                    .unwrap()
                    .accessible_label,
                "⊘ Network"
            );
            assert_eq!(
                node_by_id(card, &format!("home-card-reason-{id}"))
                    .unwrap()
                    .accessible_label,
                "⊘ Network required"
            );
        }
        let ordinary = node_by_id(scene.root(), "item-ordinary").unwrap();
        assert!(node_by_id(ordinary, "home-card-badge-ordinary").is_none());
        assert!(node_by_id(ordinary, "home-card-reason-ordinary").is_none());
        assert_eq!(
            node_by_id(scene.root(), "home-shelf-label")
                .unwrap()
                .accessible_label,
            "READY NOW · 3"
        );
    }

    #[test]
    fn identity_plate_text_uses_the_selected_palette_foreground() {
        for (id, expected_ink) in [
            ("steam-link", "--deco-plate-a-fg"),
            ("tidelines", "--deco-plate-d-fg"),
            ("button-tester", "--deco-plate-c-fg"),
            // The hash-picked fallback for `setup` selects plate D.
            ("setup", "--deco-plate-d-fg"),
        ] {
            let plate = item(
                id,
                "Plate",
                vec![variant("native", id, Availability::Ready)],
            );
            let core = fixture_core(vec![plate]);
            let nodes = plate_art_nodes(
                &core.items[0],
                "home-card",
                48.0,
                388.0,
                CARD_ART_WIDTH,
                CARD_ART_HEIGHT,
                100,
            );

            for suffix in ["initial-plate", "plate-kind"] {
                let text = nodes
                    .iter()
                    .find(|node| node.id.as_str() == format!("home-card-{suffix}-{id}"))
                    .unwrap();
                assert_eq!(text.style_token, SCENE_TRANSPARENT_TOKEN);
                assert_eq!(text.ink_token.as_deref(), Some(expected_ink));
            }
        }
    }

    #[test]
    fn focused_library_search_uses_the_focus_pairing_and_full_placeholder() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.go(Route::Library);
        core.focus = 0;
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
        let search = node_by_id(scene.root(), "library-search").unwrap();
        assert_eq!(search.accessible_label, "⌕  Search 1 titles");
        assert!(search.state.focused);
        assert_eq!(search.style_token, STATE_REST_SURFACE_TOKEN);
    }

    #[test]
    fn home_hero_reports_nothing_ready_when_catalog_only_needs_setup() {
        let core = fixture_core(vec![item(
            "setup",
            "Setup Game",
            vec![variant(
                "setup",
                "setup",
                Availability::NeedsSetup {
                    reason: "choose a profile".into(),
                },
            )],
        )]);
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
        let status = node_by_id(scene.root(), "hero-status").unwrap();
        assert_eq!(status.accessible_label, "⊘ Unavailable · Game");
        assert!(!status.accessible_label.contains("Ready"));
        assert_eq!(
            node_by_id(scene.root(), "hero-title").map(|node| node.accessible_label.as_str()),
            Some("Nothing ready")
        );
    }

    #[test]
    fn home_hero_ready_suffix_follows_selected_variant_capability() {
        let cases = [
            (
                "native",
                "pocketforge/native",
                "● Ready · Game · Installed",
                true,
            ),
            (
                "stream",
                "pc-stream",
                "● Ready · Game · Available over the network",
                false,
            ),
            (
                "unknown",
                "other-runtime",
                "● Ready · Game · Source availability unknown",
                false,
            ),
        ];

        for (id, runtime_family, expected, locally_installed) in cases {
            let mut ready = variant(id, id, Availability::Ready);
            ready.provenance.runtime_family = runtime_family.into();
            let unavailable_native = variant(
                "unavailable-native",
                id,
                Availability::NeedsSetup {
                    reason: "install first".into(),
                },
            );
            let core = fixture_core(vec![item(id, "Game", vec![unavailable_native, ready])]);
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
            let status = &node_by_id(scene.root(), "hero-status")
                .unwrap()
                .accessible_label;
            assert_eq!(status, expected, "case {id}");
            assert_eq!(
                status.contains("Installed"),
                locally_installed,
                "case {id} local-installation claim"
            );
        }
    }

    #[test]
    fn details_humanizes_catalog_source_without_descriptor_filename() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("app.toml", "game", Availability::Ready)],
        )]);
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
        let source = node_by_id(scene.root(), "detail-provenance").unwrap();
        assert_eq!(
            source.accessible_label,
            "Game · Provider app toml · App toml"
        );
        assert!(!source.accessible_label.contains("app.toml"));
    }

    #[test]
    fn details_source_copy_follows_variant_provenance_and_availability() {
        let cases = [
            (
                "network",
                Availability::NeedsNetwork {
                    reason: "connect first".into(),
                },
                "native",
                "Game · Provider network · Network",
            ),
            (
                "setup",
                Availability::NeedsSetup {
                    reason: "choose a profile".into(),
                },
                "native",
                "Game · Provider setup · Setup",
            ),
            (
                "stream",
                Availability::Ready,
                "pc-stream",
                "Game · Provider stream · Stream",
            ),
            (
                "unknown",
                Availability::Ready,
                "other-runtime",
                "Game · Provider unknown · Unknown",
            ),
        ];
        for (id, availability, runtime_family, expected) in cases {
            let mut catalog_variant = variant(id, id, availability);
            catalog_variant.provenance.runtime_family = runtime_family.into();
            let mut core = fixture_core(vec![item(id, "Game", vec![catalog_variant])]);
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
            let source = node_by_id(scene.root(), "detail-provenance").unwrap();
            assert_eq!(source.accessible_label, expected, "case {id}");
            assert!(
                !source.accessible_label.contains("Installed on this device"),
                "non-local case {id} made a false local claim"
            );
        }
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
    fn details_without_a_ready_variant_omit_readiness_copy() {
        let mut unavailable = item(
            "setup",
            "Setup Game",
            vec![variant(
                "standard-edition",
                "setup-app",
                Availability::NeedsSetup {
                    reason: "choose a profile once".into(),
                },
            )],
        );
        unavailable.tags.clear();
        let mut core = fixture_core(vec![unavailable]);
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
        assert!(
            node_by_id(scene.root(), "detail-availability-reason")
                .unwrap()
                .accessible_label
                .contains("choose a profile once")
        );
        assert!(node_by_id(scene.root(), "detail-description").is_none());
        assert!(!format!("{scene:?}").contains("ready to play"));
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
    fn details_facts_flow_after_recorded_playtime() {
        fn intersects(a: Bounds, b: Bounds) -> bool {
            a.x < b.x + b.width
                && a.x + a.width > b.x
                && a.y < b.y + b.height
                && a.y + a.height > b.y
        }

        fn assert_subtree_misses(node: &Node, bounds: Bounds) {
            assert!(
                !intersects(node.bounds, bounds),
                "{} {:?} intersects playtime {:?}",
                node.id.as_str(),
                node.bounds,
                bounds
            );
            for child in &node.children {
                assert_subtree_misses(child, bounds);
            }
        }

        let mut core = fixture_core(vec![item(
            "played",
            "Played Game",
            vec![variant("native", "played", Availability::Ready)],
        )]);
        core.selected_item = Some(0);
        core.go(Route::Details);
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        core.load_history(&[history_entry(
            "played",
            Some(start),
            Some((start + Duration::from_secs(3_600), EndPrecision::Observed)),
        )]);

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
        let playtime = node_by_id(scene.root(), "detail-playtime").unwrap().bounds;
        for fact in scene
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str().starts_with("detail-fact-"))
        {
            assert_subtree_misses(fact, playtime);
        }
    }

    #[test]
    fn details_offline_claims_require_observed_native_runtime() {
        fn details_for(mut variant: Variant) -> Scene {
            variant.availability = Availability::Ready;
            let mut core = fixture_core(vec![item("game", "Game", vec![variant])]);
            core.selected_item = Some(0);
            core.go(Route::Details);
            core.scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap()
        }

        let native = details_for(variant("native", "game", Availability::Ready));
        assert_eq!(
            node_by_id(native.root(), "detail-availability-reason")
                .unwrap()
                .accessible_label,
            "● Ready"
        );
        assert!(node_by_id(native.root(), "detail-fact-offline").is_some());
        assert!(
            node_by_id(native.root(), "detail-variant-0-sub")
                .unwrap()
                .accessible_label
                .contains("works offline")
        );

        let mut streaming = variant("stream", "game", Availability::Ready);
        streaming.provenance.runtime_family = "pocketforge/stream".into();
        let streaming = details_for(streaming);
        let streaming_copy = [
            node_by_id(streaming.root(), "detail-availability-reason")
                .unwrap()
                .accessible_label
                .as_str(),
            node_by_id(streaming.root(), "detail-variant-0-name")
                .unwrap()
                .accessible_label
                .as_str(),
        ]
        .join("\n");
        assert!(streaming_copy.contains("Stream from your PC"));
        assert!(!streaming_copy.to_ascii_lowercase().contains("install"));
        assert!(node_by_id(streaming.root(), "detail-fact-installed").is_none());
        assert!(node_by_id(streaming.root(), "detail-fact-offline").is_none());
        assert!(
            !node_by_id(streaming.root(), "detail-variant-0-sub")
                .unwrap()
                .accessible_label
                .contains("offline")
        );

        let mut unknown = variant("unknown", "game", Availability::Ready);
        unknown.provenance.runtime_family.clear();
        let unknown = details_for(unknown);
        assert!(node_by_id(unknown.root(), "detail-fact-offline").is_none());
        assert_eq!(
            node_by_id(unknown.root(), "detail-variant-0-sub")
                .unwrap()
                .accessible_label,
            "Version 1.0"
        );
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
    fn details_selection_focus_and_play_follow_the_launchable_variant() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![
                variant(
                    "cloud",
                    "game-cloud",
                    Availability::NeedsNetwork {
                        reason: "offline".into(),
                    },
                ),
                variant("native", "game-native", Availability::Ready),
            ],
        )]);
        core.selected_item = Some(0);
        core.go(Route::Details);

        assert_eq!(
            core.focus(),
            0,
            "the launchable variant row has default focus"
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
                "",
            )
            .unwrap();
        let unavailable = node_by_id(scene.root(), "detail-variant-0").unwrap();
        let launchable = node_by_id(scene.root(), "detail-variant-1").unwrap();
        assert!(!unavailable.state.selected);
        assert!(!unavailable.state.focused);
        assert!(launchable.state.selected);
        assert!(launchable.state.focused);
        assert!(node_by_id(scene.root(), "detail-variant-1-selection-mark").is_some());

        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "game-native".into(),
            }))
        );
    }

    #[test]
    fn details_variant_window_includes_the_active_ready_variant() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![
                variant(
                    "cloud",
                    "game-cloud",
                    Availability::NeedsNetwork {
                        reason: "offline".into(),
                    },
                ),
                variant(
                    "setup",
                    "game-setup",
                    Availability::NeedsSetup {
                        reason: "setup required".into(),
                    },
                ),
                variant("native", "game-native", Availability::Ready),
            ],
        )]);
        core.selected_item = Some(0);
        core.go(Route::Details);

        assert_eq!(core.focus(), 0, "the active row has default focus");
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
        assert!(node_by_id(scene.root(), "detail-variant-0").is_none());
        assert!(node_by_id(scene.root(), "detail-variant-1").is_some());
        let active = node_by_id(scene.root(), "detail-variant-2").unwrap();
        assert!(active.state.selected);
        assert!(active.state.focused);
        assert!(node_by_id(active, "detail-variant-2-selection-mark").is_some());

        core.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "game-native".into(),
            }))
        );
    }

    #[test]
    fn details_description_spacing_only_applies_when_description_is_emitted() {
        fn ways_offset(catalog_item: pf_catalog::CatalogItem) -> (f32, f32) {
            let mut core = fixture_core(vec![catalog_item]);
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
            let availability = node_by_id(scene.root(), "detail-availability-reason").unwrap();
            let ways = node_by_id(scene.root(), "detail-ways-heading").unwrap();
            (
                availability.bounds.y + availability.bounds.height,
                ways.bounds.y,
            )
        }

        let without_description = item(
            "plain",
            "Plain",
            vec![variant("native", "plain-native", Availability::Ready)],
        );
        let mut with_description = without_description.clone();
        with_description
            .tags
            .push("description:A concise description.".into());

        let (plain_availability_bottom, plain_ways_top) = ways_offset(without_description);
        let (_, described_ways_top) = ways_offset(with_description);
        assert!((plain_ways_top - (plain_availability_bottom + 4.0)).abs() < f32::EPSILON);
        assert!((described_ways_top - (plain_ways_top + 50.0)).abs() < f32::EPSILON);
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
                ShellCore::boot_with_art(&snapshot, &pf_theme::flagship(), false, |_, _| {
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
    fn ready_native_and_stream_variants_have_distinct_human_labels() {
        let native = variant("native", "many-native", Availability::Ready);
        let mut stream = variant("stream", "many-stream", Availability::Ready);
        stream.provenance.runtime_family = "pocketforge/stream".into();
        let mut core = fixture_core(vec![item("many", "Many Moons", vec![native, stream])]);
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
        let native_label = &node_by_id(scene.root(), "detail-variant-0-name")
            .unwrap()
            .accessible_label;
        let stream_label = &node_by_id(scene.root(), "detail-variant-1-name")
            .unwrap()
            .accessible_label;
        assert_eq!(native_label, "Native · Installed on this device");
        assert_eq!(stream_label, "Stream · Stream from your PC");
        assert_ne!(native_label, stream_label);
    }

    #[test]
    fn ready_variants_within_the_same_capability_family_have_distinct_labels() {
        let standard = variant("standard-edition", "many-standard", Availability::Ready);
        let deluxe = variant("deluxe-edition", "many-deluxe", Availability::Ready);
        let mut core = fixture_core(vec![item("many", "Many Moons", vec![standard, deluxe])]);
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
        let standard_label = &node_by_id(scene.root(), "detail-variant-0-name")
            .unwrap()
            .accessible_label;
        let deluxe_label = &node_by_id(scene.root(), "detail-variant-1-name")
            .unwrap()
            .accessible_label;

        assert_eq!(
            standard_label,
            "Standard edition · Installed on this device"
        );
        assert_eq!(deluxe_label, "Deluxe edition · Installed on this device");
        assert_ne!(standard_label, deluxe_label);
    }

    #[test]
    fn colon_chained_variant_identity_becomes_a_human_name() {
        let mut chained = variant(
            "Installed applications:drift loop:pocketforge native",
            "drift-loop",
            Availability::Ready,
        );
        chained.provenance.runtime_family = "pocketforge/native".into();
        let label = ready_variant_label(&chained);
        assert_eq!(label, "Drift Loop · Installed on this device");
        assert!(!label.contains(':'));
        assert!(!label.starts_with("Installed applications"));
    }

    #[test]
    fn single_ready_variant_label_reads_as_a_natural_capability_phrase() {
        let native = variant("native", "only-native", Availability::Ready);

        assert_eq!(
            ready_variant_label(&native),
            "Native · Installed on this device"
        );
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

        assert!(scene.contains("Unavailable"));
        assert!(!scene.contains("home-card-reason-empty"));
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
            ShellCore::boot_with_art(&snapshot, &pf_theme::flagship(), false, |_, reference| {
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
        assert_eq!(card.accessible_label, "Paper Comet");
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
    fn emitted_routes_keep_actions_named_and_off_structural_groups() {
        fn has_accessible_name(node: &Node) -> bool {
            !node.accessible_label.trim().is_empty()
                || node.children.iter().any(has_accessible_name)
        }
        fn assert_action_contract(node: &Node) {
            assert!(
                node.role != Role::Group || node.action.is_none(),
                "structural group {} must not carry an action",
                node.id.as_str()
            );
            if node.action.is_some() {
                assert!(
                    has_accessible_name(node),
                    "actionable node {} must have an accessible name in its component tree",
                    node.id.as_str()
                );
            }
            for child in &node.children {
                assert_action_contract(child);
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
            assert_action_contract(scene.root());
        }

        core.recovery_available = true;
        for room in [
            SettingsRoom::Accessibility,
            SettingsRoom::Display,
            SettingsRoom::Controls,
            SettingsRoom::Network,
            SettingsRoom::System,
        ] {
            core.settings_room = room;
            core.settings_in_rows = true;
            let scene = core.scene(metrics, "").unwrap();
            assert_action_contract(scene.root());
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
        assert!(node_by_id(focused, "library-title-title-480").is_some());
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
                .any(|card| { card.id.as_str() == "library-item-item-7" && card.state.focused }),
            "the focused card must remain in the emitted row"
        );
    }

    #[test]
    fn scaled_compact_library_grid_starts_below_the_filter_toolbar() {
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

        for text_scale in [150, 200] {
            core.text_scale = text_scale;
            for width in [480.0, 640.0] {
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
                let toolbar_bottom = scene
                    .root()
                    .children
                    .iter()
                    .filter(|node| node.id.as_str().starts_with("library-filter-"))
                    .map(|node| node.bounds.y + node.bounds.height)
                    .fold(0.0_f32, f32::max);
                let card_top = scene
                    .root()
                    .children
                    .iter()
                    .filter(|node| node.id.as_str().starts_with("library-item-"))
                    .map(|node| node.bounds.y)
                    .reduce(f32::min)
                    .expect("compact Library must emit a visible card row");

                assert!(
                    toolbar_bottom <= card_top,
                    "filter toolbar bottom {toolbar_bottom} overlaps card top {card_top} at {text_scale}% and width {width}"
                );
            }
        }
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
            (0..12)
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
                cards.iter().all(|card| card.bounds.y < grid_bottom),
                "each emitted row must begin above the footer at {width}x{height}"
            );

            let row_height = LIB_CARD_ART_HEIGHT + CARD_LABEL_GAP + 34.0 + SPACE_5;
            let mut expected_rows = 1;
            while if geometry.columns == 6 {
                geometry.card_top + expected_rows as f32 * row_height < grid_bottom
            } else {
                geometry.card_top + expected_rows as f32 * row_height + LIB_CARD_ART_HEIGHT
                    <= grid_bottom
            } {
                expected_rows += 1;
            }
            assert_eq!(
                cards.len(),
                expected_rows * geometry.columns,
                "the maximum fitting rows must be emitted at {width}x{height}"
            );
        }
    }

    #[test]
    fn library_footer_clip_preserves_an_undistorted_second_row_peek() {
        let items = (0..24)
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
        let root = scene.root();
        let footer_top = 720.0 - PROMPTS_AREA_HEIGHT;
        let second_row_art = node_by_id(root, "library-card-art-item-6").unwrap();
        assert!(second_row_art.bounds.y < footer_top);
        assert!(second_row_art.bounds.y + second_row_art.bounds.height > footer_top);
        assert!((second_row_art.bounds.height - LIB_CARD_ART_HEIGHT).abs() < f32::EPSILON);

        let fade = node_by_id(root, "library-grid-footer-fade").unwrap();
        assert!((fade.bounds.y - (720.0 - 96.0)).abs() < f32::EPSILON);
        assert!((fade.bounds.height - 96.0).abs() < f32::EPSILON);
        assert!(matches!(fade.content, pf_scene::NodeContent::Image { .. }));
    }

    #[test]
    fn library_footer_fade_spans_the_full_band_on_wide_surfaces() {
        let mut core = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        core.go(Route::Library);
        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 1920.0,
                    logical_height: 1080.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        let fade = node_by_id(scene.root(), "library-grid-footer-fade").unwrap();
        let pf_scene::NodeContent::Image { source, .. } = &fade.content else {
            panic!("library footer fade must be an image");
        };
        let decoder = png::Decoder::new(Cursor::new(source.bytes.as_ref()));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels).unwrap();

        assert_eq!((info.width, info.height), (1920, 96));
        assert!(pixels[3] <= 3, "top row must be effectively transparent");
        let bottom_alpha = pixels[(usize::try_from(info.height).unwrap() - 1)
            * usize::try_from(info.width).unwrap()
            * 4
            + 3];
        assert_eq!(bottom_alpha, 255, "bottom row must be fully opaque");
    }

    #[test]
    fn library_footer_stacking_matches_mockup() {
        let mut core = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
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
        let children = &scene.root().children;
        let fade_index = children
            .iter()
            .position(|node| node.id.as_str() == "library-grid-footer-fade")
            .unwrap();
        let prompts_index = children
            .iter()
            .position(|node| node.id.as_str() == "prompts")
            .unwrap();
        let prompt_bar_index = children
            .iter()
            .position(|node| node.id.as_str() == "prompt-bar")
            .unwrap();
        let card_index = children
            .iter()
            .position(|node| node.id.as_str() == "library-item-ready")
            .unwrap();

        assert!(
            card_index < fade_index
                && fade_index < prompt_bar_index
                && prompt_bar_index < prompts_index,
            "cards (with labels) must paint below the fade, prompt bar, and prompts"
        );
        assert!(
            children
                .iter()
                .all(|node| !node.id.as_str().starts_with("library-label-layer-")),
            "library labels must remain card children, never lifted layers"
        );
        assert!(node_by_id(&children[card_index], "library-title-ready").is_some());
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn library_prompt_row_is_transparent_over_derived_grid_gutters() {
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
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let geometry = library_geometry(metrics.logical_width);

        for focus in [0, 5] {
            core.focus = focus;
            let scene = core.scene(metrics, "").unwrap();
            assert_eq!(
                node_by_id(scene.root(), "prompt-bar").unwrap().style_token,
                SCENE_TRANSPARENT_TOKEN
            );
            assert_eq!(
                node_by_id(scene.root(), "prompts").unwrap().style_token,
                SCENE_TRANSPARENT_TOKEN
            );

            // Prompt chips legitimately paint over the fade. Remove only those compact
            // children so this raster assertion isolates the row container surface.
            let mut root = scene.root().clone();
            root.children
                .iter_mut()
                .find(|node| node.id.as_str() == "prompts")
                .unwrap()
                .children
                .clear();
            let scene_without_chips = Scene::new(root, scene.default_focus().clone()).unwrap();
            let rendered = pf_render::Rasterizer::new()
                .render(&scene_without_chips, metrics)
                .unwrap();
            let stride = rendered.width as usize * 4;
            for column in 0..geometry.columns - 1 {
                let gutter_left = geometry.card_left
                    + (column + 1) as f32 * geometry.card_width
                    + column as f32 * geometry.card_gap;
                let gutter_right = gutter_left + geometry.card_gap;
                for y in 665..690 {
                    let reference = y * stride;
                    for x in gutter_left.ceil() as usize..gutter_right.floor() as usize {
                        let offset = y * stride + x * 4;
                        assert_eq!(
                            &rendered.rgba[offset..offset + 4],
                            &rendered.rgba[reference..reference + 4],
                            "focus {focus}: prompt-row surface painted derived gutter {column} at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn actionable_fade_band_item_keeps_its_single_in_bounds_name_ink() {
        let mut items = (0..12)
            .map(|index| {
                let (id, title) = if index == 7 {
                    ("low-orbit".to_owned(), "Low Orbit".to_owned())
                } else {
                    (format!("item-{index}"), format!("Item {index}"))
                };
                item(
                    &id,
                    &title,
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect::<Vec<_>>();
        items[7].presentation.icon_reference = Some("fixture-art:low-orbit.png".into());
        items[7].presentation.icon_decodable = true;
        let snapshot = CatalogSnapshot {
            revision: 10,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            items,
            user_projection: UserProjection::default(),
        };
        let art = encoded_png(1, 1, &[0x33, 0x44, 0x55, 0xff]);
        let mut core = ShellCore::boot_with_art(&snapshot, &pf_theme::flagship(), false, |_, _| {
            Some(art.clone())
        });
        core.authority_snapshot(false);
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
        let root = scene.root();
        let action = node_by_id(root, "library-item-low-orbit").unwrap();
        let name_ink = action
            .children
            .iter()
            .find(|child| {
                matches!(child.role, Role::Text | Role::Heading)
                    && child.accessible_label == "Low Orbit"
            })
            .unwrap();
        let fade_index = root
            .children
            .iter()
            .position(|node| node.id.as_str() == "library-grid-footer-fade")
            .unwrap();
        let action_index = root
            .children
            .iter()
            .position(|node| node.id == action.id)
            .unwrap();

        assert!(action.action.is_some());
        assert!(name_ink.bounds.x >= action.bounds.x);
        assert!(name_ink.bounds.y >= action.bounds.y);
        assert!(name_ink.bounds.x + name_ink.bounds.width <= action.bounds.x + action.bounds.width);
        assert!(
            name_ink.bounds.y + name_ink.bounds.height <= action.bounds.y + action.bounds.height
        );
        assert_eq!(
            action
                .children
                .iter()
                .filter(|child| child.accessible_label == "Low Orbit")
                .count(),
            1,
            "the actionable subtree must expose one painted name node"
        );
        assert!(
            action_index < fade_index,
            "card name ink must dim below the fade"
        );
        assert!(
            root.children[..fade_index].iter().any(|node| {
                node.id.as_str() == "library-item-low-orbit"
                    && node
                        .children
                        .iter()
                        .any(|child| child.id.as_str() == "library-card-art-low-orbit")
            }),
            "the intact card must paint below the fade"
        );
        assert!(
            !root
                .children
                .iter()
                .any(|node| { node.id.as_str().starts_with("library-fade-lift-") })
        );
    }

    #[test]
    fn desktop_library_toolbar_uses_mockup_flex_geometry() {
        let mut items = (0..24)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("item-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect::<Vec<_>>();
        for item in &mut items[21..] {
            item.kind = AppKind::Media;
        }
        let mut core = fixture_core(items);
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
        let search = node_by_id(scene.root(), "library-search").unwrap();
        let chips = (0..4)
            .map(|index| node_by_id(scene.root(), &format!("library-filter-{index}")).unwrap())
            .collect::<Vec<_>>();
        let expected_content = [
            ("Recent", None),
            ("A–Z", None),
            ("Games", Some(21)),
            ("Everything else", Some(3)),
        ];
        assert!((search.bounds.x - SPACE_7).abs() < f32::EPSILON);
        assert!(node_by_id(scene.root(), "library-toolbar-divider").is_none());
        for (index, (chip, (label, count))) in chips.iter().zip(expected_content).enumerate() {
            assert!(
                (chip.bounds.width - library_chip_width(label, count)).abs() < f32::EPSILON,
                "chip {index} must use its natural content width"
            );

            let inner_left = chip.bounds.x + CHIP_HORIZONTAL_PADDING;
            let inner_right = chip.bounds.x + chip.bounds.width - CHIP_HORIZONTAL_PADDING;
            let label_node = node_by_id(chip, &format!("library-filter-{index}-label")).unwrap();
            assert!(label_node.bounds.x >= inner_left);
            assert!(
                (label_node.bounds.width - (label_text_width(label) + 20.0)).abs() < f32::EPSILON
            );
            if count.is_some() {
                let count_node =
                    node_by_id(chip, &format!("library-filter-{index}-count")).unwrap();
                assert!(
                    count_node.bounds.x + count_node.bounds.width
                        <= inner_right + 2.0 * TEXT_NODE_INLINE_INSET,
                    "chip {index} count content must fit inside the right padding"
                );
                assert!(
                    count_node.bounds.x
                        >= label_node.bounds.x + label_node.bounds.width + CHIP_COUNT_GAP,
                    "chip {index} label and count must retain the design gap"
                );
            }
        }
        assert!((chips[3].bounds.x + chips[3].bounds.width - 1232.0).abs() < f32::EPSILON);
        for pair in chips.windows(2) {
            let gap = pair[1].bounds.x - pair[0].bounds.x - pair[0].bounds.width;
            assert!((gap - 16.0).abs() < f32::EPSILON);
        }
        let toolbar_gap = chips[0].bounds.x - search.bounds.x - search.bounds.width;
        assert!((toolbar_gap - 16.0).abs() < f32::EPSILON);
    }

    #[test]
    fn inset_library_search_derives_from_shifted_chrome_bottom() {
        let mut core = fixture_core(vec![]);
        core.go(Route::Library);
        let inset_metrics = SurfaceMetrics {
            logical_width: 480.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: pf_scene::Insets {
                top: 32.0,
                left: 24.0,
                right: 0.0,
                bottom: 0.0,
            },
            orientation: pf_scene::Orientation::Landscape,
        };
        let scene = core.scene(inset_metrics, "").unwrap();
        let chrome = node_by_id(scene.root(), "rooms-layout-anchor").unwrap();
        let search = node_by_id(scene.root(), "library-search").unwrap();
        let room_bounds = ["room-home", "room-library", "room-settings"]
            .map(|id| (id, node_by_id(scene.root(), id).unwrap().bounds));
        eprintln!(
            "safe-inset diagnosis: chrome={:?}, rooms={room_bounds:?}, library-search={:?}",
            chrome.bounds, search.bounds
        );

        let existing_gap = LIB_HEAD_TOP - STATUS_BAR_HEIGHT;
        let derived_search_top =
            inset_metrics.safe_insets.top + chrome.bounds.height + existing_gap;
        assert!(
            search.bounds.y >= derived_search_top - f32::EPSILON,
            "Library search must derive from the safe-inset-shifted chrome row"
        );

        let zero_scene = core
            .scene(
                SurfaceMetrics {
                    safe_insets: Default::default(),
                    ..inset_metrics
                },
                "",
            )
            .unwrap();
        let zero_search_top = node_by_id(zero_scene.root(), "library-search")
            .unwrap()
            .bounds
            .y;
        assert!(
            (zero_search_top - LIB_HEAD_TOP).abs() < f32::EPSILON,
            "zero-inset Library geometry must remain byte-identical"
        );
    }

    #[test]
    fn two_hundred_percent_library_search_starts_below_scaled_chrome() {
        let mut core = fixture_core(vec![]);
        core.go(Route::Library);
        core.text_scale = 200;
        let metrics = SurfaceMetrics {
            logical_width: 800.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let scene = core.scene(metrics, "").unwrap();
        let search = node_by_id(scene.root(), "library-search").unwrap();
        let scaled_chrome_bottom = chrome_row_bottom(metrics.safe_insets.top, core.text_scale);
        let standard_gap = LIB_HEAD_TOP - STATUS_BAR_HEIGHT;

        assert!(
            search.bounds.y >= scaled_chrome_bottom + standard_gap,
            "Library search top {} must clear scaled chrome bottom {scaled_chrome_bottom} plus gap {standard_gap}",
            search.bounds.y
        );
    }

    #[test]
    fn desktop_library_footer_matches_promptbar_css_without_separators() {
        let mut core = fixture_core(vec![item(
            "item-0",
            "Item 0",
            vec![variant("native", "item-0", Availability::Ready)],
        )]);
        core.set_control_bindings(vec![
            ControlBinding {
                context: "shell".into(),
                action: "Search.open".into(),
                label: "Search".into(),
                binding: "SELECT".into(),
            },
            ControlBinding {
                context: "shell".into(),
                action: "Filter.next".into(),
                label: "Filter".into(),
                binding: "Y".into(),
            },
            ControlBinding {
                context: "global".into(),
                action: "Activate".into(),
                label: "Activate".into(),
                binding: "A".into(),
            },
        ]);
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
        let prompts = node_by_id(scene.root(), "prompts").unwrap();
        assert!(!prompts.accessible_label.contains('·'));
        assert!(
            !prompts
                .children
                .iter()
                .any(|node| node.accessible_label == "·" || node.id.as_str().contains("separator"))
        );
        let last_verb = node_by_id(prompts, "home-prompt-verb-2").unwrap();
        assert!(
            (last_verb.bounds.x + last_verb.bounds.width
                - TEXT_NODE_INLINE_INSET
                - 7.0
                - (1280.0 - SPACE_7))
                .abs()
                < f32::EPSILON
        );
        let expected_top =
            720.0 - PROMPTS_AREA_HEIGHT + (PROMPTS_AREA_HEIGHT - KEYCAP_HEIGHT) / 2.0;
        assert!(
            prompts
                .children
                .iter()
                .all(|node| (node.bounds.y - expected_top).abs() < f32::EPSILON)
        );
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::float_cmp
    )]
    fn shared_prompt_rows_are_outline_only_plain_text_with_token_spacing() {
        fn assert_prompt_paint(scene: &Scene, prompt_count: usize) {
            fn clear_label(node: &mut Node, id: &str) -> bool {
                if node.id.as_str() == id {
                    node.accessible_label.clear();
                    return true;
                }
                node.children.iter_mut().any(|child| clear_label(child, id))
            }

            let prompts = node_by_id(scene.root(), "prompts").unwrap();
            assert!(
                !prompts
                    .children
                    .iter()
                    .any(|node| node.id.as_str().contains("-fill")),
                "prompt rows must not emit filled chip surfaces"
            );
            for index in 0..prompt_count {
                let prefix = format!("home-prompt-keycap-{index}");
                let keycap = node_by_id(prompts, &prefix).unwrap();
                let border = node_by_id(prompts, &format!("{prefix}-border")).unwrap();
                let verb = node_by_id(prompts, &format!("home-prompt-verb-{index}")).unwrap();
                assert_eq!(keycap.style_token, SCENE_TRANSPARENT_TOKEN);
                assert_eq!(verb.style_token, SCENE_TRANSPARENT_TOKEN);
                assert_eq!(
                    keycap.ink_token.as_deref(),
                    Some(COLOR_TEXT_SECONDARY_TOKEN)
                );
                assert_eq!(verb.ink_token.as_deref(), Some(COLOR_TEXT_SECONDARY_TOKEN));
                assert_eq!(border.style_token, SCENE_TRANSPARENT_TOKEN);
                let expected_radius = if border.bounds.width > KEYCAP_MIN_WIDTH {
                    RADIUS_S
                } else {
                    border.bounds.height / 2.0
                };
                assert_eq!(border.corner_radius, expected_radius);
                assert_eq!(
                    border.border_token.as_deref(),
                    Some(COLOR_BORDER_STRONG_TOKEN)
                );
                assert_eq!(border.border_width, KEYCAP_BORDER_WIDTH);
                assert!(
                    border.children.is_empty(),
                    "runtime border node must replace scanline outline children"
                );

                let metrics = SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                };
                let painted = Rasterizer::new().render(scene, metrics).unwrap();
                let mut blank_root = scene.root().clone();
                assert!(clear_label(&mut blank_root, &prefix));
                let blank_scene = Scene::new(blank_root, scene.default_focus().clone()).unwrap();
                let blank = Rasterizer::new().render(&blank_scene, metrics).unwrap();
                let ink_columns = painted
                    .rgba
                    .chunks_exact(4)
                    .zip(blank.rgba.chunks_exact(4))
                    .enumerate()
                    .filter_map(|(pixel, (ink_pixel, blank_pixel))| {
                        (ink_pixel != blank_pixel).then_some(pixel % painted.width as usize)
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                assert!(
                    *ink_columns.first().unwrap() > border.bounds.x as usize
                        && *ink_columns.last().unwrap()
                            < (border.bounds.x + border.bounds.width) as usize,
                    "keycap label ink must lie strictly inside its border"
                );
                assert!(
                    (verb.bounds.x - (border.bounds.x + border.bounds.width) - SPACE_2).abs()
                        < f32::EPSILON,
                    "keycap-to-label gap must be space-2"
                );
                if index + 1 < prompt_count {
                    let next =
                        node_by_id(prompts, &format!("home-prompt-keycap-{}-border", index + 1))
                            .unwrap();
                    assert!(
                        (next.bounds.x - (verb.bounds.x + verb.bounds.width) - SPACE_5).abs()
                            < f32::EPSILON,
                        "prompt-group gap must be space-5"
                    );
                }
            }
        }

        let mut core = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        core.set_control_bindings(vec![
            ControlBinding {
                context: "shell".into(),
                action: "Search.open".into(),
                label: "Search".into(),
                binding: "SELECT".into(),
            },
            ControlBinding {
                context: "shell".into(),
                action: "Filter.next".into(),
                label: "Filter".into(),
                binding: "Y".into(),
            },
            ControlBinding {
                context: "shell".into(),
                action: "Quick".into(),
                label: "Quick".into(),
                binding: "X".into(),
            },
            ControlBinding {
                context: "global".into(),
                action: "Activate".into(),
                label: "Activate".into(),
                binding: "A".into(),
            },
        ]);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        let home = core.scene(metrics, "A Open     PF Safe Return").unwrap();
        assert_prompt_paint(&home, 4);

        core.go(Route::Library);
        let library_compact = core.scene(metrics, "").unwrap();
        assert_prompt_paint(&library_compact, 2);
        core.focus = 5;
        let library_details = core.scene(metrics, "").unwrap();
        assert_prompt_paint(&library_details, 3);
    }

    #[test]
    fn desktop_library_text_is_complete_and_footer_is_margin_anchored_on_one_row() {
        fn clear_label(node: &mut Node, id: &str) -> bool {
            if node.id.as_str() == id {
                node.accessible_label.clear();
                return true;
            }
            node.children.iter_mut().any(|child| clear_label(child, id))
        }

        fn widen_label(node: &mut Node, id: &str) -> bool {
            if node.id.as_str() == id {
                node.bounds.width = 300.0;
                return true;
            }
            node.children.iter_mut().any(|child| widen_label(child, id))
        }

        fn ink_columns_with_label_suppressed(
            scene: &Scene,
            id: &str,
            metrics: SurfaceMetrics,
        ) -> (usize, usize, usize, usize, usize) {
            let rendered = pf_render::Rasterizer::new().render(scene, metrics).unwrap();
            let mut suppressed_root = scene.root().clone();
            assert!(clear_label(&mut suppressed_root, id));
            let suppressed = Scene::new(suppressed_root, scene.default_focus().clone()).unwrap();
            let blank = pf_render::Rasterizer::new()
                .render(&suppressed, metrics)
                .unwrap();
            let mut columns = std::collections::BTreeSet::new();
            let mut rows = std::collections::BTreeSet::new();
            for (pixel, (painted, blank)) in rendered
                .rgba
                .chunks_exact(4)
                .zip(blank.rgba.chunks_exact(4))
                .enumerate()
            {
                if painted != blank {
                    columns.insert(pixel % rendered.width as usize);
                    rows.insert(pixel / rendered.width as usize);
                }
            }
            (
                columns.len(),
                *columns
                    .first()
                    .unwrap_or_else(|| panic!("{id} must produce raster ink")),
                *columns.last().unwrap(),
                *rows.first().unwrap(),
                *rows.last().unwrap(),
            )
        }

        fn generous_ink_columns(scene: &Scene, id: &str, metrics: SurfaceMetrics) -> usize {
            let mut generous_root = scene.root().clone();
            assert!(widen_label(&mut generous_root, id));
            let generous = Scene::new(generous_root, scene.default_focus().clone()).unwrap();
            ink_columns_with_label_suppressed(&generous, id, metrics).0
        }

        let mut items = (0..24)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    vec![variant(
                        "native",
                        &format!("item-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect::<Vec<_>>();
        for item in &mut items[21..] {
            item.kind = AppKind::Media;
        }
        let mut core = fixture_core(items);
        core.set_control_bindings(vec![
            ControlBinding {
                context: "shell".into(),
                action: "Search.open".into(),
                label: "Search".into(),
                binding: "SELECT".into(),
            },
            ControlBinding {
                context: "shell".into(),
                action: "Filter.next".into(),
                label: "Filter".into(),
                binding: "Y".into(),
            },
            ControlBinding {
                context: "global".into(),
                action: "Activate".into(),
                label: "Activate".into(),
                binding: "A".into(),
            },
        ]);
        core.go(Route::Library);
        core.focus = 5;
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let scene = core.scene(metrics, "").unwrap();
        for id in [
            "home-prompt-verb-0",
            "home-prompt-verb-1",
            "home-prompt-verb-2",
            "library-filter-0-label",
            "library-filter-1-label",
            "library-filter-2-label",
            "library-filter-2-count",
            "library-filter-3-label",
            "library-filter-3-count",
        ] {
            let label = node_by_id(scene.root(), id).unwrap();
            let in_scene = ink_columns_with_label_suppressed(&scene, id, metrics).0;
            let standalone = generous_ink_columns(&scene, id, metrics);
            assert_eq!(
                in_scene, standalone,
                "{id} must paint every standalone ink column (label {:?})",
                label.accessible_label
            );
        }

        let (_, min_x, max_x, min_y, max_y) =
            ink_columns_with_label_suppressed(&scene, "home-prompt-verb-2", metrics);
        assert!(
            max_x.abs_diff(1231) <= 2,
            "rightmost Details ink must meet the 1232px content margin, got x={min_x}..={max_x}"
        );
        assert!(
            max_y - min_y < 15,
            "Details ink must remain on one label row, got y={min_y}..={max_y}"
        );
    }

    fn mutate_label(node: &mut Node, id: &str, label: Option<&str>, width: Option<f32>) -> bool {
        if node.id.as_str() == id {
            if let Some(label) = label {
                node.accessible_label = label.into();
            }
            if let Some(width) = width {
                node.bounds.width = width;
            }
            return true;
        }
        node.children
            .iter_mut()
            .any(|child| mutate_label(child, id, label, width))
    }

    fn label_ink(
        scene: &Scene,
        id: &str,
        metrics: SurfaceMetrics,
        text_scale: u16,
    ) -> (usize, usize) {
        let mut rasterizer = Rasterizer::new();
        rasterizer
            .set_text_scale(f32::from(text_scale) / 100.0)
            .unwrap();
        let rendered = rasterizer.render(scene, metrics).unwrap();
        let mut blank_root = scene.root().clone();
        assert!(mutate_label(&mut blank_root, id, Some(""), None));
        let blank_scene = Scene::new(blank_root, scene.default_focus().clone()).unwrap();
        let blank = rasterizer.render(&blank_scene, metrics).unwrap();
        let mut columns = std::collections::BTreeSet::new();
        let mut rows = std::collections::BTreeSet::new();
        for (pixel, (painted, blank)) in rendered
            .rgba
            .chunks_exact(4)
            .zip(blank.rgba.chunks_exact(4))
            .enumerate()
        {
            if painted != blank {
                columns.insert(pixel % rendered.width as usize);
                rows.insert(pixel / rendered.width as usize);
            }
        }
        let first_row = *rows
            .first()
            .unwrap_or_else(|| panic!("{id} must paint ink"));
        (columns.len(), rows.last().unwrap() - first_row)
    }

    fn generous_ink_columns(
        scene: &Scene,
        id: &str,
        metrics: SurfaceMetrics,
        text_scale: u16,
    ) -> usize {
        let current_width = node_by_id(scene.root(), id).unwrap().bounds.width;
        let mut root = scene.root().clone();
        // Centered paint phase depends on (width - advance) / 2; an even-delta
        // widening preserves its subpixel bin while providing unclipped reference ink.
        fn widen(node: &mut Node, id: &str, current_width: f32) -> bool {
            if node.id.as_str() == id {
                node.bounds.x -= 128.0;
                node.bounds.width = current_width + 256.0;
                return true;
            }
            node.children
                .iter_mut()
                .any(|child| widen(child, id, current_width))
        }
        assert!(widen(&mut root, id, current_width));
        let generous = Scene::new(root, scene.default_focus().clone()).unwrap();
        label_ink(&generous, id, metrics, text_scale).0
    }

    #[test]
    fn statusbar_text_is_complete_on_one_row_for_every_route() {
        let mut core = fixture_core(vec![item(
            "many",
            "Many Moons",
            vec![variant("native", "many-native", Availability::Ready)],
        )]);
        core.time_state = Ok(pf_ports::TimeState {
            wall_clock: SystemTime::UNIX_EPOCH + Duration::from_secs(9 * 3_600 + 41 * 60),
            timezone: "UTC".into(),
            ntp_state: NtpState::Active,
        });
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        for text_scale in [100, 150, 200] {
            core.text_scale = text_scale;
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
                assert!(
                    node_by_id(scene.root(), "rooms-layout-anchor").is_some(),
                    "{route:?} must render the status strip through the layout seam"
                );
                for id in [
                    "room-home",
                    "room-library",
                    "room-settings",
                    "status-cluster",
                ] {
                    let (columns, row_span) = label_ink(&scene, id, metrics, text_scale);
                    assert_eq!(
                        columns,
                        generous_ink_columns(&scene, id, metrics, text_scale),
                        "{id} must paint every unclipped ink column on {route:?} at {text_scale}%"
                    );
                    assert!(
                        row_span < 16 * text_scale as usize / 100,
                        "{id} must stay on one text row on {route:?} at {text_scale}%"
                    );
                }
            }
        }
    }

    #[test]
    fn status_group_is_atomic_and_yields_when_scaled_extent_cannot_clear_room_strip() {
        let mut core = fixture_core(vec![]);
        core.text_scale = 200;
        let mut connected = pf_ports::FakeNetworkPort::new(NetworkState {
            interface_present: true,
            enabled: true,
            connected_ssid: Some("Moonlit Arcade".into()),
            signal: Some(78),
        });
        core.load_network(&mut connected);

        for (logical_width, expected_group) in [(640.0, false), (800.0, false), (1280.0, true)] {
            let scene = core
                .scene(
                    SurfaceMetrics {
                        logical_width,
                        logical_height: 720.0,
                        scale: 1.0,
                        safe_insets: Default::default(),
                        orientation: pf_scene::Orientation::Landscape,
                    },
                    "",
                )
                .unwrap();
            for id in [
                "wifi-glyph",
                "battery-outline",
                "battery-cavity",
                "battery-level",
                "battery-terminal",
                "status-cluster",
            ] {
                assert_eq!(
                    node_by_id(scene.root(), id).is_some(),
                    expected_group,
                    "the status group must be admitted atomically from its 200%-scale extent at {logical_width}px ({id})"
                );
            }
        }
    }

    #[test]
    fn completeness_guard_detects_clipped_columns() {
        let mut core = fixture_core(vec![]);
        core.text_scale = 100;
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let scene = core.scene(metrics, "").unwrap();
        let mut clipped_root = scene.root().clone();
        let width = node_by_id(&clipped_root, "room-home").unwrap().bounds.width;
        assert!(mutate_label(
            &mut clipped_root,
            "room-home",
            None,
            Some(width - 8.0)
        ));
        let clipped = Scene::new(clipped_root, scene.default_focus().clone()).unwrap();

        assert!(
            label_ink(&clipped, "room-home", metrics, 100).0
                < generous_ink_columns(&clipped, "room-home", metrics, 100),
            "the completeness guard must detect genuinely clipped room-label ink"
        );
    }

    #[test]
    fn selected_library_chip_uses_uniform_strong_border_and_full_inner_accent() {
        let mut core = fixture_core(vec![item(
            "item-0",
            "Item 0",
            vec![variant("native", "item-0", Availability::Ready)],
        )]);
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
        let chip = node_by_id(scene.root(), "library-filter-0").unwrap();
        assert_eq!(chip.style_token, STATE_REST_SURFACE_TOKEN);
        for edge in ["top", "right", "bottom", "left"] {
            assert_eq!(
                node_by_id(chip, &format!("library-filter-0-border-{edge}"))
                    .unwrap()
                    .style_token,
                COLOR_BORDER_STRONG_TOKEN
            );
        }
        let accent = node_by_id(scene.root(), "library-selected-underline-0").unwrap();
        assert!((accent.bounds.x - chip.bounds.x - CHIP_BORDER_WIDTH).abs() < f32::EPSILON);
        assert!(
            (accent.bounds.width - chip.bounds.width + 2.0 * CHIP_BORDER_WIDTH).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn desktop_library_scene_uses_generated_css_geometry() {
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

        let toolbar = node_by_id(scene.root(), "library-search").unwrap();
        assert!((toolbar.bounds.y - LIB_HEAD_TOP).abs() < f32::EPSILON);
        assert!((toolbar.bounds.height - LIB_TOOLBAR_HEIGHT).abs() < f32::EPSILON);

        let first_art = node_by_id(scene.root(), "library-card-art-item-0").unwrap();
        assert!((first_art.bounds.y - (LIB_GRID_TOP + SPACE_3)).abs() < f32::EPSILON);
        assert!((first_art.bounds.height - LIB_CARD_ART_HEIGHT).abs() < f32::EPSILON);
    }

    #[test]
    fn library_card_label_paint_order_never_occludes_footer_ink() {
        fn has_library_title(node: &Node) -> bool {
            node.id.as_str().starts_with("library-title-")
                || node.children.iter().any(has_library_title)
        }
        fn collect_text<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
            if matches!(node.role, Role::Text | Role::Heading) {
                out.push(node);
            }
            for child in &node.children {
                collect_text(child, out);
            }
        }

        let items = (0..12)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    &format!("Library Label {index}"),
                    vec![variant(
                        "native",
                        &format!("app-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let mut core = fixture_core(items);
        core.set_control_bindings(vec![ControlBinding {
            context: "global".into(),
            action: "Activate".into(),
            label: "Activate".into(),
            binding: "A".into(),
        }]);
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

        let children = &scene.root().children;
        let fade_index = children
            .iter()
            .position(|node| node.id.as_str() == "library-grid-footer-fade")
            .unwrap();
        let prompts_index = children
            .iter()
            .position(|node| node.id.as_str() == "prompts")
            .unwrap();
        let label_roots = children
            .iter()
            .enumerate()
            .filter(|(_, node)| has_library_title(node))
            .collect::<Vec<_>>();
        assert!(!label_roots.is_empty());
        assert!(label_roots.iter().all(|(index, _)| *index < fade_index));
        assert!(fade_index < prompts_index);
        assert!(
            children
                .iter()
                .all(|node| !node.id.as_str().starts_with("library-label-layer-"))
        );

        // Row-two title raster bounds may geometrically intersect the footer band by
        // design. Scene children are painter ordered, so the honest invariant is that
        // every title paints below the fade and prompts. Inspect the real text nodes so
        // a wrapper-only assertion cannot hide a lifted title layer (the 7c881ab bug).
        let mut footer_ink = Vec::new();
        collect_text(&children[prompts_index], &mut footer_ink);
        let mut label_ink = Vec::new();
        for (_, card) in &label_roots {
            collect_text(card, &mut label_ink);
        }
        assert!(!footer_ink.is_empty());
        let title_ink = label_ink
            .iter()
            .filter(|node| node.id.as_str().starts_with("library-title-"))
            .collect::<Vec<_>>();
        assert_eq!(title_ink.len(), label_roots.len());
        assert!(!title_ink.is_empty());
    }

    #[test]
    fn focused_library_card_footer_only_advertises_resolvable_actions() {
        let mut core = fixture_core(vec![item(
            "game",
            "Game",
            vec![variant("native", "game", Availability::Ready)],
        )]);
        core.set_control_bindings(vec![
            ControlBinding {
                context: "global".into(),
                action: "Search.open".into(),
                label: "Search".into(),
                binding: "SELECT".into(),
            },
            ControlBinding {
                context: "library".into(),
                action: "Filter.next".into(),
                label: "Filter".into(),
                binding: "Y".into(),
            },
            ControlBinding {
                context: "global".into(),
                action: "Activate".into(),
                label: "Activate".into(),
                binding: "A".into(),
            },
        ]);
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
        assert_eq!(
            node_by_id(scene.root(), "prompts")
                .unwrap()
                .accessible_label,
            "SELECT Search     Y Filter     A Details"
        );

        core.action(&ShellAction::Custom("Filter.next".into()));
        assert_eq!(core.library_filter, LibraryFilter::Alphabetical);
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
            assert!(
                find(scene.root(), &format!("home-card-plate-{id}")).is_none(),
                "Home cards must not paint an edition plate below their art"
            );
            assert_eq!(
                find(scene.root(), &format!("home-card-title-{id}"))
                    .map(|node| (node.type_role, node.style_token.as_str())),
                Some((TypeRole::Label, COLOR_SURFACE_CANVAS_TOKEN))
            );
        }
        assert_eq!(
            find(scene.root(), "home-shelf-label").map(|node| node.accessible_label.as_str()),
            Some("READY NOW · 2")
        );
    }

    #[test]
    fn chrome_status_uses_observed_battery_and_optional_attention() {
        struct Status(u8, Option<&'static str>);
        impl DeviceStatusPort for Status {
            fn status(&self) -> Result<DeviceStatus, String> {
                Ok(DeviceStatus {
                    battery_percent: self.0,
                    attention_message: self.1.map(str::to_owned),
                })
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
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        core.load_device_status(&Status(25, None));
        let scene = core.scene(metrics, "").unwrap();
        assert!(
            (node_by_id(scene.root(), "battery-level")
                .unwrap()
                .bounds
                .width
                - 4.0)
                .abs()
                < f32::EPSILON
        );
        assert!(node_by_id(scene.root(), "attention-pill").is_none());
        assert!(node_by_id(scene.root(), "wifi-glyph").is_none());

        let mut connected = pf_ports::FakeNetworkPort::new(NetworkState {
            interface_present: true,
            enabled: true,
            connected_ssid: Some("Moonlit Arcade".into()),
            signal: Some(78),
        });
        core.load_network(&mut connected);
        let scene = core.scene(metrics, "").unwrap();
        assert!(node_by_id(scene.root(), "wifi-glyph").is_some());

        core.load_device_status(&Status(100, Some("Controller battery low")));
        let scene = core.scene(metrics, "").unwrap();
        assert!(
            (node_by_id(scene.root(), "battery-level")
                .unwrap()
                .bounds
                .width
                - 16.0)
                .abs()
                < f32::EPSILON
        );
        assert_eq!(
            node_by_id(scene.root(), "attention").map(|node| node.accessible_label.as_str()),
            Some("Controller battery low")
        );
        let root = scene.root();
        let item_ids = [
            "room-keycap-left-border",
            "room-home",
            "room-library",
            "room-settings",
            "room-keycap-right-border",
        ];
        for pair in item_ids.windows(2) {
            let left = node_by_id(root, pair[0]).unwrap().bounds;
            let right = node_by_id(root, pair[1]).unwrap().bounds;
            assert!((right.x - (left.x + left.width) - ROOM_STRIP_GAP).abs() <= 1.0);
        }
        for (id, label) in [
            ("room-home", "Home"),
            ("room-library", "Library"),
            ("room-settings", "Settings"),
        ] {
            let room_bounds = node_by_id(root, id).unwrap().bounds;
            let advance = room_label_advance(label, 100);
            assert!((room_bounds.width - (advance + 2.0 * ROOM_HORIZONTAL_PADDING)).abs() <= 1.0);
            assert!((room_bounds.y - 16.0).abs() <= 1.0);
            assert!((room_bounds.height - 32.0).abs() <= 1.0);
            if let Some(underline) = node_by_id(root, &format!("{id}-underline")) {
                assert!((underline.bounds.width - advance).abs() <= 1.0);
                assert!(
                    (underline.bounds.x + underline.bounds.width / 2.0
                        - (room_bounds.x + room_bounds.width / 2.0))
                        .abs()
                        <= 1.0
                );
                assert!((underline.bounds.y - 49.0).abs() <= 1.0);
                assert!((underline.bounds.height - 3.0).abs() <= 1.0);
            }
        }
        for id in ["room-keycap-left", "room-keycap-right"] {
            let border = node_by_id(root, &format!("{id}-border")).unwrap().bounds;
            let fill = node_by_id(root, &format!("{id}-fill")).unwrap().bounds;
            let glyph = node_by_id(root, id).unwrap().bounds;
            assert!((border.y - 20.0).abs() <= 1.0);
            assert!((border.width - 24.0).abs() <= 1.0);
            assert!((border.height - 24.0).abs() <= 1.0);
            for inner in [fill, glyph] {
                assert!((inner.x - (border.x + KEYCAP_BORDER_WIDTH)).abs() <= 1.0);
                assert!((inner.y - (border.y + KEYCAP_BORDER_WIDTH)).abs() <= 1.0);
                assert!((inner.width - (border.width - 2.0 * KEYCAP_BORDER_WIDTH)).abs() <= 1.0);
                assert!((inner.height - (border.height - 2.0 * KEYCAP_BORDER_WIDTH)).abs() <= 1.0);
            }
        }
        let left = node_by_id(root, "room-keycap-left-border").unwrap().bounds;
        let right = node_by_id(root, "room-keycap-right-border").unwrap().bounds;
        assert!(((left.x + right.x + right.width) / 2.0 - 640.0).abs() <= 1.0);
        let pill_border = node_by_id(scene.root(), "attention-pill-border").unwrap();
        let rem_scale = f32::from(core.text_scale) / 100.0;
        let expected_pill_gap = SPACE_2 * rem_scale - TEXT_NODE_INLINE_INSET;
        let expected_pill_width = 2.0 * ATTENTION_PILL_HORIZONTAL_PADDING
            + ATTENTION_PILL_DOT_SIZE * rem_scale
            + expected_pill_gap
            + caption_text_width("Controller battery low", core.text_scale);
        let expected_pill_x =
            metrics.logical_width - ATTENTION_PILL_RIGHT_MARGIN - expected_pill_width;
        let expected_pill_height =
            ATTENTION_PILL_LABEL_HEIGHT * rem_scale + 2.0 * ATTENTION_PILL_VERTICAL_PADDING;
        assert!(
            (pill_border.bounds.x - expected_pill_x).abs() <= 2.0,
            "resolved pill bounds: {:?}",
            pill_border.bounds
        );
        assert!((pill_border.bounds.y - ATTENTION_PILL_TOP).abs() < f32::EPSILON);
        assert!((pill_border.bounds.width - expected_pill_width).abs() <= 2.0);
        assert!((pill_border.bounds.height - expected_pill_height).abs() < f32::EPSILON);
        assert!((pill_border.corner_radius - pill_border.bounds.height / 2.0).abs() < f32::EPSILON);
        assert_eq!(pill_border.style_token, COLOR_BORDER_HAIRLINE_TOKEN);
        assert_eq!(
            node_by_id(scene.root(), "attention-pill")
                .unwrap()
                .style_token,
            COLOR_SURFACE_RAISED_TOKEN
        );
        assert_eq!(
            node_by_id(scene.root(), "attention")
                .unwrap()
                .ink_token
                .as_deref(),
            Some(COLOR_TEXT_SECONDARY_TOKEN)
        );
        assert_eq!(
            node_by_id(scene.root(), "attention-dot")
                .unwrap()
                .style_token,
            COLOR_STATUS_ATTENTION_TOKEN
        );
        let wash = node_by_id(scene.root(), "hero-wash").unwrap();
        assert!(wash.accessible_label.contains("rgba(201,111,87,0.5)"));
        assert!(wash.accessible_label.contains("transparent 68%"));
        assert!(wash.accessible_label.contains("rgba(58,43,78,0.65)"));
        assert!(wash.accessible_label.contains("transparent 70%"));
        assert!(wash.accessible_label.contains("opacity 0.55"));
        assert!(matches!(wash.content, pf_scene::NodeContent::Image { .. }));
    }

    #[test]
    fn chrome_navigation_and_system_status_float_on_the_wash() {
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let core = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        let scene = core.scene(metrics, "").unwrap();

        for id in [
            "rooms",
            "status-cluster",
            "room-home",
            "room-library",
            "room-settings",
        ] {
            assert_eq!(
                node_by_id(scene.root(), id).unwrap().style_token,
                SCENE_TRANSPARENT_TOKEN,
                "{id} must not paint a raised strip behind floating chrome"
            );
        }
        for id in ["room-keycap-left-fill", "room-keycap-right-fill"] {
            assert_eq!(
                node_by_id(scene.root(), id).unwrap().style_token,
                COLOR_SURFACE_RAISED_TOKEN,
                "keycaps retain their designed raised fill"
            );
        }
    }

    #[test]
    fn attention_pill_internal_geometry_matches_tokens_at_every_text_scale() {
        fn clear_attention_label(node: &mut Node) -> bool {
            if node.id.as_str() == "attention" {
                node.accessible_label.clear();
                return true;
            }
            node.children.iter_mut().any(clear_attention_label)
        }

        fn hide_attention_dot(node: &mut Node) -> bool {
            if node.id.as_str() == "attention-dot" {
                node.style_token = SCENE_TRANSPARENT_TOKEN.into();
                return true;
            }
            node.children.iter_mut().any(hide_attention_dot)
        }

        fn changed_x_extent(rendered: &[u8], without: &[u8], width: usize) -> (usize, usize) {
            let columns = rendered
                .chunks_exact(4)
                .zip(without.chunks_exact(4))
                .enumerate()
                .filter_map(|(pixel, (painted, blank))| (painted != blank).then_some(pixel % width))
                .collect::<std::collections::BTreeSet<_>>();
            (
                *columns.first().expect("node must paint ink"),
                *columns.last().unwrap(),
            )
        }

        struct Status;
        impl DeviceStatusPort for Status {
            fn status(&self) -> Result<DeviceStatus, String> {
                Ok(DeviceStatus {
                    battery_percent: 50,
                    attention_message: Some("Controller battery low".to_owned()),
                })
            }
        }

        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let mut core = core();
        core.load_device_status(&Status);

        for text_scale in [100, 150, 200] {
            core.text_scale = text_scale;
            let scene = core.scene(metrics, "").unwrap();
            let border = node_by_id(scene.root(), "attention-pill-border")
                .expect("attention pill border")
                .bounds;
            let dot_node = node_by_id(scene.root(), "attention-dot").expect("attention dot");
            let dot = dot_node.bounds;
            let label = node_by_id(scene.root(), "attention")
                .expect("attention label")
                .bounds;
            let rem_scale = f32::from(text_scale) / 100.0;

            assert!((dot.width - 6.4 * rem_scale).abs() <= 0.01);
            assert!((dot.height - 6.4 * rem_scale).abs() <= 0.01);
            assert_eq!(dot_node.style_token, COLOR_STATUS_ATTENTION_TOKEN);
            assert!((dot_node.corner_radius - dot.width / 2.0).abs() <= 0.01);
            let control_box_gap = SPACE_2 * rem_scale - TEXT_NODE_INLINE_INSET;
            assert!((label.x - (dot.x + dot.width) - control_box_gap).abs() <= 0.01);
            assert!(
                (dot.y + dot.height / 2.0 - (label.y + label.height / 2.0)).abs() <= 0.01,
                "dot and its sibling label must share a center at {text_scale}%: dot={dot:?} label={label:?}"
            );
            assert!((dot.x - border.x - 16.0).abs() <= 0.01);
            assert!((border.x + border.width - label.x - label.width - 16.0).abs() <= 0.01);
            assert!((label.y - border.y - 8.0).abs() <= 0.01);
            assert!((border.y + border.height - label.y - label.height - 8.0).abs() <= 0.01);

            let mut rasterizer = Rasterizer::new();
            rasterizer.set_text_scale(rem_scale).unwrap();
            let rendered = rasterizer.render(&scene, metrics).unwrap();
            let mut without_label_root = scene.root().clone();
            assert!(clear_attention_label(&mut without_label_root));
            let without_label = rasterizer
                .render(
                    &Scene::new(without_label_root, scene.default_focus().clone()).unwrap(),
                    metrics,
                )
                .unwrap();
            let mut without_dot_root = scene.root().clone();
            assert!(hide_attention_dot(&mut without_dot_root));
            let without_dot = rasterizer
                .render(
                    &Scene::new(without_dot_root, scene.default_focus().clone()).unwrap(),
                    metrics,
                )
                .unwrap();
            let (label_ink_left, _) =
                changed_x_extent(&rendered.rgba, &without_label.rgba, rendered.width as usize);
            let (_, dot_ink_right) =
                changed_x_extent(&rendered.rgba, &without_dot.rgba, rendered.width as usize);
            let ink_gap = label_ink_left - dot_ink_right - 1;
            let expected_gap = usize::from(text_scale) * 8 / 100;
            assert!(
                ink_gap.abs_diff(expected_gap) <= 1,
                "attention dot-to-label ink gap must scale from space-2 at {text_scale}%: expected {expected_gap}px, got {ink_gap}px"
            );
        }
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn attention_text_change_damages_union_of_old_and_new_painted_bounds() {
        struct Status(&'static str);
        impl DeviceStatusPort for Status {
            fn status(&self) -> Result<DeviceStatus, String> {
                Ok(DeviceStatus {
                    battery_percent: 50,
                    attention_message: Some(self.0.to_owned()),
                })
            }
        }
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let mut core = core();
        let mut rasterizer = Rasterizer::new();
        core.load_device_status(&Status("Battery low"));
        let old = core.scene(metrics, "").unwrap();
        let old_bounds = node_by_id(old.root(), "attention-pill-border")
            .unwrap()
            .bounds;
        rasterizer.render(&old, metrics).unwrap();

        core.load_device_status(&Status("Controller battery critically low"));
        let new = core.scene(metrics, "").unwrap();
        let new_bounds = node_by_id(new.root(), "attention-pill-border")
            .unwrap()
            .bounds;
        let damage = rasterizer
            .render(&new, metrics)
            .unwrap()
            .damage
            .expect("message change damages the pill");
        let union_left = old_bounds.x.min(new_bounds.x).floor() as u32;
        let union_right = (old_bounds.x + old_bounds.width)
            .max(new_bounds.x + new_bounds.width)
            .ceil() as u32;
        let union_top = old_bounds.y.min(new_bounds.y).floor() as u32;
        let union_bottom = (old_bounds.y + old_bounds.height)
            .max(new_bounds.y + new_bounds.height)
            .ceil() as u32;
        assert!(damage.x <= union_left);
        assert!(damage.x + damage.width >= union_right);
        assert!(damage.y <= union_top);
        assert!(damage.y + damage.height >= union_bottom);
    }

    #[test]
    fn home_layout_pass_desktop_smoke_indicator() {
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let core = core();
        let mut samples = (0..12)
            .map(|_| {
                let started = std::time::Instant::now();
                let scene = core.scene(metrics, "").unwrap();
                assert!(node_by_id(scene.root(), "rooms-layout-anchor").is_some());
                started.elapsed()
            })
            .collect::<Vec<_>>();
        samples.sort_unstable();
        let p95 = samples[samples.len() * 95 / 100];
        println!("home-layout desktop p95={p95:?}");
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
            let text = theme.resolve(base, STATE_REST_TEXT_TOKEN).unwrap();
            let surface = theme.resolve(base, COLOR_SURFACE_RAISED_TOKEN).unwrap();
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
        fn contains_label(node: &Node, text: &str) -> bool {
            node.accessible_label.contains(text)
                || node
                    .children
                    .iter()
                    .any(|child| contains_label(child, text))
        }

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
        assert!(contains_label(
            scene.root(),
            "connect to Wi-Fi to use this edition"
        ));
        assert!(!card.accessible_label.contains("EDITION PLATE"));
        assert!(
            card.children
                .iter()
                .all(|node| node.id.as_str() != "library-card-plate-long"),
            "Library captions sit on the canvas without an edition plate"
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
        for part in ["art", "initial-plate", "plate-kind"] {
            assert!(
                favorite
                    .children
                    .iter()
                    .any(|node| node.id.as_str() == format!("home-card-{part}-ridge"))
            );
        }
        assert!(
            favorite
                .children
                .iter()
                .any(|node| node.id.as_str() == "home-card-title-ridge"),
            "the painted Home title belongs to its focus owner"
        );
        assert!(favorite.children.iter().any(|node| {
            node.id.as_str() == "home-card-initial-plate-ridge" && node.accessible_label == "R"
        }));
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
    #[allow(clippy::float_cmp)] // Exact identity at 100% is part of the layout contract.
    fn home_vertical_budget_keeps_the_full_chain_and_prompts_disjoint() {
        fn intersects(a: Bounds, b: Bounds) -> bool {
            a.x < b.x + b.width
                && a.x + a.width > b.x
                && a.y < b.y + b.height
                && a.y + a.height > b.y
        }

        fn collect_matching<'a>(
            node: &'a Node,
            predicate: &impl Fn(&str) -> bool,
            matches: &mut Vec<&'a Node>,
        ) {
            if predicate(node.id.as_str()) {
                matches.push(node);
            }
            for child in &node.children {
                collect_matching(child, predicate, matches);
            }
        }

        let mut ready = variant("stream", "network-game", Availability::Ready);
        ready.requirements.push(Requirement {
            capability: "network".into(),
            optional: false,
        });
        let network_game = item("network-game", "Network Game", vec![ready]);
        let mut core = fixture_core(vec![network_game]);
        core.items[0].favorite = true;
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        for text_scale in [100, 150, 200] {
            core.text_scale = text_scale;
            let scene = core.scene(metrics, "A Open · X Details").unwrap();
            let title = node_by_id(scene.root(), "hero-title").unwrap();
            let status = node_by_id(scene.root(), "hero-status").unwrap();
            let shelf = node_by_id(scene.root(), "home-shelf-label").unwrap();
            let card = node_by_id(scene.root(), "item-network-game").unwrap();
            let layout = home_vertical_layout(text_scale);

            for (before, after) in [(title, status), (status, shelf), (shelf, card)] {
                assert!(
                    before.bounds.y + before.bounds.height <= after.bounds.y,
                    "{} {:?} overlaps {} {:?} at {text_scale}%",
                    before.id.as_str(),
                    before.bounds,
                    after.id.as_str(),
                    after.bounds
                );
            }
            let card_bottom = card.bounds.y + card.bounds.height;
            assert!(card_bottom <= HOME_STACK_HARD_LIMIT);
            if text_scale <= 150 {
                assert!(card_bottom <= HOME_STACK_BUDGET);
            }

            let mut card_text = Vec::new();
            collect_matching(
                card,
                &|id| id.starts_with("home-card-") || id.starts_with("favorite-pin-"),
                &mut card_text,
            );
            card_text.retain(|node| node.role == Role::Text);
            let mut prompt_nodes = Vec::new();
            collect_matching(
                scene.root(),
                &|id| id.starts_with("home-prompt-keycap-") || id.starts_with("home-prompt-verb-"),
                &mut prompt_nodes,
            );
            for text in &card_text {
                for prompt in &prompt_nodes {
                    assert!(
                        !intersects(text.bounds, prompt.bounds),
                        "card text {} {:?} overlaps prompt {} {:?} at {text_scale}%",
                        text.id.as_str(),
                        text.bounds,
                        prompt.id.as_str(),
                        prompt.bounds
                    );
                }
            }

            if text_scale == 100 {
                assert_eq!(layout.title_y, 144.0);
                assert_eq!(
                    layout.shelf_label_y - status.bounds.y - status.bounds.height,
                    88.0
                );
                assert_eq!(title.bounds.y, 144.0);
                assert_eq!(shelf.bounds.y, 344.0);
            }
        }

        for (text_scale, expected_top, expected_air) in
            [(150, 107.142_86, 38.857_14), (200, 96.0, 24.0)]
        {
            let layout = home_vertical_layout(text_scale);
            let status_height = scaled_text_box_height(32.0, text_scale);
            assert!((layout.title_y - expected_top).abs() < 0.000_1);
            assert!(
                (layout.shelf_label_y - layout.status_y - status_height - expected_air).abs()
                    < 0.000_1
            );
        }
        core.text_scale = 200;
        let scene = core.scene(metrics, "A Open · X Details").unwrap();
        assert!(node_by_id(scene.root(), "home-card-reason-network-game").is_none());
    }

    #[test]
    fn narrow_home_shelf_actionable_cards_do_not_overlap() {
        fn intersects(a: Bounds, b: Bounds) -> bool {
            a.x < b.x + b.width
                && a.x + a.width > b.x
                && a.y < b.y + b.height
                && a.y + a.height > b.y
        }

        let items = (0..6)
            .map(|index| {
                item(
                    &format!("game-{index}"),
                    &format!("Game {index}"),
                    vec![variant(
                        &format!("variant-{index}"),
                        &format!("game-{index}"),
                        Availability::Ready,
                    )],
                )
            })
            .collect();
        let scene = fixture_core(items)
            .scene(
                SurfaceMetrics {
                    logical_width: 640.0,
                    logical_height: 480.0,
                    scale: 1.0,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        let cards = scene
            .root()
            .children
            .iter()
            .filter(|node| node.action == Some(NodeAction::Activate))
            .collect::<Vec<_>>();

        assert_eq!(cards.len(), 2, "640px surface fits exactly two shelf cards");
        for (index, card) in cards.iter().enumerate() {
            for other in cards.iter().skip(index + 1) {
                assert!(
                    !intersects(card.bounds, other.bounds),
                    "{} {:?} overlaps {} {:?}",
                    card.id.as_str(),
                    card.bounds,
                    other.id.as_str(),
                    other.bounds
                );
            }
        }
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
    fn home_ready_card_focus_owner_is_actionable() {
        fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            (node.id.as_str() == id)
                .then_some(node)
                .or_else(|| node.children.iter().find_map(|child| find(child, id)))
        }

        let mut core = fixture_core(vec![item(
            "ready",
            "Ready Game",
            vec![variant("native", "ready-app", Availability::Ready)],
        )]);
        core.focus = 0;
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
        let card = find(scene.root(), "item-ready").unwrap();
        let art = find(scene.root(), "home-card-art-ready").unwrap();

        assert_eq!(card.role, Role::ListItem);
        assert!(card.bounds.width >= art.bounds.width);
        assert!(card.bounds.height > art.bounds.height);
        assert_eq!(scene.default_focus(), &card.id);
        assert!(card.action.is_some(), "the focus owner must be actionable");
        assert!(
            !card.accessible_label.trim().is_empty(),
            "the focus owner must have an accessible label"
        );
        assert_eq!(card.style_token, COLOR_SURFACE_CANVAS_TOKEN);
        let focus_bounds = find(scene.root(), scene.default_focus().as_str())
            .unwrap()
            .bounds;
        assert!(focus_bounds.width >= art.bounds.width);
        assert!(focus_bounds.height > art.bounds.height);
        assert!(find(scene.root(), "home-card-initial-plate-ready").is_some());
        assert!(
            find(scene.root(), "home-card-veil-ready").is_none(),
            "a ready card must not receive an unavailable veil"
        );
        assert!(
            find(card, "home-card-title-ready").is_some(),
            "the painted title must belong to the card focus owner"
        );
    }

    #[test]
    fn home_activation_and_footer_follow_the_filtered_ready_shelf() {
        fn prompt(scene: &Scene) -> &str {
            scene
                .root()
                .children
                .iter()
                .find(|node| node.id.as_str() == "prompts")
                .map(|node| node.accessible_label.as_str())
                .unwrap()
        }

        let bindings = || {
            vec![ControlBinding {
                context: "global".into(),
                action: "Activate".into(),
                label: "Activate".into(),
                binding: "A".into(),
            }]
        };
        let supplied_footer = "A  Open     PF  Safe Return";
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        let mut core = fixture_core(vec![
            item(
                "ready",
                "Ready Game",
                vec![variant("native", "ready-app", Availability::Ready)],
            ),
            item(
                "unavailable",
                "Unavailable Game",
                vec![variant(
                    "stream",
                    "unavailable-app",
                    Availability::NeedsNetwork {
                        reason: "connect to Wi-Fi".into(),
                    },
                )],
            ),
            item(
                "setup",
                "Setup Game",
                vec![variant(
                    "setup",
                    "setup-app",
                    Availability::NeedsSetup {
                        reason: "choose a profile".into(),
                    },
                )],
            ),
            item(
                "ready-two",
                "Ready Game Two",
                vec![variant("native", "ready-app-two", Availability::Ready)],
            ),
        ]);
        core.items[3].favorite = true;
        core.set_control_bindings(bindings());
        let home = core.scene(metrics, supplied_footer).unwrap();
        assert_eq!(prompt(&home), "A Open · PF  Safe Return");
        let prompts = node_by_id(home.root(), "prompts").unwrap();
        let footer_top = metrics.logical_height - PROMPTS_AREA_HEIGHT;
        assert!(prompts.bounds.x >= 0.0);
        assert!(prompts.bounds.x + prompts.bounds.width <= metrics.logical_width);
        assert!(prompts.bounds.y >= footer_top);
        assert!(
            prompts
                .children
                .iter()
                .all(|child| child.bounds.y >= footer_top
                    && child.bounds.y + child.bounds.height <= metrics.logical_height)
        );
        assert!(node_by_id(home.root(), "home-prompt-keycap-0").is_some());
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "ready-app".into(),
            }))
        );

        core.go(Route::Home);
        core.focus = 1;
        assert_eq!(
            prompt(&core.scene(metrics, supplied_footer).unwrap()),
            "A Open · PF  Safe Return"
        );
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "ready-app-two".into(),
            }))
        );
    }

    #[test]
    fn quiet_console_mockup_cues_and_binding_derived_footers_are_emitted() {
        fn find<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            (node.id.as_str() == id)
                .then_some(node)
                .or_else(|| node.children.iter().find_map(|child| find(child, id)))
        }
        fn prompt_labels(node: &Node) -> Vec<&str> {
            node.children
                .iter()
                .filter(|child| {
                    let id = child.id.as_str();
                    id.strip_prefix("home-prompt-keycap-")
                        .is_some_and(|suffix| suffix.parse::<usize>().is_ok())
                        || id
                            .strip_prefix("home-prompt-verb-")
                            .is_some_and(|suffix| suffix.parse::<usize>().is_ok())
                })
                .map(|child| child.accessible_label.as_str())
                .collect()
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
        core.set_control_bindings(
            [
                ("Back", "Back", "B"),
                ("Quick", "Quick", "X"),
                ("Activate", "Open", "A"),
            ]
            .into_iter()
            .map(|(action, label, binding)| ControlBinding {
                context: "shell".into(),
                action: action.into(),
                label: label.into(),
                binding: binding.into(),
            })
            .collect(),
        );
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
        let footer_clusters = ready
            .root()
            .children
            .iter()
            .filter(|node| node.id.as_str() == "prompts")
            .collect::<Vec<_>>();
        assert_eq!(
            footer_clusters.len(),
            1,
            "Details must emit exactly one footer hint cluster"
        );
        let details_prompts = footer_clusters[0];
        assert_eq!(
            details_prompts.role,
            Role::Group,
            "Details footer must not retain a centered plain-text prompt sibling"
        );
        assert!(
            details_prompts.accessible_label.is_empty(),
            "Details footer cluster must not carry the legacy centered text label"
        );
        assert!(
            details_prompts
                .children
                .iter()
                .any(|node| node.id.as_str().starts_with("home-prompt-keycap-")),
            "Details footer cluster must contain the keycap chips"
        );
        assert_eq!(
            find(ready.root(), "detail-title").map(|node| node.style_token.as_str()),
            Some(COLOR_SURFACE_CANVAS_TOKEN)
        );
        assert!(find(ready.root(), "detail-ways-heading").is_some());
        assert!(
            find(ready.root(), "detail-open").is_some_and(|node| node.accessible_label == "▶ Play")
        );
        assert_eq!(
            prompt_labels(find(ready.root(), "prompts").unwrap()),
            ["B", "Back", "X", "Favorite", "A", "Play"]
        );

        let mut remapped = desktop_bindings();
        remapped
            .iter_mut()
            .find(|binding| binding.action == "Quick")
            .unwrap()
            .binding = "START".into();
        core.set_control_bindings(remapped);
        let remapped = core.scene(metrics, "wrong caller footer").unwrap();
        let remapped_labels = prompt_labels(find(remapped.root(), "prompts").unwrap());
        assert!(remapped_labels.contains(&"START") && remapped_labels.contains(&"Favorite"));

        core.set_control_bindings(
            desktop_bindings()
                .into_iter()
                .filter(|binding| binding.action != "Quick")
                .collect(),
        );
        let favorite_unbound = core.scene(metrics, "wrong caller footer").unwrap();
        let favorite_unbound_labels =
            prompt_labels(find(favorite_unbound.root(), "prompts").unwrap());
        assert!(!favorite_unbound_labels.contains(&"Favorite"));
        assert!(!favorite_unbound_labels.contains(&"Unfavorite"));

        core.selected_item = Some(1);
        let unavailable = core.scene(metrics, "wrong caller footer").unwrap();
        let labels = format!("{unavailable:?}");
        assert!(labels.contains("⊘ Stream"));
        assert!(labels.contains("choose a profile"));
        assert!(find(unavailable.root(), "detail-open").is_none());
        let unavailable_labels = prompt_labels(find(unavailable.root(), "prompts").unwrap());
        assert!(!unavailable_labels.contains(&"Play"));
        assert!(!unavailable_labels.contains(&"Open"));
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
        let chip_label = find(library.root(), "library-filter-2-label").unwrap();
        assert_eq!(chip_label.style_token, STATE_REST_SURFACE_TOKEN);

        core.selected_item = Some(0);
        core.go(Route::Details);
        let details = core.scene(metrics, "").unwrap();
        for id in [
            "detail-provenance",
            "detail-availability-reason",
            "detail-ways-heading",
        ] {
            assert_eq!(
                find(details.root(), id).unwrap().style_token,
                COLOR_SURFACE_CANVAS_TOKEN
            );
        }
        assert_eq!(
            find(details.root(), "detail-variant-0-name")
                .unwrap()
                .style_token,
            STATE_REST_SURFACE_TOKEN
        );

        core.go(Route::Settings);
        let settings = core.scene(metrics, "").unwrap();
        assert_eq!(
            find(settings.root(), "settings-section-title")
                .unwrap()
                .style_token,
            COLOR_SURFACE_CANVAS_TOKEN
        );
        assert_eq!(
            find(settings.root(), "settings-nav-accessibility-label")
                .unwrap()
                .style_token,
            STATE_REST_SURFACE_TOKEN
        );
        assert!(
            find(settings.root(), "settings-nav-accessibility")
                .unwrap()
                .state
                .focused,
            "the navigation label must declare the surface painted by its focused parent"
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
        fn paint_order<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
            out.push(node);
            for child in &node.children {
                paint_order(child, out);
            }
        }
        let mut children = Vec::new();
        paint_order(scene.root(), &mut children);

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

            let border = children[border_index];
            let fill = children[fill_index];
            let label = children[label_index];
            assert_eq!(border.style_token, COLOR_BORDER_STRONG_TOKEN);
            assert_eq!(fill.style_token, COLOR_SURFACE_RAISED_TOKEN);
            assert_eq!(label.style_token, COLOR_SURFACE_RAISED_TOKEN);
            assert!((fill.bounds.x - border.bounds.x - 1.0).abs() < f32::EPSILON);
            assert!((fill.bounds.y - border.bounds.y - 1.0).abs() < f32::EPSILON);
            assert!((border.bounds.width - fill.bounds.width - 2.0).abs() < f32::EPSILON);
            assert!((border.bounds.height - fill.bounds.height - 2.0).abs() < f32::EPSILON);
            assert!(label.bounds.x >= fill.bounds.x && label.bounds.y >= fill.bounds.y);
            assert!(
                label.bounds.x + label.bounds.width <= fill.bounds.x + fill.bounds.width
                    && label.bounds.y + label.bounds.height <= fill.bounds.y + fill.bounds.height
            );
        }
    }

    #[test]
    fn chrome_room_strip_follows_css_geometry() {
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        let mut core = core();
        for text_scale in [100, 150, 200] {
            core.text_scale = text_scale;
            for route in [Route::Home, Route::Library, Route::Settings] {
                core.go(route);
                let scene = core.scene(metrics, "").unwrap();
                let root = scene.root();
                assert!(
                    node_by_id(root, "rooms-layout-anchor").is_some(),
                    "{route:?} must render the status strip through the layout seam"
                );
                let rooms = node_by_id(root, "rooms").unwrap();
                assert!(
                    (rooms.bounds.x + rooms.bounds.width / 2.0 - 640.0).abs() < f32::EPSILON,
                    "{route:?} rooms bounds={:?}",
                    rooms.bounds
                );
                assert!(
                    (rooms.bounds.y + rooms.bounds.height / 2.0 - 32.0).abs() < f32::EPSILON,
                    "{route:?} rooms bounds={:?}",
                    rooms.bounds
                );

                let item_ids = [
                    "room-keycap-left-border",
                    "room-home",
                    "room-library",
                    "room-settings",
                    "room-keycap-right-border",
                ];
                for pair in item_ids.windows(2) {
                    let left = node_by_id(root, pair[0]).unwrap();
                    let right = node_by_id(root, pair[1]).unwrap();
                    let gap = right.bounds.x - (left.bounds.x + left.bounds.width);
                    assert!(
                        (gap - ROOM_STRIP_GAP).abs() < f32::EPSILON,
                        "{pair:?} gap={gap}"
                    );
                }

                for (id, label) in [
                    ("room-home", "Home"),
                    ("room-library", "Library"),
                    ("room-settings", "Settings"),
                ] {
                    let label_node = node_by_id(root, id).unwrap();
                    let expected_width =
                        room_label_advance(label, text_scale) + 2.0 * ROOM_HORIZONTAL_PADDING;
                    assert!(
                        (label_node.bounds.width - expected_width).abs() < f32::EPSILON,
                        "{route:?} {label} at {text_scale}% width={} expected={expected_width}",
                        label_node.bounds.width
                    );
                    assert_eq!(label_node.text_align, TextAlign::Center);
                    let scale_delta = f32::from(text_scale) / 100.0 - 1.0;
                    assert!(
                        (label_node.bounds.height - (32.0 + 12.0 * scale_delta)).abs()
                            < f32::EPSILON
                    );
                    assert!(
                        (label_node.bounds.y + label_node.bounds.height / 2.0 - 32.0).abs()
                            < f32::EPSILON
                    );
                    if let Some(underline) = node_by_id(root, &format!("{id}-underline")) {
                        assert!(
                            (underline.bounds.width - room_label_advance(label, text_scale)).abs()
                                < f32::EPSILON
                        );
                        assert!(
                            (underline.bounds.x + underline.bounds.width / 2.0
                                - (label_node.bounds.x + label_node.bounds.width / 2.0))
                                .abs()
                                < f32::EPSILON
                        );
                    }
                }

                for id in ["room-keycap-left", "room-keycap-right"] {
                    let border = node_by_id(root, &format!("{id}-border")).unwrap();
                    let glyph = node_by_id(root, id).unwrap();
                    assert_eq!(
                        (border.bounds.width, border.bounds.height),
                        (
                            KEYCAP_MIN_WIDTH,
                            KEYCAP_HEIGHT + 8.0 * (f32::from(text_scale) / 100.0 - 1.0)
                        )
                    );
                    assert!(
                        (glyph.bounds.x + glyph.bounds.width / 2.0
                            - (border.bounds.x + border.bounds.width / 2.0))
                            .abs()
                            <= 1.0
                    );
                    assert!(
                        (glyph.bounds.y + glyph.bounds.height / 2.0
                            - (border.bounds.y + border.bounds.height / 2.0))
                            .abs()
                            <= 1.0
                    );
                    assert_eq!(glyph.text_align, TextAlign::Center);
                }
            }
        }
    }

    #[test]
    #[allow(clippy::float_cmp)] // Token multiplication is exact for these integer-valued f32s.
    fn quiet_console_component_radii_follow_tokens_and_text_scale() {
        fn find<'a>(node: &'a Node, id: &str) -> &'a Node {
            if node.id.as_str() == id {
                return node;
            }
            node.children
                .iter()
                .find_map(|child| {
                    (child.id.as_str() == id)
                        .then_some(child)
                        .or_else(|| find_optional(child, id))
                })
                .unwrap_or_else(|| panic!("missing scene node {id}"))
        }

        fn find_optional<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
            (node.id.as_str() == id).then_some(node).or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| find_optional(child, id))
            })
        }

        fn assert_radius(root: &Node, id: &str, token_radius: f32) {
            let node = find(root, id);
            assert_eq!(
                node.corner_radius,
                token_radius.min(node.bounds.width.min(node.bounds.height) / 2.0),
                "{id} must clamp its token radius to CSS corner bounds"
            );
        }

        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };

        for (text_scale, multiplier) in [(100, 1.0), (200, 2.0)] {
            let mut core = core();
            core.text_scale = text_scale;

            let home = core.scene(metrics, "").unwrap();
            assert_radius(home.root(), "item-i0", 10.0 * multiplier);
            assert_radius(home.root(), "home-card-art-i0", 10.0 * multiplier);
            assert_radius(home.root(), "room-keycap-left", 6.0 * multiplier);

            core.go(Route::Library);
            let library = core.scene(metrics, "").unwrap();
            assert_radius(library.root(), "library-search", 10.0 * multiplier);
            assert_radius(library.root(), "library-filter-0", 10.0 * multiplier);
            assert_radius(library.root(), "library-item-i0", 10.0 * multiplier);

            core.selected_item = Some(0);
            core.go(Route::Details);
            let details = core.scene(metrics, "").unwrap();
            assert_radius(details.root(), "detail-cover", 16.0 * multiplier);
            assert_radius(details.root(), "detail-variant-0", 10.0 * multiplier);

            core.load_preferences(&preferences(true), true).unwrap();
            core.text_scale = text_scale;
            core.go(Route::Settings);
            let settings = settings_scene(&core);
            assert_radius(
                settings.root(),
                "settings-nav-accessibility",
                10.0 * multiplier,
            );
            assert_radius(
                settings.root(),
                "settings-row-accessibility-textScale",
                10.0 * multiplier,
            );
            assert_radius(
                settings.root(),
                "settings-text-scale-segmented-control",
                10.0 * multiplier,
            );
            assert_radius(
                settings.root(),
                "settings-toggle-accessibility-highContrast-track",
                999.0 * multiplier,
            );
            assert_radius(
                settings.root(),
                "settings-toggle-accessibility-highContrast-knob",
                999.0 * multiplier,
            );
        }
    }

    #[test]
    fn component_fills_never_paint_over_overlapping_text() {
        fn overlaps(a: Bounds, b: Bounds) -> bool {
            a.x < b.x + b.width
                && a.x + a.width > b.x
                && a.y < b.y + b.height
                && a.y + a.height > b.y
        }
        fn assert_order(node: &Node) {
            if node.id.as_str() != "quiet-console" {
                for (index, text) in node.children.iter().enumerate().filter(|(_, child)| {
                    matches!(child.role, Role::Text | Role::Heading)
                        && !child.accessible_label.trim().is_empty()
                }) {
                    for fill in &node.children[index + 1..] {
                        assert!(
                            fill.role != Role::Group || !overlaps(text.bounds, fill.bounds),
                            "component {} paints fill {} over text {}",
                            node.id.as_str(),
                            fill.id.as_str(),
                            text.id.as_str()
                        );
                    }
                }
            }
            for child in &node.children {
                assert_order(child);
            }
        }

        let mut core = core();
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        for route in [Route::Home, Route::Library, Route::Details, Route::Settings] {
            core.go(route);
            if route == Route::Details {
                core.selected_item = Some(0);
            }
            assert_order(core.scene(metrics, "").unwrap().root());
        }
    }

    #[test]
    fn every_action_in_evidence_scenes_has_an_accessible_label() {
        let mut core = core();
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Default::default(),
            orientation: pf_scene::Orientation::Landscape,
        };
        for route in [Route::Home, Route::Library, Route::Details, Route::Settings] {
            core.go(route);
            if route == Route::Details {
                core.selected_item = Some(0);
            }
            let scene = core.scene(metrics, "").unwrap();
            fn check(node: &Node) {
                if node.action.is_some() {
                    assert!(
                        !node.accessible_label.trim().is_empty(),
                        "action has no accessible label on {}",
                        node.id.as_str()
                    );
                }
                for child in &node.children {
                    check(child);
                }
            }
            check(scene.root());
        }
    }

    #[test]
    fn focused_variant_is_one_surface_without_per_line_plates() {
        let mut core = core();
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
        let row = node_by_id(scene.root(), "detail-variant-0").unwrap();
        assert!(row.state.focused);
        assert_eq!(row.style_token, STATE_REST_SURFACE_TOKEN);
        assert!(
            row.children
                .iter()
                .all(|child| { child.role == Role::Text && child.style_token == row.style_token })
        );
    }

    #[test]
    fn details_match_quiet_console_structural_roles() {
        let mut catalog_item = item(
            "quiet-detail",
            "Quiet Detail",
            vec![
                variant("native", "quiet-detail", Availability::Ready),
                variant("stream", "quiet-detail", Availability::Ready),
            ],
        );
        catalog_item.tags.extend([
            "description:A measured catalog description that may wrap across two lines.".into(),
            "last-played:Yesterday".into(),
            "size:2.4 GB".into(),
        ]);
        let mut core = fixture_core(vec![catalog_item]);
        core.set_control_bindings(
            [
                ("Back", "Back", "B"),
                ("Quick", "Quick", "X"),
                ("Activate", "Open", "A"),
            ]
            .into_iter()
            .map(|(action, label, binding)| ControlBinding {
                context: "shell".into(),
                action: action.into(),
                label: label.into(),
                binding: binding.into(),
            })
            .collect(),
        );
        core.selected_item = Some(0);
        core.go(Route::Details);
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        core.load_history(&[history_entry(
            "quiet-detail",
            Some(start),
            Some((start + Duration::from_secs(3_600), EndPrecision::Observed)),
        )]);
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

        let cover = node_by_id(root, "detail-cover").unwrap();
        assert!((cover.bounds.x - 48.0).abs() < f32::EPSILON);
        assert!((cover.bounds.y - 96.0).abs() < f32::EPSILON);
        assert!((cover.bounds.width - 320.0).abs() < f32::EPSILON);
        assert!((cover.bounds.height - 428.0).abs() < f32::EPSILON);
        let provenance = node_by_id(root, "detail-provenance").unwrap();
        assert!((provenance.bounds.x - 416.0).abs() < f32::EPSILON);
        assert!((provenance.bounds.y - 96.0).abs() < f32::EPSILON);
        assert!((provenance.bounds.width - 816.0).abs() < f32::EPSILON);

        assert_eq!(
            node_by_id(root, "detail-title").unwrap().type_role,
            TypeRole::Hero
        );
        let first = node_by_id(root, "detail-variant-0").unwrap();
        let second = node_by_id(root, "detail-variant-1").unwrap();
        assert!(first.bounds.y + first.bounds.height < second.bounds.y);
        assert!(node_by_id(first, "detail-variant-0-selection-mark").is_some());
        assert!(node_by_id(second, "detail-variant-1-selection-mark").is_none());

        let fact_heading_y = node_by_id(root, "detail-fact-developer-heading")
            .unwrap()
            .bounds
            .y;
        for id in [
            "detail-fact-installed-heading",
            "detail-fact-time-played-heading",
            "detail-fact-offline-heading",
        ] {
            assert!((node_by_id(root, id).unwrap().bounds.y - fact_heading_y).abs() < f32::EPSILON);
        }

        let prompts = node_by_id(root, "prompts").unwrap();
        for index in 0..3 {
            assert!(node_by_id(prompts, &format!("home-prompt-keycap-{index}-border")).is_some());
        }
        let prompt_right = prompts
            .children
            .iter()
            .map(|node| node.bounds.x + node.bounds.width)
            .fold(0.0_f32, f32::max);
        assert!(
            prompt_right > 1200.0,
            "prompt keycaps must remain right-aligned"
        );
    }

    #[test]
    fn text_scale_values_are_individual_bordered_chips() {
        let mut core = core();
        core.load_preferences(&preferences(true), true).unwrap();
        core.go(Route::Settings);
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
        for value in ["100%", "150%", "200%"] {
            let chip =
                node_by_id(scene.root(), &format!("settings-text-scale-chip-{value}")).unwrap();
            assert!(chip.corner_radius > 0.0);
            assert_eq!(
                chip.border_token.as_deref(),
                Some(COLOR_BORDER_HAIRLINE_TOKEN)
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
            "quick-panel-surface",
            "quick-section-divider",
            "quick-power-power-off",
            "quick-power-restart",
            "quick-power-idle",
        ] {
            assert!(
                ids.contains(&required),
                "missing semantic anatomy {required}"
            );
        }
        assert!(!ids.contains(&"quick-power-heading"));
        assert!(!ids.contains(&"quick-power-sleep"));

        let panel = node_by_id(unsupported_scene.root(), "quick-panel-surface").unwrap();
        assert_eq!(panel.style_token, COLOR_SURFACE_RAISED_TOKEN);
        assert_eq!(
            panel.border_token.as_deref(),
            Some(COLOR_BORDER_HAIRLINE_TOKEN)
        );
        assert!((panel.border_width - 1.0).abs() < f32::EPSILON);
        assert_eq!(panel.elevation, Elevation::Elev2);
        assert!((panel.corner_radius - RADIUS_L).abs() < f32::EPSILON);
        let focused = node_by_id(unsupported_scene.root(), "quick-0").unwrap();
        assert_eq!(focused.style_token, STATE_REST_SURFACE_TOKEN);
        assert_eq!(
            focused.border_token.as_deref(),
            Some(STATE_FOCUSED_RING_TOKEN)
        );
        assert!((focused.border_width - 2.0).abs() < f32::EPSILON);
        assert!((focused.corner_radius - RADIUS_M).abs() < f32::EPSILON);
        let prompts = node_by_id(unsupported_scene.root(), "prompts").unwrap();
        assert_eq!(prompts.role, Role::Group);
        assert_eq!(
            prompts.children.len(),
            6,
            "two prompts use one keycap cluster"
        );

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

    fn assert_quick_vertical_flow(scene: &Scene, ids: &[&str]) {
        let nodes: Vec<_> = ids
            .iter()
            .map(|id| node_by_id(scene.root(), id).unwrap_or_else(|| panic!("missing {id}")))
            .collect();
        for pair in nodes.windows(2) {
            let before = pair[0];
            let after = pair[1];
            assert!(
                before.bounds.y + before.bounds.height <= after.bounds.y,
                "{} {:?} overlaps {} {:?}",
                before.id.as_str(),
                before.bounds,
                after.id.as_str(),
                after.bounds
            );
        }
    }

    fn max_scale_quick_core(power_status: Option<&str>) -> ShellCore {
        let mut core = core();
        core.text_scale = 200;
        core.load_power(&FakePowerPort::new(
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
                    support: Support::Supported,
                },
            ],
            IdlePolicy::default(),
        ));
        core.power_status = power_status.map(str::to_owned);
        core.go(Route::Quick);
        core
    }

    fn assert_quick_content_within_budget(scene: &Scene) {
        let panel = node_by_id(scene.root(), "quick-panel-surface").unwrap();
        let budget_bottom = panel.bounds.y + panel.bounds.height - SPACE_5;
        for node in &scene.root().children {
            if node.id.as_str().starts_with("quick-") && node.id.as_str() != "quick-panel-surface" {
                assert!(
                    node.bounds.y + node.bounds.height <= budget_bottom,
                    "{} {:?} escapes Quick budget bottom {budget_bottom}",
                    node.id.as_str(),
                    node.bounds
                );
            }
        }
    }

    #[test]
    fn quick_max_scale_yields_truth_to_keep_rows_within_panel() {
        let core = max_scale_quick_core(None);
        let scene = quick_scene(&core);

        assert_quick_content_within_budget(&scene);
        assert!(node_by_id(scene.root(), "quick-truth").is_none());
        assert_quick_vertical_flow(
            &scene,
            &[
                "quick-power-sleep",
                "quick-power-idle",
                "quick-capture-screenshot",
            ],
        );
    }

    #[test]
    fn quick_max_scale_never_drops_or_overflows_power_status() {
        let core = max_scale_quick_core(Some("Power actions are unavailable"));
        let scene = quick_scene(&core);

        assert_quick_content_within_budget(&scene);
        assert!(node_by_id(scene.root(), "quick-power-status").is_some());
        assert!(node_by_id(scene.root(), "quick-truth").is_none());
        assert_quick_vertical_flow(&scene, &["quick-capture-screenshot", "quick-power-status"]);
    }

    #[test]
    fn quick_default_scale_keeps_truth_note() {
        let mut core = core();
        core.go(Route::Quick);

        assert!(node_by_id(quick_scene(&core).root(), "quick-truth").is_some());
    }

    #[test]
    fn quick_rows_follow_scaled_sleep_auto_sleep_order() {
        let mut core = core();
        core.text_scale = 200;
        core.load_power(&FakePowerPort::new(
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
                    support: Support::Supported,
                },
            ],
            IdlePolicy::default(),
        ));
        core.go(Route::Quick);

        assert_quick_vertical_flow(
            &quick_scene(&core),
            &[
                "quick-power-sleep",
                "quick-power-idle",
                "quick-capture-screenshot",
            ],
        );
    }

    #[test]
    fn quick_power_status_flows_between_capture_row_and_truth() {
        let mut core = core();
        core.text_scale = 100;
        core.power_status = Some("Power actions are unavailable".into());
        core.go(Route::Quick);

        assert_quick_vertical_flow(
            &quick_scene(&core),
            &[
                "quick-capture-screenshot",
                "quick-power-status",
                "quick-truth",
            ],
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
        assert!(idle.children.iter().all(|child| !child.state.disabled));
        assert_eq!(
            idle.children[0].ink_token.as_deref(),
            Some(STATE_REST_TEXT_TOKEN)
        );
        assert_eq!(
            idle.children[1].ink_token.as_deref(),
            Some(COLOR_TEXT_MUTED_TOKEN)
        );
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
            assert!(!row.children.is_empty());
            for child in &row.children {
                assert_eq!(
                    child.ink_token.as_deref(),
                    Some(STATE_UNAVAILABLE_TEXT_TOKEN)
                );
                assert!(child.state.disabled);
            }
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
