//! The shared Supabase account provider: the one account system every
//! deployed product signs in with.
//!
//! `ocad` web reads the session `web-console` wrote (same origin); `ocad`
//! desktop runs Supabase's authorization-code + PKCE flow against a loopback
//! redirect. Both come from [`product_kit::supabase::SupabaseAccount`], which
//! this provider only maps onto the framework's [`AccountProvider`] seam.
//!
//! Signing in is the product's business (the framework never authenticates):
//! the product calls [`SupabaseProvider::begin_sign_in`], opens
//! `request.url`, and polls [`SupabaseProvider::poll_sign_in`] until it
//! answers. A capability is allowed only while a non-expired account is
//! cached, the same rule as [`crate::OAuth2Provider`].

use nanobot_boot::{Account, AccountProvider, EntitlementDecision};
use product_kit::supabase::{Config, Poll, SignInRequest, SupabaseAccount as SharedAccount};

/// The account provider over the shared Supabase account.
#[derive(Clone, Debug)]
pub struct SupabaseProvider {
    account: SharedAccount,
}

impl SupabaseProvider {
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    /// Signs in against an explicit Supabase project configuration.
    pub fn with_config(config: Config) -> Self {
        Self {
            account: SharedAccount::new(config),
        }
    }

    /// The underlying capability (a product with its own sign-in loop, or a
    /// custom configuration).
    pub fn supabase(&self) -> &SharedAccount {
        &self.account
    }

    /// Starts the platform's sign-in; the product opens `request.url`.
    pub fn begin_sign_in(&self) -> Result<SignInRequest, String> {
        self.account.begin()
    }

    /// One sign-in poll.
    pub fn poll_sign_in(&self) -> Result<Poll, String> {
        self.account.poll()
    }

    /// The bearer token for the site's own services while signed in.
    pub fn access_token(&self) -> Option<String> {
        self.account.access_token()
    }

    /// Ends the session.
    pub fn logout(&self) {
        self.account.logout();
    }

    /// Opens `url` in the system browser (the desktop sign-in hand-off).
    #[cfg(not(target_family = "wasm"))]
    pub fn open_url(url: &str) -> Result<(), String> {
        SharedAccount::open_url(url)
    }
}

impl Default for SupabaseProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountProvider for SupabaseProvider {
    fn account(&self) -> Option<Account> {
        self.account.account().map(|account| {
            let mut mapped = Account::new(account.id, account.name);
            mapped.expires_at_ms = account.expires_at.map(|secs| secs.max(0) as u64 * 1000);
            mapped
        })
    }

    fn check(&self, _capability: &str) -> EntitlementDecision {
        match self.account.account() {
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
        assert!(
            !provider
                .check(nanobot_boot::account::TURN_CAPABILITY)
                .is_allowed()
        );
        assert_eq!(provider.access_token(), None);
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn the_desktop_sign_in_points_at_supabase() {
        let provider = SupabaseProvider::new();
        let request = provider.begin_sign_in().expect("the sign-in starts");
        assert!(request.user_code.is_none());
        assert!(
            request.url.contains("/auth/v1/authorize"),
            "{}",
            request.url
        );
        assert_eq!(provider.poll_sign_in(), Ok(Poll::Pending));
    }
}
