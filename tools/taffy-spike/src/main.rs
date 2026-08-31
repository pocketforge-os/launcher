//! Standalone Taffy adoption spike. This is deliberately not wired into `pf-shell`.

use pf_render::Rasterizer;
use pf_scene::{
    Bounds, Insets, Node, NodeId as SceneNodeId, Orientation, Role, Scene, SurfaceMetrics, TypeRole,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap, env, fmt::Write as _, fs, path::Path, process::Command, time::Instant,
};
use taffy::prelude::*;

const LABELS: [&str; 6] = [
    "Bibliothèque des aventures extraordinaires",
    "Wiederaufnahme der zuletzt gespielten Expedition",
    "コレクションとクラウド保存データの管理",
    "Configuración de accesibilidad y controles",
    "Продолжить приключение с последнего сохранения",
    "مكتبة الألعاب والإعدادات المتقدمة",
];
const SURFACES: [(&str, f32, f32); 4] = [
    ("small", 960.0, 540.0),
    ("standard", 1280.0, 720.0),
    ("portrait", 720.0, 1280.0),
    ("large", 1920.0, 1080.0),
];

#[derive(Clone, Debug, PartialEq)]
struct ShapeConfig {
    type_role: TypeRole,
    line_height: f32,
    text_scale: f32,
}

#[derive(Clone)]
struct TextContext {
    id: &'static str,
    label: &'static str,
    config: ShapeConfig,
}

#[derive(Clone, Copy)]
struct RectF {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn main() {
    if env::args().any(|arg| arg == "--matrix-child") {
        let (digest, p95) = run_matrix().unwrap_or_else(|error| panic!("{error}"));
        let binary_delta = binary_delta_metric();
        eprintln!(
            "SMOKE cpu_layout_p95_us={p95} peak_rss_kib={} binary_bytes={} {binary_delta}",
            peak_rss_kib(),
            binary_size()
        );
        println!("MATRIX digest={digest}");
        return;
    }

