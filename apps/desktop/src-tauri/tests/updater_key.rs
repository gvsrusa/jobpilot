//! The public key in `tauri.conf.json` is the one release signatures are made with: it verifies
//! a payload signed by the release key, and refuses the same payload with one byte changed.
//! The check is the updater plugin's own: base64 text, then minisign.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use minisign_verify::{PublicKey, Signature};

fn here() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn decoded(text: &str) -> String {
    String::from_utf8(STANDARD.decode(text.trim()).expect("base64")).expect("utf-8")
}

fn pinned_key() -> PublicKey {
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(here().join("tauri.conf.json")).unwrap()).unwrap();
    let pubkey = config["plugins"]["updater"]["pubkey"]
        .as_str()
        .expect("plugins.updater.pubkey");
    PublicKey::decode(&decoded(pubkey)).expect("a minisign public key")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(&here()).join("tests/fixtures/update").join(name)).unwrap()
}

fn signature() -> Signature {
    let text = String::from_utf8(fixture("payload.bin.sig")).unwrap();
    Signature::decode(&decoded(&text)).expect("a minisign signature")
}

#[test]
fn the_pinned_key_verifies_a_release_signature() {
    let payload = fixture("payload.bin");
    assert!(pinned_key().verify(&payload, &signature(), true).is_ok());
}

#[test]
fn a_tampered_file_is_refused() {
    let mut payload = fixture("payload.bin");
    payload[0] ^= 0x01;
    assert!(pinned_key().verify(&payload, &signature(), true).is_err());
}
