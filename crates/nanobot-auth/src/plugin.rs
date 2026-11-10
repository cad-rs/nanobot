//! Plugin licenses: the closed half of the open-core model.
//!
//! The framework is MIT; a vendor's **plugin packages** are their own
//! commercial property. This module is what makes that boundary enforceable
//! rather than merely declared:
//!
//! - **Integrity** — a plugin license is an ed25519-signed token over the
//!   plugin's claims, so a tampered or transplanted license does not verify.
//! - **Entitlement** — expiry and a per-plugin id check decide whether the
//!   plugin may run.
//! - **Anti-copy** — a license may bind to one machine's
//!   [`machine_fingerprint`], so copying the package elsewhere does not work.
//!
//! A plugin that ships **no** license file is open and loads freely — the
//! default, so open plugins need no tooling. A plugin that ships one must
//! pass verification; the plugin host reports the refusal in its
//! `plugins_rejected` list, next to the other load failures.
//!
//! The framework verifies; it never signs. The vendor holds the signing key
//! and ships only a base64 verifying key (see the `sign-plugin` example).

use std::collections::HashMap;
use std::path::Path;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use crate::error::AuthError;
use crate::{public_key_from_base64, sign_token, verify_token};

/// The license file a plugin package carries, next to `plugin.toml`.
pub const PLUGIN_LICENSE_FILE: &str = "license.lic";

/// What a plugin license claims. The payload is signed; the claims are the
/// vendor's statement about who may run the plugin, and until when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginClaims {
    /// The plugin id this license covers (the manifest's `id`).
    pub plugin_id: String,
    /// The vendor that issued it.
    pub vendor: String,
    /// The capabilities the license grants; empty means everything.
    #[serde(default)]
    pub features: Vec<String>,
    /// Reporting only — seat counting is the deployment's business.
    #[serde(default)]
    pub seats: u32,
    #[serde(default)]
    pub issued_at_ms: u64,
    /// When the license stops working, when it does at all.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    /// When set, the license runs only on the machine whose
    /// [`machine_fingerprint`] matches — the anti-copy mechanism.
    #[serde(default)]
    pub machine_fingerprint: Option<String>,
}

/// Whether a plugin may run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginLicenseDecision {
    Allowed,
    Denied { reason: String },
}

impl PluginLicenseDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    pub fn denied_reason(&self) -> Option<&str> {
        match self {
            Self::Allowed => None,
            Self::Denied { reason } => Some(reason),
        }
    }
}

/// The seam the plugin host calls: given a plugin id and its license token
/// (`None` when the plugin ships no license — an open plugin), may it run?
///
/// The framework never signs or holds keys: the host supplies the vendor's
/// verifying keys and the policy.
pub trait PluginLicenseVerifier: Send + Sync {
    fn verify(&self, plugin_id: &str, license: Option<&str>) -> PluginLicenseDecision;
}

/// An ed25519 verifier over a set of vendor keys, keyed by plugin id
/// (`"*"` covers the vendor's whole line).
pub struct Ed25519PluginVerifier {
    keys: HashMap<String, VerifyingKey>,
    /// Time override for deterministic expiry tests; real time when `None`.
    now_ms: Option<u64>,
}

impl Ed25519PluginVerifier {
    pub fn new() -> Self {
        Self {
            keys: HashMap::new(),
            now_ms: None,
        }
    }

    /// Registers a base64url verifying key for one plugin id, or `"*"` for
    /// the vendor's whole line.
    pub fn with_key(
        mut self,
        plugin_id: &str,
        verifying_key_base64: &str,
    ) -> Result<Self, AuthError> {
        self.keys.insert(
            plugin_id.to_string(),
            public_key_from_base64(verifying_key_base64)?,
        );
        Ok(self)
    }

    /// Pins "now" (unix ms) — deterministic expiry tests.
    pub fn with_now_ms(mut self, now_ms: u64) -> Self {
        self.now_ms = Some(now_ms);
        self
    }

    fn key_for(&self, plugin_id: &str) -> Option<&VerifyingKey> {
        self.keys
            .get(plugin_id)
            .or_else(|| self.keys.get("*"))
    }

