# Mockup ratchet waivers

Threshold decreases require an exact waiver added in the same change. Waivers document
intentional exceptions; they do not authorize later decreases. Removing or renaming a
ratchet entry requires a waiver whose new value is `REMOVED` and whose reason names the
PR in the same change (a rename is a removal plus an unratcheted addition).

RATCHET-WAIVER: library 0.976464625 -> 0.976248729121278 — Commit 9bd715f fixed text truncation, which legitimately painted more content and lowered whole-frame similarity; coordinator retroactively approved 2026-08-31.
RATCHET-WAIVER: detail 0.940459872288036 -> 0.939874452500908 — The Details fixture has one variant and different title content while the design mockup has two variants; the CSS-exact 96px wrap anchor, 48px margins and gap, and 320x428 cover establish this honest baseline for coordinator ratification in pocketforge-os/launcher#77.
RATCHET-WAIVER: detail 0.939874452500908 -> 0.937870503699165 — Round 6 intentionally collapses the empty description slot in the one-variant Details fixture, moving WAYS TO PLAY and its controls upward; the changed pixels establish the exact new rendered baseline for coordinator ratification in pocketforge-os/launcher#77.
