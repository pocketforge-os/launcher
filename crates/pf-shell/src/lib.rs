//! Concrete shell adapters kept outside the pure reducer.

use pf_catalog::{
    CatalogRevision, CatalogSnapshot, FavoriteCommitResult, InstalledAppProvider,
    VariantPinCommitResult,
};
use pf_input_map::{
    Binding, BindingShape, DeviceContract, EffectiveMap, MapError, MemoryStore, RemapEngine,
    RemapStore, TransactionOutcome,
};
use pf_ports::{
    ActionEvent, ActionPoll, ActionSource, ActionSourceError, Deadline, GlyphResolver, GlyphResult,
    InputSourceId, ShellAction,
};
use pf_scene::AxisMove;
use pf_shell_core::ControlBinding;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::{self, Read},
    os::fd::OwnedFd,
    os::unix::fs::FileTypeExt,
    path::Path,
    time::Duration,
};

const DEFAULT_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const SYN_DROPPED: u16 = 0x03;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

/// Last reported position of the d-pad hat (`ABS_HAT0X`/`ABS_HAT0Y`, each -1, 0 or +1).
///
/// A hat is one physical d-pad reported as two axes (the a133 descriptor's `id="dpad"
/// kind="hat"`, emitted by `pf-input-decode`). Each axis position is presented to the rest of the
/// shell as the matching d-pad direction control being held: the transition to -1 or +1 presses
/// that direction's control, the return to centre releases it, and a direct -1/+1 flip releases
/// the old direction before pressing the new one. The direction controls are the contract's own
/// `KEY_LEFT`/`KEY_RIGHT`/`KEY_UP`/`KEY_DOWN` controls, so the effective map (including user
/// remaps and capture) chooses the action and the key repeat policy applies unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct HatPosition {
    x: i32,
    y: i32,
}

/// The d-pad direction control code for one hat axis position, or `None` at centre.
fn hat_direction_code(axis: u16, position: i32) -> Option<u16> {
    let name = match (axis, position) {
        (ABS_HAT0X, -1) => "KEY_LEFT",
        (ABS_HAT0X, 1) => "KEY_RIGHT",
        (ABS_HAT0Y, -1) => "KEY_UP",
        (ABS_HAT0Y, 1) => "KEY_DOWN",
        _ => return None,
    };
    linux_key_code(name)
}

/// The real control state of the device, read from the kernel when the event stream cannot be
/// trusted (after `SYN_DROPPED`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlStateSnapshot {
    /// `ABS_HAT0X` position (-1, 0 or +1).
    pub hat_x: i32,
    /// `ABS_HAT0Y` position (-1, 0 or +1).
    pub hat_y: i32,
    /// Key codes currently held down.
    pub keys: BTreeSet<u16>,
}

/// Reads the device's real control state (`EVIOCGKEY` and `EVIOCGABS`) for a resync.
pub trait ControlStateQuery {
    /// Returns the current state, or an error when the kernel cannot be asked.
    ///
    /// # Errors
    /// Returns the ioctl error when the state cannot be read.
    fn query(&self) -> io::Result<ControlStateSnapshot>;
}

impl ControlStateQuery for evdev::Device {
    fn query(&self) -> io::Result<ControlStateSnapshot> {
        let mut snapshot = ControlStateSnapshot {
            keys: self
                .get_key_state()?
                .iter()
                .map(evdev::KeyCode::code)
                .collect(),
            ..ControlStateSnapshot::default()
        };
        for (axis, info) in self.get_absinfo()? {
            match axis.0 {
                ABS_HAT0X => snapshot.hat_x = info.value().signum(),
                ABS_HAT0Y => snapshot.hat_y = info.value().signum(),
                _ => {}
            }
        }
        Ok(snapshot)
    }
}

/// Minimal Linux evdev source. It reads complete native `input_event` records without unsafe code
/// and maps press events through the descriptor's effective semantic map. Key presses (`EV_KEY`)
/// and d-pad hat positions (`EV_ABS` `ABS_HAT0X`/`ABS_HAT0Y`, see `HatPosition`) both become
/// control transitions.
pub struct EvdevActionSource {
    file: File,
    // evdev releases EVIOCGRAB in Device::drop; the clone keeps the grab alive
    // for exactly as long as this action source. It also answers the resync state query.
    state_query: Option<Box<dyn ControlStateQuery>>,
    by_code: BTreeMap<u16, ShellAction>,
    control_by_code: BTreeMap<u16, String>,
    capture_next: bool,
    source: InputSourceId,
    announced: bool,
    hat: HatPosition,
    /// `EV_KEY` codes this source has reported pressed and not yet released.
    held_keys: BTreeSet<u16>,
    /// The kernel dropped events (`SYN_DROPPED`): records are discarded until the next
    /// `SYN_REPORT`, where the source resynchronises from the device's real state.
    resyncing: bool,
    /// Transitions decoded from one record beyond the first (a hat flip yields two).
    queued: VecDeque<EvdevInputEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvdevInputEvent {
    ActiveSourceChanged,
    Pressed {
        code: u16,
        action: Option<ShellAction>,
    },
    Released {
        code: u16,
    },
}

fn evdev_grab_enabled(no_grab: bool, is_character_device: bool) -> bool {
    !no_grab && is_character_device
}

impl EvdevActionSource {
    /// Opens a device and consumes the `pf-input-map` platform contract.
    ///
    /// # Errors
    /// Returns an error if the contract is invalid, its effective map cannot load, or the
    /// evdev node cannot be opened.
    pub fn open(
        path: impl AsRef<Path>,
        contract_json: &str,
    ) -> Result<(Self, EffectiveMap), AdapterError> {
        let contract = DeviceContract::parse_json(contract_json)
            .map_err(|e| AdapterError::Map(format!("{e:?}")))?;
        let effective = EffectiveMap::load(contract.clone(), &MemoryStore::default())
            .map_err(|e| AdapterError::Map(format!("{e:?}")))?;
        Self::open_with_map(path, &contract, effective)
    }

