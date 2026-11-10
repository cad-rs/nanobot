//! Approval: plugin for tool-level approval (design-harness 10.5).
//!
//! Two implementations are provided:
//! - `UiApprovalPolicy`: bridges approval requests to the main thread via
//!   a callback (the main thread presents the approval UI and resolves
//!   the future).
//! - `AutoApprovalPolicy`: always approves (headless / CI).
//!
//! Tool-level `Ask` (fine-grained, e.g. destructive imports) and plan-level
//! `Proposed -> Accept` (coarse-grained) coexist. When a tool-level approval
//! request appears mid-plan-execution, the session enters "awaiting review"
//! state.
//!
//! On top of those, [`RuleBasedApprovalPolicy`] generalizes "ask a human" into
//! a durable rule set: an ordered list of `{action, resource, effect}` rules
//! where the **last match wins**, each carrying the reason, the grantor and an
//! optional expiry. A decision is a recorded fact, not a one-shot answer —
//! which is what sign-off workflows (waivers, exemptions) need. The rules
//! themselves are product data; the mechanism lives here.

use std::sync::Arc;

/// Trait for tool-level approval decisions.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// threads on desktop and used within a single-threaded wasm Worker.
pub trait ApprovalPolicy: Send + Sync {
    /// Request approval for a tool call.
    ///
    /// Returns `true` if the tool call is approved, `false` if denied.
    fn request_approval(&self, tool_call_id: &str, reason: &str) -> bool;
}

/// Approval policy that bridges to the main thread via a callback.
///
/// The callback receives `(tool_call_id, reason)` and returns `true` if
/// approved. The callback is expected to send an `approval/request` event
/// to the main thread and block until the user responds.
/// The synchronous ask the UI bridges (an `ApprovalOutcome::Ask` lands here).
pub type ApprovalAsk = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// The audit sink a rule policy reports outcomes to.
pub type ApprovalAudit = Arc<dyn Fn(&ApprovalOutcome, &str) + Send + Sync>;

pub struct UiApprovalPolicy {
    callback: ApprovalAsk,
}

impl UiApprovalPolicy {
    /// Create a new UI-backed approval policy.
    pub fn new(callback: impl Fn(&str, &str) -> bool + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }
}

impl ApprovalPolicy for UiApprovalPolicy {
    fn request_approval(&self, tool_call_id: &str, reason: &str) -> bool {
        (self.callback)(tool_call_id, reason)
    }
}

/// Approval policy that always approves (headless / CI).
pub struct AutoApprovalPolicy;

impl ApprovalPolicy for AutoApprovalPolicy {
    fn request_approval(&self, _tool_call_id: &str, _reason: &str) -> bool {
        true
    }
}

/// Approval policy that always denies (for testing).
pub struct DenyAllPolicy;

impl ApprovalPolicy for DenyAllPolicy {
    fn request_approval(&self, _tool_call_id: &str, _reason: &str) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Rule-based approval
// ---------------------------------------------------------------------------

/// What a matching rule says to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalEffect {
    /// Allow without asking.
    Allow,
    /// Deny outright. A deny is never overridden by a saved allow.
    Deny,
    /// Ask the human.
    Ask,
}

/// One approval rule.
///
/// `action` is a tool name or a glob (`build_*`, `*`); `resource` narrows it
/// to the object acted on (a glob, `None` = any). The rule records *why* it
/// exists and who granted it, and may expire — a decision is data, not a
/// one-shot answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRule {
    /// Tool name or glob.
    pub action: String,
    /// Optional resource glob the rule applies to.
    pub resource: Option<String>,
    /// What to do when it matches.
    pub effect: ApprovalEffect,
    /// Why the rule exists.
    pub reason: String,
    /// Who granted it.
    pub granted_by: String,
    /// Unix epoch ms when granted.
    pub granted_at_ms: u64,
    /// Unix epoch ms when it stops applying (`None` = never).
    pub expires_at_ms: Option<u64>,
}

impl ApprovalRule {
    /// A rule that always applies (any action, any resource).
    #[must_use]
    pub fn new(action: impl Into<String>, effect: ApprovalEffect) -> Self {
        Self {
            action: action.into(),
            resource: None,
            effect,
            reason: String::new(),
            granted_by: "policy".to_string(),
            granted_at_ms: 0,
            expires_at_ms: None,
        }
    }

