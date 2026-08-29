use pf_catalog::CatalogSnapshot;
use pf_framehost::{FbdevHost, OffscreenHost};
use pf_input_map::{DeviceContract, EffectiveMap, MemoryStore};
use pf_ports::{
    ActionEvent, ActionPoll, ActionSource, ChangeAuthority, Deadline, EffectivePreference,
    FakePreferencePort, FrameHost, LaunchResult, MonotonicTime, ObservedSessionState,
    PreferenceChange, PreferenceChangeResult, PreferenceError, PreferenceKey, PreferencePoll,
    PreferencePort, PreferenceValue, SessionError, SessionEvent, SessionPoll, SessionPort,
    ShellAction, TerminalReceipt,
};
use pf_prefs::PrefsStore;
use pf_prefs_port::PrefsPreferencePort;
use pf_scene::{Insets, Orientation, SurfaceMetrics};
use pf_shell::{EvdevActionSource, GamepadRemap, footer_prompt, safe_return_options};
use pf_shell_core::{Effect, ShellCore};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{
    collections::VecDeque,
    env, fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let snapshot: CatalogSnapshot = serde_json::from_str(include_str!("../fixtures/catalog.json"))
        .map_err(|e| e.to_string())?;
    let theme = pf_theme::flagship();
    let reduced = env::var_os("PF_REDUCE_MOTION").is_some();
    let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json"))
        .map_err(|e| format!("{e:?}"))?;
    let options = safe_return_options(&contract);
    let glyphs =
        EffectiveMap::load(contract, &MemoryStore::default()).map_err(|e| format!("{e:?}"))?;
    let footer = footer_prompt(&glyphs);
    let mut core = ShellCore::boot(&snapshot, &theme, reduced);
    core.authority_snapshot(false);
    core.set_safe_return_options(options.iter().map(|(_, label)| label.clone()));
    let fixture_mode = args
        .iter()
        .any(|a| matches!(a.as_str(), "--sim-frame" | "--settings-evidence"))
        || !args.iter().any(|a| a == "--fbdev");
    let state_dir = PathBuf::from(value(&args, "--state-dir").unwrap_or("./state"));
    let mut durable;
    let mut fixture;
    let preferences: &mut dyn PreferencePort;
    let first_run_complete;
    if fixture_mode {
        fixture = fixture_preferences();
        preferences = &mut fixture;
        first_run_complete = !args.iter().any(|a| a == "--first-run");
    } else {
        durable = DurablePreferences::open(&state_dir)?;
        first_run_complete = durable.first_run_complete()?;
        if let Some(label) = durable.safe_return_binding()? {
            core.set_safe_return_binding(label);
        }
        preferences = &mut durable;
    }
    core.load_preferences(preferences, first_run_complete)
        .map_err(|e| format!("preferences: {e:?}"))?;
    if args.iter().any(|a| a == "--sim-frame") {
        let path = value(&args, "--device").map_or_else(
            || env::var("PF_FB0").unwrap_or_else(|_| "/dev/fb0".into()),
            str::to_owned,
        );
        return emit_sim_frame(&core, &footer, Path::new(&path));
    }
    if args.iter().any(|a| a == "--fbdev") {
        let framebuffer = value(&args, "--device").unwrap_or("/dev/fb0");
        let input = value(&args, "--input").unwrap_or("/dev/input/event0");
        let mut host = FbdevHost::open(framebuffer).map_err(|e| e.to_string())?;
        let (mut actions, _) =
            EvdevActionSource::open(input, include_str!("../fixtures/device.json"))
                .map_err(|e| format!("input adapter: {e:?}"))?;
        host.present(
            core.scene(host.metrics(), &footer)
                .as_ref()
                .ok_or("shell has no frame")?,
        )
        .map_err(|e| e.to_string())?;
        return run_fbdev(
            &mut host,
            &mut actions,
            &mut core,
            &footer,
            preferences,
            glyphs,
        );
    }
    let out = Path::new(value(&args, "--out").unwrap_or("evidence/offscreen"));
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let metrics = SurfaceMetrics {
        logical_width: 1280.0,
        logical_height: 720.0,
        scale: 1.0,
        safe_insets: Insets::default(),
        orientation: Orientation::Landscape,
    };
    let mut host = OffscreenHost::new(metrics);
    if args.iter().any(|a| a == "--settings-evidence") {
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        emit(&mut host, &core, &footer, out, "settings")?;
        core.reset_first_run();
        emit(&mut host, &core, &footer, out, "first-run")?;
        return Ok(());
    }
    emit(&mut host, &core, &footer, out, "boot-home")?;
    core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    emit(&mut host, &core, &footer, out, "focus-moved")?;
    let effect = core
        .action(&ShellAction::Activate)
        .ok_or("fixture must launch")?;
    emit(&mut host, &core, &footer, out, "launch-dimmed")?;
    let mut session = pf_ports::FakeSession::new(
        Ok(LaunchResult::Accepted {
            session_id: "fake-session".into(),
        }),
        [
            pf_ports::ScriptedSession::Event(SessionEvent::Observed(ObservedSessionState::Running)),
            pf_ports::ScriptedSession::Event(SessionEvent::Observed(
                ObservedSessionState::ObservationComplete,
            )),
            pf_ports::ScriptedSession::Event(SessionEvent::Terminal(TerminalReceipt::Returned {
                session_id: "fake-session".into(),
            })),
            pf_ports::ScriptedSession::Idle,
        ],
    );
    let Effect::Launch(request) = effect else {
        return Err("unexpected safe return".into());
    };
    core.launch_result(&session.launch(request).map_err(|e| format!("{e:?}"))?);
    core.drive_session(&mut session)
        .map_err(|e| format!("{e:?}"))?;
    emit(&mut host, &core, &footer, out, "returned")?;
    emit_f10_evidence(&mut host, &snapshot, &theme, &footer, out)?;
    Ok(())
}

