//! Source watches for automatic verification-code retrieval (secure HITL
//! plan §6.2, P6): bounded, value-free reads of the accounts the owner
//! permitted, handed to `magician_v2::verification_codes` as evidence.
//!
//! Every watch reads through the provider client directly — never through
//! ingest — so no model-facing copy of a verification message is made, and
//! every read is capped: a handful of messages per poll, bodies bounded,
//! nothing kept after matching. Logs carry counts and reasons only.
pub mod agentmail;
pub mod gmail;
pub mod imessage;

pub use agentmail::AgentMailVerificationWatch;
pub use gmail::GmailVerificationWatch;
pub use imessage::MessagesVerificationWatch;

/// Bodies are read up to this many bytes; a verification message is short.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

pub(crate) fn bounded(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}
