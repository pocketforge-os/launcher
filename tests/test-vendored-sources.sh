#!/usr/bin/env bash
# Verify every vendored host-route file matches the sha256 pinned in its
# .SOURCE (tsp-ihne1.56.5 port). Red control: a tampered copy must fail.
# Positive control: the committed tree passes. Both run in this one invocation.
set -euo pipefail

self="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
repo="${PF_VENDORED_SOURCES_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$repo"

fail=0

check_line() { # <file> <expected-sha256>
  local actual
  actual="$(sha256sum "$1" | cut -d' ' -f1)"
  if [ "$actual" != "$2" ]; then
    echo "vendored_sources=FAIL path=$1 pinned=$2 actual=$actual"
    fail=1
  else
    echo "vendored_sources=PASS path=$1"
  fi
}

# host-route-policy-v1.SOURCE: fixture=<path> sha256=<hex> lines.
policy_source="tests/fixtures/host-route-policy-v1.SOURCE"
if [ ! -f "$policy_source" ]; then
  echo "vendored_sources=FAIL path=$policy_source reason=missing"
  exit 1
fi
seen=0
# tests/uinput_inventory.py carries a documented launcher adaptation (the
# canonical runtime#150 seed_creator_self_test indexes occurrences()[0], which
# raises IndexError on this repo's zero-occurrence creator tree; see the comment
# inside the script and the port PR). The .SOURCE line still pins the CANONICAL
# sha256; the adapted file's sha256 is pinned here so tampering still fails.
canonical_inventory_sha256="2ab81480358a83ed8f521205539be8023c6a94e3436eb5ac6137eed28a9afca1"
adapted_inventory_sha256="092663f6dce2ba23358ecb05780e396104e27997da73fa00f0a772de62ee2e08"
while read -r path sha; do
  [ -n "$path" ] || continue
  if [ ! -f "$path" ]; then
    echo "vendored_sources=FAIL path=$path reason=missing"
    fail=1
    continue
  fi
  if [ "$path" = "tests/uinput_inventory.py" ]; then
    if [ "$sha" != "$canonical_inventory_sha256" ]; then
      echo "vendored_sources=FAIL path=$path reason=source-pin-drift pinned=$sha canonical=$canonical_inventory_sha256"
      fail=1
      continue
    fi
    actual="$(sha256sum "$path" | cut -d' ' -f1)"
    if [ "$actual" != "$adapted_inventory_sha256" ]; then
      echo "vendored_sources=FAIL path=$path reason=adapted-sha-drift pinned=$adapted_inventory_sha256 actual=$actual"
      fail=1
    else
      echo "vendored_sources=PASS path=$path (launcher adaptation; canonical_sha256=$canonical_inventory_sha256)"
    fi
    seen=$((seen + 1))
    continue
  fi
  check_line "$path" "$sha"
  seen=$((seen + 1))
done < <(sed -n 's/^fixture=\([^ ]*\) sha256=\([0-9a-f]\{64\}\)$/\1 \2/p' "$policy_source")
if [ "$seen" -lt 1 ]; then
  echo "vendored_sources=FAIL path=$policy_source reason=no-fixture-lines-parsed"
  fail=1
fi

# input-runner-host-map-v1.SOURCE: fixture_sha256=<hex> for the map json.
map_source="tests/fixtures/input-runner-host-map-v1.SOURCE"
map_json="tests/fixtures/input-runner-host-map-v1.json"
if [ ! -f "$map_source" ] || [ ! -f "$map_json" ]; then
  echo "vendored_sources=FAIL path=$map_source reason=missing"
  fail=1
else
  map_sha="$(sed -n 's/^fixture_sha256=\([0-9a-f]\{64\}\)$/\1/p' "$map_source")"
  if [ -z "$map_sha" ]; then
    echo "vendored_sources=FAIL path=$map_source reason=no-fixture_sha256"
    fail=1
  else
    check_line "$map_json" "$map_sha"
  fi
fi

# Negative control: tamper with a scratch copy of the tree layout and prove the
# checker rejects it (a mutated vendored file must fail, not warn).
if [ "${PF_VENDORED_SOURCES_NEGATIVE_CONTROL:-1}" != "1" ]; then
  if [ "$fail" -ne 0 ]; then
    echo "vendored_sources=FAIL_COUNT"
    exit 1
  fi
  echo "vendored_sources=PASS all pinned sha256s match"
  exit 0
fi
tmp="$(mktemp -d "${RUNNER_TEMP:-/tmp}/pf-vendored-sources.XXXXXX")"
cleanup() { find "$tmp" -mindepth 1 -delete; rmdir "$tmp"; }
trap cleanup EXIT
mkdir -p "$tmp/tests/fixtures"
cp -a tests/uinput_inventory.py "$tmp/tests/"
cp -a tests/fixtures/. "$tmp/tests/fixtures/"
printf '\n# tamper\n' >>"$tmp/tests/fixtures/host-route-policy-v1.json"
if PF_VENDORED_SOURCES_ROOT="$tmp" PF_VENDORED_SOURCES_NEGATIVE_CONTROL=0 bash "$self" >/dev/null 2>&1; then
  echo "vendored_sources=FAIL reason=negative-control-passed (tampered tree accepted)"
  fail=1
else
  echo "vendored_sources=PASS negative-control (tampered tree rejected)"
fi

if [ "$fail" -ne 0 ]; then
  echo "vendored_sources=FAIL_COUNT"
  exit 1
fi
echo "vendored_sources=PASS all pinned sha256s match"