fn emit_f10_evidence(
    host: &mut OffscreenHost,
    snapshot: &CatalogSnapshot,
    theme: &pf_theme::Theme,
    footer: &str,
    out: &Path,
) -> Result<(), String> {
    let mut core = ShellCore::boot(snapshot, theme, false);
    core.authority_snapshot(false);
    core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
    emit(host, &core, footer, out, "library")?;
    core.action(&ShellAction::Custom("Search".into()));
    core.set_search_query("hollow");
    emit(host, &core, footer, out, "search")?;
    core.action(&ShellAction::Activate);
    emit(host, &core, footer, out, "details")?;

    let mut chooser_snapshot = snapshot.clone();
    let item = chooser_snapshot
        .items
        .iter_mut()
        .find(|item| item.id == "glass-harbor")
        .ok_or("chooser fixture item missing")?;
    let mut second = item
        .variants
        .iter()
        .find(|variant| matches!(variant.availability, pf_catalog::Availability::Ready))
        .cloned()
        .ok_or("chooser fixture ready variant missing")?;
    second.id = "handheld".into();
    second.provider_id = "fixture-c".into();
    second.provenance.provider_id = "fixture-c".into();
    second.launch_target.app_id = "glass-harbor-handheld".into();
    item.variants.push(second);
    let mut chooser = ShellCore::boot(&chooser_snapshot, theme, false);
    chooser.authority_snapshot(false);
    for _ in 0..3 {
        chooser.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    }
    chooser.action(&ShellAction::Activate);
    emit(host, &chooser, footer, out, "variant-chooser")?;
    Ok(())
}

