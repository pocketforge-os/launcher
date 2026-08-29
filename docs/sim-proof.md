# F09a simulator proof

The shell has no device dependency. Build the ARM64 binary using the project cross-build lane,
then use S1's run-app path and offscreen shot capture:

```sh
cargo build --release --target aarch64-unknown-linux-gnu -p pf-shell
pf-sim run-app --binary target/aarch64-unknown-linux-gnu/release/pf-shell -- --fbdev --device /dev/fb0
pf-sim --shot evidence/sim/skin-window.png
```

For deterministic CI evidence (the same `pf-render` pipeline without the simulator skin):

```sh
cargo run -p pf-shell -- --offscreen --out evidence/offscreen
PF_REDUCE_MOTION=1 cargo run -p pf-shell -- --offscreen --out evidence/reduced-motion
```

The committed boot frame is the 1280×720 fidelity artifact. The renderer's embedded fonts and
rectangular semantic-node treatment differ from the design raster, while the persistent frame,
large Home hero, READY NOW hierarchy, card shelf, single lamplight focus, honest stub navigation,
and bottom prompt rail follow the specified layout. A simulator skin capture must be produced by
the PR coordinator's S1 lane; this execution worker is expressly barred from simulator/lab hosts.