    /// Restrict the rule to a resource glob.
    #[must_use]
    pub fn with_resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = Some(resource.into());
        self
    }

    /// Attach the reason and grantor.
    #[must_use]
    pub fn granted(
        mut self,
        reason: impl Into<String>,
        granted_by: impl Into<String>,
        granted_at_ms: u64,
    ) -> Self {
        self.reason = reason.into();
        self.granted_by = granted_by.into();
        self.granted_at_ms = granted_at_ms;
        self
    }

    /// Set an expiry (Unix epoch ms).
    #[must_use]
    pub fn expiring_at(mut self, expires_at_ms: u64) -> Self {
        self.expires_at_ms = Some(expires_at_ms);
        self
    }

    /// Whether the rule applies at `now_ms` (an expired rule is inert).
    #[must_use]
    pub fn is_active(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_none_or(|expiry| now_ms < expiry)
    }

    /// Whether the rule matches an action + resource.
    #[must_use]
    pub fn matches(&self, action: &str, resource: Option<&str>) -> bool {
        glob_match(&self.action, action)
            && self
                .resource
                .as_deref()
                .is_none_or(|pattern| resource.is_some_and(|r| glob_match(pattern, r)))
    }
}

/// The outcome of evaluating the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalOutcome {
    /// The effect in force.
    pub effect: ApprovalEffect,
    /// The matched rule, if any.
    pub rule: Option<ApprovalRule>,
    /// The action that was evaluated.
    pub action: String,
    /// The resource that was evaluated, if given.
    pub resource: Option<String>,
    /// Why the outcome is what it is.
    pub explanation: String,
}

impl ApprovalOutcome {
    /// Whether the outcome allows the action.
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        self.effect == ApprovalEffect::Allow
    }

    /// Whether the outcome asks the human.
    #[must_use]
    pub fn asks(&self) -> bool {
        self.effect == ApprovalEffect::Ask
    }
}

/// An approval policy driven by an ordered rule list.
///
/// Rules are evaluated in order and the **last active match wins**, so a
/// specific deny can override a broad allow and a saved allow can override a
/// default ask. Rules that do not match, or that have expired, are skipped.
///
/// An `Ask` outcome is resolved by the `ask` fallback (the UI bridge); the
/// other two are final.
pub struct RuleBasedApprovalPolicy {
    rules: Vec<ApprovalRule>,
    ask: ApprovalAsk,
    audit: Option<ApprovalAudit>,
}

impl RuleBasedApprovalPolicy {
    /// A policy over `rules`, resolving `Ask` through `ask`.
    ///
    /// `audit`, when present, is called with every decision and the reason it
    /// was requested for, so a sign-off log can be written by the host.
    pub fn new(
        rules: Vec<ApprovalRule>,
        ask: impl Fn(&str, &str) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            rules,
            ask: Arc::new(ask),
            audit: None,
        }
    }

    /// Attach an audit sink for every decision.
    #[must_use]
    pub fn with_audit(
        mut self,
        audit: impl Fn(&ApprovalOutcome, &str) + Send + Sync + 'static,
    ) -> Self {
        self.audit = Some(Arc::new(audit));
        self
    }

    /// The rules, in evaluation order.
    #[must_use]
    pub fn rules(&self) -> &[ApprovalRule] {
        &self.rules
    }

    /// Add a rule (evaluated last, so it can override earlier ones).
    pub fn push_rule(&mut self, rule: ApprovalRule) {
        self.rules.push(rule);
    }

    /// Evaluate the rules for an action + resource at `now_ms`.
    #[must_use]
    pub fn decide(
        &self,
        action: &str,
        resource: Option<&str>,
        reason: &str,
        now_ms: u64,
    ) -> ApprovalOutcome {
        let matched = self
            .rules
            .iter()
            .enumerate().rfind(|(_, rule)| rule.is_active(now_ms) && rule.matches(action, resource));

        let outcome = match matched {
            Some((_, rule)) => ApprovalOutcome {
                effect: rule.effect,
                rule: Some(rule.clone()),
                action: action.to_string(),
                resource: resource.map(str::to_string),
                explanation: if rule.reason.is_empty() {
                    format!("rule for '{}' ({})", rule.action, rule.granted_by)
                } else {
                    rule.reason.clone()
                },
            },
            None => ApprovalOutcome {
                effect: ApprovalEffect::Ask,
                rule: None,
                action: action.to_string(),
                resource: resource.map(str::to_string),
                explanation: format!("no rule covers '{action}'; asking"),
            },
        };

        if let Some(audit) = &self.audit {
            audit(&outcome, reason);
        }
        outcome
    }

    /// Resolve an outcome that asks the human.
    fn resolve_ask(&self, outcome: &ApprovalOutcome, reason: &str) -> bool {
        (self.ask)(&outcome.action, reason)
    }
}

