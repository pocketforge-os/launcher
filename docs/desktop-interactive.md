# Desktop interactive shell

This lane is a desktop-only interaction check. It exercises the real session authority with its
`desktop-sim` command preset; it does not run a real game, test audio, or prove device behavior.

## Build and run

On Ubuntu/Debian, install the Wayland keyboard dependency, then build both programs from the
launcher repository root. The runtime revision is read from this workspace rather than copied by
hand:

```sh
sudo apt-get update
sudo apt-get install -y libxkbcommon-dev
runtime_rev=$(sed -n 's/.*runtime\.git", rev = "\([0-9a-f][0-9a-f]*\)".*/\1/p' Cargo.toml | head -n 1)
cargo install --locked --git https://github.com/pocketforge-os/runtime.git --rev "$runtime_rev" \
  --bin pf-session-authorityd pf-session-authority
cargo build -p pf-shell --features wayland
```

Use a fresh temporary directory and start the authority in one terminal:

```sh
desktop_state=$(mktemp -d "${TMPDIR:-/tmp}/pf-desktop.XXXXXX")
printf 'desktop_state=%s\n' "$desktop_state"
pf-session-authorityd --command-preset desktop-sim \
  --state-dir "$desktop_state/authority" \
  --socket "$desktop_state/session-authority.sock"
```

In a second terminal, reuse the printed `desktop_state` value (or paste its path), then start the
desktop supervisor:

```sh
desktop_state=/tmp/pf-desktop.REPLACE_WITH_YOURS
scripts/desktop-sim-soak.sh --supervise "$desktop_state"
```

Keep this process running for the whole interactive session; Ctrl-C stops it cleanly. It stands in
for the real device's systemd supervisor: it observes the stub session starting and stopping, then
reports those observations to the authority so its launch and restoration state machines can
advance.

In a third terminal, reuse the same `desktop_state` value, then start the shell:

```sh
desktop_state=/tmp/pf-desktop.REPLACE_WITH_YOURS
target/debug/pf-shell --wayland \
  --state-dir "$desktop_state/shell" \
  --session-socket "$desktop_state/session-authority.sock"
```

The PocketForge home screen should appear in a Wayland window. Arrow keys move focus, Enter
activates, Escape or Backspace goes back, Tab or `F` opens Quick/toggles the favorite action for
the current context, and `S` is the safe-return binding. Closing the window quits `pf-shell`.

Launching a catalog title does **not** run that title. Under `desktop-sim`, the authority creates
an `authority/sessions/<session-id>.running` marker representing a stub foreground process. This
preset exists only to exercise session lifecycle wiring. During an active session, press `S` to
request graceful return. The stub session ends, the authority restores the shell, and the shell
returns to the foreground after consuming its `Returned` receipt. You can launch and return again
while the supervisor remains running. Then close the window and stop the supervisor and authority
with Ctrl-C. Remove the temporary directory after all three processes have exited:

```sh
find "$desktop_state" -depth -delete
```

## Automation seam

Interactive `--wayland` may take `--input /dev/input/eventN`; when present, evdev is the only
action source and every controller event passes through the real `pf-input-map`. Compositor
keyboard events are drained but ignored. Without `--input`, Wayland keyboard behavior is
unchanged. `--fbdev` continues to use evdev (default `/dev/input/event0`).

For simulator control, set `PF_SHELL_AUTOMATION=1` and pass `--automation-socket /absolute/path`
to either interactive mode. The flag is rejected unless that exact environment gate is set. The
Unix socket accepts newline-delimited JSON objects, keeps connections open, and emits one JSON
response line per command. Every response includes the current `frames` counter and shell
`revision`. The shell refuses a non-socket path or a socket with a live listener; it reclaims a
stale socket.

- `{"op":"ping"}` reports `ok`, the current `route`, and `input_source` (`evdev` or
  `wayland-keyboard`).
- `{"op":"scene"}` reports surface `metrics`, `text_scale`, `high_contrast`, `search_query`,
  `search_result_ids`, focus IDs, and the complete recursive semantic node tree. A foreground
  session is represented by `"scene":null`.
- `{"op":"capture","path":"/absolute/frame.png"}` writes the latest composed frame at exactly
  that path and reports its SHA-256; it reports `error:"no_frame"` before any frame exists.
- `{"op":"text","value":"ridge"}` replaces the Search query and schedules a redraw.
- `{"op":"wait_idle","quiet_ms":150,"timeout_ms":5000}` waits asynchronously until the input
  quiet period has elapsed and the latest revision has been presented. Both durations are
  optional and use the shown defaults; timeout reports `ok:false,error:"timeout"`.

Malformed JSON, unknown operations, and absent required fields report `invalid_json`,
`unknown_op`, or `missing_field:<name>`. For deterministic battery/controller status, set
`PF_POWER_SUPPLY_ROOT` to a fake sysfs tree containing `<root>/<name>/{type,capacity,status,scope}`;
in interactive modes this override takes precedence over `--device-fixtures`.

## Troubleshooting

- `xkbcommon` build or linker errors: install the `libxkbcommon-dev` package and rebuild.
- `WAYLAND_DISPLAY` is unset or connection fails: log into a Wayland desktop session and run the
  shell from a terminal in that session. This mode does not create a compositor.
- `another authority is already listening` or a stale socket remains: first confirm no
  `pf-session-authorityd` is using that exact path. Then choose a new `mktemp` directory; do not
  remove a socket owned by a live daemon.