    fn now(&self) -> u64 {
        self.now_ms.unwrap_or_else(crate::now_ms)
    }
}

impl Default for Ed25519PluginVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginLicenseVerifier for Ed25519PluginVerifier {
    fn verify(&self, plugin_id: &str, license: Option<&str>) -> PluginLicenseDecision {
        // No license file: an open plugin.
        let Some(token) = license else {
            return PluginLicenseDecision::Allowed;
        };
        let Some(key) = self.key_for(plugin_id) else {
            return PluginLicenseDecision::Denied {
                reason: format!("no verifying key for plugin `{plugin_id}`"),
            };
        };
        let claims: PluginClaims = match verify_token(token.trim(), key) {
            Ok(claims) => claims,
            Err(error) => {
                return PluginLicenseDecision::Denied {
                    reason: format!("plugin license for `{plugin_id}`: {error}"),
                }
            }
        };
        if claims.plugin_id != plugin_id {
            return PluginLicenseDecision::Denied {
                reason: format!(
                    "plugin license names `{}`, not `{plugin_id}`",
                    claims.plugin_id
                ),
            };
        }
        if let Some(expires_at) = claims.expires_at_ms {
            if self.now() >= expires_at {
                return PluginLicenseDecision::Denied {
                    reason: format!("plugin license expired at {expires_at} ms"),
                };
            }
        }
        if let Some(expected) = &claims.machine_fingerprint {
            let actual = machine_fingerprint(&claims.vendor);
            if expected != &actual {
                return PluginLicenseDecision::Denied {
                    reason: "plugin license is bound to another machine".to_string(),
                };
            }
        }
        PluginLicenseDecision::Allowed
    }
}

/// The host-side helper: read a plugin's license file and hand it to the
/// verifier. `Ok(())` means the plugin may load; `Err(reason)` is what the
/// plugin host reports in `plugins_rejected`.
pub fn check_plugin_license(
    plugin_id: &str,
    dir: &Path,
    verifier: &dyn PluginLicenseVerifier,
) -> Result<(), String> {
    let license = std::fs::read_to_string(dir.join(PLUGIN_LICENSE_FILE))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    match verifier.verify(plugin_id, license.as_deref()) {
        PluginLicenseDecision::Allowed => Ok(()),
        PluginLicenseDecision::Denied { reason } => Err(reason),
    }
}

/// Signs a plugin license for a vendor's tooling. The signing key stays with
/// the vendor; the verifying key ships with the product.
pub fn sign_plugin_license(claims: &PluginClaims, signing_key: &ed25519_dalek::SigningKey) -> String {
    sign_token(claims, signing_key)
}

