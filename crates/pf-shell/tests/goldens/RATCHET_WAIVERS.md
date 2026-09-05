# Mockup ratchet waivers

Threshold decreases require an exact waiver added in the same change. Waivers document
intentional exceptions; they do not authorize later decreases. Removing or renaming a
ratchet entry requires a waiver whose new value is `REMOVED` and whose reason names the
PR in the same change (a rename is a removal plus an unratcheted addition).

RATCHET-WAIVER: library 0.976464625 -> 0.976248729121278 — Commit 9bd715f fixed text truncation, which legitimately painted more content and lowered whole-frame similarity; coordinator retroactively approved 2026-08-31.
RATCHET-WAIVER: detail 0.940459872288036 -> 0.938965574221587 — Round 7 gives the Details fixture a fictional two-line description so its content shape exercises the described-entry path and better matches the mockup; the remaining delta reflects the fixture's one variant and different title/content, establishing the exact rendered baseline for coordinator ratification in pocketforge-os/launcher#77.
RATCHET-WAIVER: settings 0.969325 -> 0.969316 — Fidelity E (pocketforge-os/launcher#96) wires settings-section-title from Body 15px/500 to its correct H1 22px/700 role (.set-title binds --type-h1); the larger route-level title shifts whole-frame perceptual weighting by a sub-perceptual 9.4e-6, while the row name adopts Label 14/600 because the seven-role vocabulary cannot express the mockup's 15/700 name pair without editing tokens (out of scope for role wiring). Hierarchy restoration is verified by the new fidelity_e_type_role_hierarchy_is_wired_per_screen guard; detail rises 0.940679 -> 0.942868 in the same change.
