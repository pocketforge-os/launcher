#!/usr/bin/env bash
# Committed controls for vendor staleness and path-dependency closure.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/pocketforge-vendor-check.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/scripts" "$tmp/src" "$tmp/vendor/source" "$tmp/vendor/target/src"
cp "$root/scripts/check-vendor.sh" "$tmp/scripts/"
cat >"$tmp/Cargo.toml" <<'EOF'
[package]
name = "vendor-check-fixture"
version = "0.0.0"
edition = "2021"
EOF
printf 'pub fn fixture() {}\n' >"$tmp/src/lib.rs"
cat >"$tmp/vendor/source/Cargo.toml" <<'EOF'
[package]
name = "source"
version = "0.0.0"
edition = "2021"

[dev-dependencies.target]
path = "../target"
EOF
cat >"$tmp/vendor/target/Cargo.toml" <<'EOF'
[package]
name = "target"
version = "0.0.0"
edition = "2021"
EOF
printf 'pub fn present() {}\n' >"$tmp/vendor/target/src/lib.rs"
(
  cd "$tmp"
  cargo generate-lockfile --offline >/dev/null
)
printf 'cargo_lock_sha256=%s\n' "$(sha256sum "$tmp/Cargo.lock" | cut -d' ' -f1)" \
  >"$tmp/vendor/.pocketforge-vendor-lock"

"$tmp/scripts/check-vendor.sh" >"$tmp/output" 2>&1 || {
  cat "$tmp/output" >&2
  echo "test-check-vendor: resolvable path dependency unexpectedly failed" >&2
  exit 1
}
grep -Fq 'path dependencies resolve' "$tmp/output" || {
  cat "$tmp/output" >&2
  echo "test-check-vendor: positive control returned the wrong output" >&2
  exit 1
}
echo "test-check-vendor: resolvable path dependency accepted"

cat >>"$tmp/vendor/source/Cargo.toml" <<'EOF'

[dev-dependencies.missing]
path = "../missing"
EOF

if "$tmp/scripts/check-vendor.sh" >"$tmp/output" 2>&1; then
  echo "test-check-vendor: dangling path dependency unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'unresolved vendored path dependency' "$tmp/output" || {
  cat "$tmp/output" >&2
  echo "test-check-vendor: dangling path guard failed for the wrong reason" >&2
  exit 1
}
echo "test-check-vendor: dangling dev path dependency rejected"

sed -i '/\[dev-dependencies.missing\]/,$d' "$tmp/vendor/source/Cargo.toml"
printf '\n# synthetic lock drift\n' >>"$tmp/Cargo.lock"

if "$tmp/scripts/check-vendor.sh" >"$tmp/output" 2>&1; then
  echo "test-check-vendor: synthetic lock drift unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'Cargo.lock changed without a vendor refresh' "$tmp/output" || {
  cat "$tmp/output" >&2
  echo "test-check-vendor: guard failed for the wrong reason" >&2
  exit 1
}
echo "test-check-vendor: synthetic lock drift rejected"
