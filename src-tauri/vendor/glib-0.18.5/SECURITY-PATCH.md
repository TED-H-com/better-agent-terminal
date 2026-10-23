# GLib 0.18 security backport

Source: the unmodified glib 0.18.5 crate from crates.io, except for `src/variant_iter.rs`.
Crate SHA-256: `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.

This copy backports [gtk-rs-core#1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343)
for [GHSA-wrw7-89jp-8q8g](https://github.com/advisories/GHSA-wrw7-89jp-8q8g).
The C variadic function writes an output pointer, so `VariantStrIter::impl_get`
now declares the pointer mutable and passes `&mut p` instead of `&p`.

Tauri's GTK3 dependency graph requires glib 0.18. Keep its API and ABI intact.
Remove this patch when the desktop dependency graph supports glib 0.20 or newer.

Validation must include the optimized iterator tests, since debug-only and
headless builds do not exercise the original failure:

```sh
cargo test --manifest-path src-tauri/vendor/glib-0.18.5/Cargo.toml --release --lib variant_iter::tests
cargo check --manifest-path src-tauri/Cargo.toml --workspace --locked
```
