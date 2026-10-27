fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        // PhotoKit bridge. Compiled into the app so macOS attributes the
        // Photos permission to O2 Cloud itself (not to a helper process).
        cc::Build::new()
            .file("src/photos.m")
            .flag("-fobjc-arc")
            .flag("-Wno-deprecated-declarations")
            .compile("o2_photos");
        println!("cargo:rustc-link-lib=framework=Photos");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }

    tauri_build::build()
}
