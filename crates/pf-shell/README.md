# pf-shell framebuffer orientation

`pf-shell --fbdev` always lays out a logical scene in the display's intended
orientation and converts it to native framebuffer coordinates only while
presenting. The scene-to-buffer rotation is selected once at startup in this
order:

1. `--rotate 0|90|180|270` (clockwise scene-to-buffer rotation),
2. the backing DRM connector's `panel orientation` property,
3. `/sys/class/graphics/fbcon/rotate`, and
4. framebuffer geometry (portrait native framebuffers default to 90 degrees
   clockwise; landscape framebuffers default to 0).

Unreadable or unsupported hints fall through to the next source. Startup logs
the native geometry, selected rotation, and source. No service flag is required
for the PocketForge panel: its DRM property is authoritative when available and
the portrait-geometry fallback matches the measured mounting otherwise.
