#!/usr/bin/env bash
set -euo pipefail

# Provenance: pocketforge-os/design commit 999b5c991ee407b491bd279e1d3f68a8001c7f41
# Source: directions/quiet-console/assets/covers/*.svg
# hollow-tides.svg intentionally paints a filled 6 px center circle plus a 12 px teal
# ring at (150, 210); the varying center mark in scaled captures is source artwork.
# resvg is version-pinned and installed locked so reruns do not follow dependency drift.
design_sha=999b5c991ee407b491bd279e1d3f68a8001c7f41
resvg_version=0.48.1
repo_root=$(git rev-parse --show-toplevel)
output_dir="$repo_root/crates/pf-shell/fixtures/art"
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

if ! command -v resvg >/dev/null || [[ "$(resvg --version)" != "resvg $resvg_version" ]]; then
  cargo install --locked --version "$resvg_version" resvg
fi

mkdir -p "$output_dir"
git clone --filter=blob:none --no-checkout https://github.com/pocketforge-os/design.git "$work_dir/design"
git -C "$work_dir/design" checkout --detach "$design_sha"
for cover in ridgeline hollow-tides sunwake moth-and-lantern bellwether torchbug northlight petrichor \
  lumen-vale redshift-alley quiet-machines low-orbit paper-armada vega-crossing iron-meridian \
  signal-decay milewide orchard-of-glass cinder-loop halfmoon-harbor fern-and-fathom; do
  source="$work_dir/design/directions/quiet-console/assets/covers/$cover.svg"
  resvg --width 600 --height 800 "$source" "$output_dir/$cover.png"
done
