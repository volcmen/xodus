# Maintained dependency backports

These sources are committed so the release does not depend on a mutable fork or
an unpublished registry patch. Original license files remain alongside each crate.

- `xal`: AllanVester/xal-rs commit `231f7528cda1dee1aac09225d09fcd67d76980e0`,
  previously selected by this branch's Cargo patch. Retains caller-held proof keys.
  Changes: reqwest 0.13 / HTTP 1, OAuth2 4 with its default transport disabled,
  a bounded transport bridge (no redirects, 10-second connection timeout,
  30-second total timeout, 1 MiB response limit), and local regression tests.
  Cargo manifest normalized by `cargo vendor`; workspace membership is managed here.
- `glib-0.18`: crates.io glib 0.18.5, archive SHA-256
  `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.
  The only source change is the two-line mutable out-pointer backport in
  `src/variant_iter.rs`, from [gtk-rs/core #1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343).
  This addresses [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html)
  without replacing the GTK3/WebKit ABI used by the login window. Its 11 existing
  iterator regressions are run with release optimizations.

Do not remove the GLib patch just because a registry-only audit omits path crates.
Do not re-enable OAuth2's default reqwest feature: that restores the obsolete TLS stack.