fn emit_sim_frame(core: &ShellCore, prompt: &str, path: &Path) -> Result<(), String> {
    let width = env_dimension("PF_FB_WIDTH", 1280)?;
    let height = env_dimension("PF_FB_HEIGHT", 720)?;
    let stride = env_dimension("PF_FB_STRIDE", width * 4)?;
    if stride < width * 4 {
        return Err("PF_FB_STRIDE is smaller than one XRGB8888 row".into());
    }
    let logical_width = u16::try_from(width)
        .map(f32::from)
        .map_err(|_| "PF_FB_WIDTH exceeds the supported logical surface".to_owned())?;
    let logical_height = u16::try_from(height)
        .map(f32::from)
        .map_err(|_| "PF_FB_HEIGHT exceeds the supported logical surface".to_owned())?;
    let metrics = SurfaceMetrics {
        logical_width,
        logical_height,
        scale: 1.0,
        safe_insets: Insets::default(),
        orientation: if width >= height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        },
    };
    let mut host = OffscreenHost::new(metrics);
    host.present(
        core.scene(metrics, prompt)
            .as_ref()
            .ok_or("shell has no frame")?,
    )
    .map_err(|e| e.to_string())?;
    let frame = host.frame().ok_or("sim frame missing")?;
    let mut out = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let padding = vec![0_u8; (stride - width * 4) as usize];
    for row in frame.rgba.chunks_exact(width as usize * 4) {
        for pixel in row.chunks_exact(4) {
            out.write_all(&[pixel[2], pixel[1], pixel[0], 0xff])
                .map_err(|e| e.to_string())?;
        }
        out.write_all(&padding).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())
}

fn env_dimension(name: &str, default: u32) -> Result<u32, String> {
    env::var(name).map_or(Ok(default), |value| {
        value.parse::<u32>().map_err(|e| format!("{name}: {e}"))
    })
}

fn run_fbdev(
    host: &mut FbdevHost,
    actions: &mut dyn ActionSource,
    core: &mut ShellCore,
    activate: &str,
    preferences: &mut dyn PreferencePort,
    map: EffectiveMap,
) -> Result<(), String> {
    let deadline = Deadline(MonotonicTime::ZERO);
    let mut session = InteractiveSession::default();
    let mut remap = GamepadRemap::new(map);
    loop {
        let poll = actions
            .next_action(deadline)
            .map_err(|e| format!("input: {e:?}"))?;
        let ActionPoll::Event(ActionEvent::Action(action)) = poll else {
            if matches!(poll, ActionPoll::Closed) {
                return Ok(());
            }
            continue;
        };
        let before = (core.presentation().clone(), core.focus());
        match core.action(&action) {
            Some(Effect::SafeReturn) => {
                session.safe_return();
                core.drive_session(&mut session)
                    .map_err(|e| format!("{e:?}"))?;
            }
            Some(Effect::Launch(request)) => {
                let result = session.launch(request).map_err(|e| format!("{e:?}"))?;
                core.launch_result(&result);
                host.present(
                    core.scene(host.metrics(), activate)
                        .as_ref()
                        .ok_or("shell has no frame")?,
                )
                .map_err(|e| e.to_string())?;
                core.drive_session(&mut session)
                    .map_err(|e| format!("{e:?}"))?;
            }
            Some(Effect::EnterRecovery) => return Ok(()),
            Some(Effect::ChangePreference(change)) => {
                preferences
                    .submit_change(change)
                    .map_err(|e| format!("preferences: {e:?}"))?;
                core.drive_preferences(preferences)
                    .map_err(|e| format!("preferences: {e:?}"))?;
            }
            Some(Effect::ResetFirstRun) => core.reset_first_run(),
            Some(Effect::BeginRemap) => {
                remap
                    .preview("global", "Activate", pf_input_map::Binding::single("north"))
                    .map_err(|e| format!("remap preview: {e:?}"))?;
            }
            Some(Effect::ConfirmRemap) => {
                remap
                    .gamepad_action(&ShellAction::Activate)
                    .map_err(|e| format!("remap confirm: {e:?}"))?;
            }
            Some(Effect::RollbackRemap) => {
                remap
                    .gamepad_action(&ShellAction::Back)
                    .map_err(|e| format!("remap rollback: {e:?}"))?;
            }
            Some(Effect::CompleteFirstRun) => {
                preferences
                    .submit_change(PreferenceChange {
                        key: PreferenceKey("firstRunComplete".into()),
                        value: PreferenceValue::Bool(true),
                        authority: ChangeAuthority("user".into()),
                    })
                    .map_err(|e| format!("preferences: {e:?}"))?;
            }
            None => {}
        }
        if before != (core.presentation().clone(), core.focus()) {
            // Rasterizer damage tracking makes unchanged parts of the retained
            // scene a no-op at the fbdev boundary.
            if let Some(scene) = core.scene(host.metrics(), activate) {
                host.present(&scene).map_err(|e| e.to_string())?;
            }
        }
    }
}

