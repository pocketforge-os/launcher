# `fidelity-audit` — per-component mockup-vs-render fidelity ledger

Parent bead `tsp-op5a.389`. Whole-route similarity scoring (`pf-shell`'s
`mockup_diff`) is blind to small local divergences — a mis-aligned WiFi icon, a
focus treatment rendered as a thin partial underline instead of the mockup's
treatment, text a size too small. Every one of those shipped in the Quiet Console
restyle while the route scorer stayed green. This tool compares the shell to the
design mockups **per component** and enumerates each divergence as a structured
ledger row.

## What it is

A pure **artifact consumer** — it links no `pf-shell` internals, so it is
decoupled from scene-construction code and never touches pixels. It reads:

- **Ground truth** — the committed [`design-facts/`](design-facts/) (a vendored
  snapshot of the `design` repo generator output; provenance in
  [`design-facts/VENDOR.md`](design-facts/VENDOR.md)).
- **Shell facts** — `<slug>.semantic.txt`, the scene snapshot emitted by
  `pf-shell --offscreen --out <dir>` (node id, role, post-layout bounds, state).
  Read as a text artifact so the audit links no shell internals.
- **Renders** — the shell `<slug>.png` and the approved golden
  `crates/pf-shell/tests/goldens/<screen>.png`, for the perceptual crop diff.
- **Correspondence** — the committed [`mapping/mapping.json`](mapping/mapping.json)
  (mockup selector ↔ shell scene node id, seeded from the design repo
  `CONFORMANCE-CHECKLIST.md` §1–2), which selects the comparators per component.

## The three layers (bead architecture)

1. **Structural facts diff (deterministic).** Per mapped component:
   - `geometry` — node bounds vs mockup bbox, ±1px at 100%. A text node rendered
     at the wrong size shifts its bounds, so this also surfaces the "text slightly
     smaller than design" class today.
   - `font-size` — the node's `type_role` base size (× text scale) vs the mockup's
     computed size, exact (±0.5px); a divergence means the wrong type role was
     assigned. Base sizes are cited from
     `crates/pf-shell-core/src/design_generated.rs`. **Implemented and unit-tested,
     but not wired in the seed mapping:** the offscreen `<slug>.semantic.txt`
     snapshot does not carry `type_role`, so this comparator activates once a
     richer scene dump exposes it (a small follow-up — a `pf-shell` scene-JSON
     emit or the automation channel). Geometry covers the size divergence in the
     interim.
   - `decoration` — structural underline treatment: does the shell paint an
     underline node where the mockup facts show none (or vice versa)? The
     "focus underline" class.
   - `color` — render-sampled dominant component color vs the mockup computed
     color (opt-in; disabled in the seed mapping pending crop-region refinement).
2. **Perceptual per-component crop diff** — `crop` crops the golden and the shell
   render to the component box and reports the mean-absolute-error, writing a
   delta PNG artifact. Catches treatment divergences geometry misses.
3. **Documented-baseline gate** — the ledger is emitted in **report mode**
   (non-gating) by default. [`baseline/accepted.json`](baseline/accepted.json) is
   the frozen, cited accepted-divergence list; in `--gate` mode any divergence not
   listed there fails. Triaging the first ledger, filing per-defect fix beads, and
   flipping the gate on is the **follow-up sweep bead**, not this one.

## Running

```bash
# Render + audit in one shot (build pf-shell first):
cargo build -p pf-shell --locked
cargo run -p fidelity-audit --locked -- \
  --shell-bin target/debug/pf-shell --format table

# Or audit a pre-produced offscreen output:
cargo run -p fidelity-audit --locked -- --renders-dir <offscreen-out>

# Single route, JSON ledger:
cargo run -p fidelity-audit --locked -- --shell-bin target/debug/pf-shell \
  --route quiet-console/home --format json
```

The ledger is written to `target/fidelity-audit/ledger.json`; crop delta artifacts
land beside it. Exit code: `0` report ok / gate passed, `1` gate failed, `2` usage
or input error. CI runs it non-gating (`ci.yml` "Fidelity-audit report").

## Extending

- **A new component** → add a `{selector, node, classes}` entry to the route in
  `mapping/mapping.json` (`index` for an `instances` selector). `cargo test -p
  fidelity-audit` fails if a selector does not resolve in the vendored facts.
- **A new route** → vendor its `design-facts/quiet-console/<route>.json`
  (see `design-facts/VENDOR.md`) and add a route block to the mapping.
- **A finer WiFi-icon / sys sub-component check** needs an isolated selector in the
  design-facts generator (a `design`-repo change) — a coordinated follow-up.
