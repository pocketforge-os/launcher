#!/bin/sh
set -eu

step="initialization"
work_dir=""
authority_pid=""

cleanup() {
    if [ -n "$authority_pid" ]; then
        kill "$authority_pid" 2>/dev/null || true
        wait "$authority_pid" 2>/dev/null || true
    fi
    if [ -n "$work_dir" ] && [ -d "$work_dir" ]; then
        find "$work_dir" -depth -delete
    fi
}

finish() {
    rc=$?
    cleanup
    if [ "$rc" -ne 0 ]; then
        printf 'FAIL step=%s rc=%s\n' "$step" "$rc" >&2
    fi
    exit "$rc"
}

trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
trap finish EXIT

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/pf-desktop-soak.XXXXXX")
runtime_rev=$(sed -n 's/.*runtime\.git", rev = "\([0-9a-f][0-9a-f]*\)".*/\1/p' "$repo_dir/Cargo.toml" | head -n 1)
if [ -z "$runtime_rev" ]; then
    step="read-runtime-pin"
    false
fi

step="build-real-authority"
cargo install --quiet --locked \
    --git https://github.com/pocketforge-os/runtime.git \
    --rev "$runtime_rev" --root "$work_dir/install" \
    --bin pf-session-authorityd pf-session-authority

step="build-pf-shell"
cargo build --quiet --locked --manifest-path "$repo_dir/Cargo.toml" -p pf-shell

state_dir="$work_dir/authority"
socket="$work_dir/session-authority.sock"
authority_log="$work_dir/authority.log"
shell_log="$work_dir/shell.log"
mkdir -p "$state_dir"

step="start-real-authority"
"$work_dir/install/bin/pf-session-authorityd" \
    --command-preset desktop-sim --state-dir "$state_dir" --socket "$socket" \
    >"$authority_log" 2>&1 &
authority_pid=$!
deadline=$(( $(date +%s) + 5 ))
while [ ! -S "$socket" ]; do
    if ! kill -0 "$authority_pid" 2>/dev/null; then
        sed -n '1,120p' "$authority_log" >&2
        false
    fi
    if [ "$(date +%s)" -ge "$deadline" ]; then
        false
    fi
    sleep 0.05
done
if [ "${PF_SOAK_KILL_AUTHORITY_AFTER_READY:-0}" = 1 ]; then
    kill "$authority_pid"
    wait "$authority_pid" 2>/dev/null || true
    authority_pid=""
fi

step="launch-marker-return-cycle"
timeout 20s "$repo_dir/target/debug/pf-shell" \
    --desktop-sim-script --session-socket "$socket" \
    --authority-state-dir "$state_dir" --state-dir "$work_dir/shell-state" \
    >"$shell_log" 2>&1 || {
        sed -n '1,160p' "$shell_log" >&2
        false
    }
cat "$shell_log"

step="assert-session-launched"
grep -q '^SOAK launched session_id=' "$shell_log"
step="assert-desktop-marker-observed"
grep -q '^SOAK marker=' "$shell_log"
step="assert-return-redraw-and-clean-state"
grep -q '^SOAK returned .* redraw_advanced=true state_clean=true$' "$shell_log"
grep -q '"phase":"Idle"' "$state_dir/authority.json"
test -f "$state_dir/shell-selected"
if find "$state_dir/sessions" -name '*.running' -print -quit 2>/dev/null | grep -q .; then
    false
fi

step="authority-still-alive"
kill -0 "$authority_pid"
printf 'PASS desktop-sim-soak runtime_rev=%s\n' "$runtime_rev"