/// A stable-ish per-machine id, salted by the vendor so two vendors cannot
/// compare fingerprints. This is a deterrent, not a vault: it raises the
/// cost of copying a licensed plugin to another machine, it does not make it
/// impossible.
pub fn machine_fingerprint(salt: &str) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    salt.hash(&mut hasher);
    std::env::consts::OS.hash(&mut hasher);
    std::env::consts::ARCH.hash(&mut hasher);
    if let Ok(host) = std::env::var("HOSTNAME") {
        host.hash(&mut hasher);
    }
    if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
        user.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn vendor_key() -> SigningKey {
        SigningKey::from_bytes(&[11_u8; 32])
    }

    fn other_key() -> SigningKey {
        SigningKey::from_bytes(&[12_u8; 32])
    }

    fn claims(plugin_id: &str) -> PluginClaims {
        PluginClaims {
            plugin_id: plugin_id.to_string(),
            vendor: "acme".to_string(),
            features: Vec::new(),
            seats: 5,
            issued_at_ms: crate::now_ms().saturating_sub(1_000),
            expires_at_ms: Some(crate::now_ms() + 3_600_000),
            machine_fingerprint: None,
        }
    }

    #[test]
    fn a_signed_license_loads_and_an_open_plugin_needs_no_file() {
        let key = vendor_key();
        let verifier = Ed25519PluginVerifier::new()
            .with_key("acme.chart", &crate::public_key_to_base64(&key.verifying_key()))
            .expect("key");
        let token = sign_plugin_license(&claims("acme.chart"), &key);

        assert!(
            verifier
                .verify("acme.chart", Some(&token))
                .is_allowed(),
            "a genuine license is allowed"
        );
        assert!(
            verifier.verify("acme.open", None).is_allowed(),
            "a plugin with no license file is open"
        );
    }

    #[test]
    fn tampering_foreign_keys_and_id_mismatch_are_denied() {
        let key = vendor_key();
        let verifier = Ed25519PluginVerifier::new()
            .with_key("*", &crate::public_key_to_base64(&key.verifying_key()))
            .expect("key");
        let token = sign_plugin_license(&claims("acme.chart"), &key);

        // A transplanted signature from a different plugin.
        let other = sign_plugin_license(&claims("acme.other"), &key);
        let transplanted = format!("{}.{}", token.split('.').next().unwrap(), other.split('.').nth(1).unwrap());
        assert!(
            !verifier.verify("acme.chart", Some(&transplanted)).is_allowed(),
            "a transplanted signature does not verify"
        );

        // A license signed by a key the product does not trust.
        let foreign = sign_plugin_license(&claims("acme.chart"), &other_key());
        assert!(
            !verifier.verify("acme.chart", Some(&foreign)).is_allowed(),
            "a foreign signing key does not verify"
        );

        // A license that names another plugin.
        assert!(
            !verifier.verify("acme.log", Some(&token)).is_allowed(),
            "an id mismatch is a denial"
        );

        // No key registered for the plugin.
        assert!(
            !verifier.verify("acme.unknown", Some(&token)).is_allowed(),
            "an unregistered plugin is a denial"
        );
    }

    #[test]
    fn expiry_and_machine_binding_are_enforced() {
        let key = vendor_key();
        let verifier = Ed25519PluginVerifier::new()
            .with_key("*", &crate::public_key_to_base64(&key.verifying_key()))
            .expect("key")
            .with_now_ms(5_000);

        let mut expired = claims("acme.chart");
        expired.expires_at_ms = Some(4_000);
        let decision = verifier.verify("acme.chart", Some(&sign_plugin_license(&expired, &key)));
        assert!(!decision.is_allowed(), "an expired license is denied");
        assert!(
            decision.denied_reason().unwrap().contains("expired"),
            "the reason names the expiry"
        );

        // Machine binding: the license carries a fingerprint that is not this
        // machine's, so it is denied.
        let mut bound = claims("acme.chart");
        bound.machine_fingerprint = Some("some-other-machine".to_string());
        let decision = verifier.verify("acme.chart", Some(&sign_plugin_license(&bound, &key)));
        assert!(!decision.is_allowed(), "a license for another machine is denied");

        // The right fingerprint passes.
        let mut matching = claims("acme.chart");
        matching.machine_fingerprint = Some(machine_fingerprint("acme"));
        assert!(
            verifier
                .verify("acme.chart", Some(&sign_plugin_license(&matching, &key)))
                .is_allowed(),
            "the matching machine runs it"
        );
    }

    #[test]
    fn the_license_file_is_read_from_the_package() {
        let key = vendor_key();
        let verifier = Ed25519PluginVerifier::new()
            .with_key("*", &crate::public_key_to_base64(&key.verifying_key()))
            .expect("key");

        let dir = std::env::temp_dir().join(format!("nb-plugin-license-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No license file: allowed.
        assert!(check_plugin_license("acme.chart", &dir, &verifier).is_ok());

        // A genuine one: allowed.
        std::fs::write(
            dir.join(PLUGIN_LICENSE_FILE),
            sign_plugin_license(&claims("acme.chart"), &key),
        )
        .unwrap();
        assert!(check_plugin_license("acme.chart", &dir, &verifier).is_ok());

        // A garbage one: denied, with the reason the host reports.
        std::fs::write(dir.join(PLUGIN_LICENSE_FILE), "not-a-license").unwrap();
        let error = check_plugin_license("acme.chart", &dir, &verifier).expect_err("garbage");
        assert!(error.contains("plugin license"), "{error}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
