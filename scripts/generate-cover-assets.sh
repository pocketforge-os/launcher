#!/usr/bin/env bash
set -euo pipefail

# Provenance: pocketforge-os/design commit 999b5c991ee407b491bd279e1d3f68a8001c7f41
# Source: directions/quiet-console/assets/covers/*.svg
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
for cover in ridgeline hollow-tides sunwake moth-and-lantern bellwether torchbug northlight petrichor; do
  source="$work_dir/design/directions/quiet-console/assets/covers/$cover.svg"
  resvg --width 600 --height 800 "$source" "$output_dir/$cover.png"
done
