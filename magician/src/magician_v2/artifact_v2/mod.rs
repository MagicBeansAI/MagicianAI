pub(crate) mod app_agent_tool;
pub mod bridge;
pub mod capabilities;
pub mod events;
pub mod execution_artifacts;
pub mod io;
pub mod memory;
pub mod models;
pub mod pipeline_store;
pub mod pipeline_terminal_settlement;
pub mod progress;
pub mod publications;
pub mod read_gate;
pub mod recipe_compile_hook;
pub mod recipe_replay_hook;
pub(crate) mod recipe_replay_receipt;
pub mod reducer;
pub mod scheduler;
pub mod service;
pub mod spoken_hitl_log;
pub mod synthesis;
pub mod task_writes;
/// Boundary B's deterministic evaluation suite. Test-gated until a live-eval
/// runner exists to lift the fixtures; the activation rule it exercises lives
/// in `content_sources` and ships in production builds.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod working_set_eval;
pub mod working_sets;
pub mod workspace;
pub mod writers;

pub use capabilities::{
    CapabilityScopePaths, CapabilityWorkspaceError, CapabilityWorkspaceManager,
};
pub use events::{
    canonical_runtime_fact_of, map_v2_realtime_event, task_status_hint, ArtifactV2EventType,
    CanonicalEventScope, MappedRuntimeEvent, RuntimeCanonicalEventObserver,
    RuntimeCanonicalEventReceipt, RuntimeCanonicalEventSink, CANONICAL_UI_THREAD_ID_FIELD,
    SOURCE_EVENT_REF_FIELD,
};
pub use execution_artifacts::FilesystemExecutionArtifactIndexStore;
pub use memory::{
    FilesystemV3EpisodeRecorder, RecordedEpisode, V3EpisodeProvenance, V3EpisodeRecord,
    V3EpisodeRecorder, V3EpisodeRecordingContext, V3MemoryTierRecord,
};
pub use models::{
    PersistedExecutionArtifactRecord, PersistedExecutionArtifactsIndex,
    CONTINUATION_CONTEXT_OUTPUT_ROLE,
};
pub use pipeline_store::{ExecutionPipelineStoreError, FilesystemExecutionPipelineStore};
pub use progress::{
    V3ActivitySummary, V3AttentionSummary, V3ExecutionProgressSummary, V3OutputAvailableSummary,
    V3ProgressProjectionRecord, V3TaskAttentionRecord, V3TaskProgressProjectionAdapter,
    V3TaskProgressSummary,
};
pub use publications::{
    published_surface_changed_payload, FilesystemPublishedSurfaceStore,
    PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
};
pub use read_gate::{OutputReadOutcome, SynthesisReadiness};
pub use reducer::{ArtifactV2Reducer, FilesystemArtifactV2Reducer, SharedArtifactV2Reducer};
pub use service::{
    AcceptedRuntimeLaunchRequest, ArtifactV2Error, ArtifactV2Service, CanonicalEventWriter,
    CreateTaskInput, ExecutionFinalizer, ExecutionRetryDisposition, ExecutionWithDetails,
    RetrySynthesisOutcome, ScopeRef, TaskAgentFinalizer, TaskUserFinalizer,
    TaskWithExecutionsEnriched, UpdateTaskInput, V3ReadApi, WriteUserOutputBody,
    WriteUserOutputDirectInput,
};
pub use synthesis::{
    execution_output_prompt_spec, task_agent_output_prompt_spec, task_user_output_prompt_spec,
    ArtifactV2BundleBuilder, EventSnippet, ExecutionOutputSynthesisBundle,
    ExecutionOutputSynthesizer, FilesystemArtifactV2BundleBuilder, OutputEvidence,
    PlanSynthesisContext, PromptManagerExecutionOutputSynthesizer,
    PromptManagerTaskAgentOutputSynthesizer, PromptManagerTaskUserOutputSynthesizer,
    SynthesisDependencies, SynthesisPromptSpec, TaskAgentOutputSynthesisBundle,
    TaskAgentOutputSynthesizer, TaskUserOutputSynthesisBundle, TaskUserOutputSynthesizer,
};
pub use task_writes::TaskWriteReconciler;
pub use working_sets::{
    CreateWorkingSetRequest, WorkingSetChunk, WorkingSetChunkRead, WorkingSetExecutionActivation,
    WorkingSetExecutionDecision, WorkingSetExecutionIndex, WorkingSetExecutionMember,
    WorkingSetExecutionSearch, WorkingSetManifest, WorkingSetScope, WorkingSetSearchMatch,
    WorkingSetSource, WorkingSetSourceInput, WorkingSetStore, MAX_WORKING_SET_CHUNK_BYTES,
    MAX_WORKING_SET_SOURCES, MAX_WORKING_SET_TOTAL_BYTES,
};
pub use writers::{
    AllowedOutputFormatsPolicy, DefaultAllowedOutputFormatsPolicy, OutputBody, OutputClass,
    OutputDocument, OutputWriter, OutputWriterRegistry, PersistedOutput, TextOutputWriter,
};
