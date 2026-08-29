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
    ChangeAuthority, Deadline, EffectivePreference, LaunchRequest, LaunchResult, MonotonicTime,
    ObservedSessionState, PreferenceChange, PreferenceKey, PreferencePoll, PreferencePort,
    PreferenceValue, SessionEvent, SessionPoll, SessionPort, ShellAction, TerminalReceipt,
};
use pf_scene::{
    AxisMove, Bounds, ImageFit, ImageSource, Node, NodeAction, NodeId, Role, Scene, SurfaceMetrics,
};
use pf_theme::Theme;
use sha2::{Digest, Sha256};
use std::sync::Arc;

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
    BeginRemap,
    ConfirmRemap,
    RollbackRemap,
    CompleteFirstRun,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlsFlow {
    Rows,
    RemapPreview,
    SafeReturnPicker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsRoom {
    Display,
    Controls,
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
    launch_focus: usize,
    active_title: String,
    crash_summary: String,
    crash_receipt_id: String,
    crash_exit_detail: String,
    recovery_available: bool,
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
        let items = snapshot
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
                }
            })
            .collect();
        Self {
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
            launch_focus: 0,
            active_title: String::new(),
            crash_summary: String::new(),
            crash_receipt_id: String::new(),
            crash_exit_detail: String::new(),
            recovery_available: false,
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
        }
    }

    /// Loads Settings exclusively through the runtime preference boundary. A row is interactive
    /// only when the port reports an applied value; stored-only values remain visibly honest.
    pub fn load_preferences(
        &mut self,
        port: &dyn PreferencePort,
        first_run_complete: bool,
    ) -> Result<(), pf_ports::PreferenceError> {
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
        self.safe_return_binding = label.into();
    }
    pub fn set_safe_return_options(&mut self, options: impl IntoIterator<Item = String>) {
        self.safe_return_options = options.into_iter().collect();
    }
    fn first_run_preferences(&self) -> Vec<&DisplayPreference> {
        self.display_preferences
            .iter()
            .filter(|row| row.interactive)
            .collect()
    }
    pub fn reset_first_run(&mut self) {
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
        if matches!(action, ShellAction::Custom(name) if name == "SafeReturn") {
            return Some(Effect::SafeReturn);
        }
        if !self.has_shell_frame() {
            return None;
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
                ControlsFlow::RemapPreview => {
                    return match action {
                        ShellAction::Activate => {
                            self.controls_flow = ControlsFlow::Rows;
                            self.focus = 0;
                            Some(Effect::ConfirmRemap)
                        }
                        ShellAction::Back => {
                            self.controls_flow = ControlsFlow::Rows;
                            self.focus = 0;
                            Some(Effect::RollbackRemap)
                        }
                        _ => None,
                    };
                }
                ControlsFlow::SafeReturnPicker => {
                    return match action {
                        ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                            self.focus = (self.focus + 1)
                                .min(self.safe_return_options.len().saturating_sub(1));
                            None
                        }
                        ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                            self.focus = self.focus.saturating_sub(1);
                            None
                        }
                        ShellAction::Back => {
                            self.controls_flow = ControlsFlow::Rows;
                            self.focus = 1;
                            None
                        }
                        ShellAction::Activate => {
                            let label = self.safe_return_options.get(self.focus)?.clone();
                            self.safe_return_binding.clone_from(&label);
                            self.controls_flow = ControlsFlow::Rows;
                            self.focus = 1;
                            Some(Effect::ChangePreference(PreferenceChange {
                                key: PreferenceKey("safeReturnBinding".into()),
                                value: PreferenceValue::Text(label),
                                authority: ChangeAuthority("user".into()),
                            }))
                        }
                        _ => None,
                    };
                }
                ControlsFlow::Rows => {}
            }
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
            ShellAction::Move(AxisMove::Right) if self.route == Route::Library => {
                self.go(Route::Settings)
            }
            ShellAction::Move(AxisMove::Right) if self.route == Route::Settings => {
                self.settings_room = match self.settings_room {
                    SettingsRoom::Display => SettingsRoom::Controls,
                    SettingsRoom::Controls | SettingsRoom::System => SettingsRoom::System,
                };
                self.focus = 0;
            }
            ShellAction::Move(AxisMove::Left)
                if self.route == Route::Settings && self.settings_room != SettingsRoom::Display =>
            {
                self.settings_room = match self.settings_room {
                    SettingsRoom::System => SettingsRoom::Controls,
                    SettingsRoom::Controls | SettingsRoom::Display => SettingsRoom::Display,
                };
                self.focus = 0;
            }
            ShellAction::Move(AxisMove::Left) if self.route == Route::Settings => {
                self.go(Route::Library)
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
                SettingsRoom::Controls if self.focus == 0 => {
                    self.controls_flow = ControlsFlow::RemapPreview;
                    self.focus = 0;
                    Some(Effect::BeginRemap)
                }
                SettingsRoom::Controls
                    if self.focus == 1 && !self.safe_return_options.is_empty() =>
                {
                    self.controls_flow = ControlsFlow::SafeReturnPicker;
                    self.focus = 0;
                    None
                }
                SettingsRoom::System if self.focus == 1 => Some(Effect::ResetFirstRun),
                _ => None,
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
            self.selected_item = Some(self.focus - 1);
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
        let ready = self.ready_variants(self.focus);
        match ready.len() {
            0 => None,
            1 => self.launch_variant(self.focus, ready[0]),
            _ => {
                self.selected_item = Some(self.focus);
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
            Route::Home => self.items.len().max(1),
            Route::Library => self.items.len() + 1,
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
                    ControlsFlow::Rows | ControlsFlow::RemapPreview => 2,
                    ControlsFlow::SafeReturnPicker => self.safe_return_options.len().max(1),
                },
                SettingsRoom::System => 2 + usize::from(self.recovery_available),
            },
            Route::Quick => 2,
        }
    }

    pub fn launch_result(&mut self, result: &LaunchResult) {
        match result {
            LaunchResult::Accepted { .. } => self.presentation = Presentation::Starting,
            _ => self.presentation = Presentation::Ready,
        }
    }
    pub fn session_event(&mut self, event: &SessionEvent) {
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
                "Wi-Fi   82%   9:41",
                w - 248.0,
                16.0,
                200.0,
                32.0,
                "--color-text-secondary",
            ),
        ];
        match self.presentation {
            Presentation::FirstRun => self.first_run_nodes(&mut children, w),
            Presentation::Crash => self.crash_nodes(&mut children, w, h),
            _ if self.route == Route::Quick => self.quick_nodes(&mut children, w, h),
            _ => self.route_nodes(&mut children, w, h),
        }
        children.push(node(
            "prompts",
            Role::Text,
            footer,
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
            let focused = self.items.get(self.focus);
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
            let columns = if w >= 1100.0 {
                6
            } else if w >= 760.0 {
                4
            } else {
                3
            };
            let card_width = (w - 96.0 - (columns - 1) as f32 * 16.0) / columns as f32;
            let row_height = 250.0;
            let card_top = 244.0;
            let mut visible_rows: usize = 1;
            while card_top + (visible_rows + 1) as f32 * row_height - 18.0 <= h {
                visible_rows += 1;
            }
            let focused_row = self.focus.saturating_sub(1) / columns;
            let first_visible_row = focused_row.saturating_sub(visible_rows.saturating_sub(1));
            for (i, item) in self.items.iter().enumerate() {
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
                    state_token(availability, self.focus == i + 1),
                );
                card.state.focused = self.focus == i + 1;
                card.action = Some(NodeAction::Activate);
                card.children = art_nodes(
                    item,
                    "library-card",
                    48.0 + column as f32 * (card_width + 16.0),
                    card_top + 8.0 + (row as f32 - first_visible_row as f32) * row_height,
                    card_width,
                    self.focus == i + 1,
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
            out.extend(art_nodes(item, "detail-art", 48.0, 165.0, 304.0, false));
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
                    290.0 + variant_index as f32 * 55.0,
                    w - 448.0,
                    48.0,
                    state_token(&variant.availability, false),
                ));
            }
            let ready = self.ready_variants(item_index);
            if self.route == Route::VariantChooser {
                out.push(node(
                    "chooser-note",
                    Role::Text,
                    "Ready right now. Back leaves without opening anything.",
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
                            "{} · Provider {}",
                            variant.id, variant.provenance.provider_id
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
        if self.settings_room == SettingsRoom::Controls {
            match self.controls_flow {
                ControlsFlow::RemapPreview => {
                    let mut preview = node(
                        "remap-preview",
                        Role::Button,
                        "Previewing Activate on the north button · Activate to confirm · Back to roll back",
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
                ControlsFlow::SafeReturnPicker => {
                    for (i, label) in self.safe_return_options.iter().enumerate() {
                        let mut option = node(
                            &format!("safe-return-option-{i}"),
                            Role::Button,
                            label,
                            48.0,
                            180.0 + i as f32 * 72.0,
                            w - 96.0,
                            58.0,
                            if i == self.focus {
                                "--state-focused-ring"
                            } else {
                                "--state-rest-surface"
                            },
                        );
                        option.state.focused = i == self.focus;
                        option.action = Some(NodeAction::Activate);
                        out.push(option);
                    }
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
            SettingsRoom::Controls => vec![
                (
                    "Button remap · preview and gamepad-safe rollback".into(),
                    true,
                ),
                (
                    format!(
                        "Safe Return · {} · choose binding",
                        self.safe_return_binding
                    ),
                    true,
                ),
            ],
            SettingsRoom::System => vec![
                ("Device · PocketForge simulator · Runtime 1".into(), false),
                ("Show accessibility comfort panel again".into(), true),
            ],
        };
        for (i, (label, interactive)) in labels.into_iter().enumerate() {
            let mut n = node(
                &format!("settings-row-{i}"),
                if interactive {
                    Role::Button
                } else {
                    Role::Text
                },
                &label,
                48.0,
                180.0 + i as f32 * 72.0 * f32::from(self.text_scale) / 100.0,
                w - 96.0,
                58.0 * f32::from(self.text_scale) / 100.0,
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
            let options = self.safe_return_options.join(" · ");
            out.push(node(
                "safe-options",
                Role::Text,
                &format!("Options · {options}"),
                48.0,
                350.0,
                w - 96.0,
                100.0,
                "--color-text-secondary",
            ));
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
    use pf_catalog::{
        AppKind, AppManifestRef, Presentation as CP, Provenance, UserProjection, Variant,
    };
    use pf_ports::{FakePreferencePort, PreferenceChangeResult};
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
    fn controls_rows_enter_real_flows_and_picker_uses_only_supplied_options() {
        let mut c = core();
        c.set_safe_return_options(["Select + Start".into(), "Double-tap PF".into()]);
        c.go(Route::Settings);
        c.settings_room = SettingsRoom::Controls;
        assert_eq!(c.action(&ShellAction::Activate), Some(Effect::BeginRemap));
        assert_eq!(c.action(&ShellAction::Back), Some(Effect::RollbackRemap));

        c.action(&ShellAction::Move(AxisMove::Down));
        assert_eq!(c.action(&ShellAction::Activate), None);
        c.action(&ShellAction::Move(AxisMove::Down));
        let Some(Effect::ChangePreference(change)) = c.action(&ShellAction::Activate) else {
            panic!("picker must submit the shown option");
        };
        assert_eq!(change.key, PreferenceKey("safeReturnBinding".into()));
        assert_eq!(change.value, PreferenceValue::Text("Double-tap PF".into()));
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
        assert!(!debug.contains("L1"));
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
        for _ in 0..481 {
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
}
