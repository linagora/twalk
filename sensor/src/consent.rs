//! Consent: the data-processing agreement state of a contact or channel
//! (CONTEXT.md). The Companion Gateway is the single writer of consent
//! state (ADR 0006); the Sensor only labels events with the current state.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Granted,
    Pending,
    Revoked,
}

impl Consent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Pending => "pending",
            Self::Revoked => "revoked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_match_the_contract_strings() {
        assert_eq!(Consent::Granted.as_str(), "granted");
        assert_eq!(Consent::Pending.as_str(), "pending");
        assert_eq!(Consent::Revoked.as_str(), "revoked");
    }
}
