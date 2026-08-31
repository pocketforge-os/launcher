# Quiet Console design source

`tokens.css` and `shell.css` are exact copies of
`directions/quiet-console/{tokens,shell}.css` from
`pocketforge-os/design` commit
`999b5c991ee407b491bd279e1d3f68a8001c7f41`.

Refresh both files from that repository, update the commit above and the generator's
generated header, then run:

```console
cargo run -p design-token-codegen
cargo run -p design-token-codegen -- --check
```
