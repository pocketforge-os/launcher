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
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    os::fd::OwnedFd,
    os::unix::fs::FileTypeExt,
    path::Path,
};

/// Minimal Linux evdev source. It reads complete native `input_event` records without unsafe code
/// and maps press events through the descriptor's effective semantic map.
pub struct EvdevActionSource {
    file: File,
    // evdev releases EVIOCGRAB in Device::drop; the clone keeps the grab alive
    // for exactly as long as this action source.
    _grab: Option<evdev::Device>,
    by_code: BTreeMap<u16, ShellAction>,
    control_by_code: BTreeMap<u16, String>,
    capture_next: bool,
    source: InputSourceId,
    announced: bool,
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
        let file = File::open(path)?;
        let grab = if evdev_grab_enabled(
            std::env::var_os("PF_NO_EVDEV_GRAB").is_some(),
            file.metadata()?.file_type().is_char_device(),
        ) {
            let fd: OwnedFd = file.try_clone()?.into();
            let mut device = evdev::Device::from_fd(fd)?;
            device.grab()?;
            Some(device)
        } else {
            None
        };
        Ok((
            Self {
                file,
                _grab: grab,
                by_code,
                control_by_code,
                capture_next: false,
                source: InputSourceId(effective.device_id().into()),
                announced: false,
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
    /// Polls one physical key transition, including releases needed by repeat schedulers.
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
        if !self.announced {
            self.announced = true;
            return Ok(Some(EvdevInputEvent::ActiveSourceChanged));
        }
        let mut descriptors = [rustix::event::PollFd::new(
            &self.file,
            rustix::event::PollFlags::IN,
        )];
        // Keep the framebuffer loop responsive to session-authority receipts even
        // while the player is not touching an input device.
        let ready = rustix::event::poll(
            &mut descriptors,
            Some(&rustix::event::Timespec {
                tv_sec: 0,
                tv_nsec: 16_000_000,
            }),
        )
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
        if event_type == 1 && value == 1 {
            if self.capture_next {
                self.capture_next = false;
                if let Some(control) = self.control_by_code.get(&code) {
                    return Ok(Some(EvdevInputEvent::Pressed {
                        code,
                        action: Some(ShellAction::Custom(format!("Capture.{control}"))),
                    }));
                }
            }
            return Ok(Some(EvdevInputEvent::Pressed {
                code,
                action: self.by_code.get(&code).cloned(),
            }));
        }
        if event_type == 1 && value == 0 {
            return Ok(Some(EvdevInputEvent::Released { code }));
        }
        Ok(None)
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

fn linux_key_code(name: &str) -> Option<u16> {
    Some(match name {
        "BTN_EAST" => 305,
        "BTN_SOUTH" => 304,
        "BTN_NORTH" => 307,
        "BTN_WEST" => 308,
        "BTN_MODE" => 316,
        "BTN_SELECT" => 314,
        "BTN_START" => 315,
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

    const CONTRACT: &str = include_str!("../fixtures/device.json");

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
}
