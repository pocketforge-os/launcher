//! # fidelity-audit
//!
//! Per-component mockup-vs-render structural + perceptual fidelity audit for the
//! Quiet Console shell (parent bead `tsp-op5a.389`).
//!
//! Whole-route similarity scoring (`mockup_diff`) is blind to small local
//! divergences — a mis-aligned icon, a focus treatment rendered as the wrong
//! decoration, text a size too small. This tool compares the shell to the design
//! mockups **per component**, through a committed selector<->node MAPPING table,
//! and enumerates every divergence as a structured ledger row.
//!
//! It is a pure ARTIFACT consumer — it reads the committed design-facts ground
//! truth, the shell's `<slug>.json` scene dump and `<slug>.png` render, and the
//! approved golden renders. It never links pf-shell-core, so it is decoupled from
//! scene-construction internals, and it never mutates pixels.
//!
//! Layers (see the bead architecture): (1) deterministic structural facts diff
//! (geometry / font-size / decoration), (2) perceptual per-component crop diff,
//! (3) a documented-baseline gate for CI (report-only by default; the gate flips
//! on in the follow-up sweep bead once the first ledger is triaged).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::module_name_repetitions,
    clippy::missing_panics_doc
)]

pub mod baseline;
pub mod compare;
pub mod facts;
pub mod ledger;
pub mod mapping;
pub mod raster;
pub mod scene;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use baseline::Baseline;
use facts::{DesignFacts, Facts};
use ledger::{Finding, Ledger, Severity};
use mapping::{Component, FactClass, Mapping, RouteMap};
use raster::Raster;
use scene::SceneTree;

/// How the ledger is printed to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Json,
}

/// Audit configuration (resolved from CLI args).
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding `design-facts/`, `mapping/`, `baseline/` (the crate root).
    pub crate_dir: PathBuf,
    /// Launcher repo root, for `golden_png` paths.
    pub repo_root: PathBuf,
    /// Directory with `<slug>.json` + `<slug>.png` (produced by `pf-shell --offscreen`).
    pub renders_dir: PathBuf,
    /// Output directory for crop artifacts and the ledger JSON.
    pub out_dir: PathBuf,
    /// If set, render the slugs by invoking this `pf-shell` binary first.
    pub shell_bin: Option<PathBuf>,
    /// Route filter (audit route ids); `None` audits every mapped route.
    pub routes: Option<Vec<String>>,
    /// Fail on any unaccepted divergence.
    pub gate: bool,
    pub format: OutputFormat,
}

impl Config {
    /// The default crate directory (compile-time location of this crate).
    #[must_use]
    pub fn default_crate_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }
}

/// The result of an audit run.
#[derive(Debug)]
pub struct RunResult {
    pub ledger: Ledger,
    pub ledger_path: PathBuf,
}

/// Run the audit end to end.
///
/// # Errors
/// Returns an error on unreadable inputs, a failed render, or a write failure.
pub fn run(config: &Config) -> Result<RunResult, String> {
    let mapping = Mapping::load(&config.crate_dir.join("mapping/mapping.json"))?;
    let baseline = Baseline::load(&config.crate_dir.join("baseline/accepted.json"))?;

    let selected: Vec<&RouteMap> = mapping
        .routes
        .iter()
        .filter(|r| {
            config
                .routes
                .as_ref()
                .is_none_or(|want| want.iter().any(|w| w == &r.route))
        })
        .collect();

    if let Some(bin) = &config.shell_bin {
        render_slugs(bin, &selected, &config.renders_dir)?;
    }

    std::fs::create_dir_all(&config.out_dir)
        .map_err(|e| format!("create out dir {}: {e}", config.out_dir.display()))?;

    let mut ledger = Ledger::new();
    for route in &selected {
        audit_route(config, route, &mut ledger)?;
    }

    baseline.apply(&mut ledger);

    let ledger_path = config.out_dir.join("ledger.json");
    std::fs::write(&ledger_path, ledger.to_json()?)
        .map_err(|e| format!("write ledger {}: {e}", ledger_path.display()))?;

    Ok(RunResult {
        ledger,
        ledger_path,
    })
}

