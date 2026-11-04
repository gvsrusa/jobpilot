//! The webview's capability is the short list the frontend uses (M-01, review-1.0 S-4).
//! No `core:default`, so no menu, tray, image, path, or webview permission: script in the
//! webview cannot replace the menu or disable Safety > Emergency pause.

use std::path::PathBuf;

/// Every permission the main window holds, and why.
const EXPECTED: &[&str] = &[
    // `listen` in lib/ipc.ts: update-available, pty:data, pty:exit; the returned unlisten.
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    // `ptyAck` emits pty:ack.
    "core:event:allow-emit",
    // RecoveryView's Quit.
    "core:window:allow-close",
];

fn permissions() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capabilities/default.json");
    let text = std::fs::read_to_string(path).expect("capabilities/default.json");
    let capability: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    capability["permissions"]
        .as_array()
        .expect("a permissions list")
        .iter()
        .map(|p| {
            p.as_str()
                .expect("plain identifiers, no scoped objects")
                .to_owned()
        })
        .collect()
}

#[test]
fn the_webview_holds_only_what_the_frontend_uses() {
    let mut held = permissions();
    held.sort();
    let mut expected: Vec<String> = EXPECTED.iter().map(|p| p.to_string()).collect();
    expected.sort();
    assert_eq!(held, expected);
}

/// Tauri loads every file in `capabilities/`, and the config can hold one inline.
#[test]
fn default_json_is_the_only_capability() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files: Vec<_> = std::fs::read_dir(root.join("capabilities"))
        .expect("capabilities/")
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, ["default.json"]);
    let text = std::fs::read_to_string(root.join("tauri.conf.json")).expect("tauri.conf.json");
    let config: serde_json::Value = serde_json::from_str(&text).expect("json");
    let inline = &config["app"]["security"]["capabilities"];
    assert!(
        inline.is_null() || inline == &serde_json::json!(["default"]),
        "{inline}"
    );
}

#[test]
fn no_default_set_and_no_menu_tray_or_path_permission() {
    for permission in permissions() {
        assert!(
            !permission.ends_with(":default"),
            "{permission} is a default set"
        );
        for refused in [
            "core:menu:",
            "core:tray:",
            "core:image:",
            "core:path:",
            "core:webview:",
        ] {
            assert!(!permission.starts_with(refused), "{permission} is refused");
        }
        // Rust opens every dialog itself (M-13); the restore folder too (S-6).
        assert!(!permission.starts_with("dialog:"), "{permission}");
    }
}