    let exe = env::current_exe().expect("current executable");
    let mut outputs = Vec::new();
    for run in 1..=2 {
        let output = Command::new(&exe)
            .arg("--matrix-child")
            .output()
            .expect("start fresh matrix process");
        assert!(
            output.status.success(),
            "run {run}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("run={run} {}", String::from_utf8_lossy(&output.stderr));
        outputs.push(output.stdout);
    }
    assert_eq!(
        outputs[0], outputs[1],
        "fresh-process bounds/frame digest differs"
    );
    print!("{}", String::from_utf8_lossy(&outputs[0]));
}

fn run_matrix() -> Result<(String, u128), String> {
    let mut matrix_hash = Sha256::new();
    let mut timings = Vec::new();
    for (surface, base_width, base_height) in SURFACES {
        for orientation in [Orientation::Landscape, Orientation::Portrait] {
            let (width, height) = match orientation {
                Orientation::Landscape | Orientation::LandscapeFlipped => {
                    (base_width.max(base_height), base_width.min(base_height))
                }
                Orientation::Portrait | Orientation::PortraitFlipped => {
                    (base_width.min(base_height), base_width.max(base_height))
                }
            };
            for percent in [100_u16, 150, 200] {
                let (record, layout_us) =
                    layout_and_paint(surface, width, height, orientation, percent)?;
                timings.push(layout_us);
                matrix_hash.update(record.as_bytes());
            }
        }
    }
    timings.sort_unstable();
    let p95_index = (timings.len() * 95).div_ceil(100).saturating_sub(1);
    Ok((hex(&matrix_hash.finalize()), timings[p95_index]))
}

#[allow(clippy::too_many_lines)]
fn layout_and_paint(
    surface: &str,
    width: f32,
    height: f32,
    orientation: Orientation,
    scale_percent: u16,
) -> Result<(String, u128), String> {
    let text_scale = f32::from(scale_percent) / 100.0;
    let paint_config = ShapeConfig {
        type_role: TypeRole::Label,
        line_height: 1.25,
        text_scale,
    };
    let mut tree = TaffyTree::<TextContext>::new();
    let mut cards = Vec::new();
    for (index, label) in LABELS.iter().enumerate() {
        let text = tree
            .new_leaf_with_context(
                Style {
                    min_size: Size {
                        width: zero(),
                        height: length(48.0_f32),
                    },
                    ..Style::default()
                },
                TextContext {
                    id: LABELS[index],
                    label,
                    config: paint_config.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        cards.push(text);
    }
    let grid = tree
        .new_with_children(
            Style {
                display: Display::Grid,
                grid_template_columns: vec![fr(1.0_f32), fr(1.0_f32), fr(1.0_f32)],
                gap: Size {
                    width: length(12.0_f32),
                    height: length(12.0_f32),
                },
                flex_grow: 1.0,
                min_size: Size {
                    width: zero(),
                    height: zero(),
                },
                ..Style::default()
            },
            &cards,
        )
        .map_err(|error| error.to_string())?;
    let heading = tree
        .new_leaf_with_context(
            Style {
                min_size: Size {
                    width: zero(),
                    height: length(48.0_f32),
                },
                ..Style::default()
            },
            TextContext {
                id: "shelf-heading",
                label: "Recently played • Reprendre votre aventure",
                config: paint_config.clone(),
            },
        )
        .map_err(|error| error.to_string())?;
    let shelf = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                gap: Size {
                    width: zero(),
                    height: length(12.0_f32),
                },
                padding: Rect {
                    left: length(24.0_f32),
                    right: length(24.0_f32),
                    top: length(20.0_f32),
                    bottom: length(20.0_f32),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Style::default()
            },
            &[heading, grid],
        )
        .map_err(|error| error.to_string())?;
    let overlay = tree
        .new_leaf_with_context(
            Style {
                position: Position::Absolute,
                inset: Rect {
                    left: auto(),
                    right: length(24.0_f32),
                    top: length(24.0_f32),
                    bottom: auto(),
                },
                size: Size {
                    width: length((width * 0.34).max(280.0)),
                    height: auto(),
                },
                min_size: Size {
                    width: zero(),
                    height: length(48.0_f32),
                },
                ..Style::default()
            },
            TextContext {
                id: "overlay",
                label: "Quick actions — Einstellungen und Barrierefreiheit",
                config: paint_config.clone(),
            },
        )
        .map_err(|error| error.to_string())?;
    let root = tree
        .new_with_children(
            Style {
                size: Size {
                    width: length(width),
                    height: length(height),
                },
                ..Style::default()
            },
            &[shelf, overlay],
        )
        .map_err(|error| error.to_string())?;

    let started = Instant::now();
    let mut measure_cache = HashMap::new();
    tree.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        },
        |known, available, _, context, _| match context {
            Some(context) => {
                assert_eq!(
                    context.config, paint_config,
                    "measure and paint shaping config differ"
                );
                let key = format!("{:?}:{known:?}:{available:?}", context.config);
                *measure_cache
                    .entry((context.id, key))
                    .or_insert_with(|| measure_with_production_shaper(context, known, available))
            }
            None => Size::ZERO,
        },
    )
    .map_err(|error| error.to_string())?;
    let layout_us = started.elapsed().as_micros();

    let mut painted = Vec::new();
    collect_text_nodes(&tree, root, 0.0, 0.0, &paint_config, &mut painted)?;
    validate_geometry(width, height, &painted)?;
    let children = painted
        .iter()
        .map(|(context, rect)| text_node(context, *rect))
        .collect::<Vec<_>>();
    let root_node = Node::new(
        SceneNodeId::new("root").unwrap(),
        Role::Group,
        "fixture",
        Bounds::new(0.0, 0.0, width, height),
        "--color-surface-canvas",
    )
    .with_children(children);
    let scene = Scene::new(root_node, SceneNodeId::new("root").unwrap())
        .map_err(|error| error.to_string())?;
    let metrics = SurfaceMetrics {
        logical_width: width,
        logical_height: height,
        scale: 1.0,
        safe_insets: Insets::default(),
        orientation,
    };
    let mut renderer = Rasterizer::new();
    renderer
        .set_text_scale(text_scale)
        .map_err(|error| format!("{error:?}"))?;
    let frame = renderer
        .render(&scene, metrics)
        .map_err(|error| format!("{error:?}"))?;
    ensure_text_ink_is_clipped(&scene, metrics, text_scale)?;

