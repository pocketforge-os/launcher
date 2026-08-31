# Pre-Taffy rendering baseline

Launcher commit: `00a6a8fa737829075a06e788114dfa6f5a2dd577` (the pre-change branch head).
Design contract: `pocketforge-os/design@999b5c991ee407b491bd279e1d3f68a8001c7f41`.

The `small` (960×540), `standard` (1280×720), `portrait` (720×1280), and
`large` (1920×1080) directories contain Home, Library, Settings, and Quick-overlay
semantic snapshots plus JSON records at 100%, 150%, and 200% text scale. JSON records
contain the raw RGBA SHA-256, raster guard verdict, damage rectangle, render notes, and
measured frame presentation time. Timings are observations, not performance thresholds.

Exact capture command (run from the repository root):

```sh
for surface in small:960x540 standard:1280x720 portrait:720x1280 large:1920x1080; do
  name=${surface%%:*}; size=${surface#*:}
  for scale in 100 150 200; do
    PF_BASELINE_RECORD_ONLY=1 PF_RASTER_INK_GUARD=1 cargo run --locked -p pf-shell -- \
      --taffy-baseline --surface "$size" --text-scale "$scale" \
      --out "docs/taffy-baseline/$name/$scale"
  done
done
```

Failures are intentionally retained in each JSON `raster_guard` value and summarized in
`KNOWN_FAILURES.md`; this spike does not alter rendering or normalize baseline failures.
