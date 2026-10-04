fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let header = std::path::Path::new(&crate_dir).join("../../include/rapidpdftrans.h");
    if let Some(parent) = header.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let bindings = cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(cbindgen::Config::from_file("cbindgen.toml").expect("cbindgen.toml"))
        .generate()
        .expect("cbindgen failed");
    let mut header_text = Vec::new();
    bindings.write(&mut header_text);
    let mut header_text = String::from_utf8(header_text).expect("cbindgen utf-8");
    header_text = header_text.replace(
        "typedef struct RptDocument {\n    uint8_t _private[0];\n} RptDocument;",
        "typedef struct RptDocument RptDocument;",
    );
    std::fs::write(&header, header_text).expect("write header");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");
}
