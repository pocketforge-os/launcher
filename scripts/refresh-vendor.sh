#!/usr/bin/env bash
# Refresh the committed registry sources after an intentional Cargo.lock update.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

test -f Cargo.lock || { echo "refresh-vendor: Cargo.lock is missing" >&2; exit 1; }

# A dependency refresh normally starts with dirty Cargo manifests and Cargo.lock,
# and may be repeated with a partially refreshed vendor tree. Reject everything
# else so replacing vendor cannot accidentally hide unrelated in-progress work.
while IFS= read -r -d '' entry; do
  path="${entry:3}"
  case "$path" in
    Cargo.lock|Cargo.toml|*/Cargo.toml|vendor|vendor/*) ;;
    *)
      echo "refresh-vendor: unrelated dirty path: $path" >&2
      exit 1
      ;;
  esac
done < <(git status --porcelain=v1 -z --untracked-files=all)

tmp="$(mktemp -d "${TMPDIR:-/tmp}/pocketforge-vendor.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# Run outside the repository so Cargo does not discover .cargo/config.toml's
# deliberately offline crates-io replacement. Refresh is the sole networked
# operation; validation and all production builds below remain offline.
(
  cd "$tmp"
  CARGO_NET_OFFLINE=false cargo vendor --locked \
    --manifest-path "$root/Cargo.toml" "$tmp/vendor" >"$tmp/vendor-config"
)

# Preserve Cargo's complete generated replacement list, including every pinned
# Git source. Replace only cargo vendor's temporary absolute directory so the
# committed configuration remains relocatable.
awk '/^\[source\.crates-io\]/{copy=1} copy' "$tmp/vendor-config" |
  sed 's|^directory = ".*"$|directory = "vendor"|' >"$tmp/config.toml"
cat >>"$tmp/config.toml" <<'EOF'

[net]
offline = true
EOF
mv "$tmp/config.toml" .cargo/config.toml

rm -rf vendor
mv "$tmp/vendor" vendor

# pf-render's pinned Git source references assets outside its Cargo package.
# Bring those exact-revision files inside the vendored package so directory
# source replacement remains self-contained in a fresh checkout.
runtime_rev="$(sed -n 's/^rev = "\([0-9a-f]\{40\}\)"$/\1/p' .cargo/config.toml)"
test -n "$runtime_rev" || { echo "refresh-vendor: runtime revision is missing" >&2; exit 1; }
git clone --quiet --filter=blob:none --no-checkout \
  https://github.com/pocketforge-os/runtime.git "$tmp/runtime"
git -C "$tmp/runtime" checkout --quiet "$runtime_rev" -- \
  spikes/render-text/fonts spikes/consent-ui/baseline/s01-initial.png
mkdir -p vendor/pf-render/upstream-assets
cp -R "$tmp/runtime/spikes/." vendor/pf-render/upstream-assets/
sed -i 's|\.\./\.\./\.\./spikes/|../upstream-assets/|g' vendor/pf-render/src/lib.rs
python3 - vendor/pf-render <<'PY'
import hashlib
import json
import pathlib
import sys

package = pathlib.Path(sys.argv[1])
checksum_path = package / ".cargo-checksum.json"
checksum = json.loads(checksum_path.read_text())
checksum["files"] = {
    str(path.relative_to(package)): hashlib.sha256(path.read_bytes()).hexdigest()
    for path in sorted(package.rglob("*"))
    if path.is_file() and path != checksum_path
}
checksum_path.write_text(json.dumps(checksum, separators=(",", ":")))
PY

lock_sha="$(sha256sum Cargo.lock | cut -d' ' -f1)"
cargo_version="$(cargo -V | tr -s ' ')"
package_count="$(find vendor -mindepth 2 -maxdepth 2 -name .cargo-checksum.json | wc -l | tr -d ' ')"
{
  printf 'cargo_lock_sha256=%s\n' "$lock_sha"
  printf 'cargo_version=%s\n' "$cargo_version"
  printf 'vendored_packages=%s\n' "$package_count"
} > vendor/.pocketforge-vendor-lock

cargo metadata --offline --locked --format-version 1 >/dev/null
cargo build --offline --locked --workspace
cargo test --offline --locked --workspace --no-fail-fast
echo "refresh-vendor: refreshed $package_count packages"
