//! Build script for generating FFI bindings.
//!
//! This script uses bindgen to generate Rust bindings for macOS sys/proc_info.h
//! structures, specifically proc_bsdshortinfo and related constants.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=src/macos/sys_proc_info.h");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR should be set by cargo");
    let out_path = Path::new(&out_dir).join("sys_proc_info.rs");

    let builder = bindgen::Builder::default()
        .header("src/macos/sys_proc_info.h")
        .allowlist_type("proc_bsdshortinfo")
        .allowlist_var("PROC_PIDT_SHORTBSDINFO");

    // Minimize generated code by disabling unused derives and making unused types opaque
    let builder = builder
        .derive_copy(false)
        .derive_debug(false)
        .opaque_type("gid_t")
        .opaque_type("uid_t");

    builder
        .generate()
        .expect("unable to generate bindings")
        .write_to_file(out_path)
        .expect("couldn't write bindings");
}
