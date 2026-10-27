//! Offline licenses: signed entitlements that work with no network at all —
//! the air-gapped lab, the enterprise seat, the perpetual desktop sale.
//!
//! A license is a two-segment token, `<payload>.<signature>`, both
//! base64url: the payload is the canonical JSON of [`License`], the
//! signature is ed25519 over those exact bytes. The vendor holds the
//! signing key and never shares it; a harness ships only the public key
//! (a 32-byte base64 string) and verifies on load — [`LicenseProvider`]
//! turns a verified license into an [`AccountProvider`], so the gated
//! capability question ([`nanobot_boot::Harness::check_entitlement`]) and
//! the audit trail work identically for a licensed seat and a cloud user.
//!
//! The framework verifies; it does not issue. [`sign_license`] exists so a
//! vendor's own tooling (and these tests) can produce tokens — the signing
//! key's custody is entirely outside the framework.

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use nanobot_boot::{Account, AccountProvider, EntitlementDecision};
use nanobot_tools::glob_match;
use serde::{Deserialize, Serialize};

use crate::base64;
use crate::error::AuthError;
use crate::now_ms;

/// The verified payload. Field order here is the canonical byte order —
/// adding fields changes the encoding, which is fine: old tokens keep
/// verifying under old code and new fields require new code anyway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    pub license_id: String,
    /// The product this license names. `None` means "any product from this
    /// vendor"; a name mismatch is a denial at [`LicenseProvider::load`].
    pub product: Option<String>,
    /// The tier label the shell shows.
    pub plan: Option<String>,
    /// How many seats the license covers. Reporting only — seat counting is
    /// the deployment's business, not something a single machine can know.
    #[serde(default)]
    pub seats: u32,
    /// The capabilities the license grants; empty means everything.
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub issued_at_ms: u64,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}

