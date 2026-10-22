//! Tool contracts: what a tool promises about the subject it acts on.
//!
//! The framework keeps only the two promises every harness needs — a tool
//! either leaves its subject alone or changes it — plus one open slot for the
//! product's own, stronger invariants. The framework never interprets a
//! `Custom` contract: it carries the name through to the product's write
//! verifier (see the architecture doc, decision C).

use serde::{Deserialize, Serialize};

/// What a tool promises about the subject it acts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolContract {
    /// Must not change the subject at all.
    ReadOnly,
    /// Changes the subject; no stronger structural promise.
    Mutating,
    /// A **product-defined** contract (e.g. a CAD harness's
    /// `adds_one_feature`). The framework does not interpret it — it only
    /// passes the name to the product's verifier, which knows what the
    /// promise means and how to check it.
    Custom(String),
}

impl ToolContract {
    /// The stable name, for the check panel and the audit trail.
    pub fn name(&self) -> &str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Mutating => "mutating",
            Self::Custom(name) => name.as_str(),
        }
    }
}

impl std::fmt::Display for ToolContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