    /// Opens a device using an effective map already loaded by the application.
    ///
    /// # Errors
    /// Returns an error if the evdev node cannot be opened.
    pub fn open_with_map(
        path: impl AsRef<Path>,
        contract: &DeviceContract,
        effective: EffectiveMap,
    ) -> Result<(Self, EffectiveMap), AdapterError> {
        let controls = contract
            .physical_controls
            .iter()
            .filter_map(|control| {
                control
                    .input_code
                    .as_deref()
                    .and_then(linux_key_code)
                    .map(|code| (control.position.clone(), code))
            })
            .collect::<BTreeMap<_, _>>();
        let control_by_code = controls
            .iter()
            .map(|(position, code)| (*code, position.clone()))
            .collect();
        let mut by_code = BTreeMap::new();
        for mapping in effective.mappings() {
            if mapping.binding.shape != BindingShape::SinglePress {
                continue;
            }
            let Some(code) = mapping
                .binding
                .controls
                .first()
                .and_then(|c| controls.get(c))
            else {
                continue;
            };
            if let Some(action) = semantic_action(&mapping.action) {
                by_code.insert(*code, action);
            }
        }
        add_room_shoulder_actions(&mut by_code);
        let file = File::open(path)?;
        let is_character_device = file.metadata()?.file_type().is_char_device();
        let state_query: Option<Box<dyn ControlStateQuery>> = if evdev_grab_enabled(
            std::env::var_os("PF_NO_EVDEV_GRAB").is_some(),
            is_character_device,
        ) {
            let fd: OwnedFd = file.try_clone()?.into();
            let mut device = evdev::Device::from_fd(fd)?;
            device.grab()?;
            Some(Box::new(device))
        } else if is_character_device {
            // Debug escape (no grab): the state query is best effort; resync then falls back to
            // releasing every held control.
            file.try_clone()
                .ok()
                .and_then(|clone| evdev::Device::from_fd(OwnedFd::from(clone)).ok())
                .map(|device| Box::new(device) as Box<dyn ControlStateQuery>)
        } else {
            None
        };
        Ok((
            Self {
                file,
                state_query,
                by_code,
                control_by_code,
                capture_next: false,
                source: InputSourceId(effective.device_id().into()),
                announced: false,
                hat: HatPosition::default(),
                held_keys: BTreeSet::new(),
                resyncing: false,
                queued: VecDeque::new(),
            },
            effective,
        ))
    }
}

impl ActionSource for EvdevActionSource {
    fn next_action(&mut self, deadline: Deadline) -> Result<ActionPoll, ActionSourceError> {
        match self.next_input_event(deadline)? {
            Some(EvdevInputEvent::ActiveSourceChanged) => Ok(ActionPoll::Event(
                ActionEvent::ActiveSourceChanged(Some(self.source.clone())),
            )),
            Some(EvdevInputEvent::Pressed {
                action: Some(action),
                ..
            }) => Ok(ActionPoll::Event(ActionEvent::Action(action))),
            Some(EvdevInputEvent::Pressed { .. } | EvdevInputEvent::Released { .. }) | None => {
                Ok(ActionPoll::DeadlineReached)
            }
        }
    }
}

impl EvdevActionSource {
    /// Polls one physical control transition (a key, or a d-pad hat direction), including releases
    /// needed by repeat schedulers.
    ///
    /// # Errors
    /// Returns [`ActionSourceError::Unavailable`] when the device or its grab is lost, and
    /// [`ActionSourceError::CorruptSequence`] when an incomplete event record is read.
    ///
    /// # Panics
    /// Panics only if the internally allocated native `input_event` record has an invalid size.
    pub fn next_input_event(
        &mut self,
        _deadline: Deadline,
    ) -> Result<Option<EvdevInputEvent>, ActionSourceError> {
        self.next_input_event_timeout(DEFAULT_IDLE_POLL_INTERVAL)
    }

    /// Polls one physical control transition, waiting no longer than `timeout`. A transition
    /// already decoded from an earlier record is returned without waiting.
    ///
    /// # Errors
    /// Returns [`ActionSourceError::Unavailable`] when polling fails or the device is lost,
    /// and [`ActionSourceError::CorruptSequence`] for an incomplete event record.
    ///
    /// # Panics
    /// Panics only if the internally allocated native `input_event` record has an invalid size.
    pub fn next_input_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<EvdevInputEvent>, ActionSourceError> {
        if !self.announced {
            self.announced = true;
            return Ok(Some(EvdevInputEvent::ActiveSourceChanged));
        }
        if let Some(event) = self.queued.pop_front() {
            return Ok(Some(event));
        }
        // Consume records until one yields a transition or none is immediately available, so
        // `None` means the kernel queue is drained (a SYN_REPORT or an unmapped axis sample is
        // not mistaken for "nothing pending").
        let mut timeout = timeout;
        loop {
            let mut descriptors = [rustix::event::PollFd::new(
                &self.file,
                rustix::event::PollFlags::IN,
            )];
            let wait = rustix::event::Timespec::try_from(timeout)
                .map_err(|_| ActionSourceError::Unavailable)?;
            let ready = rustix::event::poll(&mut descriptors, Some(&wait))
                .map_err(|_| ActionSourceError::Unavailable)?;
            if ready == 0 {
                return Ok(None);
            }
            if !descriptors[0]
                .revents()
                .contains(rustix::event::PollFlags::IN)
            {
                return Err(ActionSourceError::Unavailable);
            }
            let word = std::mem::size_of::<libc::c_long>();
            let mut record = vec![0_u8; word * 2 + 8];
            self.file.read_exact(&mut record).map_err(|e| {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    ActionSourceError::Unavailable
                } else {
                    ActionSourceError::CorruptSequence
                }
            })?;
            let offset = word * 2;
            let event_type = u16::from_ne_bytes([record[offset], record[offset + 1]]);
            let code = u16::from_ne_bytes([record[offset + 2], record[offset + 3]]);
            let value = i32::from_ne_bytes(
                record[offset + 4..offset + 8]
                    .try_into()
                    .expect("four bytes"),
            );
            self.decode_record(event_type, code, value);
            if let Some(event) = self.queued.pop_front() {
                return Ok(Some(event));
            }
            timeout = Duration::ZERO;
        }
    }

    /// Queues the control transitions one raw `input_event` implies.
    fn decode_record(&mut self, event_type: u16, code: u16, value: i32) {
        if event_type == EV_SYN && code == SYN_DROPPED {
            // The client buffer overflowed: every event since the last SYN_REPORT, and all up
            // to the next one, is unreliable (a lost hat release would leave its direction
            // held and repeating forever). Discard until SYN_REPORT, then resync.
            self.resyncing = true;
            return;
        }
        if self.resyncing {
            if event_type == EV_SYN && code == SYN_REPORT {
                self.resyncing = false;
                self.resync();
            }
            return;
        }
        if event_type == EV_KEY && value == 1 {
            self.held_keys.insert(code);
            let pressed = self.pressed(code);
            self.queued.push_back(pressed);
        } else if event_type == EV_KEY && value == 0 {
            self.held_keys.remove(&code);
            self.queued.push_back(EvdevInputEvent::Released { code });
        } else if event_type == EV_ABS && matches!(code, ABS_HAT0X | ABS_HAT0Y) {
            self.hat_moved(code, value.signum());
        }
    }

    /// Brings the held controls in line with the device's real state after `SYN_DROPPED`,
    /// libevdev style: releases for what is no longer held, presses for what newly is. When the
    /// state cannot be read, every held control is released (a spurious release is harmless; a
    /// lost one repeats forever).
    fn resync(&mut self) {
        let snapshot = self
            .state_query
            .as_ref()
            .and_then(|query| query.query().ok())
            .unwrap_or_default();
        let released: Vec<u16> = self
            .held_keys
            .iter()
            .copied()
            .filter(|code| !snapshot.keys.contains(code))
            .collect();
        for code in released {
            self.held_keys.remove(&code);
            self.queued.push_back(EvdevInputEvent::Released { code });
        }
        self.hat_moved(ABS_HAT0X, snapshot.hat_x.signum());
        self.hat_moved(ABS_HAT0Y, snapshot.hat_y.signum());
        let pressed: Vec<u16> = snapshot
            .keys
            .iter()
            .copied()
            .filter(|code| {
                self.control_by_code.contains_key(code) && !self.held_keys.contains(code)
            })
            .collect();
        for code in pressed {
            self.held_keys.insert(code);
            let event = self.pressed(code);
            self.queued.push_back(event);
        }
    }

    /// Replaces the resync state query (tests stand in for the kernel's `EVIOCGKEY`/`EVIOCGABS`).
    pub fn set_control_state_query(&mut self, query: Box<dyn ControlStateQuery>) {
        self.state_query = Some(query);
    }

    /// Whether transitions already decoded from a read record are waiting to be returned.
    #[must_use]
    pub fn has_queued_events(&self) -> bool {
        !self.queued.is_empty()
    }

    fn pressed(&mut self, code: u16) -> EvdevInputEvent {
        if self.capture_next {
            self.capture_next = false;
            if let Some(control) = self.control_by_code.get(&code) {
                return EvdevInputEvent::Pressed {
                    code,
                    action: Some(ShellAction::Custom(format!("Capture.{control}"))),
                };
            }
        }
        EvdevInputEvent::Pressed {
            code,
            action: self.by_code.get(&code).cloned(),
        }
    }

