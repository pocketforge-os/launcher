use std::{fs, path::Path, process::Command};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

// Keep every design-match ratchet in this one reviewable table. The PR checklist
// explicitly forbids lowering these values; improvements should raise them.
const SCREENS: [(&str, &str, f64); 5] = [
    ("home", "boot-home.png", 0.938_694),
    ("library", "library.png", 0.973_947_543_629_720_2),
    ("detail", "details.png", 0.940_459),
    ("settings", "settings.png", 0.959_066),
    ("high-contrast", "high-contrast.png", 0.911_423),
];

#[test]
fn offscreen_routes_match_approved_design_renders() {
    let rendered = tempfile::tempdir().unwrap();
    render(rendered.path(), &[]);
    render(rendered.path(), &["--settings-evidence"]);
    render(rendered.path(), &["--high-contrast-evidence"]);

    let diff_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/mockup-diff");
    fs::create_dir_all(&diff_dir).unwrap();
    let mut failures = Vec::new();

    for (screen, actual_name, threshold) in SCREENS {
        let golden_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/goldens")
            .join(format!("{screen}.png"));
        let actual_path = rendered.path().join(actual_name);
        let golden = decode_rgba(&golden_path);
        let actual = decode_rgba(&actual_path);
        let (score, diff) = similarity_and_diff(&golden, &actual);
        let diff_path = diff_dir.join(format!("{screen}-diff.png"));
        write_rgba(&diff_path, &diff);
        println!(
            "mockup-diff screen={screen} score={score:.15} score_bits={:#018x} threshold={threshold:.15} diff={}",
            score.to_bits(),
            diff_path.display()
        );
        if score + f64::EPSILON < threshold {
            failures.push(format!(
                "{screen}: score {score:.6} < threshold {threshold:.6}; diff {}",
                diff_path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "mockup similarity failures:\n{}",
        failures.join("\n")
    );
}

fn render(out: &Path, extra: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_pf-shell"))
        .arg("--offscreen")
        .args(extra)
        .args(["--out", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "offscreen render failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn decode_rgba(path: &Path) -> Vec<u8> {
    let decoder = png::Decoder::new(fs::File::open(path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!(
        (info.width, info.height),
        (WIDTH, HEIGHT),
        "{}",
        path.display()
    );
    match info.color_type {
        png::ColorType::Rgba => bytes[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => bytes[..info.buffer_size()]
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        other => panic!("unsupported PNG color type {other:?}: {}", path.display()),
    }
}

fn similarity_and_diff(golden: &[u8], actual: &[u8]) -> (f64, Vec<u8>) {
    assert_eq!(golden.len(), actual.len());
    let mut absolute_error = 0_u32;
    let mut diff = Vec::with_capacity(golden.len());
    for (expected, observed) in golden.chunks_exact(4).zip(actual.chunks_exact(4)) {
        for channel in 0..3 {
            let delta = expected[channel].abs_diff(observed[channel]);
            absolute_error += u32::from(delta);
            diff.push(delta.saturating_mul(3));
        }
        diff.push(255);
    }
    let channels = WIDTH * HEIGHT * 3;
    (
        1.0 - f64::from(absolute_error) / f64::from(channels * 255),
        diff,
    )
}

fn write_rgba(path: &Path, rgba: &[u8]) {
    let file = fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(file, WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(rgba)
        .unwrap();
}