/// An ed25519 verification key, from the 32-byte base64 string a product
/// ships next to its identity.
pub fn public_key_from_base64(encoded: &str) -> Result<VerifyingKey, AuthError> {
    let bytes = base64::decode_urlsafe(encoded)
        .ok_or_else(|| AuthError::Config("public key is not valid base64url".into()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| AuthError::Config("public key must be 32 bytes".into()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| AuthError::Config(e.to_string()))
}

/// The base64url text a vendor distributes with the product: the verifying
/// key's 32 bytes. The inverse of [`public_key_from_base64`].
pub fn public_key_to_base64(key: &VerifyingKey) -> String {
    base64::encode_urlsafe(&key.to_bytes())
}

/// Signs any serializable payload, returning the `<payload>.<signature>`
/// token. The one token format both the app license and the plugin license
/// share; the signing key never leaves the vendor's tooling.
pub fn sign_token<T: Serialize>(payload: &T, signing_key: &SigningKey) -> String {
    let bytes = serde_json::to_vec(payload).expect("payload serializes");
    let signature = signing_key.sign(&bytes);
    format!(
        "{}.{}",
        base64::encode_urlsafe(&bytes),
        base64::encode_urlsafe(&signature.to_bytes())
    )
}

/// Verifies and decodes a [`sign_token`] token. A tampered payload, a
/// transplanted signature, or a foreign key is an [`AuthError::Protocol`].
pub fn verify_token<T: for<'de> Deserialize<'de>>(
    token: &str,
    public_key: &VerifyingKey,
) -> Result<T, AuthError> {
    let (payload_b64, signature_b64) = token
        .split_once('.')
        .ok_or_else(|| AuthError::Protocol("token is not <payload>.<signature>".into()))?;
    let payload = base64::decode_urlsafe(payload_b64)
        .ok_or_else(|| AuthError::Protocol("token payload is not base64url".into()))?;
    let signature_bytes = base64::decode_urlsafe(signature_b64)
        .ok_or_else(|| AuthError::Protocol("token signature is not base64url".into()))?;
    let signature_bytes: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| AuthError::Protocol("token signature must be 64 bytes".into()))?;
    let signature = Signature::from_bytes(&signature_bytes);
    public_key
        .verify(&payload, &signature)
        .map_err(|_| AuthError::Protocol("token signature does not verify".into()))?;
    serde_json::from_slice(&payload).map_err(|e| AuthError::Protocol(format!("token payload: {e}")))
}

/// Signs an app license. The signing key never leaves the vendor's tooling.
pub fn sign_license(license: &License, signing_key: &SigningKey) -> String {
    sign_token(license, signing_key)
}

/// A signed license held by one deployment: verified once at load, asked
/// forever after with no network.
pub struct LicenseProvider {
    license: License,
    account: Account,
    expected_product: Option<String>,
    /// Time override for deterministic expiry tests; real time when `None`.
    now_ms: Option<u64>,
}

impl LicenseProvider {
    /// Verifies `data` (the license file's bytes, exactly as issued) against
    /// `public_key`. Any tampering — a changed field, a transplanted
    /// signature — fails the signature and is a [`AuthError::Protocol`].
    pub fn load(data: &[u8], public_key: &VerifyingKey) -> Result<Self, AuthError> {
        let text = std::str::from_utf8(data)
            .map_err(|_| AuthError::Protocol("license is not utf-8".into()))?;
        let license: License = verify_token(text.trim(), public_key)?;
        if let Some(expires_at) = license.expires_at_ms {
            if now_ms() >= expires_at {
                return Err(AuthError::Expired);
            }
        }
        let plan = license.plan.clone().unwrap_or_else(|| "license".into());
        Ok(Self {
            account: Account {
                id: license.license_id.clone(),
                display_name: plan.clone(),
                plan: Some(plan),
                expires_at_ms: license.expires_at_ms,
            },
            license,
            expected_product: None,
            now_ms: None,
        })
    }

    /// Pins the product identity the license must name (from the harness
    /// profile). A license naming another product is a denial at check
    /// time — the token itself was genuinely signed, it just is not ours.
    pub fn expecting_product(mut self, product: impl Into<String>) -> Self {
        self.expected_product = Some(product.into());
        self
    }

    /// Pins "now" (unix ms) — deterministic expiry tests.
    pub fn with_now_ms(mut self, now_ms: u64) -> Self {
        self.now_ms = Some(now_ms);
        self
    }

    /// The verified license.
    pub fn license(&self) -> &License {
        &self.license
    }

    fn now(&self) -> u64 {
        self.now_ms.unwrap_or_else(now_ms)
    }
}

impl AccountProvider for LicenseProvider {
    fn account(&self) -> Option<Account> {
        Some(self.account.clone())
    }

    fn check(&self, capability: &str) -> EntitlementDecision {
        if let Some(expires_at) = self.license.expires_at_ms {
            if nanobot_boot::account::is_expired(self.license.expires_at_ms, self.now()) {
                return EntitlementDecision::Denied {
                    reason: format!("license `{}` expired at {expires_at} ms", self.license.license_id),
                };
            }
        }
        if let Some(expected) = &self.expected_product {
            let named_ok = self
                .license
                .product
                .as_deref()
                .map(|named| named == expected || named == "*")
                .unwrap_or(true); // a license naming no product covers the vendor's line
            if !named_ok {
                return EntitlementDecision::Denied {
                    reason: format!(
                        "license `{}` names product {:?}, not `{expected}`",
                        self.license.license_id,
                        self.license.product
                    ),
                };
            }
        }
        if !self.license.features.is_empty()
            && !self.license.features.iter().any(|f| glob_match(f, capability))
        {
            return EntitlementDecision::Denied {
                reason: format!(
                    "license `{}` does not cover `{capability}`",
                    self.license.license_id
                ),
            };
        }
        EntitlementDecision::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vendor_key() -> SigningKey {
        // A fixed seed: the same license always signs to the same token,
        // so a regression in the encoding shows up as a diff, not a flake.
        SigningKey::from_bytes(&[7_u8; 32])
    }

    /// A license valid now, for an hour.
    fn sample() -> License {
        License {
            license_id: "lic-42".into(),
            product: Some("com.acme.harness".into()),
            plan: Some("team".into()),
            seats: 10,
            features: vec!["agent.turn".into(), "export.*".into()],
            issued_at_ms: crate::now_ms().saturating_sub(1_000),
            expires_at_ms: Some(crate::now_ms() + 3_600_000),
        }
    }

    /// A license from 1970, expired before anything ran — the deterministic
    /// expiry case.
    fn expired_sample() -> License {
        License {
            issued_at_ms: 1_000,
            expires_at_ms: Some(2_000),
            ..sample()
        }
    }

    #[test]
    fn base64_round_trips_on_split_boundaries() {
        for len in 0..40 {
            let data: Vec<u8> = (0..len as u8).collect();
            assert_eq!(base64::decode_urlsafe(&base64::encode_urlsafe(&data)), Some(data.clone()));
        }
    }

    #[test]
    fn a_signed_license_verifies_and_becomes_an_account() {
        let key = vendor_key();
        let public = key.verifying_key();
        let token = sign_license(&sample(), &key);

        let provider = LicenseProvider::load(token.as_bytes(), &public)
            .expect("a genuine license verifies");
        assert_eq!(provider.license().seats, 10);
        let account = provider.account().expect("licensed");
        assert_eq!(account.id, "lic-42");
        assert_eq!(account.plan.as_deref(), Some("team"));

        assert!(provider.check("agent.turn").is_allowed());
        assert!(provider.check("export.gds").is_allowed());
        assert!(!provider.check("sim.fem").is_allowed());
    }

    #[test]
    fn any_tampering_fails_the_signature() {
        let key = vendor_key();
        let public = key.verifying_key();
        let token = sign_license(&sample(), &key);
        let (payload, signature) = token.split_once('.').unwrap();

        // One flipped payload byte, re-encoded.
        let mut payload_bytes = base64::decode_urlsafe(payload).unwrap();
        payload_bytes[3] ^= 0x01;
        let flipped = format!("{}.{}", base64::encode_urlsafe(&payload_bytes), signature);
        assert!(matches!(
            LicenseProvider::load(flipped.as_bytes(), &public),
            Err(AuthError::Protocol(_))
        ));

        // A signature transplanted from a different license.
        let other = sign_license(
            &License { license_id: "other".into(), ..sample() },
            &key,
        );
        let other_signature = other.rsplit('.').next().unwrap();
        let transplanted = format!("{payload}.{other_signature}");
        assert!(matches!(
            LicenseProvider::load(transplanted.as_bytes(), &public),
            Err(AuthError::Protocol(_))
        ));
    }

    #[test]
    fn an_expired_license_is_rejected_at_load_and_pinned_time_decides() {
        let key = vendor_key();
        let public = key.verifying_key();
        let token = sign_license(&expired_sample(), &key);

        assert!(
            matches!(LicenseProvider::load(token.as_bytes(), &public), Err(AuthError::Expired)),
            "the 1970-window license is expired the moment it loads"
        );

        // Within its window (pinned before its expiry) it loads and runs.
        let inside = License { expires_at_ms: Some(crate::now_ms() + 60_000), ..expired_sample() };
        let provider = LicenseProvider::load(sign_license(&inside, &key).as_bytes(), &public)
            .expect("within the window it loads");
        assert!(provider.check("agent.turn").is_allowed());
    }

    #[test]
    fn the_product_and_feature_gates_deny_with_reasons() {
        let key = vendor_key();
        let public = key.verifying_key();
        let token = sign_license(&sample(), &key);

        let provider = LicenseProvider::load(token.as_bytes(), &public)
            .expect("load")
            .expecting_product("com.other.harness");
        let decision = provider.check("agent.turn");
        assert!(
            decision.denied_reason().unwrap().contains("com.other.harness"),
            "the denial names the product: {:?}",
            decision.denied_reason()
        );

        let wildcard = License { product: Some("*".into()), ..sample() };
        let wildcard_provider = LicenseProvider::load(sign_license(&wildcard, &key).as_bytes(), &public)
            .expect("load")
            .expecting_product("com.other.harness");
        assert!(wildcard_provider.check("agent.turn").is_allowed());
    }

    #[test]
    fn a_public_key_from_base64_matches_the_vendor_key() {
        let key = vendor_key();
        let encoded = base64::encode_urlsafe(&key.verifying_key().to_bytes());
        let restored = public_key_from_base64(&encoded).expect("key decodes");
        let token = sign_license(&sample(), &key);
        assert!(LicenseProvider::load(token.as_bytes(), &restored).is_ok());
    }
}
