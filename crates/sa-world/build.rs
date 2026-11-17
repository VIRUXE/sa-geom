// Embed data/vanilla.sagw.zst when it exists (it is produced by `sa-geom bake`).
fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/vanilla.sagw.zst");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo::rustc-check-cfg=cfg(sagw_embedded)");
    if path.exists() && std::env::var_os("CARGO_FEATURE_EMBEDDED").is_some() {
        println!("cargo:rustc-cfg=sagw_embedded");
    }
}
