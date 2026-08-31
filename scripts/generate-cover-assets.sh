#!/usr/bin/env bash
set -euo pipefail

# Provenance: pocketforge-os/design commit 999b5c991ee407b491bd279e1d3f68a8001c7f41
# Source: directions/quiet-console/assets/covers/*.svg
# Plate source: directions/quiet-console/home.html, library.html, and tokens.css.
# The plate SVGs below preserve that markup's 300x400 motif coordinates, 0.5 motif
# opacity, and inset 8px/radius 6 frame while rendering at 316x420 (2x card art).
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

plate_svg() {
  local name=$1 background=$2 foreground=$3 motif=$4
  local source="$work_dir/$name.svg"
  {
    printf '<svg xmlns="http://www.w3.org/2000/svg" width="316" height="420" viewBox="0 0 300 400" preserveAspectRatio="xMidYMid slice">\n'
    # The scene's centered type stack is max(72, 88)=88 px wide and 56+8+24=88 px
    # tall; 16 px padding per side derives a 120x120 scene-pixel safe zone. Plates
    # render at 2x scene size, so a centered 230x230 viewBox zone clears >120x120.
    printf '  <defs><mask id="motif-safe-zone"><rect width="300" height="400" fill="white"/><rect x="35" y="85" width="230" height="230" fill="black"/></mask></defs>\n'
    printf '  <rect width="300" height="400" fill="%s"/>\n' "$background"
    printf '  <g color="%s" opacity="0.5" mask="url(#motif-safe-zone)">%s</g>\n' "$foreground" "$motif"
    printf '  <rect x="8" y="8" width="284" height="384" rx="6" fill="none" stroke="%s" stroke-width="1" opacity="0.5"/>\n' "$foreground"
    printf '</svg>\n'
  } >"$source"
  resvg --width 316 --height 420 "$source" "$output_dir/$name.png"
}

plate_svg plate-a '#23303048' '#9fc4bc' '<g fill="none" stroke="currentColor" stroke-width="4"><path d="M-20 380 H60 V320 H140 V260 H220 V200 H300 V140 H380"/><path d="M-20 320 H30 V260 H110 V200 H190 V140 H270 V80 H350"/><path d="M-60 260 H0 V200 H80 V140 H160 V80 H240 V20 H320"/></g>'
plate_svg plate-d '#2b333f48' '#93b1cd' '<g fill="none" stroke="currentColor" stroke-width="4"><path d="M-20 80 Q55 60 130 80 T280 80 T430 80"/><path d="M-20 140 Q55 120 130 140 T280 140 T430 140"/><path d="M-20 200 Q55 180 130 200 T280 200 T430 200"/><path d="M-20 260 Q55 240 130 260 T280 260 T430 260"/><path d="M-20 320 Q55 300 130 320 T280 320 T430 320"/></g>'
plate_svg plate-c '#38302348' '#cfb08a' '<g fill="currentColor"><circle cx="30" cy="40" r="4"/><circle cx="90" cy="40" r="4"/><circle cx="150" cy="40" r="4"/><circle cx="210" cy="40" r="4"/><circle cx="270" cy="40" r="4"/><circle cx="60" cy="100" r="4"/><circle cx="120" cy="100" r="4"/><circle cx="180" cy="100" r="4"/><circle cx="240" cy="100" r="4"/><circle cx="300" cy="100" r="4"/><circle cx="30" cy="160" r="4"/><circle cx="90" cy="160" r="4"/><circle cx="150" cy="160" r="4"/><circle cx="210" cy="160" r="4"/><circle cx="270" cy="160" r="4"/><circle cx="60" cy="220" r="4"/><circle cx="120" cy="220" r="4"/><circle cx="180" cy="220" r="4"/><circle cx="240" cy="220" r="4"/><circle cx="300" cy="220" r="4"/><circle cx="30" cy="280" r="4"/><circle cx="90" cy="280" r="4"/><circle cx="150" cy="280" r="4"/><circle cx="210" cy="280" r="4"/><circle cx="270" cy="280" r="4"/><circle cx="60" cy="340" r="4"/><circle cx="120" cy="340" r="4"/><circle cx="180" cy="340" r="4"/><circle cx="240" cy="340" r="4"/><circle cx="300" cy="340" r="4"/></g>'
