use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let header_path = PathBuf::from(&out_dir).join("awob_client.h");

    match cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(
            cbindgen::Config::from_file(format!("{crate_dir}/cbindgen.toml")).unwrap_or_default(),
        )
        .with_language(cbindgen::Language::C)
        .generate()
    {
        Ok(b) => {
            b.write_to_file(&header_path);
        }
        Err(e) => {
            // Don't fail the build during early development; emit a warning
            // so the rest of the workspace stays buildable while the FFI
            // surface is iterated on.
            println!("cargo:warning=cbindgen header generation skipped: {e}");
        }
    }

    let exposed = PathBuf::from(&crate_dir)
        .join("include")
        .join("awob_client.h");
    if header_path.exists() {
        std::fs::create_dir_all(exposed.parent().unwrap()).ok();
        let _ = std::fs::copy(&header_path, &exposed);
    }
}
