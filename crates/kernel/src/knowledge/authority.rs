//! Authority model — MRFC-0070 Phase A0.
//!
//! Authority ranks the trustworthiness of a Knowledge Object's source.
//! Authority ≠ Confidence (confidence is how certain the assertion is within its source).
//!
//! Extension key: `"authority"` — stored as the variant's snake_case name.

/// Eleven authority levels, ordered from most to least authoritative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Authority {
    /// Approved by a human with domain authority.
    HumanApproved,
    /// Derived from organizational policy (e.g. coding standards).
    OrganizationPolicy,
    /// Captured from an Architecture Decision Record.
    ArchitectureDecision,
    /// Extracted directly from source code (ground truth).
    SourceCode,
    /// Verified by a passing test.
    TestVerified,
    /// Verified by CI pipeline execution.
    CiVerified,
    /// Observed from a deployment/runtime.
    DeploymentObserved,
    /// Extracted from documentation (may be stale).
    Documentation,
    /// Derived by an agent through analysis.
    AgentDerived,
    /// Inferred by an LLM (lowest trusted AI source).
    LlmInferred,
    /// From an untrusted external source.
    UntrustedExternal,
}

impl Authority {
    /// Numeric rank: higher = more authoritative.
    /// HumanApproved = 10, UntrustedExternal = 0.
    pub fn rank(self) -> u8 {
        match self {
            Authority::HumanApproved => 10,
            Authority::OrganizationPolicy => 9,
            Authority::ArchitectureDecision => 8,
            Authority::SourceCode => 7,
            Authority::TestVerified => 6,
            Authority::CiVerified => 5,
            Authority::DeploymentObserved => 4,
            Authority::Documentation => 3,
            Authority::AgentDerived => 2,
            Authority::LlmInferred => 1,
            Authority::UntrustedExternal => 0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Authority::HumanApproved => "human_approved",
            Authority::OrganizationPolicy => "organization_policy",
            Authority::ArchitectureDecision => "architecture_decision",
            Authority::SourceCode => "source_code",
            Authority::TestVerified => "test_verified",
            Authority::CiVerified => "ci_verified",
            Authority::DeploymentObserved => "deployment_observed",
            Authority::Documentation => "documentation",
            Authority::AgentDerived => "agent_derived",
            Authority::LlmInferred => "llm_inferred",
            Authority::UntrustedExternal => "untrusted_external",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "human_approved" => Some(Authority::HumanApproved),
            "organization_policy" => Some(Authority::OrganizationPolicy),
            "architecture_decision" => Some(Authority::ArchitectureDecision),
            "source_code" => Some(Authority::SourceCode),
            "test_verified" => Some(Authority::TestVerified),
            "ci_verified" => Some(Authority::CiVerified),
            "deployment_observed" => Some(Authority::DeploymentObserved),
            "documentation" => Some(Authority::Documentation),
            "agent_derived" => Some(Authority::AgentDerived),
            "llm_inferred" => Some(Authority::LlmInferred),
            "untrusted_external" => Some(Authority::UntrustedExternal),
            _ => None,
        }
    }
}

/// Configurable precedence policy for authority filtering.
/// Maps each authority level to a weight used in the context ranking formula:
///   `score = relevance × authority_weight × freshness_weight × ...`
#[derive(Clone, Debug)]
pub struct AuthorityRanking {
    weights: [f32; 11],
}

impl Default for AuthorityRanking {
    fn default() -> Self {
        // Sensible defaults: higher authority = higher weight
        let mut w = [0.0f32; 11];
        w[Authority::HumanApproved.rank() as usize] = 1.0;
        w[Authority::OrganizationPolicy.rank() as usize] = 0.95;
        w[Authority::ArchitectureDecision.rank() as usize] = 0.90;
        w[Authority::SourceCode.rank() as usize] = 0.85;
        w[Authority::TestVerified.rank() as usize] = 0.80;
        w[Authority::CiVerified.rank() as usize] = 0.75;
        w[Authority::DeploymentObserved.rank() as usize] = 0.70;
        w[Authority::Documentation.rank() as usize] = 0.50;
        w[Authority::AgentDerived.rank() as usize] = 0.35;
        w[Authority::LlmInferred.rank() as usize] = 0.15;
        w[Authority::UntrustedExternal.rank() as usize] = 0.05;
        AuthorityRanking { weights: w }
    }
}

impl AuthorityRanking {
    pub fn weight(&self, a: Authority) -> f32 {
        self.weights[a.rank() as usize]
    }

    pub fn set_weight(&mut self, a: Authority, w: f32) {
        self.weights[a.rank() as usize] = w;
    }

    /// Filter KOs: only keep those with authority at or above `min_authority`.
    pub fn filter<T>(&self, items: Vec<(Authority, T)>, min_authority: Authority) -> Vec<T> {
        let threshold = min_authority.rank();
        items
            .into_iter()
            .filter(|(a, _)| a.rank() >= threshold)
            .map(|(_, v)| v)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_ranking_is_monotonic() {
        // Higher rank = more authoritative
        assert!(Authority::HumanApproved.rank() > Authority::SourceCode.rank());
        assert!(Authority::SourceCode.rank() > Authority::Documentation.rank());
        assert!(Authority::Documentation.rank() > Authority::LlmInferred.rank());
        assert!(Authority::LlmInferred.rank() > Authority::UntrustedExternal.rank());
    }

    #[test]
    fn authority_round_trip() {
        for a in [
            Authority::HumanApproved,
            Authority::SourceCode,
            Authority::UntrustedExternal,
        ] {
            let s = a.as_str();
            let back = Authority::from_str(s);
            assert_eq!(back, Some(a));
        }
    }

    #[test]
    fn ranking_filter_respects_threshold() {
        let ranking = AuthorityRanking::default();
        let items = vec![
            (Authority::SourceCode, "rust"),
            (Authority::Documentation, "readme"),
            (Authority::LlmInferred, "guess"),
        ];
        let filtered = ranking.filter(items, Authority::Documentation);
        assert_eq!(filtered, vec!["rust", "readme"]);
    }
}
