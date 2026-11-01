//! Native subscription advice stays closed until executable-tool restrictions are proven.
//!
//! Codex 0.159 exposes account/model discovery, but no supported pre-inference
//! receipt for the complete effective tool registry. Environment selection and
//! output schemas do not establish that prerequisite. Never start a canary turn
//! or accept a caller-supplied qualification flag as a substitute.
use super::types::Model_advice;

pub const QUALIFICATION_REASON: &str = "tool_restriction_unverified";

pub fn unavailable_advice() -> Model_advice {
    Model_advice {
        status: QUALIFICATION_REASON.into(),
        source: "native_codex_subscription".into(),
        model: String::new(),
        confidence: None,
        summary: "Native GPT-6 Luna advice is unavailable: the installed app-server cannot yet provide a verified pre-inference restriction on executable tools. No inference turn was started; the deterministic policy remains authoritative.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_tool_contract_never_claims_accepted_model_or_confidence() {
        let advice = unavailable_advice();
        assert_eq!(advice.status, "tool_restriction_unverified");
        assert!(advice.model.is_empty());
        assert!(advice.confidence.is_none());
    }
}
