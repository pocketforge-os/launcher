//! The committed mapping must reference real vendored design-facts: every route
//! points at a present facts file, and every component selector resolves in it.
//! This guards the mapping table against selector typos in CI without a render.

use std::path::PathBuf;

use fidelity_audit::facts::DesignFacts;
use fidelity_audit::mapping::Mapping;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn mapping_loads_and_every_selector_resolves_in_vendored_facts() {
    let dir = crate_dir();
    let mapping = Mapping::load(&dir.join("mapping/mapping.json")).expect("mapping loads");
    assert!(!mapping.routes.is_empty(), "mapping has routes");

    let mut problems = Vec::new();
    for route in &mapping.routes {
        let facts_path = dir.join("design-facts").join(&route.design_facts);
        let facts = match DesignFacts::load(&facts_path) {
            Ok(f) => f,
            Err(e) => {
                problems.push(format!("{}: {e}", route.route));
                continue;
            }
        };
        for comp in &route.components {
            let resolved = match comp.index {
                Some(i) => facts.instance(&comp.selector, i).is_some(),
                None => facts.unique(&comp.selector).is_some(),
            };
            if !resolved {
                problems.push(format!(
                    "{}: selector `{}` (node `{}`) not found in {}",
                    route.route,
                    comp.label(),
                    comp.node,
                    route.design_facts
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "mapping problems:\n{}",
        problems.join("\n")
    );
}

#[test]
fn baseline_loads() {
    // The frozen baseline must parse (report mode reads it too).
    let dir = crate_dir();
    fidelity_audit::baseline::Baseline::load(&dir.join("baseline/accepted.json"))
        .expect("baseline loads");
}
