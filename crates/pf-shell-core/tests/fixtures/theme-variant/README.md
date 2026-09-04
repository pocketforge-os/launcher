# theme-variant — a deliberately-divergent theme test double

A copy of the flagship Quiet Console theme package (`pf_theme`'s
`vendor/package/`) with **one** token intentionally changed:

- `bases.light` (Day) `--color-text-secondary`: `#57503f` → `#0d1b2a`

It exists so `glyph_ink_follows_the_supplied_theme_not_a_static_flagship`
(in `pf-shell-core`'s unit tests) can boot the shell with a theme whose token
values differ from flagship and prove the drawn chrome glyphs resolve their ink
against the **supplied** theme, not a process-static `pf_theme::flagship()`.

It is loaded through `pf_theme::load`, so its contrast / asset-hash / scrim gates
stay enforced — the patched token is near-black and keeps Day's ≥4.5:1 text
contrast. This is a test double, not a shipped theme; it does not track flagship.
