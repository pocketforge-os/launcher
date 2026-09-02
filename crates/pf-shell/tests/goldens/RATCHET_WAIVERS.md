# Mockup ratchet waivers

Threshold decreases require an exact waiver added in the same change. Waivers document
intentional exceptions; they do not authorize later decreases. Removing or renaming a
ratchet entry requires a waiver whose new value is `REMOVED` and whose reason names the
PR in the same change (a rename is a removal plus an unratcheted addition).

RATCHET-WAIVER: library 0.976464625 -> 0.976248729121278 — Commit 9bd715f fixed text truncation, which legitimately painted more content and lowered whole-frame similarity; coordinator retroactively approved 2026-08-31.
RATCHET-WAIVER: detail 0.940459872288036 -> 0.938965574221587 — Round 7 gives the Details fixture a fictional two-line description so its content shape exercises the described-entry path and better matches the mockup; the remaining delta reflects the fixture's one variant and different title/content, establishing the exact rendered baseline for coordinator ratification in pocketforge-os/launcher#77.
