# Mockup ratchet waivers

Threshold decreases require an exact waiver added in the same change. Waivers document
intentional exceptions; they do not authorize later decreases. Removing or renaming a
ratchet entry requires a waiver whose new value is `REMOVED` and whose reason names the
PR in the same change (a rename is a removal plus an unratcheted addition).

RATCHET-WAIVER: library 0.976464625 -> 0.976248729121278 — Commit 9bd715f fixed text truncation, which legitimately painted more content and lowered whole-frame similarity; coordinator retroactively approved 2026-08-31.
