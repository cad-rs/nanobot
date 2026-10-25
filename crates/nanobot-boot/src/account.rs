//! The account concept: who is using the harness, and what they are allowed
//! to run (design: the seam mirrors [`crate::harness_toml`]'s identity
//! descriptor — the framework owns the *mechanism*, the product owns the
//! *authentication*).
//!
//! The framework never authenticates. A product supplies an
//! [`AccountProvider`]: a signed-in cloud user, an offline license seat, an
//! OS identity — or nothing at all ([`AnonymousProvider`], the default,
//! under which every capability is allowed and nothing changes for existing
//! consumers). The framework asks it two questions:
//!
//! - [`AccountProvider::account`] — the identity snapshot, for the shell's
//!   account surface. `None` means not signed in; the shell shows a guest.
//! - [`AccountProvider::check`] — may a gated capability run right now.
//!
//! A denial is **auditable, not silent**: [`crate::Harness::check_entitlement`]
//! records it on the session log as a `CheckResult` (`check_id:
//! "account:<capability>"`) with the reason in `details` and the structured
//! denial in `payload`, reusing the existing check vocabulary rather than
//! growing the event enum — old log readers keep parsing new logs.

use std::sync::Arc;

use nanobot_session::SessionEvent;

/// The identity snapshot a provider hands the framework. Display data only:
/// the framework never interprets `plan` beyond showing it, and never sees a
/// credential (tokens stay inside the product's provider).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// Stable identity string (a user id, a license id).
    pub id: String,
    /// What the shell shows next to the avatar.
    pub display_name: String,
    /// The commercial plan / tier label, when the product has one.
    pub plan: Option<String>,
    /// When the account's validity ends (unix ms), when it does at all.
    pub expires_at_ms: Option<u64>,
}

impl Account {
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            plan: None,
            expires_at_ms: None,
        }
    }

    /// Builder-style plan label.
    pub fn with_plan(mut self, plan: impl Into<String>) -> Self {
        self.plan = Some(plan.into());
        self
    }

    /// Builder-style expiry (unix ms).
    pub fn with_expiry_ms(mut self, expires_at_ms: u64) -> Self {
        self.expires_at_ms = Some(expires_at_ms);
        self
    }
}

/// Whether a gated capability may run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntitlementDecision {
    Allowed,
    /// The capability is refused. `reason` is operator-facing prose (it lands
    /// in the audit trail verbatim).
    Denied { reason: String },
}

impl EntitlementDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// The denial reason, when there is one.
    pub fn denied_reason(&self) -> Option<&str> {
        match self {
            Self::Allowed => None,
            Self::Denied { reason } => Some(reason),
        }
    }
}

/// The product's account backend. Implementations must be callable from any
/// thread the harness runs on and must **never block on the network** — a
/// snapshot question ([`account`](Self::account)) and an allow/deny question
/// ([`check`](Self::check)) have to answer from local state. Refreshing
/// credentials is the product's own (async) business, driven on the app
/// layer's schedule.
pub trait AccountProvider: Send + Sync {
    /// The identity snapshot, `None` when not signed in.
    fn account(&self) -> Option<Account>;

    /// Whether `capability` may run right now. Capabilities are plain
    /// strings the product and its provider agree on (`"agent.turn"`,
    /// `"export.gds"`); the framework only defines the ones it gates.
    fn check(&self, capability: &str) -> EntitlementDecision;
}

/// The default provider: not signed in, everything allowed. Existing
/// consumers (headless harnesses, tests, the selfcheck) keep their exact
/// behaviour under it.
#[derive(Clone, Copy, Debug, Default)]
pub struct AnonymousProvider;

impl AccountProvider for AnonymousProvider {
    fn account(&self) -> Option<Account> {
        None
    }

    fn check(&self, _capability: &str) -> EntitlementDecision {
        EntitlementDecision::Allowed
    }
}

/// A fixed account with an optional feature list — the embedding scenario
/// (a bundled entitlement) and the test stand-in for richer providers.
///
/// `check` denies when the account has expired, and — when `features` is
/// non-empty — when the capability is not named (or `"*"`). An empty feature
/// list allows everything: the provider makes no feature promises.
pub struct StaticProvider {
    account: Account,
    features: Vec<String>,
    /// Time override for deterministic expiry tests; real time when `None`.
    now_ms: Option<u64>,
}

impl StaticProvider {
    pub fn new(account: Account) -> Self {
        Self {
            account,
            features: Vec::new(),
            now_ms: None,
        }
    }

