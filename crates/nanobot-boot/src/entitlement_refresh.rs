//! When to ask the product for the latest entitlements, and what a change
//! means for the session already running.
//!
//! The schedule and the mid-session grace live in `product_kit`, shared with
//! the workbench framework. This module keeps the nanobot-facing wrapper: it
//! answers the [`AccountProvider`] question through the grace and forwards
//! the identity snapshot.

use std::sync::Arc;

use product_kit::{EntitlementAnswer, EntitlementSource, SessionGrace as Grace};

use crate::account::{Account, AccountProvider, EntitlementDecision};

impl EntitlementAnswer for EntitlementDecision {
    fn is_allowed(&self) -> bool {
        EntitlementDecision::is_allowed(self)
    }

    fn allowed() -> Self {
        EntitlementDecision::Allowed
    }
}

impl EntitlementSource for dyn AccountProvider {
    type Answer = EntitlementDecision;

    fn check(&self, capability: &str) -> EntitlementDecision {
        AccountProvider::check(self, capability)
    }
}

/// The product's answers, except that what the user could use stays usable
/// until the app closes: a plan lapsing mid-session must not take away what
/// they are working in. The next launch starts from the product's answers.
///
/// Wrap the product's provider in one and hand that to the harness; call
/// [`settle`](Self::settle) when the first refresh finishes and
/// [`forget`](Self::forget) when who is signed in changes.
pub struct SessionGrace {
    grace: Grace<dyn AccountProvider>,
}

impl SessionGrace {
    pub fn new(product: Arc<dyn AccountProvider>) -> Self {
        Self {
            grace: Grace::new(product),
        }
    }

    /// The latest answers are in; from here on what is allowed is kept.
    pub fn settle(&self) {
        self.grace.settle();
    }

    /// Someone else is signed in: nothing carries over, and their answers
    /// are not known until the next check finishes.
    pub fn forget(&self) {
        self.grace.forget();
    }
}

impl AccountProvider for SessionGrace {
    fn account(&self) -> Option<Account> {
        self.grace.product().account()
    }

    fn check(&self, capability: &str) -> EntitlementDecision {
        self.grace.check(capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Product {
        capabilities: Vec<&'static str>,
        account: Option<Account>,
    }

    impl AccountProvider for Product {
        fn account(&self) -> Option<Account> {
            self.account.clone()
        }

        fn check(&self, capability: &str) -> EntitlementDecision {
            if self.capabilities.contains(&capability) {
                EntitlementDecision::Allowed
            } else {
                EntitlementDecision::Denied {
                    reason: "not in the plan".to_string(),
                }
            }
        }
    }

    fn session(product: Arc<Product>) -> SessionGrace {
        SessionGrace::new(product)
    }

    #[test]
    fn the_identity_snapshot_reaches_the_harness() {
        let account = Account::new("u1", "Ada");
        let product = Arc::new(Product {
            capabilities: vec!["agent.turn"],
            account: Some(account.clone()),
        });
        let session = session(product);
        assert_eq!(session.account(), Some(account));
    }

    #[test]
    fn a_denial_keeps_its_reason_and_the_grace_comes_from_product_kit() {
        let product = Arc::new(Product {
            capabilities: vec!["agent.turn"],
            account: None,
        });
        let session = session(product);
        session.settle();
        assert_eq!(session.check("agent.turn"), EntitlementDecision::Allowed);
        assert_eq!(
            session.check("export.gds"),
            EntitlementDecision::Denied {
                reason: "not in the plan".to_string()
            }
        );
    }
}
