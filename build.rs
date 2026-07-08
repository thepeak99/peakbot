// build.rs — see `webui.md` §4 fail-SAFE guard.
//
// Without this, a node-free `cargo build` (CI's pre-Phase-1 state, or a
// backend-only contributor) would either fail on an empty `web/dist/` or
// silently embed nothing. We write a self-explaining `index.html` instead:
// the binary compiles, runs, and the browser tells the operator exactly
// what to do. CI and Docker builds run the real Node stage first, so
// this stub never ships.

use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=web/dist/.gitkeep");

    let dist = Path::new("web/dist");
    let index = dist.join("index.html");

    if !index.exists() {
        fs::create_dir_all(dist).expect("create web/dist");
        fs::write(&index, include_str!("build/stub_index.html"))
            .expect("write web/dist/index.html stub");
    }
}