fn render_slugs(bin: &Path, routes: &[&RouteMap], renders_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(renders_dir)
        .map_err(|e| format!("create renders dir {}: {e}", renders_dir.display()))?;
    // One `--offscreen` invocation per distinct flag-set covers every slug it emits.
    let mut flag_sets: BTreeSet<Vec<String>> = BTreeSet::new();
    for r in routes {
        flag_sets.insert(r.render_flags.clone());
    }
    for flags in flag_sets {
        let status = Command::new(bin)
            .arg("--offscreen")
            .args(&flags)
            .arg("--out")
            .arg(renders_dir)
            .status()
            .map_err(|e| format!("run {} --offscreen: {e}", bin.display()))?;
        if !status.success() {
            return Err(format!(
                "{} --offscreen {} failed: {status}",
                bin.display(),
                flags.join(" ")
            ));
        }
    }
    Ok(())
}

fn audit_route(config: &Config, route: &RouteMap, ledger: &mut Ledger) -> Result<(), String> {
    let design = DesignFacts::load(
        &config
            .crate_dir
            .join("design-facts")
            .join(&route.design_facts),
    )?;
    let scene_path = config
        .renders_dir
        .join(format!("{}.semantic.txt", route.shell_slug));
    let tree = SceneTree::load_semantic(&scene_path)?;
    let index = tree.index();
    let scale = tree.text_scale;

    // PNGs are optional inputs; a comparator that needs a missing one degrades to
    // an info finding rather than aborting the whole route.
    let shell_png =
        load_optional_raster(&config.renders_dir.join(format!("{}.png", route.shell_slug)));
    let golden_png = route
        .golden_png
        .as_ref()
        .map(|g| load_optional_raster(&config.repo_root.join(g)))
        .and_then(|r| r);

    for comp in &route.components {
        let Some(mockup) = resolve_facts(&design, comp) else {
            ledger.extend([info(
                route,
                comp,
                "mapping",
                "missing design-facts selector",
            )]);
            continue;
        };
        let Some(node) = index.get(comp.node.as_str()) else {
            ledger.extend([info(route, comp, "mapping", "mapped scene node not found")]);
            continue;
        };

        if comp.wants(FactClass::Geometry) {
            ledger.extend(compare::geometry(
                &route.route,
                comp,
                &mockup.bbox,
                &node.bounds,
            ));
        }
        if comp.wants(FactClass::FontSize) {
            if let Some(px) = mockup.font.size_px() {
                ledger.extend(compare::font_size(&route.route, comp, px, node, scale));
            }
        }
        if comp.wants(FactClass::Decoration) {
            let has_underline = comp
                .underline_node
                .as_deref()
                .is_some_and(|id| index.contains_key(id));
            ledger.extend(compare::decoration(
                &route.route,
                comp,
                mockup,
                has_underline,
            ));
        }
        if comp.wants(FactClass::Color) {
            match &shell_png {
                Some(shell) => {
                    ledger.extend(compare::color(
                        &route.route,
                        comp,
                        &mockup.color,
                        shell,
                        &mockup.bbox,
                    ));
                }
                None => ledger.extend([info(route, comp, "color", "shell render png unavailable")]),
            }
        }
        if comp.wants(FactClass::Crop) {
            run_crop(
                config,
                route,
                comp,
                mockup,
                shell_png.as_ref(),
                golden_png.as_ref(),
                ledger,
            )?;
        }
    }
    Ok(())
}

fn run_crop(
    config: &Config,
    route: &RouteMap,
    comp: &Component,
    mockup: &Facts,
    shell: Option<&Raster>,
    golden: Option<&Raster>,
    ledger: &mut Ledger,
) -> Result<(), String> {
    match (golden, shell) {
        (Some(g), Some(s)) => {
            if let Some(f) = compare::crop(&route.route, comp, &mockup.bbox, g, s, &config.out_dir)?
            {
                ledger.extend([f]);
            }
        }
        _ => ledger.extend([info(
            route,
            comp,
            "crop",
            "golden or shell render unavailable",
        )]),
    }
    Ok(())
}

fn resolve_facts<'a>(design: &'a DesignFacts, comp: &Component) -> Option<&'a Facts> {
    match comp.index {
        Some(i) => design.instance(&comp.selector, i),
        None => design.unique(&comp.selector),
    }
}

fn load_optional_raster(path: &Path) -> Option<Raster> {
    if path.exists() {
        Raster::load(path).ok()
    } else {
        None
    }
}

fn info(route: &RouteMap, comp: &Component, class: &str, detail: &str) -> Finding {
    Finding {
        route: route.route.clone(),
        selector: comp.label(),
        node: comp.node.clone(),
        fact_class: class.to_string(),
        expected: String::new(),
        shipped: String::new(),
        delta: detail.to_string(),
        severity: Severity::Info,
        artifact: None,
        accepted: false,
    }
}
