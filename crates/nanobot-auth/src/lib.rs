//! nanobot-auth: auth backends for the [`nanobot_boot::AccountProvider`]
//! seam. The seam's discipline holds here — the provider's `account()` /
//! `check()` answer from local state only; everything that talks to a
//! server is an explicit `async fn` the app layer schedules.
//!
//! - [`oauth`] — the RFC 8628 device flow (the shape a desktop harness can
//!   run: show a code, open a URL, poll), the [`oauth::OAuth2Provider`] over
//!   its token cache, and the opt-in settings persistence for grants.
//! - [`license`] — offline licenses: an ed25519-signed entitlement token
//!   the harness verifies at load with only a public key, for the air-gapped
//!   lab and the perpetual desktop sale.

pub mod error;
pub mod license;
pub mod oauth;

pub use error::AuthError;
pub use license::{public_key_from_base64, public_key_to_base64, License, LicenseProvider, sign_license};
pub use oauth::{
    forget_tokens, persist_tokens, restore_tokens, DeviceCode, DeviceFlow, OAuth2Config,
    OAuth2Provider, PollOutcome, TokenSet,
};
