use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by Cargo"),
    );
    let private_feeds_path = manifest_dir.join("resources/default-feeds.json");
    println!("cargo:rerun-if-changed={}", private_feeds_path.display());

    let bundled_feeds =
        fs::read_to_string(&private_feeds_path).unwrap_or_else(|_| "[]\n".to_string());
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR should be set by Cargo"));
    fs::write(out_dir.join("default-feeds.json"), bundled_feeds)
        .expect("failed to write generated default-feeds.json");

    tauri_build::build()
}
