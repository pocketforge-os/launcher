# PocketForge launcher

This workspace contains `pf-catalog`, the launcher's source-owned installed-application
catalog foundation. It validates canonical `app.toml` descriptors, preserves invalid
descriptors as typed provider results, builds immutable content-revisioned snapshots,
and stores favorites as a separate atomic PocketForge projection.

The model deliberately contains only a typed `AppManifestRef`; raw executable and
health commands remain descriptor/trust-authority inputs and are never catalog data.
The public revision, provider-result, and favorite-commit semantics mirror the merged
`pocketforge-os/runtime` `pf-ports::CatalogPort` contract (runtime#46) without adding a
runtime dependency. A later adapter can narrow the richer catalog items into that port.

Run the complete local gate with:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```