    let mut record = format!("{surface}:{orientation:?}:{scale_percent}:{width:.0}x{height:.0}\n");
    for (context, rect) in &painted {
        writeln!(
            record,
            "{}={:.2},{:.2},{:.2},{:.2}\n",
            context.id, rect.x, rect.y, rect.width, rect.height
        )
        .unwrap();
    }
    writeln!(record, "frame={}", hex(&Sha256::digest(&frame.rgba))).unwrap();
    Ok((record, layout_us))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn measure_with_production_shaper(
    context: &TextContext,
    known: Size<Option<f32>>,
    available: Size<AvailableSpace>,
) -> Size<f32> {
    if let Size {
        width: Some(width),
        height: Some(height),
    } = known
    {
        return Size { width, height };
    }
    let width = known.width.unwrap_or_else(|| match available.width {
        AvailableSpace::Definite(value) => value.max(1.0),
        AvailableSpace::MinContent => 48.0,
        AvailableSpace::MaxContent => 640.0,
    });
    let probe_height = 512.0;
    let rect = RectF {
        x: 0.0,
        y: 0.0,
        width,
        height: probe_height,
    };
    let text = text_node(context, rect);
    let blank = Node::new(
        SceneNodeId::new("measure").unwrap(),
        Role::Group,
        context.label,
        Bounds::new(0.0, 0.0, width, probe_height),
        "--color-surface-raised",
    );
    let metrics = SurfaceMetrics {
        logical_width: width.max(1.0),
        logical_height: probe_height,
        scale: 1.0,
        safe_insets: Insets::default(),
        orientation: Orientation::Landscape,
    };
    let render = |node: Node| {
        let focus = node.id.clone();
        let scene = Scene::new(node, focus).unwrap();
        let mut rasterizer = Rasterizer::new();
        rasterizer
            .set_text_scale(context.config.text_scale)
            .unwrap();
        rasterizer.render(&scene, metrics).unwrap().rgba
    };
    let ink = render(text);
    let background = render(blank);
    let mut right = 0_usize;
    let mut bottom = 0_usize;
    for (index, (a, b)) in ink
        .chunks_exact(4)
        .zip(background.chunks_exact(4))
        .enumerate()
    {
        if a != b {
            right = right.max(index % width.ceil() as usize + 1);
            bottom = bottom.max(index / width.ceil() as usize + 1);
        }
    }
    Size {
        width: known
            .width
            .unwrap_or((right as f32 + 12.0).min(width).max(48.0)),
        height: known.height.unwrap_or((bottom as f32 + 8.0).max(48.0)),
    }
}

fn text_node(context: &TextContext, rect: RectF) -> Node {
    Node::new(
        SceneNodeId::new(if context.id == context.label {
            format!("label-{}", short_hash(context.label))
        } else {
            context.id.into()
        })
        .unwrap(),
        Role::Text,
        context.label,
        Bounds::new(rect.x, rect.y, rect.width, rect.height),
        "--color-surface-raised",
    )
    .with_type_role(context.config.type_role)
    .with_line_height(context.config.line_height)
}

fn collect_text_nodes(
    tree: &TaffyTree<TextContext>,
    node: taffy::NodeId,
    parent_x: f32,
    parent_y: f32,
    paint_config: &ShapeConfig,
    output: &mut Vec<(TextContext, RectF)>,
) -> Result<(), String> {
    let layout = *tree.layout(node).map_err(|error| error.to_string())?;
    let x = parent_x + layout.location.x;
    let y = parent_y + layout.location.y;
    if let Some(context) = tree.get_node_context(node) {
        assert_eq!(
            &context.config, paint_config,
            "measure and paint shaping config differ"
        );
        output.push((
            context.clone(),
            RectF {
                x,
                y,
                width: layout.size.width,
                height: layout.size.height,
            },
        ));
    }
    for child in tree.children(node).map_err(|error| error.to_string())? {
        collect_text_nodes(tree, child, x, y, paint_config, output)?;
    }
    Ok(())
}

fn validate_geometry(
    width: f32,
    height: f32,
    nodes: &[(TextContext, RectF)],
) -> Result<(), String> {
    for (context, rect) in nodes {
        if rect.width < 48.0 || rect.height < 48.0 {
            return Err(format!(
                "{} misses 48px target: {}x{}",
                context.id, rect.width, rect.height
            ));
        }
        if rect.x < 0.0
            || rect.y < 0.0
            || rect.x + rect.width > width + 0.01
            || rect.y + rect.height > height + 0.01
        {
            return Err(format!("{} escapes viewport", context.id));
        }
    }
    let required = nodes
        .iter()
        .filter(|(context, _)| context.id != "overlay")
        .collect::<Vec<_>>();
    for (index, (_, a)) in required.iter().enumerate() {
        for (_, b) in required.iter().skip(index + 1) {
            if a.x < b.x + b.width
                && b.x < a.x + a.width
                && a.y < b.y + b.height
                && b.y < a.y + a.height
            {
                return Err("required siblings overlap".into());
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn ensure_text_ink_is_clipped(
    scene: &Scene,
    metrics: SurfaceMetrics,
    text_scale: f32,
) -> Result<(), String> {
    for node in &scene.root().children {
        // Preserve the computed width (and therefore the production wrapping), but
        // remove the bottom clip so ink from lines that do not fit is observable.
        let mut ink_node = node.clone();
        ink_node.bounds.height = (metrics.logical_height - node.bounds.y).max(node.bounds.height);
        let mut ink_root = scene.root().clone();
        ink_root.children = vec![ink_node.clone()];
        let mut blank_node = ink_node;
        blank_node.role = Role::Group;
        let mut blank_root = scene.root().clone();
        blank_root.children = vec![blank_node];
        let render = |root| -> Result<Vec<u8>, String> {
            let isolated = Scene::new(root, SceneNodeId::new("root").unwrap())
                .map_err(|error| error.to_string())?;
            let mut rasterizer = Rasterizer::new();
            rasterizer
                .set_text_scale(text_scale)
                .map_err(|error| format!("{error:?}"))?;
            rasterizer
                .render(&isolated, metrics)
                .map(|frame| frame.rgba)
                .map_err(|error| format!("{error:?}"))
        };
        let ink = render(ink_root)?;
        let background = render(blank_root)?;
        for (index, (a, b)) in ink
            .chunks_exact(4)
            .zip(background.chunks_exact(4))
            .enumerate()
        {
            if a != b {
                let x = (index % metrics.logical_width as usize) as f32;
                let y = (index / metrics.logical_width as usize) as f32;
                // Pixel coordinates are integral, so no sub-pixel AA tolerance is used.
                if x < node.bounds.x
                    || x >= node.bounds.x + node.bounds.width
                    || y < node.bounds.y
                    || y >= node.bounds.y + node.bounds.height
                {
                    return Err(format!(
                        "required text ink from {:?} escaped its computed node",
                        node.id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn short_hash(value: &str) -> String {
    hex(&Sha256::digest(value.as_bytes()))[..12].into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        write!(output, "{byte:02x}").unwrap();
        output
    })
}
fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}
fn binary_size() -> i64 {
    env::current_exe()
        .ok()
        .and_then(|path| fs::metadata(path).ok())
        .map_or(0, |meta| i64::try_from(meta.len()).unwrap_or(i64::MAX))
}
fn binary_delta_metric() -> String {
    let Some(executable) = env::current_exe().ok() else {
        return "binary_delta_vs_pf_shell_bytes=unavailable reason=current-exe-unavailable".into();
    };
    binary_delta_metric_for(&executable)
}

fn binary_delta_metric_for(executable: &Path) -> String {
    let shell = executable.with_file_name("pf-shell");
    if fs::metadata(&shell).is_err() {
        return "binary_delta_vs_pf_shell_bytes=unavailable reason=pf-shell-artifact-missing"
            .into();
    }
    match binary_delta(executable, &shell) {
        Some(delta) => format!("binary_delta_vs_pf_shell_bytes={delta}"),
        None => "binary_delta_vs_pf_shell_bytes=unavailable reason=spike-artifact-missing".into(),
    }
}

fn binary_delta(executable: &Path, shell: &Path) -> Option<i64> {
    let executable = i64::try_from(fs::metadata(executable).ok()?.len()).unwrap_or(i64::MAX);
    let shell = i64::try_from(fs::metadata(shell).ok()?.len()).unwrap_or(i64::MAX);
    Some(executable - shell)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn binary_delta_requires_a_real_pf_shell_artifact() {
        let fixture =
            env::temp_dir().join(format!("taffy-spike-binary-delta-{}", std::process::id()));
        fs::create_dir_all(&fixture).unwrap();
        let spike = fixture.join("taffy-spike");
        let shell = fixture.join("pf-shell");
        let _ = fs::remove_file(&shell);
        fs::File::create(&spike)
            .unwrap()
            .write_all(&[0; 13])
            .unwrap();

        assert_eq!(binary_delta(&spike, &shell), None);
        assert_eq!(
            binary_delta_metric_for(&spike),
            "binary_delta_vs_pf_shell_bytes=unavailable reason=pf-shell-artifact-missing"
        );

        fs::File::create(&shell)
            .unwrap()
            .write_all(&[0; 21])
            .unwrap();
        assert_eq!(binary_delta(&spike, &shell), Some(-8));
        assert_eq!(
            binary_delta_metric_for(&spike),
            "binary_delta_vs_pf_shell_bytes=-8"
        );

        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn overflow_is_attributed_to_the_text_node_that_produced_it() {
        let overflowing = Node::new(
            SceneNodeId::new("overflowing-label").unwrap(),
            Role::Text,
            "Configuración de accesibilidad y controles",
            Bounds::new(8.0, 8.0, 48.0, 48.0),
            "--color-surface-raised",
        )
        .with_type_role(TypeRole::Label)
        .with_line_height(1.25);
        // This sibling deliberately covers the overflowing label's shaped advance.
        // The former any-node check therefore accepted the escaped pixels.
        let covering_sibling = Node::new(
            SceneNodeId::new("covering-sibling").unwrap(),
            Role::Group,
            "cover",
            Bounds::new(56.0, 8.0, 248.0, 48.0),
            "--color-surface-raised",
        );
        let root = Node::new(
            SceneNodeId::new("root").unwrap(),
            Role::Group,
            "fixture",
            Bounds::new(0.0, 0.0, 320.0, 80.0),
            "--color-surface-canvas",
        )
        .with_children(vec![overflowing, covering_sibling]);
        let scene = Scene::new(root, SceneNodeId::new("root").unwrap()).unwrap();
        let metrics = SurfaceMetrics {
            logical_width: 320.0,
            logical_height: 80.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };

        let error = ensure_text_ink_is_clipped(&scene, metrics, 1.0).unwrap_err();
        assert!(error.contains("overflowing-label"), "{error}");

        let contained = Node::new(
            SceneNodeId::new("contained-label").unwrap(),
            Role::Text,
            "Configuración de accesibilidad y controles",
            Bounds::new(8.0, 8.0, 296.0, 64.0),
            "--color-surface-raised",
        )
        .with_type_role(TypeRole::Label)
        .with_line_height(1.25);
        let contained_root = Node::new(
            SceneNodeId::new("root").unwrap(),
            Role::Group,
            "fixture",
            Bounds::new(0.0, 0.0, 320.0, 80.0),
            "--color-surface-canvas",
        )
        .with_children(vec![contained]);
        let contained_scene =
            Scene::new(contained_root, SceneNodeId::new("root").unwrap()).unwrap();
        ensure_text_ink_is_clipped(&contained_scene, metrics, 1.0).unwrap();
    }
}
