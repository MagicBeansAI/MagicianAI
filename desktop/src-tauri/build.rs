fn main() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    println!("cargo:rerun-if-env-changed=MAGICIAN_REQUIRE_NATIVE_BACKEND");
    println!("cargo:rerun-if-changed=native-backend");
    if std::env::var("MAGICIAN_REQUIRE_NATIVE_BACKEND").as_deref() == Ok("1") {
        let native = manifest_dir.join("native-backend");
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let required = if target_os == "windows" {
            vec![
                "MANIFEST.yaml",
                "SHA256SUMS",
                "install.ps1",
                "tool-runtime-config.yaml",
                "magician.exe",
                "magicutor.exe",
                "magic-supervisor.exe",
            ]
        } else {
            vec![
                "MANIFEST.yaml",
                "SHA256SUMS",
                "install.sh",
                "tool-runtime-config.yaml",
                "magician.bin",
                "magicutor.bin",
                "magic-supervisor.bin",
            ]
        };
        let has_directory_package = required.iter().all(|name| native.join(name).is_file());
        let archive_count = std::fs::read_dir(&native)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("magician-") && name.ends_with(".tar.gz"))
            })
            .count();
        if !has_directory_package && archive_count != 1 {
            panic!(
                "MAGICIAN_REQUIRE_NATIVE_BACKEND=1 but native-backend contains neither a complete directory package nor exactly one archive"
            );
        }
    }

    // `tauri_build::build()` copies/validates `bundle.resources` AND (on macOS)
    // `bundle.macOS.frameworks` on EVERY cargo build — not just the
    // `--features native-wake` desktop-wake build, and not just at bundle time.
    // The Vosk model dir + libvosk.dylib are gitignored (fetched by
    // `make setup-desktop-vosk`), so we materialize empty placeholders here to
    // keep plain `cargo build` / `cargo check` / `cargo test` green. Without the
    // feature, native wake is a no-op at runtime anyway, so empty stand-ins are
    // harmless; the real artifacts (from setup) override them for wake builds.

    // 1. `bundle.resources` → vosk-model dir (validated on all targets).
    let _ = std::fs::create_dir_all(manifest_dir.join("vosk-model"));

    // 2. `bundle.macOS.frameworks` → libvosk.dylib (copied on darwin targets).
    //    tauri-build errors `Library not found` if the .dylib is absent, so drop
    //    a 0-byte placeholder when there's no real one yet. `setup-desktop-vosk`
    //    checks for a NON-EMPTY file (`-s`), so the placeholder doesn't block the
    //    real universal-binary fetch.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let vendor_dir = manifest_dir.join("vendor/vosk");
        let _ = std::fs::create_dir_all(&vendor_dir);
        let dylib = vendor_dir.join("libvosk.dylib");
        if !dylib.exists() {
            let _ = std::fs::write(&dylib, []);
        }
    }

    tauri_build::build();

    // macOS dev builds (raw cargo binaries, no .app bundle) don't get
    // an Info.plist from Tauri's bundler — so WKWebView refuses to
    // expose mic / camera APIs because the OS can't find the
    // NS*UsageDescription strings. Embed Info.plist into the MachO
    // `__TEXT,__info_plist` section so macOS reads it directly out
    // of the binary even when there's no surrounding bundle. The
    // bundled production build still uses the same Info.plist via
    // `bundle.macOS` config; this just makes dev work too.
    #[cfg(target_os = "macos")]
    {
        let info_plist = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Info.plist");
        if info_plist.exists() {
            println!("cargo:rerun-if-changed={}", info_plist.display());
            println!(
                "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
                info_plist.display()
            );
        }

        // Native desktop wake word (Vosk): tell the linker where `libvosk` lives,
        // and add rpaths so the binary loads `libvosk.dylib` at runtime — from the
        // vendored dir in dev (populated by `make setup-desktop-vosk`) and from the
        // .app's `Contents/Frameworks` in the bundled release. The `vosk` crate
        // emits the `-lvosk` link directive; we only provide the search path + rpath.
        // Gated on the `native-wake` feature so plain builds don't need libvosk
        // (cargo sets CARGO_FEATURE_<NAME> for the build script per enabled feature).
        if std::env::var("CARGO_FEATURE_NATIVE_WAKE").is_ok() {
            println!("cargo:rerun-if-env-changed=VOSK_LIB_DIR");
            let vosk_lib_dir = std::env::var("VOSK_LIB_DIR").unwrap_or_else(|_| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("vendor/vosk")
                    .to_string_lossy()
                    .into_owned()
            });
            if std::path::Path::new(&vosk_lib_dir).is_dir() {
                println!("cargo:rustc-link-search=native={vosk_lib_dir}");
                // dev: load libvosk.dylib straight from the vendored dir.
                println!("cargo:rustc-link-arg=-Wl,-rpath,{vosk_lib_dir}");
            }
            // bundled .app: libvosk.dylib is copied into Contents/Frameworks.
            println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        }
    }
}
