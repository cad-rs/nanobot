//! The shared web account provider: the rcad.app Supabase session.
//!
//! `ocad` web is served from the same origin as the console, so it is signed
//! in by the console's Supabase session — the same account `creator` web and
//! the console itself use. The provider reads the session from local storage
//! and answers the framework's two questions from local state, never the
//! network: [`AccountProvider::account`] reports the identity, and
//! [`AccountProvider::check`] allows a capability only while a non-expired
//! session is cached (the same rule as [`crate::OAuth2Provider`]).
//!
//! Signing in is the product's business (the framework never authenticates):
//! the product's sign-in affordance opens [`SupabaseProvider::login_url`],
//! which returns to the current page once the console's flow completes.

use nanobot_boot::{Account, AccountProvider, EntitlementDecision};
use product_kit::supabase;

/// The account provider over the shared Supabase session.
#[derive(Clone, Copy, Debug, Default)]
pub struct SupabaseProvider;

impl SupabaseProvider {
    pub fn new() -> Self {
        Self
    }

    /// The bearer token for the site's own services while signed in.
    pub fn access_token(&self) -> Option<String> {
        self.session().map(|session| session.access_token)
    }

    /// The console's login page, returning to the current page afterwards.
    pub fn login_url(&self) -> String {
        supabase::login_url(&supabase::current_path())
    }

    /// The cached session, when it has not expired.
    fn session(&self) -> Option<supabase::Session> {
        supabase::load().filter(|session| !session.is_expired())
    }
}

impl AccountProvider for SupabaseProvider {
    fn account(&self) -> Option<Account> {
        let session = self.session()?;
        let identity = session.identity();
        let mut account = Account::new(identity.id, identity.name);
        account.expires_at_ms = Some(session.expires_at.max(0) as u64 * 1000);
        Some(account)
    }

    fn check(&self, _capability: &str) -> EntitlementDecision {
        match self.session() {
            Some(_) => EntitlementDecision::Allowed,
            None => EntitlementDecision::Denied {
                reason: "not signed in".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_out_provider_reports_no_identity_and_denies() {
        let provider = SupabaseProvider::new();
        assert!(provider.account().is_none());
        assert!(!provider.check(nanobot_boot::account::TURN_CAPABILITY).is_allowed());
        assert_eq!(provider.access_token(), None);
    }

    #[test]
    fn the_login_url_returns_to_the_current_page() {
        let url = SupabaseProvider::new().login_url();
        assert!(url.contains("/login"), "{url}");
    }
}
