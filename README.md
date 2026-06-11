# PocketForge Launcher

Thin SDL3 + GLES2 app launcher for PocketForge. Gamepad-native, ~15 MB resident. Responsibilities:

1. **Supervisor**: `fork`/`exec` selected app, `waitpid`, redraw on return.
2. **App discovery**: scan `/opt/pocketforge/apps/*/app.toml`, render grid.
3. **Fb-ownership handoff**: close `/dev/fb0` before child app, reopen on return.
4. **Network gating**: wait for `wlan0` default route when `needs_network = true`.

Consumes `libsdl3-sunxifb`'s release artifact (`libSDL3-pocketforge.so.0`). Produces the `pocketforge-launcher` binary consumed by the image builder.

Populated in **Phase 2**. See the [pocketforge-os](https://github.com/pocketforge-os) org for the full repo set.
