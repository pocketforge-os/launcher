//! Concrete shell adapters kept outside the pure reducer.

use pf_input_map::{
    Binding, BindingShape, DeviceContract, EffectiveMap, MapError, MemoryStore, RemapEngine,
    TransactionOutcome,
};
use pf_ports::{
    ActionEvent, ActionPoll, ActionSource, ActionSourceError, Deadline, GlyphResolver, GlyphResult,
    InputSourceId, ShellAction,
};
use pf_scene::AxisMove;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::Path,
};

/// Minimal Linux evdev source. It reads complete native `input_event` records without unsafe code
/// and maps press events through the descriptor's effective semantic map.
pub struct EvdevActionSource {
    file: File,
    by_code: BTreeMap<u16, ShellAction>,
    source: InputSourceId,
    announced: bool,
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
        let effective = EffectiveMap::load(contract, &MemoryStore::default())
            .map_err(|e| AdapterError::Map(format!("{e:?}")))?;
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
        Ok((
            Self {
                file: File::open(path)?,
                by_code,
                source: InputSourceId(effective.device_id().into()),
                announced: false,
            },
            effective,
        ))
    }
}

impl ActionSource for EvdevActionSource {
    fn next_action(&mut self, _deadline: Deadline) -> Result<ActionPoll, ActionSourceError> {
        if !self.announced {
            self.announced = true;
            return Ok(ActionPoll::Event(ActionEvent::ActiveSourceChanged(Some(
                self.source.clone(),
            ))));
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
            if let Some(action) = self.by_code.get(&code) {
                return Ok(ActionPoll::Event(ActionEvent::Action(action.clone())));
            }
        }
        Ok(ActionPoll::DeadlineReached)
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
            format!("{glyph}  Safe Return · button below the d-pad")
        }
        _ => "Select+Start  Safe Return".into(),
    };
    if open.is_empty() {
        safe
    } else {
        format!("{open}     {safe}")
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
pub struct GamepadRemap {
    engine: RemapEngine<MemoryStore>,
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
}

fn semantic_action(name: &str) -> Option<ShellAction> {
    Some(match name {
        "Activate" => ShellAction::Activate,
        "Back" => ShellAction::Back,
        "Move.up" => ShellAction::Move(AxisMove::Up),
        "Move.down" => ShellAction::Move(AxisMove::Down),
        "Move.left" => ShellAction::Move(AxisMove::Left),
        "Move.right" => ShellAction::Move(AxisMove::Right),
        "SafeReturn" | "Quick" => ShellAction::Custom(name.into()),
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

    #[test]
    fn footer_only_advertises_implemented_effective_map_actions() {
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
        let effective = EffectiveMap::load(contract, &MemoryStore::default()).unwrap();
        let footer = footer_prompt(&effective);
        assert_eq!(
            footer,
            "A  Open     PF  Safe Return · button below the d-pad"
        );
        assert!(!footer.contains("Search"));
        assert!(!footer.contains("Quick"));
        assert_eq!(
            effective.resolve(&ShellAction::Custom("Quick".into())),
            Ok(GlyphResult::UnsupportedAction)
        );
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
                "Double-tap PF · the button below the d-pad"
            ]
        );
        assert!(
            labels.iter().all(|label| !label.contains("L1")),
            "absent controls are not offered"
        );
    }
    #[test]
    fn rollback_is_usable_with_gamepad_back_and_preserves_effective_glyph() {
        let contract = DeviceContract::parse_json(CONTRACT).unwrap();
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
