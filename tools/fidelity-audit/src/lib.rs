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
//! truth, the shell's `<slug>.semantic.txt` scene snapshot and `<slug>.png`
//! render, and the approved golden renders. It never links pf-shell-core, so it
//! is decoupled from scene-construction internals, and it never mutates pixels.
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use baseline::Baseline;
use facts::{DesignFacts, Facts};
use ledger::{Finding, Ledger, Severity};
use mapping::{Component, FactClass, Mapping, RouteMap};
use raster::Raster;
use scene::{SceneNode, SceneTree};

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
    /// Directory with `<slug>.semantic.txt` + `<slug>.png` (produced by `pf-shell --offscreen`).
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

    // Validate every requested --route against the mapping BEFORE filtering: a
    // typo must be a hard error, not a zero-route empty-ledger exit-0.
    let known: BTreeSet<&str> = mapping.routes.iter().map(|r| r.route.as_str()).collect();
    if let Some(want) = &config.routes {
        let unknown: Vec<&str> = want
            .iter()
            .map(String::as_str)
            .filter(|w| !known.contains(w))
            .collect();
        if !unknown.is_empty() {
            let mut all: Vec<&str> = known.iter().copied().collect();
            all.sort_unstable();
            return Err(format!(
                "unknown --route id(s): {} (known routes: {})",
                unknown.join(", "),
                all.join(", ")
            ));
        }
    }

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

    // An empty selection means zero work; never silently exit 0.
    if selected.is_empty() {
        return Err("no routes selected to audit (the mapping has no routes)".to_string());
    }

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
    // A selected route that maps zero components does zero work — a mapping
    // authoring error, not a silent pass.
    if route.components.is_empty() {
        return Err(format!(
            "route {} has no components in the mapping (zero work)",
            route.route
        ));
    }
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

    // Renders are REQUIRED inputs wherever an enabled comparator reads them.
    // Downgrade only FINDINGS, never INPUT failures: a render the audit was asked
    // to read but is absent or corrupt is a hard error, not a silent info row +
    // exit 0. Load a raster only when some component on this route needs it.
    let needs_shell = route
        .components
        .iter()
        .any(|c| c.wants(FactClass::Crop) || c.wants(FactClass::Color));
    let needs_golden = route.components.iter().any(|c| c.wants(FactClass::Crop));

    let shell_png = if needs_shell {
        let path = config.renders_dir.join(format!("{}.png", route.shell_slug));
        Some(Raster::load(&path)?)
    } else {
        None
    };
    let golden_png = if needs_golden {
        let rel = route.golden_png.as_ref().ok_or_else(|| {
            format!(
                "route {} enables a crop comparator but has no golden_png in the mapping",
                route.route
            )
        })?;
        Some(Raster::load(&config.repo_root.join(rel))?)
    } else {
        None
    };

    let ctx = RouteCtx {
        route,
        index: &index,
        scale,
        shell: shell_png.as_ref(),
        golden: golden_png.as_ref(),
        out_dir: &config.out_dir,
    };
    for comp in &route.components {
        // A mapping selector that does not resolve in our OWN vendored ground
        // truth is a config/drift error (the mapping-integrity test guards the
        // committed state; this guards a drifted vendor at runtime) — hard-fail,
        // never silently skip the component.
        let mockup = resolve_facts(&design, comp).ok_or_else(|| {
            format!(
                "{} {}: mapping selector `{}` not found in vendored design-facts {} — \
                 re-vendor the facts or fix the mapping",
                route.route,
                comp.label(),
                comp.selector,
                route.design_facts
            )
        })?;
        let Some(node) = index.get(comp.node.as_str()) else {
            // A missing REQUIRED node is a gating divergence (a rename/removal
            // must fail --gate, not silently skip every comparator). A node
            // DECLARED optional in the mapping is the only non-gating carve-out.
            ledger.extend([absent_node_finding(route, comp)]);
            continue;
        };
        run_comparators(&ctx, comp, mockup, node, ledger)?;
    }
    Ok(())
}

