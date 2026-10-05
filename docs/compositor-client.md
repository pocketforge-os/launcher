# pf-shell compositor client

This change makes the `pf-shell --compositor` profile an ordinary Wayland
client of the canonical compositor session. It consumes
`/run/pocketforge/session/environment` (or the explicit test/fixture override
passed with `--session-environment`) and requires an absolute
`WAYLAND_DISPLAY`. It does not select, start, or otherwise own the compositor.

The publication is expected to be atomically replaced by the session owner only
after the socket accepts connections. The client records a SHA-256 identity of
the exact publication bytes. On a compositor loss it rereads the publication;
an unchanged publication is treated as stale and fails closed, while a changed
publication is the only input accepted for reconnect. Missing, malformed, or
relative-socket publications fail before a Wayland connection is attempted.

The logical shell surface is 1280x720 landscape. In compositor mode the client
submits a 720x1280 `wl_shm` buffer, applies `wl_surface.set_buffer_transform`
with the 90-degree transform, rotates the rendered pixels, and maps damage into
the native portrait buffer. The existing `--wayland` path remains normal
landscape Wayland, and `--fbdev` remains the legacy framebuffer path.

Compositor mode rejects `--device`, `--input`, `--rotate`, and `--sim-frame`.
Thus this client path never opens `/dev/dri`, DRM master, `/dev/fb0`, or raw
input. Keyboard events come from the compositor's Wayland seat; no private
compositor protocol is used.

## G2.2 integration assumptions

These are client-side assumptions, not a second session authority:

1. The system compositor owns `/run/pocketforge/session` and atomically
   publishes the environment after `/run/pocketforge/session/wayland-0` is
   ready.
2. `WAYLAND_DISPLAY` is an absolute socket path; clients do not reconstruct a
   runtime-directory convention or select a compositor implementation.
3. Each compositor lifecycle changes the published bytes or exposes an
   equivalent canonical generation field. If the generation key is standardized
   later, the client should use that field while retaining the same
   fail-closed semantics.
4. Reconnect is client recovery after `SurfaceLost`; readiness, lifecycle, and
   publication authority remain producer responsibilities.
5. Session switching preserves the existing shell/session socket contract and
   does not require Gamescope or Weston private APIs.

The cited recovery inputs were the compositor brief
`/home/matt/recovery/gpu14/compositor-lane-g1.md` and the audited feasibility
report `/home/matt/recovery/gamescope-feasibility/report.md` (SHA-256
`a811b9ff2da3c1c7c1caf1ae677b64ec24143df01209779e2b6a05caf1fd2434`). No
physical-device, A133 plane, direct-scanout, or Gamescope runtime claim is made
by this offline client slice.
