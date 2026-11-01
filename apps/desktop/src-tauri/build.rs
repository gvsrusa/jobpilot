fn main() {
    // tauri-build would demand the release sidecar and then copy it over `target/debug/jobpilot`,
    // the binary the CLI tests run. A release binary there ignores JOBPILOT_SECRET_SERVICE and
    // would reach the real keychain, so only release builds, which bundle it, list it.
    if std::env::var("PROFILE").is_ok_and(|profile| profile != "release")
        && std::env::var_os("TAURI_CONFIG").is_none()
    {
        // SAFETY: the build script has one thread here.
        unsafe { std::env::set_var("TAURI_CONFIG", r#"{"bundle":{"externalBin":[]}}"#) };
    }

    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let mut attributes = tauri_build::Attributes::new();

    if windows {
        // tauri-build embeds its manifest (Common Controls v6) only into the app binary,
        // so the lib's test executable fails to start with STATUS_ENTRYPOINT_NOT_FOUND.
        // Embed the same manifest through the linker so every target gets it.
        // The XML is tauri-build's own default.
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        // ponytail: MSVC only; the GNU toolchain is not a supported target.
        if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
            println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
            println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        }
    }

    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