/// The finding for a mapped scene node that is absent from the shell scene:
/// a gating divergence for a required node, a declared non-gating note for an
/// `optional` one.
fn absent_node_finding(route: &RouteMap, comp: &Component) -> Finding {
    let severity = if comp.optional {
        Severity::Info
    } else {
        Severity::Divergence
    };
    let detail = if comp.optional {
        "declared-optional mapped scene node absent (non-gating carve-out)"
    } else {
        "required mapped scene node not found in the shell scene"
    };
    Finding {
        route: route.route.clone(),
        selector: comp.label(),
        node: comp.node.clone(),
        fact_class: "mapping".to_string(),
        expected: format!("scene node `{}`", comp.node),
        shipped: "absent".to_string(),
        delta: detail.to_string(),
        severity,
        artifact: None,
        accepted: false,
    }
}

/// Shared inputs for one route's comparators.
struct RouteCtx<'a> {
    route: &'a RouteMap,
    index: &'a BTreeMap<&'a str, &'a SceneNode>,
    scale: f64,
    shell: Option<&'a Raster>,
    golden: Option<&'a Raster>,
    out_dir: &'a Path,
}

/// Run every opted-in comparator for one component.
fn run_comparators(
    ctx: &RouteCtx,
    comp: &Component,
    mockup: &Facts,
    node: &SceneNode,
    ledger: &mut Ledger,
) -> Result<(), String> {
    let route = &ctx.route.route;
    if comp.wants(FactClass::Geometry) {
        ledger.extend(compare::geometry(route, comp, &mockup.bbox, &node.bounds));
    }
    if comp.wants(FactClass::FontSize) {
        let px = mockup.font.size_px().ok_or_else(|| {
            format!(
                "{route} {}: font-size enabled but mockup font size {:?} is unparseable",
                comp.label(),
                mockup.font.size
            )
        })?;
        ledger.extend(compare::font_size(route, comp, px, node, ctx.scale));
    }
    if comp.wants(FactClass::Decoration) {
        let has_underline = comp
            .underline_node
            .as_deref()
            .is_some_and(|id| ctx.index.contains_key(id));
        ledger.extend(compare::decoration(route, comp, mockup, has_underline));
    }
    if comp.wants(FactClass::Color) {
        // `needs_shell` guaranteed this raster loaded (or we already errored).
        let shell = ctx
            .shell
            .ok_or("internal: shell raster not loaded for an enabled color comparator")?;
        ledger.extend(compare::color(
            route,
            comp,
            &mockup.color,
            shell,
            &mockup.bbox,
        ));
    }
    if comp.wants(FactClass::Crop) {
        let golden = ctx
            .golden
            .ok_or("internal: golden raster not loaded for an enabled crop comparator")?;
        let shell = ctx
            .shell
            .ok_or("internal: shell raster not loaded for an enabled crop comparator")?;
        if let Some(f) = compare::crop(route, comp, &mockup.bbox, golden, shell, ctx.out_dir)? {
            ledger.extend([f]);
        }
    }
    Ok(())
}

fn resolve_facts<'a>(design: &'a DesignFacts, comp: &Component) -> Option<&'a Facts> {
    match comp.index {
        Some(i) => design.instance(&comp.selector, i),
        None => design.unique(&comp.selector),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(json: &str) -> Component {
        serde_json::from_str(json).expect("component fixture")
    }

    fn route() -> RouteMap {
        serde_json::from_str(
            r#"{"route":"r","design_facts":"d.json","shell_slug":"s","components":[]}"#,
        )
        .expect("route fixture")
    }

    #[test]
    fn a_missing_required_node_is_a_gating_divergence() {
        let comp = component(r#"{"selector":".x","node":"n","classes":["geometry"]}"#);
        let f = absent_node_finding(&route(), &comp);
        assert_eq!(f.severity, Severity::Divergence);
        assert!(f.is_gating(), "a required node's absence must fail --gate");
    }

    #[test]
    fn a_missing_optional_node_is_a_declared_noninfo_carveout() {
        let comp =
            component(r#"{"selector":".x","node":"n","classes":["geometry"],"optional":true}"#);
        let f = absent_node_finding(&route(), &comp);
        assert_eq!(f.severity, Severity::Info);
        assert!(
            !f.is_gating(),
            "a declared-optional node's absence is non-gating"
        );
    }
}
