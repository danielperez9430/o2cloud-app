fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        // PhotoKit bridge. Compiled into the app so macOS attributes the
        // Photos permission to O2 Cloud itself (not to a helper process).
        println!("cargo:rerun-if-changed=src/photos.m");
        let mut build = cc::Build::new();
        build
            .file("src/photos.m")
            .flag("-fobjc-arc")
            .flag("-Wno-deprecated-declarations");
        let compiler = build.get_compiler().path().to_path_buf();
        build.compile("o2_photos");
        println!("cargo:rustc-link-lib=framework=Photos");
        println!("cargo:rustc-link-lib=framework=Foundation");

        // `@available` in photos.m expands to calls to
        // ___isPlatformVersionAtLeast, which lives in clang's compiler-rt.
        // Cargo links Rust binaries with -nodefaultlibs, so clang does not add
        // that archive on its own and the link fails with an undefined symbol.
        link_compiler_rt(&compiler);
    }

    tauri_build::build()
}

/// Ask the same clang that compiles the bridge where its compiler-rt archive
/// is, and link it. The path depends on the selected Xcode/Command Line Tools.
fn link_compiler_rt(compiler: &std::path::Path) {
    let output = std::process::Command::new(compiler)
        .arg("--print-file-name=libclang_rt.osx.a")
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let path = std::path::PathBuf::from(path);
            if path.is_absolute() && path.is_file() {
                if let Some(dir) = path.parent() {
                    println!("cargo:rustc-link-search=native={}", dir.display());
                }
                println!("cargo:rustc-link-lib=static=clang_rt.osx");
                return;
            }
        }
    }

    // Fall back to the linker's default search paths. This keeps the build
    // working on toolchains where `--print-file-name` cannot resolve it.
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