    /// Queues the direction release/press implied by one hat axis moving to `position`.
    fn hat_moved(&mut self, axis: u16, position: i32) {
        let held = match axis {
            ABS_HAT0X => &mut self.hat.x,
            _ => &mut self.hat.y,
        };
        let previous = std::mem::replace(held, position);
        if previous == position {
            return;
        }
        if let Some(code) = hat_direction_code(axis, previous) {
            self.queued.push_back(EvdevInputEvent::Released { code });
        }
        if let Some(code) = hat_direction_code(axis, position) {
            let pressed = self.pressed(code);
            self.queued.push_back(pressed);
        }
    }
}

impl EvdevActionSource {
    pub fn capture_next_button(&mut self) {
        self.capture_next = true;
    }

    pub fn apply_effective_map(&mut self, map: &EffectiveMap) {
        self.by_code.clear();
        for mapping in map.mappings() {
            if mapping.binding.shape != BindingShape::SinglePress {
                continue;
            }
            let Some(code) = mapping.binding.controls.first().and_then(|control| {
                self.control_by_code
                    .iter()
                    .find_map(|(code, position)| (position == control).then_some(*code))
            }) else {
                continue;
            };
            if let Some(action) = semantic_action(&mapping.action) {
                self.by_code.insert(code, action);
            }
        }
        add_room_shoulder_actions(&mut self.by_code);
    }
}

#[derive(Debug)]
pub enum AdapterError {
    Io(io::Error),
    Map(String),
}
impl From<io::Error> for AdapterError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn prompt(resolver: &dyn GlyphResolver, action: &ShellAction) -> String {
    match resolver.resolve(action) {
        Ok(GlyphResult::Resolved(binding)) if !binding.printed_label.is_empty() => {
            binding.printed_label
        }
        Ok(GlyphResult::Resolved(binding)) => binding.source_fallback,
        _ => "?".into(),
    }
}

/// Builds the footer from implemented actions that are present in the effective map.
pub fn footer_prompt(resolver: &dyn GlyphResolver) -> String {
    let open = match resolver.resolve(&ShellAction::Activate) {
        Ok(GlyphResult::Resolved(binding)) => {
            let glyph = if binding.printed_label.is_empty() {
                if binding.source_fallback.eq_ignore_ascii_case("guide") {
                    "PF".into()
                } else {
                    binding.source_fallback
                }
            } else {
                binding.printed_label
            };
            format!("{glyph}  Open")
        }
        _ => String::new(),
    };
    let safe = match resolver.resolve(&ShellAction::Custom("SafeReturn".into())) {
        Ok(GlyphResult::Resolved(binding)) => {
            let glyph = if binding.printed_label.is_empty() {
                if binding.source_fallback == "pf-guide" {
                    "PF".into()
                } else {
                    binding.source_fallback
                }
            } else {
                binding.printed_label
            };
            format!("{glyph}  Safe Return")
        }
        _ => "Select+Start  Safe Return".into(),
    };
    if open.is_empty() {
        safe
    } else {
        format!("{open}     {safe}")
    }
}

/// Adds the contextual favorite affordance only when the effective map resolves Quick.
pub fn favorite_footer_prompt(resolver: &dyn GlyphResolver, favorite: bool) -> Option<String> {
    match resolver.resolve(&ShellAction::Custom("Quick".into())) {
        Ok(GlyphResult::Resolved(binding)) => {
            let glyph = if binding.printed_label.is_empty() {
                binding.source_fallback
            } else {
                binding.printed_label
            };
            Some(format!(
                "{glyph}  {}",
                if favorite { "Unfavorite" } else { "Favorite" }
            ))
        }
        _ => None,
    }
}

pub trait FavoriteCatalog {
    /// Returns the latest immutable catalog projection.
    ///
    /// # Errors
    /// Returns a provider diagnostic when the catalog cannot be read.
    fn snapshot(&self) -> Result<CatalogSnapshot, String>;
    /// Commits a favorite value against the expected catalog revision.
    ///
    /// # Errors
    /// Returns a provider diagnostic when the overlay cannot be committed.
    fn set_favorite(
        &self,
        id: &str,
        value: bool,
        expected: CatalogRevision,
    ) -> Result<FavoriteCommitResult, String>;
    /// Commits or clears a per-title default variant against the expected revision.
    ///
    /// # Errors
    /// Returns a provider diagnostic when variant pins are unsupported or cannot be committed.
    fn set_pinned_variant(
        &self,
        item_id: &str,
        variant_id: Option<&str>,
        expected: CatalogRevision,
    ) -> Result<VariantPinCommitResult, String> {
        let _ = (item_id, variant_id, expected);
        Err("Default-version commits are unsupported".into())
    }
}

impl FavoriteCatalog for InstalledAppProvider {
    fn snapshot(&self) -> Result<CatalogSnapshot, String> {
        InstalledAppProvider::snapshot(self).map_err(|error| format!("{error:?}"))
    }
    fn set_favorite(
        &self,
        id: &str,
        value: bool,
        expected: CatalogRevision,
    ) -> Result<FavoriteCommitResult, String> {
        InstalledAppProvider::set_favorite(self, id, value, expected)
            .map_err(|error| format!("{error:?}"))
    }
    fn set_pinned_variant(
        &self,
        item_id: &str,
        variant_id: Option<&str>,
        expected: CatalogRevision,
    ) -> Result<VariantPinCommitResult, String> {
        InstalledAppProvider::set_pinned_variant(self, item_id, variant_id, expected)
            .map_err(|error| format!("{error:?}"))
    }
}

/// Performs the catalog overlay read-modify-commit, retrying one concurrent CAS conflict.
///
/// # Errors
/// Returns an honest status string when either read fails, the item disappears, or both CAS
/// attempts conflict.
pub fn commit_favorite(
    catalog: &dyn FavoriteCatalog,
    id: &str,
    value: bool,
) -> Result<CatalogSnapshot, String> {
    let first = catalog.snapshot()?;
    match catalog.set_favorite(id, value, first.revision)? {
        FavoriteCommitResult::Committed(_) => catalog.snapshot(),
        FavoriteCommitResult::RevisionConflict { .. } => {
            let refreshed = catalog.snapshot()?;
            match catalog.set_favorite(id, value, refreshed.revision)? {
                FavoriteCommitResult::Committed(_) => catalog.snapshot(),
                FavoriteCommitResult::RevisionConflict { .. } => {
                    Err("Favorites changed elsewhere; try again".into())
                }
                FavoriteCommitResult::ItemNotFound => {
                    Err("That title is no longer in the Library".into())
                }
            }
        }
        FavoriteCommitResult::ItemNotFound => Err("That title is no longer in the Library".into()),
    }
}

/// Commits a default variant, retrying one concurrent catalog projection conflict.
///
/// # Errors
/// Returns an honest status string when reads fail, the target disappears, or both CAS attempts
/// conflict.
pub fn commit_pinned_variant(
    catalog: &dyn FavoriteCatalog,
    item_id: &str,
    variant_id: Option<&str>,
) -> Result<CatalogSnapshot, String> {
    let first = catalog.snapshot()?;
    match catalog.set_pinned_variant(item_id, variant_id, first.revision)? {
        VariantPinCommitResult::Committed(_) => catalog.snapshot(),
        VariantPinCommitResult::RevisionConflict { .. } => {
            let refreshed = catalog.snapshot()?;
            match catalog.set_pinned_variant(item_id, variant_id, refreshed.revision)? {
                VariantPinCommitResult::Committed(_) => catalog.snapshot(),
                VariantPinCommitResult::RevisionConflict { .. } => {
                    Err("Default version changed elsewhere; try again".into())
                }
                VariantPinCommitResult::ItemNotFound => {
                    Err("That title is no longer in the Library".into())
                }
                VariantPinCommitResult::VariantNotFound => {
                    Err("That version is no longer available".into())
                }
            }
        }
        VariantPinCommitResult::ItemNotFound => {
            Err("That title is no longer in the Library".into())
        }
        VariantPinCommitResult::VariantNotFound => {
            Err("That version is no longer available".into())
        }
    }
}

