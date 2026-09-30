//! Channel-neutral facade over the comms-assist data plane.
//!
//! Product code imports this module through `magician_comms::channel_assist`.
//! Some underlying schema structs still carry historical `Mail*` names, but the
//! public module and route surface are channel-neutral.

pub use super::adapter_registry::{
    ChannelActionAdapter, ChannelActionContext, ChannelActionDescriptor, ChannelActionDraft,
    ChannelActionRequest, ChannelActionResult, ChannelIdentity, ChannelReaction,
    ChannelRealtimeEvent, ChannelRealtimeEventKind, RealtimeChannelAdapter,
};
pub use super::store::{
    ChannelAccountCounts, ChannelAnnotationActionClaimResult, ChannelAnnotationTransitionFeedback,
    ChannelAnnotationTransitionResult, ChannelAssistStore, ChannelDistilledBridgeRow,
    ChannelFeedbackBridgeRow, ChannelMessageAttentionHints, ChannelNeedsApprovalRow,
    ChannelPatternCorpusRow, ChannelRecentDistillEntry, ChannelThreadClassifyRow,
    RequiredActionAnnotationDisposition, RequiredActionAnnotationResult,
};
pub use super::types::{
    derive_channel_required_action, ChannelChangeFact, ChannelDetailStatus, ChannelFollowUpHint,
    ChannelInformationBrief, ChannelInformationType, ChannelLane, ChannelRequiredAction,
    ChannelRequiredActionKind, ChannelRequiredActionSource, ChannelTemporalFact,
    ChannelTemporalKind, DistillState, MessageDirection, CHANNEL_ASSIST_SCHEMA_VERSION,
};

pub type ChannelAnnotation = super::types::MailThreadAnnotation;
pub type ChannelAnnotationState = super::types::MailAnnotationState;
pub type ChannelAssistActor = super::types::MailAssistActor;
pub type ChannelAssistEvent = super::types::MailAssistEvent;
pub type ChannelAssistEventType = super::types::MailAssistEventType;
pub type ChannelDraftArtifact = super::types::MailDraftArtifact;
pub type ChannelDraftCandidate = super::types::MailDraftCandidate;
pub type ChannelFeedbackVerdict = super::types::MailFeedbackVerdict;
pub type ChannelFollowUpCandidate = super::types::MailFollowUpCandidate;
pub type ChannelMessageMeta = super::types::MailMessageMeta;
pub type ChannelRecordOrigin = super::types::MailRecordOrigin;
pub type ChannelSyncWatermark = super::types::SyncWatermark;
pub type ChannelThreadRecord = super::types::MailThreadRecord;
pub type ChannelUserFeedback = super::types::MailAssistUserFeedback;
