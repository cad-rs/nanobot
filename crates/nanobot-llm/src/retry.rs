//! Retry policy for provider requests (U33).
//!
//! A provider request fails for two different reasons: the network dropped, or
//! the provider answered and the answer was an error. Only the first kind is
//! worth repeating — re-sending a request the provider rejected with a 4xx
//! cannot change the answer, and a retry storm against a rate limit makes it
//! worse.
//!
//! The policy is data, not behaviour: [`RetryPolicy`] says how many attempts,
//! how long to back off, and which failures are transient. The adapter applies
//! it. That split is what lets a product tune it (a CI run retries less than a
//! paying customer) without the framework having an opinion about its hosts.

use std::time::Duration;

/// How many failures are worth repeating, and how long to wait between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retry attempts after the first one. `0` means one attempt, no retry.
    pub max_retries: u32,
    /// Delay before the first retry; each later retry doubles it.
    pub base_delay: Duration,
    /// Upper bound on a single delay, so a long retry chain cannot stall.
    pub max_delay: Duration,
    /// Total budget for the request including retries. `None` is unbounded.
    pub timeout: Option<Duration>,
    /// Which failures are transient.
    pub on: RetryOn,
}

/// Which failures are worth repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOn {
    /// Never retry.
    Nothing,
    /// Transport failures and 5xx. A 4xx is the provider's answer, not a blip.
    Transient,
}

impl Default for RetryPolicy {
    /// One attempt, no retry.
    ///
    /// The default is deliberately inert: a product opts in to retrying, and
    /// the framework does not silently re-send a request on its own.
    fn default() -> Self {
        Self {
            max_retries: 0,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
            timeout: None,
            on: RetryOn::Nothing,
        }
    }
}

impl RetryPolicy {
    /// A policy that retries transient failures up to `max_retries` times.
    pub fn transient(max_retries: u32) -> Self {
        Self {
            max_retries,
            on: RetryOn::Transient,
            ..Self::default()
        }
    }

    /// Whether a request that has failed `attempt` times (0 = the first failure)
    /// should be tried again.
    pub fn should_retry(&self, attempt: u32) -> bool {
        self.on != RetryOn::Nothing && attempt < self.max_retries
    }

    /// Whether an HTTP status is worth repeating.
    ///
    /// 429 and 5xx are transient; every other 4xx is the provider's answer.
    pub fn is_transient_status(&self, status: u16) -> bool {
        self.on == RetryOn::Transient && (status == 429 || (500..600).contains(&status))
    }

    /// How long to wait before retry number `attempt` (1 = the first retry).
    ///
    /// The first retry waits `base_delay`; each later one doubles it, capped at
    /// `max_delay`.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let multiplier = 2u64.saturating_pow(attempt.saturating_sub(1).min(16));
        let delay = self.base_delay.saturating_mul(multiplier as u32);
        delay.min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            timeout: None,
            on: RetryOn::Transient,
        }
    }

    #[test]
    fn the_default_never_retries() {
        let policy = RetryPolicy::default();
        assert!(!policy.should_retry(0), "the default is inert");
        assert!(!policy.is_transient_status(500));
    }

    #[test]
    fn retries_stop_at_the_budget() {
        let policy = policy();
        assert!(policy.should_retry(0));
        assert!(policy.should_retry(1));
        assert!(policy.should_retry(2));
        assert!(!policy.should_retry(3), "three retries means four attempts");
    }

    #[test]
    fn only_transient_statuses_are_retried() {
        let policy = policy();
        for status in [429, 500, 502, 503, 504] {
            assert!(policy.is_transient_status(status), "{status} is transient");
        }
        for status in [400, 401, 403, 404, 409, 422] {
            assert!(
                !policy.is_transient_status(status),
                "{status} is the provider's answer"
            );
        }
    }

    #[test]
    fn the_backoff_doubles_and_is_capped() {
        let policy = policy();
        assert_eq!(policy.delay_for(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for(2), Duration::from_millis(200));
        assert_eq!(policy.delay_for(3), Duration::from_millis(400));
        assert_eq!(policy.delay_for(4), Duration::from_millis(800));
        assert_eq!(policy.delay_for(5), Duration::from_millis(1_600));
        assert_eq!(policy.delay_for(6), Duration::from_secs(2), "3.2s capped at 2s");
        assert_eq!(policy.delay_for(9), policy.max_delay, "capped");
    }
}