/// Evidence-ranked Safe Return choices supported by the current physical controls. The shipped
/// effective binding remains the per-device default and is deliberately not duplicated here.
#[must_use]
pub fn safe_return_options(contract: &DeviceContract) -> Vec<(Binding, String)> {
    let present = |controls: &[&str]| {
        controls.iter().all(|wanted| {
            contract
                .physical_controls
                .iter()
                .any(|c| c.position == *wanted)
        })
    };
    let mut out = Vec::new();
    let mut add = |binding: Binding, label: &str| {
        if binding.controls.iter().all(|c| present(&[c])) {
            out.push((binding, label.into()));
        }
    };
    add(
        Binding {
            shape: BindingShape::Chord,
            controls: vec!["select".into(), "start".into()],
            max_interval_ms: None,
            min_duration_ms: None,
        },
        "Select + Start",
    );
    add(
        Binding {
            shape: BindingShape::Hold,
            controls: vec!["guide".into()],
            max_interval_ms: None,
            min_duration_ms: Some(1000),
        },
        "Hold PF · the button below the d-pad (about 1s)",
    );
    add(
        Binding {
            shape: BindingShape::DoublePress,
            controls: vec!["select".into(), "start".into()],
            max_interval_ms: Some(600),
            min_duration_ms: None,
        },
        "Select + Start, press twice",
    );
    add(
        Binding {
            shape: BindingShape::DoublePress,
            controls: vec!["guide".into()],
            max_interval_ms: Some(600),
            min_duration_ms: None,
        },
        "Double-tap PF · the button below the d-pad",
    );
    add(
        Binding {
            shape: BindingShape::Hold,
            controls: vec!["select".into(), "l1".into(), "r1".into()],
            max_interval_ms: None,
            min_duration_ms: Some(1000),
        },
        "Hold Select + L1 + R1 · deliberately hard to press",
    );
    out
}

/// Thin product-facing transaction wrapper. During preview all gamepad actions remain usable;
/// Back and the focused Revert action both atomically restore the effective map.
pub struct GamepadRemap<S: RemapStore = MemoryStore> {
    engine: RemapEngine<S>,
    previewing: bool,
}
impl GamepadRemap {
    #[must_use]
    pub fn new(map: EffectiveMap) -> Self {
        Self {
            engine: RemapEngine::new(map, MemoryStore::default()),
            previewing: false,
        }
    }

    #[cfg(test)]
    fn with_failing_store(map: EffectiveMap) -> Self {
        Self::with_store(map, MemoryStore::failing())
    }
}
impl<S: RemapStore> GamepadRemap<S> {
    #[must_use]
    pub fn with_store(map: EffectiveMap, store: S) -> Self {
        Self {
            engine: RemapEngine::new(map, store),
            previewing: false,
        }
    }

    /// Starts a validated candidate preview.
    ///
    /// # Errors
    /// Returns the input-map validation error when the candidate is absent, collides, or strands
    /// a protected action.
    pub fn preview(
        &mut self,
        context: &str,
        action: &str,
        binding: Binding,
    ) -> Result<(), MapError> {
        if let Some(conflict) = self.engine.map().mappings().iter().find(|mapping| {
            !(mapping.context == context && mapping.action == action)
                && (mapping.context == context
                    || mapping.context == "global"
                    || context == "global")
                && mapping.binding == binding
        }) {
            return Err(MapError::Collision {
                first: action.into(),
                second: conflict.action.clone(),
            });
        }
        self.engine.begin(context, action, binding)?;
        self.previewing = true;
        Ok(())
    }
    /// Applies a gamepad action while previewing.
    ///
    /// # Errors
    /// Returns an input-map transaction or persistence error.
    pub fn gamepad_action(
        &mut self,
        action: &ShellAction,
    ) -> Result<Option<TransactionOutcome>, MapError> {
        if !self.previewing {
            return Ok(None);
        }
        match action {
            ShellAction::Back => {
                self.previewing = false;
                self.engine.revert().map(Some)
            }
            ShellAction::Activate => {
                self.previewing = false;
                self.engine.confirm().map(Some)
            }
            _ => Ok(None),
        }
    }
    #[must_use]
    pub fn map(&self) -> &EffectiveMap {
        self.engine.map()
    }

    /// Atomically restores the device contract's shipped map.
    ///
    /// # Errors
    /// Returns a persistence error if the shipped map cannot be saved. The effective map is
    /// unchanged when persistence fails.
    pub fn reset_defaults(&mut self) -> Result<(), MapError> {
        self.engine.reset_to_shipped()?;
        Ok(())
    }
}

#[must_use]
pub fn control_bindings(map: &EffectiveMap) -> Vec<ControlBinding> {
    map.mappings()
        .iter()
        .filter(|mapping| semantic_action(&mapping.action).is_some())
        .map(|mapping| ControlBinding {
            context: mapping.context.clone(),
            action: if mapping.context == "library" && mapping.action == "Search.submit" {
                "Filter.next".into()
            } else {
                mapping.action.clone()
            },
            label: mapping.action.strip_prefix("Move.").map_or_else(
                || mapping.action.clone(),
                |direction| format!("Move {direction}"),
            ),
            binding: display_action(&mapping.action).map_or_else(
                || mapping.binding.controls.join(" + "),
                |action| {
                    let resolved = prompt(map, &action);
                    if resolved == "?" {
                        mapping.binding.controls.join(" + ")
                    } else if resolved.eq_ignore_ascii_case("guide") || resolved == "pf-guide" {
                        "PF".into()
                    } else {
                        resolved
                    }
                },
            ),
        })
        .collect()
}

fn display_action(name: &str) -> Option<ShellAction> {
    Some(match name {
        "Activate" => ShellAction::Activate,
        "Back" => ShellAction::Back,
        "Move.up" => ShellAction::Move(AxisMove::Up),
        "Move.down" => ShellAction::Move(AxisMove::Down),
        "Move.left" => ShellAction::Move(AxisMove::Left),
        "Move.right" => ShellAction::Move(AxisMove::Right),
        custom @ ("SafeReturn" | "Quick" | "Search.open" | "Search.submit" | "Start"
        | "Room.next" | "Room.previous") => ShellAction::Custom(custom.into()),
        _ => return None,
    })
}

fn semantic_action(name: &str) -> Option<ShellAction> {
    Some(match name {
        "Activate" => ShellAction::Activate,
        "Back" => ShellAction::Back,
        "Move.up" => ShellAction::Move(AxisMove::Up),
        "Move.down" => ShellAction::Move(AxisMove::Down),
        "Move.left" => ShellAction::Move(AxisMove::Left),
        "Move.right" => ShellAction::Move(AxisMove::Right),
        "SafeReturn" | "Search.open" | "Start" | "Room.next" | "Room.previous" => {
            ShellAction::Custom(name.into())
        }
        "Search.submit" => ShellAction::Custom("Filter.next".into()),
        "Quick" => ShellAction::Custom("Quick".into()),
        _ => return None,
    })
}

