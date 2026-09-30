use once_cell::sync::Lazy;
use regex::RegexSet;

/// Conservative, affirmatively-sensitive SUBJECT patterns (OTP /
/// verification code / 2FA / password reset / banking alerts). The list
/// errs toward precision — a missed sensitive row is recoverable (the
/// review surface still gates memory), a suppressed normal row loses
/// signal. Matching rows keep ids + sender; only the subject is redacted.
const SENSITIVE_SUBJECT_PATTERNS: &[&str] = &[
    r"(?i)\botp\b",
    r"(?i)\bone[\s-]?time\s+(pass(word|code)?|pin)\b",
    r"(?i)\bverification\s+code\b",
    r"(?i)\bsecurity\s+code\b",
    r"(?i)\b2fa\b",
    r"(?i)\btwo[\s-]?factor\b",
    r"(?i)\b(login|sign[\s-]?in)\s+code\b",
    r"(?i)\bauth(entication)?\s+code\b",
    r"(?i)\byour\s+code\s+is\b",
    r"(?i)\bcode\s*[:\-]\s*\d{4,8}\b",
    r"(?i)\bpassword\s+reset\b",
    r"(?i)\btransaction\s+(alert|otp)\b",
    r"(?i)\b(debited|credited)\b",
    r"(?i)\baccount\s+(balance|statement)\b",
];

/// Conservative SENDER patterns: bank-ish sender domains and dedicated
/// OTP/verification mailers.
const SENSITIVE_SENDER_PATTERNS: &[&str] = &[
    r"(?i)@[a-z0-9.-]*bank[a-z0-9.-]*\.",
    r"(?i)^otp[@.-]",
    r"(?i)@(otp|verify|2fa)\.",
];

static SENSITIVE_SUBJECT_SET: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new(SENSITIVE_SUBJECT_PATTERNS).expect("sensitive subject patterns compile")
});
static SENSITIVE_SENDER_SET: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new(SENSITIVE_SENDER_PATTERNS).expect("sensitive sender patterns compile")
});

/// Whether a message/chat/contact looks affirmatively sensitive from its
/// subject-like label or sender alone. Bodies are never fetched for this check.
pub fn is_sensitive(subject: Option<&str>, from_address: Option<&str>) -> bool {
    subject
        .map(|s| SENSITIVE_SUBJECT_SET.is_match(s))
        .unwrap_or(false)
        || from_address
            .map(|a| SENSITIVE_SENDER_SET.is_match(a))
            .unwrap_or(false)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn sensitive_subjects_match_otp_verification_and_bank_patterns() {
        for subject in [
            "Your OTP for login",
            "123456 is your verification code",
            "One-time password inside",
            "Security code: 998877",
            "Two-factor authentication enabled",
            "Your sign-in code",
            "Password reset requested",
            "Transaction alert for your account",
            "INR 500 debited from your account",
            "Your account statement is ready",
        ] {
            assert!(is_sensitive(Some(subject), None), "should match: {subject}");
        }
    }

    #[test]
    fn normal_subjects_and_senders_pass_through() {
        for subject in [
            "Quarterly sync notes",
            "Lunch tomorrow?",
            "Re: contract draft v3",
            "Codebase review follow-up",
        ] {
            assert!(
                !is_sensitive(Some(subject), None),
                "false positive: {subject}"
            );
        }
        assert!(!is_sensitive(None, Some("colleague@partner.example")));
        assert!(!is_sensitive(None, None));
    }

    #[test]
    fn sensitive_senders_match_bank_and_otp_mailers() {
        assert!(is_sensitive(None, Some("alerts@examplebank.example")));
        assert!(is_sensitive(None, Some("otp@service.example")));
        assert!(is_sensitive(None, Some("noreply@verify.example")));
    }
}