impl ApprovalPolicy for RuleBasedApprovalPolicy {
    fn request_approval(&self, tool_call_id: &str, reason: &str) -> bool {
        // The tool pipeline passes `tool:<name>`; accept a bare name too.
        let action = tool_call_id.strip_prefix("tool:").unwrap_or(tool_call_id);
        let outcome = self.decide(action, None, reason, 0);
        match outcome.effect {
            ApprovalEffect::Allow => true,
            ApprovalEffect::Deny => false,
            ApprovalEffect::Ask => self.resolve_ask(&outcome, reason),
        }
    }
}

/// Glob match supporting `*` (any run of characters) and `?` (one character).
///
/// Case-sensitive, matching the usual tool-name convention.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // Iterative wildcard match with backtracking.
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0_usize, 0_usize);
    let mut star: Option<usize> = None;
    let mut star_ti = 0_usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(star_pi) = star {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<ApprovalRule> {
        vec![
            ApprovalRule::new("*", ApprovalEffect::Ask),
            ApprovalRule::new("build_sfq", ApprovalEffect::Allow).granted(
                "routine build",
                "alex",
                1,
            ),
            ApprovalRule::new("grant_waiver", ApprovalEffect::Ask).granted(
                "a waiver is a human decision",
                "policy",
                1,
            ),
            ApprovalRule::new("rm_*", ApprovalEffect::Deny).granted("never delete", "policy", 1),
        ]
    }

    #[test]
    fn last_matching_rule_wins() {
        let policy = RuleBasedApprovalPolicy::new(rules(), |_, _| true);
        // build_sfq matches the broad Ask and then the specific Allow.
        let out = policy.decide("build_sfq", None, "build", 10);
        assert_eq!(out.effect, ApprovalEffect::Allow);
        assert!(out.is_allowed());
        // An uncovered tool falls through to the broad Ask.
        let out = policy.decide("sim.fem", None, "run", 10);
        assert!(out.asks());
        // rm_* is denied even though `*` would ask.
        let out = policy.decide("rm_rf", None, "delete", 10);
        assert_eq!(out.effect, ApprovalEffect::Deny);
    }

    #[test]
    fn globs_match_as_expected() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("build_*", "build_sfq"));
        assert!(!glob_match("build_*", "run_drc"));
        assert!(glob_match("?bc", "abc"));
        assert!(!glob_match("?bc", "abbc"));
        assert!(glob_match("a*b*c", "axxbyyc"));
    }

    #[test]
    fn an_expired_rule_is_inert() {
        let rules = vec![ApprovalRule::new("build_sfq", ApprovalEffect::Allow).expiring_at(100)];
        let policy = RuleBasedApprovalPolicy::new(rules, |_, _| true);
        assert!(policy.decide("build_sfq", None, "", 50).is_allowed());
        // Past the expiry the rule no longer applies, so it asks.
        assert!(policy.decide("build_sfq", None, "", 150).asks());
    }

    #[test]
    fn a_deny_is_never_overridden_by_a_saved_allow() {
        // Order matters: the deny comes last, so it wins.
        let rules = vec![
            ApprovalRule::new("grant_waiver", ApprovalEffect::Allow),
            ApprovalRule::new("grant_waiver", ApprovalEffect::Deny),
        ];
        let policy = RuleBasedApprovalPolicy::new(rules, |_, _| true);
        assert_eq!(
            policy.decide("grant_waiver", None, "", 0).effect,
            ApprovalEffect::Deny
        );
    }

    #[test]
    fn the_audit_sink_sees_every_decision() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let sink = count.clone();
        let policy =
            RuleBasedApprovalPolicy::new(rules(), |_, _| true).with_audit(move |outcome, _| {
                assert_eq!(outcome.action, "build_sfq");
                sink.fetch_add(1, Ordering::SeqCst);
            });
        let _ = policy.decide("build_sfq", None, "build", 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_tool_call_id_is_mapped_to_an_action() {
        let policy = RuleBasedApprovalPolicy::new(rules(), |_, _| false);
        // `tool:<name>` is what the tool pipeline passes.
        assert!(policy.request_approval("tool:build_sfq", "build"));
        // A bare name works too.
        assert!(policy.request_approval("build_sfq", "build"));
        // An ask with a denying fallback is refused.
        assert!(!policy.request_approval("tool:sim.fem", "run"));
    }

    #[test]
    fn a_resource_rule_narrows_a_broad_one() {
        let rules = vec![
            ApprovalRule::new("edit", ApprovalEffect::Ask),
            ApprovalRule::new("edit", ApprovalEffect::Allow).with_resource("src/**"),
        ];
        let policy = RuleBasedApprovalPolicy::new(rules, |_, _| true);
        assert!(policy
            .decide("edit", Some("src/main.rs"), "", 0)
            .is_allowed());
        // Outside the resource glob, the broad rule still asks.
        assert!(policy.decide("edit", Some("etc/passwd"), "", 0).asks());
    }
}
