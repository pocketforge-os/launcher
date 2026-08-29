use pf_catalog::CatalogSnapshot;
use pf_framehost::{FbdevHost, OffscreenHost};
use pf_input_map::{DeviceContract, EffectiveMap, MemoryStore};
use pf_ports::{
    FrameHost, LaunchResult, ObservedSessionState, SessionEvent, SessionPort, ShellAction,
    TerminalReceipt,
};
use pf_scene::{Insets, Orientation, SurfaceMetrics};
use pf_shell::prompt;
use pf_shell_core::{Effect, ShellCore};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{env, fs, io::BufWriter, path::Path};

fn main() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let snapshot: CatalogSnapshot = serde_json::from_str(include_str!("../fixtures/catalog.json"))
        .map_err(|e| e.to_string())?;
    let theme = pf_theme::flagship();
    let reduced = env::var_os("PF_REDUCE_MOTION").is_some();
    let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json"))
        .map_err(|e| format!("{e:?}"))?;
    let glyphs =
        EffectiveMap::load(contract, &MemoryStore::default()).map_err(|e| format!("{e:?}"))?;
    let activate = prompt(&glyphs, &ShellAction::Activate);
    let mut core = ShellCore::boot(&snapshot, &theme, reduced);
    if args.iter().any(|a| a == "--fbdev") {
        let path = value(&args, "--device").unwrap_or("/dev/fb0");
        let mut host = FbdevHost::open(path).map_err(|e| e.to_string())?;
        return host
            .present(&core.scene(host.metrics(), &activate))
            .map(|_| ())
            .map_err(|e| e.to_string());
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
    emit(&mut host, &core, &activate, out, "boot-home")?;
    core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
    emit(&mut host, &core, &activate, out, "focus-moved")?;
    let effect = core
        .action(&ShellAction::Activate)
        .ok_or("fixture must launch")?;
    emit(&mut host, &core, &activate, out, "launch-dimmed")?;
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
    emit(&mut host, &core, &activate, out, "returned")?;
    Ok(())
}

fn emit(
    host: &mut OffscreenHost,
    core: &ShellCore,
    prompt: &str,
    out: &Path,
    name: &str,
) -> Result<(), String> {
    host.present(&core.scene(host.metrics(), prompt))
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