fn fixture_preferences() -> FakePreferencePort {
    let values = [
        ("textScale", PreferenceValue::Text("100%".into())),
        ("highContrast", PreferenceValue::Bool(false)),
        ("reduceMotion", PreferenceValue::Bool(false)),
        ("reduceFlashing", PreferenceValue::Bool(false)),
    ]
    .into_iter()
    .map(|(key, value)| EffectivePreference {
        key: PreferenceKey(key.into()),
        effective: value.clone(),
        stored: value,
        applied: true,
    });
    FakePreferencePort::new(values, ChangeAuthority("user".into()))
}

struct DurablePreferences {
    inner: PrefsPreferencePort,
    state_file: PathBuf,
    pending: VecDeque<EffectivePreference>,
}

impl DurablePreferences {
    fn open(state_dir: &Path) -> Result<Self, String> {
        let store = PrefsStore::at(state_dir);
        let state_file = store.path().to_owned();
        let inner =
            PrefsPreferencePort::for_user(store).map_err(|e| format!("preferences: {e:?}"))?;
        Ok(Self {
            inner,
            // Launcher-owned forward-compatible keys live in the same atomic preference
            // document. `pf-prefs` preserves unknown keys when it updates schema-owned values.
            state_file,
            pending: VecDeque::new(),
        })
    }

    fn launcher_state(
        &self,
    ) -> Result<serde_json::Map<String, serde_json::Value>, PreferenceError> {
        match fs::read_to_string(&self.state_file) {
            Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .ok_or(PreferenceError::BackendUnavailable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(serde_json::Map::default())
            }
            Err(_) => Err(PreferenceError::BackendUnavailable),
        }
    }

    fn write_launcher_state(
        &self,
        state: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), PreferenceError> {
        let parent = self
            .state_file
            .parent()
            .ok_or(PreferenceError::BackendUnavailable)?;
        fs::create_dir_all(parent).map_err(|_| PreferenceError::BackendUnavailable)?;
        let temporary = self
            .state_file
            .with_extension(format!("json.tmp.{}", std::process::id()));
        let bytes =
            serde_json::to_vec_pretty(state).map_err(|_| PreferenceError::BackendUnavailable)?;
        fs::write(&temporary, bytes).map_err(|_| PreferenceError::BackendUnavailable)?;
        fs::rename(temporary, &self.state_file).map_err(|_| PreferenceError::BackendUnavailable)
    }

