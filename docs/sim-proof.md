# F09a simulator proof

The shell has no device dependency. Build the ARM64 binary using the project cross-build lane,
then use the simulator's offscreen `run-app --shot` path. `--sim-frame` writes one deterministic
XRGB8888 frame to the simulator-owned framebuffer and does not open evdev:

```sh
cargo build --release --target aarch64-unknown-linux-gnu -p pf-shell
./sim run-app a133 target/aarch64-unknown-linux-gnu/release/pf-shell \
  --shot evidence/sim/skin-window.ppm -- --sim-frame
```

For deterministic CI evidence (the same `pf-render` pipeline without the simulator skin):

```sh
cargo run -p pf-shell -- --offscreen --out evidence/offscreen
PF_REDUCE_MOTION=1 cargo run -p pf-shell -- --offscreen --out evidence/reduced-motion
```

The committed boot frame is the 1280×720 fidelity artifact. The renderer uses its vendored
Manrope/Fraunces font assets and theme-owned style keys. The persistent 64 px status frame, centered
room strip, hero mirror, 158×210 deterministic Edition Plates, single focused card, and 60 px prompt
rail follow SPEC §2 and §4.1. The exact command above reproduces the committed simulator shot
at `evidence/sim/skin-window.ppm` (1480×640 Netpbm; SHA-256
`75d40411b6d0e1efc2b731dc14ce5c0af1a00c2d27e947b4dc813e12c7c198bf`).
