# vendor/

Vendored patches for crates.io dependencies, referenced from
`[patch.crates-io]` in `Cargo.toml`.

## termios-0.2.2

Byte-copy of [termios 0.2.2](https://crates.io/crates/termios/0.2.2)
(MIT, license in the directory) with one addition: `src/os/android.rs`
plus the two `#[cfg(target_os = "android")]` gates in `src/os/mod.rs`
(ported from termios 0.3.3's `android.rs`, adapted to 0.2.2's struct/
constant layout — Bionic's `termios` has no `c_line`/`c_ispeed`/
`c_ospeed` fields and `NCCS` is 19).

Why: `portable-pty` is pinned to `=0.8.1` (0.9.0 deadlocks ConPTY
children, wezterm/portable-pty#6783), and 0.8.1 pulls
`serial 0.4.0` → `serial-unix 0.4.0` → `termios 0.2.2`, whose
`src/os/mod.rs` has no `android` module — so
`cargo ndk -t aarch64-linux-android build` failed with
E0432/E0433 "could not find target in os". PeakBot itself never calls
serial/termios; the patch only needs the crate to *compile* for
Android. The patch applies to the 0.2.x semver line only — termwiz's
termios 0.3.3 resolves as before.

Upstreaming note: if termios 0.2.x ever ships an `os::android` module
or portable-pty is unpinned, delete this directory and the
`[patch.crates-io]` section.
