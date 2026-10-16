//! Gate-specific receipts for alignment holds (plan §9.4, P0 contracts §6.4).
//!
//! The native replay receipt (`ApprovedConfirmationAction`) is an exact
//! string match on the act's canonical JSON. In the main confirmation path a
//! matching receipt skips approval-rule, criticality and the model's
//! confirmation flag — so reusing that bare string for an alignment answer
//! would grant consent the owner never gave.
//!
//! Alignment receipts are therefore the same canonical JSON under a versioned
//! purpose prefix:
//!
//! - an alignment receipt can never equal a bare legacy token, so it never
//!   satisfies approval, criticality or any other gate;
//! - a legacy token never carries the prefix, so no other gate's approval can
//!   satisfy alignment;
//! - the persisted pause shape is unchanged — the prefix rides in the existing
//!   `confirmation_action_json` field — so no stored pause needs migrating and
//!   existing effect ids and retry keys are untouched.
//!
//! Each receipt is consumed once, bound to the owner snapshot, by the existing
//! matcher.

use serde::{Deserialize, Serialize};

pub const ALIGNMENT_EXCEPTION_PREFIX: &str = "alignment_exception.v1:";
pub const ALIGNMENT_CHECK_WAIVER_PREFIX: &str = "alignment_check_waiver.v1:";

/// What an owner's confirmation of an alignment hold resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentReceiptPurpose {
    /// `allow_once`: an informed exception to the cited intentions for this
    /// exact act. The intentions stay as they are.
    Exception,
    /// `proceed_without_check`: the owner chose to continue although the check
    /// could not run (plan §11.1). Not an alignment verdict.
    CheckWaiver,
}

impl AlignmentReceiptPurpose {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Exception => ALIGNMENT_EXCEPTION_PREFIX,
            Self::CheckWaiver => ALIGNMENT_CHECK_WAIVER_PREFIX,
        }
    }
}

/// The receipt token for `purpose` over an act's canonical confirmation JSON.
/// `None` for an empty canonical form: there is no binding to receipt.
pub fn receipt_token(
    purpose: AlignmentReceiptPurpose,
    canonical_action_json: &str,
) -> Option<String> {
    if canonical_action_json.is_empty() {
        return None;
    }
    Some(format!("{}{canonical_action_json}", purpose.prefix()))
}

/// Which alignment purpose a stored token carries, if any.
pub fn token_purpose(token: &str) -> Option<AlignmentReceiptPurpose> {
    [
        AlignmentReceiptPurpose::Exception,
        AlignmentReceiptPurpose::CheckWaiver,
    ]
    .into_iter()
    .find(|purpose| token.starts_with(purpose.prefix()))
}

/// The canonical act JSON under an alignment token: what a bare approval
/// for the same act matches. Used only when a combined ask presented that
/// approval and the owner gave the ask's consent.
pub fn canonical_of(token: &str) -> Option<&str> {
    token_purpose(token).map(|purpose| &token[purpose.prefix().len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alignment_receipt_never_equals_a_legacy_token() {
        let canonical = r#"{"type":"pack","capability_name":"shopping"}"#;
        for purpose in [
            AlignmentReceiptPurpose::Exception,
            AlignmentReceiptPurpose::CheckWaiver,
        ] {
            let token = receipt_token(purpose, canonical).expect("token");
            assert_ne!(token, canonical);
            assert_eq!(token_purpose(&token), Some(purpose));
        }
        assert_eq!(token_purpose(canonical), None);
        let token = receipt_token(AlignmentReceiptPurpose::Exception, canonical).expect("token");
        assert_eq!(canonical_of(&token), Some(canonical));
        assert_eq!(canonical_of(canonical), None);
    }

    #[test]
    fn an_empty_binding_has_no_receipt() {
        assert!(receipt_token(AlignmentReceiptPurpose::Exception, "").is_none());
    }

    #[test]
    fn purposes_do_not_satisfy_each_other() {
        let canonical = r#"{"a":1}"#;
        assert_ne!(
            receipt_token(AlignmentReceiptPurpose::Exception, canonical),
            receipt_token(AlignmentReceiptPurpose::CheckWaiver, canonical)
        );
    }
}