    fn first_run_complete(&self) -> Result<bool, String> {
        self.launcher_state()
            .map(|state| {
                state
                    .get("firstRunComplete")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .map_err(|e| format!("preferences: {e:?}"))
    }

    fn safe_return_binding(&self) -> Result<Option<String>, String> {
        self.launcher_state()
            .map(|state| {
                state
                    .get("safeReturnBinding")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .map_err(|e| format!("preferences: {e:?}"))
    }
}

impl PreferencePort for DurablePreferences {
    fn read(&self, key: &PreferenceKey) -> Result<Option<EffectivePreference>, PreferenceError> {
        if matches!(key.0.as_str(), "firstRunComplete" | "safeReturnBinding") {
            return Ok(None);
        }
        self.inner.read(key).map(|value| {
            value.map(|mut observed| {
                observed.effective = observed.stored.clone();
                observed.applied = true;
                observed
            })
        })
    }

    fn next_change(&mut self, deadline: Deadline) -> Result<PreferencePoll, PreferenceError> {
        if let Some(change) = self.pending.pop_front() {
            return Ok(PreferencePoll::Changed(change));
        }
        self.inner.next_change(deadline).map(|poll| match poll {
            PreferencePoll::Changed(mut change) => {
                change.effective = change.stored.clone();
                change.applied = true;
                PreferencePoll::Changed(change)
            }
            other => other,
        })
    }

    fn submit_change(
        &mut self,
        change: PreferenceChange,
    ) -> Result<PreferenceChangeResult, PreferenceError> {
        if matches!(
            change.key.0.as_str(),
            "firstRunComplete" | "safeReturnBinding"
        ) {
            if change.authority != ChangeAuthority("user".into()) {
                return Ok(PreferenceChangeResult::Unauthorized);
            }
            let mut state = self.launcher_state()?;
            let value = match change.value {
                PreferenceValue::Bool(value) => serde_json::Value::Bool(value),
                PreferenceValue::Text(value) => serde_json::Value::String(value),
                PreferenceValue::Integer(value) => serde_json::Value::Number(value.into()),
            };
            state.insert(change.key.0, value);
            self.write_launcher_state(&state)?;
            return Ok(PreferenceChangeResult::Accepted);
        }
        let key = change.key.clone();
        let result = self.inner.submit_change(change)?;
        if matches!(
            result,
            PreferenceChangeResult::StoredNotApplied | PreferenceChangeResult::Accepted
        ) {
            if let Some(mut observed) = self.inner.read(&key)? {
                observed.effective = observed.stored.clone();
                observed.applied = true;
                self.pending.push_back(observed);
            }
            return Ok(PreferenceChangeResult::Accepted);
        }
        Ok(result)
    }
}

#[derive(Default)]
struct InteractiveSession {
    active: Option<String>,
    pending: VecDeque<SessionEvent>,
    history: Vec<SessionEvent>,
}

impl InteractiveSession {
    fn safe_return(&mut self) {
        let Some(session_id) = self.active.take() else {
            return;
        };
        self.pending.extend([
            SessionEvent::Observed(ObservedSessionState::ObservationComplete),
            SessionEvent::Terminal(TerminalReceipt::Returned { session_id }),
        ]);
    }
}

impl SessionPort for InteractiveSession {
    fn launch(&mut self, request: pf_ports::LaunchRequest) -> Result<LaunchResult, SessionError> {
        if self.active.is_some() {
            return Ok(LaunchResult::RejectedBusy);
        }
        let session_id = format!("launcher-{}", request.item_id);
        self.active = Some(session_id.clone());
        self.pending
            .push_back(SessionEvent::Observed(ObservedSessionState::Running));
        Ok(LaunchResult::Accepted { session_id })
    }

    fn next_event(&mut self, _deadline: Deadline) -> Result<SessionPoll, SessionError> {
        let Some(event) = self.pending.pop_front() else {
            return Ok(SessionPoll::Idle);
        };
        self.history.push(event.clone());
        Ok(SessionPoll::Event(event))
    }

    fn history(&self) -> &[SessionEvent] {
        &self.history
    }
}

fn emit(
    host: &mut OffscreenHost,
    core: &ShellCore,
    prompt: &str,
    out: &Path,
    name: &str,
) -> Result<(), String> {
    host.present(
        core.scene(host.metrics(), prompt)
            .as_ref()
            .ok_or("shell has no frame")?,
    )
    .map_err(|e| e.to_string())?;
    let frame = host.frame().ok_or("frame missing")?;
    let path = out.join(format!("{name}.png"));
    let file = fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|e| e.to_string())?
        .write_image_data(&frame.rgba)
        .map_err(|e| e.to_string())?;
    println!("{}  {}", hex(&Sha256::digest(&frame.rgba)), path.display());
    Ok(())
}

fn value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        out
    })
}

#[cfg(test)]
mod durable_tests {
    use super::*;

    #[test]
    fn first_run_completion_survives_two_boots_in_one_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = DurablePreferences::open(dir.path()).unwrap();
        assert!(!first.first_run_complete().unwrap());
        assert_eq!(
            first
                .submit_change(PreferenceChange {
                    key: PreferenceKey("firstRunComplete".into()),
                    value: PreferenceValue::Bool(true),
                    authority: ChangeAuthority("user".into()),
                })
                .unwrap(),
            PreferenceChangeResult::Accepted
        );
        drop(first);
        let second = DurablePreferences::open(dir.path()).unwrap();
        assert!(second.first_run_complete().unwrap());
    }
}
