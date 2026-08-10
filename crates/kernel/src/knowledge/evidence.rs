//! Evidence standardization — MRFC-0070 Phase A0.
//!
//! Every derived Knowledge Object carries a structured evidence trail
//! recording its source artifact, location, revision, extraction method,
//! and confidence score.

/// Structured evidence trail for derived knowledge.
///
/// Stored as a JSON object in the KO's extensions under key `"evidence"`,
/// or as a top-level property block for Evidence-typed KOs.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    /// Path to the source artifact (file, URL, KOID).
    pub source_artifact: String,

    /// Location within the artifact (line range, section heading, function name).
    pub location: Option<String>,

    /// Source revision (git commit, document version, KO version).
    pub revision: Option<String>,

    /// Method used to extract/derive the knowledge.
    pub method: EvidenceMethod,

    /// Confidence in this specific piece of evidence (0.0–1.0).
    pub confidence: f32,
}

/// How the evidence was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EvidenceMethod {
    /// Parsed from source code AST.
    AstExtraction,
    /// Extracted from Markdown/documentation.
    DocExtraction,
    /// Observed from test execution.
    TestObservation,
    /// Observed from CI pipeline.
    CiObservation,
    /// Observed from deployment/runtime.
    RuntimeObservation,
    /// Derived by an agent through analysis.
    AgentAnalysis,
    /// Inferred by an LLM.
    LlmInference,
    /// Provided by a human.
    HumanProvided,
    /// Computed/derived from other KOs.
    Derivation,
}

impl EvidenceMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceMethod::AstExtraction => "ast_extraction",
            EvidenceMethod::DocExtraction => "doc_extraction",
            EvidenceMethod::TestObservation => "test_observation",
            EvidenceMethod::CiObservation => "ci_observation",
            EvidenceMethod::RuntimeObservation => "runtime_observation",
            EvidenceMethod::AgentAnalysis => "agent_analysis",
            EvidenceMethod::LlmInference => "llm_inference",
            EvidenceMethod::HumanProvided => "human_provided",
            EvidenceMethod::Derivation => "derivation",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ast_extraction" => Some(EvidenceMethod::AstExtraction),
            "doc_extraction" => Some(EvidenceMethod::DocExtraction),
            "test_observation" => Some(EvidenceMethod::TestObservation),
            "ci_observation" => Some(EvidenceMethod::CiObservation),
            "runtime_observation" => Some(EvidenceMethod::RuntimeObservation),
            "agent_analysis" => Some(EvidenceMethod::AgentAnalysis),
            "llm_inference" => Some(EvidenceMethod::LlmInference),
            "human_provided" => Some(EvidenceMethod::HumanProvided),
            "derivation" => Some(EvidenceMethod::Derivation),
            _ => None,
        }
    }
}

const DEFAULT_CONFIDENCE: f32 = 0.8;

impl Evidence {
    pub fn new(source_artifact: impl Into<String>, method: EvidenceMethod) -> Self {
        Evidence {
            source_artifact: source_artifact.into(),
            location: None,
            revision: None,
            method,
            confidence: DEFAULT_CONFIDENCE,
        }
    }

    pub fn with_location(mut self, loc: impl Into<String>) -> Self {
        self.location = Some(loc.into());
        self
    }

    pub fn with_revision(mut self, rev: impl Into<String>) -> Self {
        self.revision = Some(rev.into());
        self
    }

    pub fn with_confidence(mut self, c: f32) -> Self {
        self.confidence = c.clamp(0.0, 1.0);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_builder_produces_correct_struct() {
        let ev = Evidence::new("src/main.rs", EvidenceMethod::AstExtraction)
            .with_location("lines 42-58")
            .with_revision("abc123def")
            .with_confidence(0.95);
        assert_eq!(ev.source_artifact, "src/main.rs");
        assert_eq!(ev.location.as_deref(), Some("lines 42-58"));
        assert_eq!(ev.revision.as_deref(), Some("abc123def"));
        assert_eq!(ev.method, EvidenceMethod::AstExtraction);
        assert!((ev.confidence - 0.95).abs() < 0.001);
    }

    #[test]
    fn evidence_method_round_trip() {
        for m in [
            EvidenceMethod::AstExtraction,
            EvidenceMethod::HumanProvided,
            EvidenceMethod::LlmInference,
        ] {
            let s = m.as_str();
            let back = EvidenceMethod::from_str(s);
            assert_eq!(back, Some(m));
        }
    }
}