    pub fn with_features(mut self, features: impl IntoIterator<Item = String>) -> Self {
        self.features = features.into_iter().collect();
        self
    }

    /// Pins "now" (unix ms) — deterministic expiry tests.
    pub fn with_now_ms(mut self, now_ms: u64) -> Self {
        self.now_ms = Some(now_ms);
        self
    }

    fn now(&self) -> u64 {
        self.now_ms.unwrap_or_else(|| {
            web_time::SystemTime::now()
                .duration_since(web_time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        })
    }
}

impl AccountProvider for StaticProvider {
    fn account(&self) -> Option<Account> {
        Some(self.account.clone())
    }

    fn check(&self, capability: &str) -> EntitlementDecision {
        if let Some(expires_at) = self.account.expires_at_ms {
            if self.now() >= expires_at {
                return EntitlementDecision::Denied {
                    reason: format!(
                        "account `{}` expired at {expires_at} ms",
                        self.account.id
                    ),
                };
            }
        }
        if !self.features.is_empty()
            && !self.features.iter().any(|f| f == capability || f == "*")
        {
            return EntitlementDecision::Denied {
                reason: format!(
                    "capability `{capability}` is not in account `{}`'s entitlement",
                    self.account.id
                ),
            };
        }
        EntitlementDecision::Allowed
    }
}

/// The capability [`crate::run_one_turn`] gates: one agent turn.
pub const TURN_CAPABILITY: &str = "agent.turn";

/// Asks `provider` and returns the audit events a denial should leave on the
/// session log (empty when allowed): the run announcement plus the failed
/// result with the reason in both prose and structure.
pub fn denial_events(capability: &str, reason: &str) -> Vec<SessionEvent> {
    vec![
        SessionEvent::CheckRun {
            check_id: format!("account:{capability}"),
        },
        SessionEvent::CheckResult {
            check_id: format!("account:{capability}"),
            passed: false,
            details: format!("capability `{capability}` denied: {reason}"),
            payload: Some(serde_json::json!({
                "capability": capability,
                "denied": true,
                "reason": reason,
            })),
        },
    ]
}

/// Shared-provider alias, the way the builder stores it.
pub type SharedAccountProvider = Arc<dyn AccountProvider>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_anonymous_provider_changes_nothing() {
        let provider = AnonymousProvider;
        assert!(provider.account().is_none());
        assert!(provider.check("agent.turn").is_allowed());
        assert!(provider.check("anything.else").is_allowed());
    }

    #[test]
    fn a_static_account_reports_itself_and_allows_everything_by_default() {
        let provider = StaticProvider::new(Account::new("u1", "Alice").with_plan("pro"));
        assert_eq!(
            provider.account(),
            Some(Account::new("u1", "Alice").with_plan("pro"))
        );
        assert!(provider.check("agent.turn").is_allowed());
    }

    #[test]
    fn an_expired_account_is_denied_everything() {
        let provider = StaticProvider::new(
            Account::new("lic-1", "Site License").with_expiry_ms(1_000),
        )
        .with_now_ms(2_000);
        let decision = provider.check("agent.turn");
        assert!(!decision.is_allowed());
        assert!(
            decision.denied_reason().unwrap().contains("expired"),
            "the reason names the expiry: {:?}",
            decision.denied_reason()
        );
    }

    #[test]
    fn a_feature_list_gates_only_what_it_names() {
        let provider = StaticProvider::new(Account::new("u2", "Bob"))
            .with_features(["agent.turn".to_string(), "export.*".to_string()]);
        assert!(provider.check("agent.turn").is_allowed());
        assert!(!provider.check("sim.fem").is_allowed());

        let star = StaticProvider::new(Account::new("u3", "Carol"))
            .with_features(["*".to_string()]);
        assert!(star.check("sim.fem").is_allowed());
    }

    #[test]
    fn denial_events_speak_the_check_vocabulary() {
        let events = denial_events("agent.turn", "plan expired");
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], SessionEvent::CheckRun { check_id } if check_id == "account:agent.turn"));
        match &events[1] {
            SessionEvent::CheckResult { check_id, passed, details, payload } => {
                assert_eq!(check_id, "account:agent.turn");
                assert!(!passed);
                assert!(details.contains("plan expired"));
                assert_eq!(payload.as_ref().unwrap()["capability"], "agent.turn");
            }
            other => panic!("expected a CheckResult, got {other:?}"),
        }
    }
}
