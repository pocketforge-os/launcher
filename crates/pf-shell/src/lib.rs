//! Concrete shell adapters kept outside the pure reducer.

use pf_input_map::{BindingShape, DeviceContract, EffectiveMap, MemoryStore};
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
    match resolver.resolve(&ShellAction::Activate) {
        Ok(GlyphResult::Resolved(binding)) => {
            let glyph = if binding.printed_label.is_empty() {
                binding.source_fallback
            } else {
                binding.printed_label
            };
            format!("{glyph}  Open")
        }
        _ => String::new(),
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
        assert_eq!(footer, "A  Open");
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
}
