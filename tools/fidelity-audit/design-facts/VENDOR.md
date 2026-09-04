# Vendored design-facts (ground truth) — provenance

These `quiet-console/<route>.json` files are a **committed snapshot** of the
per-route structural ground truth generated in the `design` repo by
`tools/design-facts/generate.py` (headless Chrome, 1280×720, `document.fonts.ready`).

The launcher CI checkout cannot reach the sibling `design` repo at build/test time,
so the audit consumes a vendored copy. This mirrors the design repo's own contract:
**facts are generated, never hand-edited; regeneration is a reviewed change.**

## Source

- Repo: `github.com/pocketforge-os/design`
- Commit: `7ae5dc232b01da99a84db536b5710b92af3818b5` (design#8 — `tsp-op5a.392`,
  "Added the design-facts generator and committed per-component facts")
- Source paths: `design-facts/quiet-console/<route>.json`

## Vendored routes

Only the routes the launcher audit maps today are vendored (the report matrix in
`../mapping/mapping.json`): `home`, `library`, `detail`, `settings`, `high-contrast`.
Add a route here by copying its `design-facts/quiet-console/<route>.json` from the
pinned design commit above and extending the mapping.

## Regeneration

To refresh against a newer design commit (a reviewed PR), re-copy the files and bump
the commit sha above:

```bash
DESIGN=<path to a design checkout at the intended commit>
for r in home library detail settings high-contrast; do
  cp "$DESIGN/design-facts/quiet-console/$r.json" quiet-console/$r.json
done
```

The `header.generator_version` inside each file must match what the audit's parser
expects; a generator-schema bump is a coordinated change across both repos.
