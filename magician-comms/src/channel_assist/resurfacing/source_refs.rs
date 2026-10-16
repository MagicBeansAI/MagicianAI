//! Comms-side source-reference convenience (plan workstream 3.0). The
//! string-parts contract lives lib-side at
//! `magician::magician_v2::attention::resurfacing::source_refs`; what remains
//! here is the one `ChannelMessageMeta` convenience constructor that cannot
//! live lib-side.

use magician::magician_v2::attention::resurfacing::source_refs::comm_source_ref_parts;

use crate::channel_assist::channel::ChannelMessageMeta;

/// Build the exact-message reference from a comms message row. Same output as
/// [`comm_source_ref_parts`] fed the row's identity fields.
pub fn comm_source_ref_for_message(message: &ChannelMessageMeta) -> String {
    comm_source_ref_parts(
        &message.provider,
        &message.account_alias,
        &message.thread_id,
        &message.message_id,
        message.internal_date,
    )
}