fn add_room_shoulder_actions(by_code: &mut BTreeMap<u16, ShellAction>) {
    // Source-owned device descriptors define the digital shoulders as BTN_TL/BTN_TR.
    // They are shell chrome controls, not focus directions or user-remappable app input.
    by_code.insert(
        linux_key_code("BTN_TL").expect("known Linux input code"),
        ShellAction::Custom("Room.previous".into()),
    );
    by_code.insert(
        linux_key_code("BTN_TR").expect("known Linux input code"),
        ShellAction::Custom("Room.next".into()),
    );
}

fn linux_key_code(name: &str) -> Option<u16> {
    Some(match name {
        "BTN_EAST" => 305,
        "BTN_SOUTH" => 304,
        "BTN_NORTH" => 307,
        "BTN_WEST" => 308,
        "BTN_MODE" => 316,
        "BTN_SELECT" => 314,
        "BTN_START" => 315,
        "BTN_TL" => 0x136,
        "BTN_TR" => 0x137,
        "KEY_UP" => 103,
        "KEY_DOWN" => 108,
        "KEY_LEFT" => 105,
        "KEY_RIGHT" => 106,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_ports::MonotonicTime;
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    const CONTRACT: &str = include_str!("../fixtures/device.json");

    fn thread_cpu_ticks() -> u64 {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
        let fields = stat.rsplit_once(") ").unwrap().1.split_whitespace();
        let fields = fields.collect::<Vec<_>>();
        fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
    }

    #[test]
    fn idle_poll_consumes_less_than_five_percent_cpu() {
        let (reader, _writer) = UnixStream::pair().unwrap();
        let file = File::from(OwnedFd::from(reader));
        let mut source = EvdevActionSource {
            file,
            state_query: None,
            by_code: BTreeMap::new(),
            control_by_code: BTreeMap::new(),
            capture_next: false,
            source: InputSourceId("idle-test".into()),
            announced: true,
            hat: HatPosition::default(),
            held_keys: BTreeSet::new(),
            resyncing: false,
            queued: VecDeque::new(),
        };
        let wall_start = std::time::Instant::now();
        let cpu_start = thread_cpu_ticks();

        for _ in 0..4 {
            assert_eq!(
                source
                    .next_input_event_timeout(DEFAULT_IDLE_POLL_INTERVAL)
                    .unwrap(),
                None
            );
        }

        let elapsed = wall_start.elapsed();
        let cpu_ticks = thread_cpu_ticks() - cpu_start;
        assert!(elapsed >= Duration::from_millis(900), "elapsed={elapsed:?}");
        let tick_hz = rustix::param::clock_ticks_per_second();
        let max_cpu_ticks = u64::try_from(elapsed.as_micros())
            .unwrap()
            .saturating_mul(tick_hz)
            .saturating_mul(5)
            / 100
            / 1_000_000;
        assert!(
            cpu_ticks <= max_cpu_ticks,
            "idle consumed {cpu_ticks} CPU ticks over {elapsed:?} (5% limit: {max_cpu_ticks})"
        );
    }

    fn remap_with_bindings(activate: &str, back: &str) -> GamepadRemap {
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let mut persisted = contract.effective_map.clone();
        persisted
            .iter_mut()
            .find(|mapping| mapping.action == "Activate")
            .unwrap()
            .binding = Binding::single(activate);
        persisted
            .iter_mut()
            .find(|mapping| mapping.action == "Back")
            .unwrap()
            .binding = Binding::single(back);
        let map = EffectiveMap::from_persisted(
            contract,
            Some(("pocketforge-sim-gamepad".into(), persisted)),
        )
        .unwrap();
        GamepadRemap::new(map)
    }

    fn assert_shipped_map(map: &EffectiveMap) {
        assert!(map.mappings().iter().all(|mapping| {
            map.shipped_binding(&mapping.context, &mapping.action) == Some(&mapping.binding)
        }));
    }

    fn contract_without_library_filter() -> DeviceContract {
        let mut contract: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
        contract["effective_map"]
            .as_array_mut()
            .unwrap()
            .retain(|mapping| mapping["action"] != "Search.submit");
        DeviceContract::parse_json(&contract.to_string()).unwrap()
    }

    struct ConflictOnce {
        snapshot: CatalogSnapshot,
        calls: std::sync::Mutex<usize>,
    }
    impl FavoriteCatalog for ConflictOnce {
        fn snapshot(&self) -> Result<CatalogSnapshot, String> {
            Ok(self.snapshot.clone())
        }
        fn set_favorite(
            &self,
            _id: &str,
            _value: bool,
            _expected: CatalogRevision,
        ) -> Result<FavoriteCommitResult, String> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            Ok(if *calls == 1 {
                FavoriteCommitResult::RevisionConflict { current: 2 }
            } else {
                FavoriteCommitResult::Committed(3)
            })
        }
    }

    #[test]
    fn footer_only_advertises_implemented_effective_map_actions() {
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let effective = EffectiveMap::load(contract, &MemoryStore::default()).unwrap();
        let footer = footer_prompt(&effective);
        assert_eq!(footer, "A  Open     PF  Safe Return");
        assert!(!footer.contains("Search"));
        assert!(!footer.contains("Quick"));
        assert_eq!(
            favorite_footer_prompt(&effective, false).as_deref(),
            Some("X  Favorite")
        );
        assert_eq!(
            favorite_footer_prompt(&effective, true).as_deref(),
            Some("X  Unfavorite")
        );
    }

    #[test]
    fn physical_quick_binding_translates_to_quick_action() {
        assert_eq!(
            semantic_action("Quick"),
            Some(ShellAction::Custom("Quick".into()))
        );
    }

    #[test]
    fn favorite_commit_retries_one_cas_conflict() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let catalog = ConflictOnce {
            snapshot,
            calls: std::sync::Mutex::new(0),
        };
        commit_favorite(&catalog, "ridgeline", true).unwrap();
        assert_eq!(*catalog.calls.lock().unwrap(), 2);
    }
    #[test]
    fn evdev_effective_map_drives_focus_and_protected_guide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        let mut bytes = vec![0; std::mem::size_of::<libc::c_long>() * 2];
        bytes.extend_from_slice(&1_u16.to_ne_bytes());
        bytes.extend_from_slice(&106_u16.to_ne_bytes());
        bytes.extend_from_slice(&1_i32.to_ne_bytes());
        bytes.extend(vec![0; std::mem::size_of::<libc::c_long>() * 2]);
        bytes.extend_from_slice(&1_u16.to_ne_bytes());
        bytes.extend_from_slice(&316_u16.to_ne_bytes());
        bytes.extend_from_slice(&1_i32.to_ne_bytes());
        File::create(&path).unwrap().write_all(&bytes).unwrap();
        let (mut source, _) = EvdevActionSource::open(path, CONTRACT).unwrap();
        let deadline = Deadline(MonotonicTime::ZERO);
        source.next_action(deadline).unwrap();
        assert_eq!(
            source.next_action(deadline).unwrap(),
            ActionPoll::Event(ActionEvent::Action(ShellAction::Move(AxisMove::Right)))
        );
        assert_eq!(
            source.next_action(deadline).unwrap(),
            ActionPoll::Event(ActionEvent::Action(ShellAction::Custom(
                "SafeReturn".into()
            )))
        );
    }

    #[test]
    fn evdev_start_is_effective_map_gated_and_completes_first_run() {
        let write_start_event = |path: &Path| {
            let mut bytes = vec![0; std::mem::size_of::<libc::c_long>() * 2];
            bytes.extend_from_slice(&1_u16.to_ne_bytes());
            bytes.extend_from_slice(&315_u16.to_ne_bytes());
            bytes.extend_from_slice(&1_i32.to_ne_bytes());
            File::create(path).unwrap().write_all(&bytes).unwrap();
        };
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let map = EffectiveMap::load(contract.clone(), &MemoryStore::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("start-events");
        write_start_event(&path);
        let (mut source, _) = EvdevActionSource::open_with_map(&path, &contract, map).unwrap();
        let deadline = Deadline(MonotonicTime::ZERO);
        source.next_action(deadline).unwrap();
        let ActionPoll::Event(ActionEvent::Action(action)) = source.next_action(deadline).unwrap()
        else {
            panic!("BTN_START must emit an action when Start is mapped")
        };
        assert_eq!(action, ShellAction::Custom("Start".into()));

        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = pf_shell_core::ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        core.reset_first_run();
        assert_eq!(
            core.action(&action),
            Some(pf_shell_core::Effect::CompleteFirstRun)
        );
        assert_eq!(core.presentation(), &pf_shell_core::Presentation::Ready);

        let mut remapped_contract: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
        for mapping in remapped_contract["effective_map"].as_array_mut().unwrap() {
            match mapping["action"].as_str().unwrap() {
                "Start" => mapping["binding"]["controls"] = serde_json::json!(["r1"]),
                "Move.right" => mapping["binding"]["controls"] = serde_json::json!(["start"]),
                _ => {}
            }
        }
        let remapped_contract = DeviceContract::parse_json(&remapped_contract.to_string()).unwrap();
        let remapped_map =
            EffectiveMap::load(remapped_contract.clone(), &MemoryStore::default()).unwrap();
        let path = dir.path().join("remapped-start-events");
        write_start_event(&path);
        let (mut source, _) =
            EvdevActionSource::open_with_map(path, &remapped_contract, remapped_map).unwrap();
        source.next_action(deadline).unwrap();
        assert_eq!(
            source.next_action(deadline).unwrap(),
            ActionPoll::Event(ActionEvent::Action(ShellAction::Move(AxisMove::Right)))
        );
    }

    fn raw_key_action(code: u16) -> Option<ShellAction> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        File::create(&path)
            .unwrap()
            .write_all(&input_event(EV_KEY, code, 1))
            .unwrap();
        let (mut source, _) = EvdevActionSource::open(path, CONTRACT).unwrap();
        let deadline = Deadline(MonotonicTime::ZERO);
        assert_eq!(
            source.next_action(deadline).unwrap(),
            ActionPoll::Event(ActionEvent::ActiveSourceChanged(Some(InputSourceId(
                "pocketforge-sim-gamepad".into()
            ))))
        );
        match source.next_action(deadline).unwrap() {
            ActionPoll::Event(ActionEvent::Action(action)) => Some(action),
            ActionPoll::DeadlineReached => None,
            other => panic!("unexpected raw-key result for {code}: {other:?}"),
        }
    }

    fn assert_room_cycle(
        core: &mut pf_shell_core::ShellCore,
        action: &ShellAction,
        expected: [pf_shell_core::Route; 4],
    ) {
        assert_eq!(core.route(), expected[0]);
        for route in &expected[1..] {
            assert_eq!(core.action(action), None);
            assert_eq!(core.route(), *route);
        }
    }

    #[test]
    fn fresh_boot_shoulders_switch_every_root_and_never_escape_owned_surfaces() {
        use pf_shell_core::Route::{Home, Library, Settings};

        let previous = raw_key_action(0x136).expect("BTN_TL must map to Room.previous");
        let next = raw_key_action(0x137).expect("BTN_TR must map to Room.next");
        let start = raw_key_action(315).expect("BTN_START must remain mapped");
        assert_eq!(previous, ShellAction::Custom("Room.previous".into()));
        assert_eq!(next, ShellAction::Custom("Room.next".into()));
        assert_eq!(start, ShellAction::Custom("Start".into()));
        assert_eq!(
            raw_key_action(0x13f),
            None,
            "an unowned evdev key is the negative control"
        );

        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut fresh = pf_shell_core::ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        fresh.authority_snapshot(false);
        fresh.reset_first_run();
        assert_eq!(fresh.presentation(), &pf_shell_core::Presentation::FirstRun);
        assert_eq!(fresh.route(), pf_shell_core::Route::Home);
        assert_eq!(fresh.action(&next), None);
        assert_eq!(fresh.action(&previous), None);
        assert_eq!(fresh.presentation(), &pf_shell_core::Presentation::FirstRun);
        assert_eq!(fresh.route(), pf_shell_core::Route::Home);
        assert_eq!(
            fresh.action(&start),
            Some(pf_shell_core::Effect::CompleteFirstRun)
        );
        assert_eq!(fresh.presentation(), &pf_shell_core::Presentation::Ready);

        for expected in [
            [Home, Library, Settings, Home],
            [Library, Settings, Home, Library],
            [Settings, Home, Library, Settings],
        ] {
            while fresh.route() != expected[0] {
                fresh.action(&next);
            }
            assert_room_cycle(&mut fresh, &next, expected);
        }
        for expected in [
            [Home, Settings, Library, Home],
            [Library, Home, Settings, Library],
            [Settings, Library, Home, Settings],
        ] {
            while fresh.route() != expected[0] {
                fresh.action(&next);
            }
            assert_room_cycle(&mut fresh, &previous, expected);
        }

        while fresh.route() != Home {
            fresh.action(&next);
        }
        fresh.action(&ShellAction::Move(AxisMove::Right));
        let home_focus = fresh.focus();
        fresh.action(&next);
        fresh.action(&ShellAction::Move(AxisMove::Down));
        let library_focus = fresh.focus();
        fresh.action(&next);
        fresh.action(&ShellAction::Move(AxisMove::Down));
        let settings_focus = fresh.focus();
        fresh.action(&next);
        assert_eq!(fresh.focus(), home_focus, "Home restores route-local focus");
        fresh.action(&next);
        assert_eq!(
            fresh.focus(),
            library_focus,
            "Library restores route-local focus"
        );
        fresh.action(&next);
        assert_eq!(
            fresh.focus(),
            settings_focus,
            "Settings restores route-local focus"
        );

        while fresh.route() != Library {
            fresh.action(&next);
        }
        fresh.action(&ShellAction::Activate);
        assert_eq!(fresh.route(), pf_shell_core::Route::Details);
        assert_eq!(fresh.action(&next), None);
        assert_eq!(fresh.action(&previous), None);
        assert_eq!(fresh.route(), pf_shell_core::Route::Details);
    }

    #[test]
    fn evdev_grab_policy_is_exclusive_by_default_with_debug_escape() {
        assert!(evdev_grab_enabled(false, true));
        assert!(!evdev_grab_enabled(true, true));
        assert!(!evdev_grab_enabled(false, false));
    }
    #[test]
    fn safe_return_choices_are_ranked_and_device_filtered() {
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let labels: Vec<_> = safe_return_options(&contract)
            .into_iter()
            .map(|(_, label)| label)
            .collect();
        assert_eq!(
            labels,
            [
                "Select + Start",
                "Hold PF · the button below the d-pad (about 1s)",
                "Select + Start, press twice",
                "Double-tap PF · the button below the d-pad",
                "Hold Select + L1 + R1 · deliberately hard to press"
            ]
        );
    }
    #[test]
    fn rollback_is_usable_with_gamepad_back_and_preserves_effective_glyph() {
        let contract = contract_without_library_filter();
        let map = EffectiveMap::load(contract, &MemoryStore::default()).unwrap();
        let before = prompt(&map, &ShellAction::Activate);
        let mut remap = GamepadRemap::new(map);
        remap
            .preview("global", "Activate", Binding::single("north"))
            .unwrap();
        assert_eq!(
            remap.gamepad_action(&ShellAction::Back).unwrap(),
            Some(TransactionOutcome::RolledBack(
                pf_input_map::RollbackReason::Reverted
            ))
        );
        assert_eq!(prompt(remap.map(), &ShellAction::Activate), before);
    }
    #[test]
    fn remap_confirm_updates_the_effective_binding_and_reset_restores_default() {
        let contract = contract_without_library_filter();
        let map = EffectiveMap::load(contract, &MemoryStore::default()).unwrap();
        let mut remap = GamepadRemap::new(map);
        remap
            .preview("global", "Activate", Binding::single("north"))
            .unwrap();
        assert_eq!(
            remap.gamepad_action(&ShellAction::Activate).unwrap(),
            Some(TransactionOutcome::Committed)
        );
        assert_eq!(prompt(remap.map(), &ShellAction::Activate), "Y");
        remap.reset_defaults().unwrap();
        assert_eq!(prompt(remap.map(), &ShellAction::Activate), "A");
    }

    #[test]
    fn reset_restores_shipped_map_after_a_chained_move() {
        let mut remap = remap_with_bindings("north", "east");

        remap.reset_defaults().unwrap();

        assert_eq!(prompt(remap.map(), &ShellAction::Activate), "A");
        assert_eq!(prompt(remap.map(), &ShellAction::Back), "B");
        assert_shipped_map(remap.map());
    }

    #[test]
    fn reset_restores_shipped_map_after_a_pure_swap() {
        let mut remap = remap_with_bindings("south", "east");

        remap.reset_defaults().unwrap();

        assert_eq!(prompt(remap.map(), &ShellAction::Activate), "A");
        assert_eq!(prompt(remap.map(), &ShellAction::Back), "B");
        assert_shipped_map(remap.map());
    }

    #[test]
    fn reset_persistence_failure_is_surfaced_without_partial_change() {
        let remap = remap_with_bindings("north", "east");
        let map = remap.map().clone();
        let unchanged = map.mappings().to_vec();
        let mut remap = GamepadRemap::with_failing_store(map);

        assert!(matches!(
            remap.reset_defaults(),
            Err(MapError::Persistence(_))
        ));
        assert_eq!(remap.map().mappings(), unchanged);
    }

    #[test]
    fn control_rows_are_projected_from_the_effective_map() {
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let map = EffectiveMap::load(contract, &MemoryStore::default()).unwrap();
        let rows = control_bindings(&map);
        assert_eq!(rows.len(), map.mappings().len());
        assert!(
            rows.iter()
                .any(|row| row.label == "Activate" && row.binding == "A")
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "Move up" && row.binding == "↑")
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "Quick" && row.binding == "X")
        );
        assert!(
            rows.iter()
                .any(|row| row.action == "SafeReturn" && row.binding == "PF")
        );
    }
    #[test]
    fn stranding_collision_is_refused_before_preview() {
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let map = EffectiveMap::load(contract, &MemoryStore::default()).unwrap();
        let mut remap = GamepadRemap::new(map);
        assert!(matches!(
            remap.preview("global", "Activate", Binding::single("south")),
            Err(MapError::Collision { .. })
        ));
    }

    /// One native `input_event` record (zero timestamp) as the kernel and `pf-input-decode`
    /// write it: `timeval` (two words), then `type`, `code`, `value`.
    fn input_event(event_type: u16, code: u16, value: i32) -> Vec<u8> {
        let word = std::mem::size_of::<libc::c_long>();
        let mut record = vec![0_u8; word * 2];
        record.extend_from_slice(&event_type.to_ne_bytes());
        record.extend_from_slice(&code.to_ne_bytes());
        record.extend_from_slice(&value.to_ne_bytes());
        record
    }

    /// Each `(type, code, value)` followed by `SYN_REPORT`, like the decoder's frames.
    fn evdev_stream(events: &[(u16, u16, i32)]) -> Vec<u8> {
        events
            .iter()
            .flat_map(|&(event_type, code, value)| {
                let mut frame = input_event(event_type, code, value);
                frame.extend(input_event(0, 0, 0));
                frame
            })
            .collect()
    }

    /// Opens the shipped fixture contract on a file holding `events` and returns every decoded
    /// transition until end of stream.
    fn decode_with(
        events: &[(u16, u16, i32)],
        prepare: impl FnOnce(&mut EvdevActionSource),
    ) -> Vec<EvdevInputEvent> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pf-gamepad");
        std::fs::write(&path, evdev_stream(events)).unwrap();
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let map = EffectiveMap::load(contract.clone(), &MemoryStore::default()).unwrap();
        let (mut source, _) = EvdevActionSource::open_with_map(&path, &contract, map).unwrap();
        prepare(&mut source);
        let mut decoded = Vec::new();
        while let Ok(event) = source.next_input_event_timeout(Duration::ZERO) {
            decoded.extend(event);
        }
        assert!(!source.has_queued_events());
        decoded
    }

    fn decode(events: &[(u16, u16, i32)]) -> Vec<EvdevInputEvent> {
        decode_with(events, |_| {})
    }

    fn moved(code: u16, direction: AxisMove) -> EvdevInputEvent {
        EvdevInputEvent::Pressed {
            code,
            action: Some(ShellAction::Move(direction)),
        }
    }

    const KEY_UP: u16 = 103;
    const KEY_LEFT: u16 = 105;
    const KEY_RIGHT: u16 = 106;
    const KEY_DOWN: u16 = 108;

    #[test]
    fn dpad_hat_edges_press_and_centre_releases_the_fixture_direction_controls() {
        // pf-input-decode reports the a133 d-pad as ABS_HAT0X/ABS_HAT0Y in -1..=1 (descriptor
        // `id="dpad" kind="hat"`); d26dfa11 dropped every one of these records.
        let decoded = decode(&[
            (EV_ABS, ABS_HAT0X, 1),
            (EV_ABS, ABS_HAT0X, 0),
            (EV_ABS, ABS_HAT0X, -1),
            (EV_ABS, ABS_HAT0X, 0),
            (EV_ABS, ABS_HAT0Y, 1),
            (EV_ABS, ABS_HAT0Y, 0),
            (EV_ABS, ABS_HAT0Y, -1),
            (EV_ABS, ABS_HAT0Y, 0),
        ]);
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_RIGHT, AxisMove::Right),
                EvdevInputEvent::Released { code: KEY_RIGHT },
                moved(KEY_LEFT, AxisMove::Left),
                EvdevInputEvent::Released { code: KEY_LEFT },
                moved(KEY_DOWN, AxisMove::Down),
                EvdevInputEvent::Released { code: KEY_DOWN },
                moved(KEY_UP, AxisMove::Up),
                EvdevInputEvent::Released { code: KEY_UP },
            ]
        );
    }

    #[test]
    fn dpad_hat_flip_releases_the_old_direction_before_pressing_the_new_one() {
        let decoded = decode(&[
            (EV_ABS, ABS_HAT0X, -1),
            (EV_ABS, ABS_HAT0X, 1),
            (EV_ABS, ABS_HAT0X, 0),
        ]);
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_LEFT, AxisMove::Left),
                EvdevInputEvent::Released { code: KEY_LEFT },
                moved(KEY_RIGHT, AxisMove::Right),
                EvdevInputEvent::Released { code: KEY_RIGHT },
            ]
        );
    }

    #[test]
    fn dpad_hat_axes_are_independent_and_an_unchanged_position_is_not_a_new_press() {
        let decoded = decode(&[
            (EV_ABS, ABS_HAT0X, 1),
            (EV_ABS, ABS_HAT0Y, -1),
            (EV_ABS, ABS_HAT0X, 1),
            (EV_ABS, ABS_HAT0Y, 0),
            (EV_ABS, ABS_HAT0X, 0),
            (EV_ABS, ABS_HAT0X, 0),
        ]);
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_RIGHT, AxisMove::Right),
                moved(KEY_UP, AxisMove::Up),
                EvdevInputEvent::Released { code: KEY_UP },
                EvdevInputEvent::Released { code: KEY_RIGHT },
            ]
        );
    }

    #[test]
    fn dpad_key_path_and_other_axes_are_unchanged() {
        // Devices that report the d-pad as KEY_* keep working; sticks and triggers stay ignored.
        let decoded = decode(&[
            (EV_KEY, KEY_DOWN, 1),
            (EV_KEY, KEY_DOWN, 0),
            (EV_ABS, 0x00, 4095),
            (EV_ABS, 0x02, 255),
            (EV_KEY, 305, 1),
            (EV_KEY, 305, 0),
        ]);
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_DOWN, AxisMove::Down),
                EvdevInputEvent::Released { code: KEY_DOWN },
                EvdevInputEvent::Pressed {
                    code: 305,
                    action: Some(ShellAction::Activate),
                },
                EvdevInputEvent::Released { code: 305 },
            ]
        );
    }

    #[test]
    fn dpad_hat_actions_follow_the_effective_map_and_capture() {
        // The hat only names the direction CONTROL; the effective map picks its action, so a
        // remap applies to the hat exactly as to a KEY_* d-pad, and capture names the control.
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let mut persisted = contract.effective_map.clone();
        persisted
            .iter_mut()
            .find(|mapping| mapping.action == "Move.right")
            .unwrap()
            .binding = Binding::single("l1");
        persisted
            .iter_mut()
            .find(|mapping| mapping.action == "Move.left")
            .unwrap()
            .binding = Binding::single("r1");
        let remapped = EffectiveMap::from_persisted(
            contract,
            Some(("pocketforge-sim-gamepad".into(), persisted)),
        )
        .unwrap();
        let decoded = decode_with(
            &[(EV_ABS, ABS_HAT0X, -1), (EV_ABS, ABS_HAT0X, 0)],
            |source| {
                source.apply_effective_map(&remapped);
            },
        );
        assert_eq!(decoded[1], moved(KEY_LEFT, AxisMove::Right));

        let captured = decode_with(
            &[(EV_ABS, ABS_HAT0Y, 1), (EV_ABS, ABS_HAT0Y, 0)],
            |source| {
                source.capture_next_button();
            },
        );
        assert_eq!(
            captured[1],
            EvdevInputEvent::Pressed {
                code: KEY_DOWN,
                action: Some(ShellAction::Custom("Capture.r2".into())),
            }
        );
    }

    // --- SYN_DROPPED (tsp-f3fm.227) ---------------------------------------------------------
    //
    // The kernel reports a client buffer overflow with SYN_DROPPED; every event up to the next
    // SYN_REPORT is unreliable and the lost ones may include a hat release. ca22de0e ignored the
    // marker, so the direction stayed held and repeated forever.

    const SYN: (u16, u16, i32) = (0, 0, 0);
    const DROPPED: (u16, u16, i32) = (0, 3, 0);

    /// Decodes raw records exactly as given (no implicit `SYN_REPORT` framing).
    fn decode_raw_with(
        records: &[(u16, u16, i32)],
        prepare: impl FnOnce(&mut EvdevActionSource),
    ) -> Vec<EvdevInputEvent> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pf-gamepad");
        let bytes: Vec<u8> = records
            .iter()
            .flat_map(|&(event_type, code, value)| input_event(event_type, code, value))
            .collect();
        std::fs::write(&path, bytes).unwrap();
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let map = EffectiveMap::load(contract.clone(), &MemoryStore::default()).unwrap();
        let (mut source, _) = EvdevActionSource::open_with_map(&path, &contract, map).unwrap();
        prepare(&mut source);
        let mut decoded = Vec::new();
        while let Ok(event) = source.next_input_event_timeout(Duration::ZERO) {
            decoded.extend(event);
        }
        decoded
    }

    struct FixedState(io::Result<ControlStateSnapshot>);
    impl ControlStateQuery for FixedState {
        fn query(&self) -> io::Result<ControlStateSnapshot> {
            match &self.0 {
                Ok(snapshot) => Ok(snapshot.clone()),
                Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
            }
        }
    }

    fn with_state(state: io::Result<ControlStateSnapshot>) -> impl FnOnce(&mut EvdevActionSource) {
        move |source| source.set_control_state_query(Box::new(FixedState(state)))
    }

    #[test]
    fn syn_dropped_without_a_state_query_releases_every_held_control() {
        // Hat DOWN and A held; their releases were lost in the overflow.
        let decoded = decode_raw_with(
            &[
                (EV_ABS, ABS_HAT0Y, 1),
                SYN,
                (EV_KEY, 305, 1),
                SYN,
                (EV_ABS, 0x00, 2051),
                DROPPED,
                (EV_ABS, 0x00, 2049),
                SYN,
            ],
            |_| {},
        );
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_DOWN, AxisMove::Down),
                EvdevInputEvent::Pressed {
                    code: 305,
                    action: Some(ShellAction::Activate),
                },
                EvdevInputEvent::Released { code: 305 },
                EvdevInputEvent::Released { code: KEY_DOWN },
            ]
        );
    }

    #[test]
    fn syn_dropped_discards_records_until_the_next_syn_report() {
        let decoded = decode_raw_with(
            &[
                DROPPED,
                (EV_ABS, ABS_HAT0X, 1),
                (EV_KEY, 305, 1),
                SYN,
                (EV_ABS, ABS_HAT0Y, -1),
                SYN,
            ],
            |_| {},
        );
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_UP, AxisMove::Up),
            ]
        );
    }

    #[test]
    fn syn_dropped_resyncs_held_controls_from_the_real_device_state() {
        let held_down_then_dropped = [(EV_ABS, ABS_HAT0Y, 1), SYN, DROPPED, SYN];

        // The kernel says the hat moved to UP and A went down during the overflow.
        let decoded = decode_raw_with(
            &held_down_then_dropped,
            with_state(Ok(ControlStateSnapshot {
                hat_x: 0,
                hat_y: -1,
                keys: BTreeSet::from([305]),
            })),
        );
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_DOWN, AxisMove::Down),
                EvdevInputEvent::Released { code: KEY_DOWN },
                moved(KEY_UP, AxisMove::Up),
                EvdevInputEvent::Pressed {
                    code: 305,
                    action: Some(ShellAction::Activate),
                },
            ]
        );

        // Still held for real: no spurious release, no second press.
        let decoded = decode_raw_with(
            &held_down_then_dropped,
            with_state(Ok(ControlStateSnapshot {
                hat_x: 0,
                hat_y: 1,
                keys: BTreeSet::new(),
            })),
        );
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_DOWN, AxisMove::Down),
            ]
        );

        // The query fails: fall back to releasing what was held.
        let decoded = decode_raw_with(
            &held_down_then_dropped,
            with_state(Err(io::Error::other("EVIOCGABS failed"))),
        );
        assert_eq!(
            decoded,
            vec![
                EvdevInputEvent::ActiveSourceChanged,
                moved(KEY_DOWN, AxisMove::Down),
                EvdevInputEvent::Released { code: KEY_DOWN },
            ]
        );
    }
}
