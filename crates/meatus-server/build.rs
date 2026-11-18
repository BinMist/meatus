//! M5：面板产物必须先构建，避免 `cargo build` 产出没有 UI 的 server。
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = manifest.join("../../web/dist");
    println!("cargo:rerun-if-changed=../../web/dist");
    println!("cargo:rerun-if-changed=../../web/package.json");
    if !dist.join("index.html").is_file() {
        panic!(
            "web/dist/index.html missing: run `npm ci && npm run build` in web/ before cargo build"
        );
    }
}
