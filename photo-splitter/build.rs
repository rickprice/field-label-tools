use std::env;
use std::path::PathBuf;

fn main() {
    // libraw_r is the reentrant (thread-safe-to-instantiate) build of
    // LibRaw; this crate only ever uses one libraw_data_t per call so
    // reentrancy itself doesn't matter, but it's what nixpkgs' `libraw`
    // ships a .pc file for.
    let lib = pkg_config::Config::new()
        .probe("libraw_r")
        .expect("libraw_r not found via pkg-config (NixOS: add `pkgs.libraw` and `pkgs.rustPlatform.bindgenHook`)");

    let clang_args: Vec<String> = lib
        .include_paths
        .iter()
        .map(|path| format!("-I{}", path.display()))
        .collect();

    // libraw.h is a C++ header (it declares the LibRaw class alongside the
    // plain-C API this crate actually binds to), so it needs C++ parsing to
    // parse at all -- the allowlists below keep the generated bindings down
    // to just the C functions/types we call.
    let bindings = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <libraw/libraw.h>")
        .clang_args(["-x", "c++"])
        .clang_args(clang_args)
        .allowlist_item("libraw_init")
        .allowlist_item("libraw_open_file")
        .allowlist_item("libraw_unpack")
        .allowlist_item("libraw_dcraw_process")
        .allowlist_item("libraw_dcraw_make_mem_image")
        .allowlist_item("libraw_dcraw_clear_mem")
        .allowlist_item("libraw_close")
        .allowlist_item("libraw_strerror")
        .allowlist_item("libraw_data_t")
        .allowlist_item("libraw_processed_image_t")
        .allowlist_item("LibRaw_image_formats")
        .allowlist_item("LIBRAW_IMAGE_.*")
        .generate()
        .expect("failed to generate libraw bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("libraw_bindings.rs"))
        .expect("failed to write libraw bindings");
}
