//! The license gate: a plugin that ships a license must verify before it loads,
//! and a plugin that ships none is open.

use std::path::{Path, PathBuf};

use nanobot_rhai::{load_all_with, PluginManifest};

const MANIFEST: &str = r#"
[plugin]
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
api_version = "1"
entry_point = "plugin.rhai"

[compatibility]
products = ["*"]

[permissions]
document_read = false
network = false
"#;

const ENTRY: &str = r#"
fn register() {
    register_command("acme.analyze", "分析", "analyze");
}
fn analyze(args) { "分析完成" }
"#;

/// A parent directory holding one plugin per call, the shape `load_all_with`
/// discovers from.
fn plugin_parent(test: &str) -> PathBuf {
    let parent = std::env::temp_dir().join(format!(
        "nanobot-rhai-test-{test}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).expect("create parent dir");
    parent
}

/// Writes one plugin into `parent/<name>` and returns its directory.
fn write_plugin(
    parent: &Path,
    name: &str,
    manifest: &str,
    entry: &str,
    extra: &[(&str, &str)],
) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create plugin dir");
    std::fs::write(dir.join("plugin.toml"), manifest).expect("write manifest");
    std::fs::write(dir.join("plugin.rhai"), entry).expect("write entry");
    for (file, content) in extra {
        std::fs::write(dir.join(file), content).expect("write extra script");
    }
    dir
}

#[test]
fn the_license_gate_rejects_an_unlicensed_copy_and_admits_the_real_one() {
    use ed25519_dalek::SigningKey;
    use nanobot_auth::{
        check_plugin_license, public_key_to_base64, sign_plugin_license, Ed25519PluginVerifier,
        PluginClaims, PLUGIN_LICENSE_FILE,
    };

    let key = SigningKey::from_bytes(&[3_u8; 32]);
    let verifier = Ed25519PluginVerifier::new()
        .with_key("*", &public_key_to_base64(&key.verifying_key()))
        .expect("key");
    // `discover` walks parent directories, so each case gets its own.
    let gate = |manifest: &PluginManifest, dir: &Path| {
        check_plugin_license(&manifest.plugin.id, dir, &verifier)
    };
    let claims = || PluginClaims {
        plugin_id: "com.example.demo".to_string(),
        vendor: "acme".to_string(),
        features: Vec::new(),
        seats: 1,
        issued_at_ms: 1_000,
        expires_at_ms: Some(4_102_444_800_000), // 2100: always valid
        machine_fingerprint: None,
    };

    // No license file: an open plugin, loads.
    let parent = plugin_parent("gate-open");
    write_plugin(&parent, "open", MANIFEST, ENTRY, &[]);
    let loaded = load_all_with(std::slice::from_ref(&parent), &gate);
    assert_eq!(loaded.plugins_loaded.len(), 1, "{:?}", loaded.plugins_rejected);
    assert!(loaded.plugins_rejected.is_empty());
    let _ = std::fs::remove_dir_all(&parent);

    // A genuine license: loads.
    let parent = plugin_parent("gate-licensed");
    write_plugin(
        &parent,
        "licensed",
        MANIFEST,
        ENTRY,
        &[(
            PLUGIN_LICENSE_FILE,
            sign_plugin_license(&claims(), &key).as_str(),
        )],
    );
    let loaded = load_all_with(std::slice::from_ref(&parent), &gate);
    assert_eq!(loaded.plugins_loaded.len(), 1, "{:?}", loaded.plugins_rejected);
    let _ = std::fs::remove_dir_all(&parent);

    // A tampered license: rejected with the reason the host reports.
    let parent = plugin_parent("gate-tampered");
    write_plugin(
        &parent,
        "tampered",
        MANIFEST,
        ENTRY,
        &[(PLUGIN_LICENSE_FILE, "not-a-license-at-all")],
    );
    let loaded = load_all_with(std::slice::from_ref(&parent), &gate);
    assert!(
        loaded.plugins_loaded.is_empty(),
        "a garbage license does not load the plugin"
    );
    let reason = &loaded.plugins_rejected[0].1;
    assert!(reason.starts_with("license: "), "{reason}");
    assert!(reason.contains("plugin license"), "{reason}");
    let _ = std::fs::remove_dir_all(&parent);
}
