/**
 * Event taxonomy — TS mirror of `magician/src/magician_v2/realtime_events.rs`
 * `RuntimeTransportEvent::taxonomy()`.
 *
 * GENERATED FILE — DO NOT EDIT BY HAND.
 *
 * Run `make event-taxonomy-codegen` to regenerate from the Rust source
 * (the `taxonomies!` macro invocation in realtime_events.rs is the single
 * source of truth). `make test`, `build-all-debug`, and `build-all-release`
 * run the fast `make event-taxonomy-check` gate as a prerequisite — it
 * verifies the `// SOURCE_HASH:` marker below still matches a freshly
 * computed hash of the Rust sources and fails the build with a clear
 * remediation message if codegen needs to be re-run.
 */
// SOURCE_HASH: 5b35992968cc994484936fed0b66572b719bedbbd2b61464e216cf01aeecc5a6

export const EVENT_CATEGORIES = [
	'pipeline',
	'plan',
	'tool',
	'slot',
	'clarification',
	'execution',
	'llm',
	'agentic',
	'hitl',
	'agent',
	'task',
	'feed',
	'observability',
	'media',
	'activity',
] as const;

export type EventCategory = (typeof EVENT_CATEGORIES)[number];

export const EVENT_SEVERITIES = ['info', 'warn', 'error', 'decision', 'attention'] as const;

export type EventSeverity = (typeof EVENT_SEVERITIES)[number];

export interface EventTaxonomy {
	category: EventCategory;
	severity: EventSeverity;
	user_relevant: boolean;
}

const t = (
	category: EventCategory,
	severity: EventSeverity,
	user_relevant: boolean
): EventTaxonomy => ({ category, severity, user_relevant });

export const EVENT_TAXONOMY: Readonly<Record<string, EventTaxonomy>> = Object.freeze({
	// ─── Pipeline ───
	MessageProcessingStarted: t('pipeline', 'info', true),
	QueryAnalysisCompleted: t('pipeline', 'info', false),
	StrategySelected: t('pipeline', 'decision', true),
	ExplorationProgress: t('pipeline', 'info', false),
	MessageCompleted: t('pipeline', 'info', true),
	ProcessingError: t('pipeline', 'error', true),
	PipelineStarted: t('pipeline', 'info', true),
	PipelineStepStarted: t('pipeline', 'info', true),
	PipelineStepCompleted: t('pipeline', 'info', true),
	PipelineCompleted: t('pipeline', 'info', true),
	PipelineFailed: t('pipeline', 'error', true),

	// ─── Plan ───
	AtomicPlanOutlineStarted: t('plan', 'info', true),
	AtomicPlanOutlineCompleted: t('plan', 'info', true),
	AtomicPlanExpansionStarted: t('plan', 'info', false),
	AtomicPlanGenerated: t('plan', 'info', true),
	V3PlanningStarted: t('plan', 'info', true),
	V3PlanningProgress: t('plan', 'info', false),
	V3PlanningCompleted: t('plan', 'info', true),
	V3PlanningFailed: t('plan', 'error', true),

	// ─── Tool matching ───
	ToolMatchingTierStarted: t('tool', 'info', false),
	ToolMatchingTierCompleted: t('tool', 'info', false),

	// ─── Slot extraction / parameter resolution ───
	SlotExtractionStarted: t('slot', 'info', false),
	SlotExtracted: t('slot', 'info', false),
	SlotEnrichmentStarted: t('slot', 'info', false),
	SlotEnrichmentCompleted: t('slot', 'info', false),
	SlotConfidenceUpdated: t('slot', 'info', false),
	SlotGraphDiff: t('slot', 'info', false),
	ClarifiedTaskReady: t('slot', 'info', true),
	ParameterInferenceAttempted: t('slot', 'info', false),
	ParameterInferred: t('slot', 'info', false),
	ParameterInferenceFailed: t('slot', 'warn', false),
	ParameterDiscoveryAttempted: t('slot', 'info', false),
	ParameterDiscovered: t('slot', 'info', false),
	ParameterDiscoveryFailed: t('slot', 'warn', false),
	ParameterResolutionProgress: t('slot', 'info', false),

	// ─── Clarification (planning HITL) ───
	ClarificationSessionSnapshot: t('clarification', 'info', true),
	ClarificationConfidenceSnapshot: t('clarification', 'info', false),
	ClarificationMetricsSnapshot: t('clarification', 'info', false),

	// ─── Execution + Workflow lifecycle ───
	ExecutionStarted: t('execution', 'info', true),
	ExecutionStepStarted: t('execution', 'info', true),
	ExecutionStepCompleted: t('execution', 'info', true),
	ExecutionPaused: t('execution', 'warn', true),
	ExecutionResumed: t('execution', 'info', true),
	ExecutionFailed: t('execution', 'error', true),
	ExecutionCancelled: t('execution', 'warn', true),
	ExecutionCompleted: t('execution', 'info', true),
	ExecutionInflightResent: t('execution', 'info', false),
	ExecutionInflightDropped: t('execution', 'warn', false),
	ExecutionRestoreFailed: t('execution', 'error', true),
	ExecutionStatusChanged: t('execution', 'info', false),
	ExecutionResponsibilityChanged: t('execution', 'info', false),
	WorkflowResumed: t('execution', 'info', true),
	WorkflowStageResumed: t('execution', 'info', false),
	WorkflowResumeFailed: t('execution', 'error', true),

	// ─── LLM I/O ───
	LLMAnalysisStarted: t('llm', 'info', false),
	LLMAnalysisCompleted: t('llm', 'info', false),
	LLMAnalysisFailed: t('llm', 'error', true),
	LLMRequestSent: t('llm', 'info', false),
	LLMResponseReceived: t('llm', 'info', false),
	InferenceAttempted: t('llm', 'info', false),
	ThinkingModeActivated: t('llm', 'info', true),
	ThinkingModeCompleted: t('llm', 'info', false),

	// ─── Agentic loop ───
	AgenticExecutionStarted: t('agentic', 'info', true),
	AgenticExecutionCompleted: t('agentic', 'info', true),
	AgenticIterationStarted: t('agentic', 'info', false),
	AgenticIterationCompleted: t('agentic', 'info', false),
	AgenticStepStarted: t('agentic', 'info', true),
	AgenticStepCompleted: t('agentic', 'info', true),
	AgenticStepFailed: t('agentic', 'error', true),
	AgenticStepStuckWarning: t('agentic', 'warn', true),
	AgenticPageUnderstanding: t('agentic', 'info', false),
	AgenticDecisionMade: t('agentic', 'decision', true),
	AgenticActionExecuted: t('agentic', 'info', true),
	AgenticClickFallbackUsed: t('agentic', 'warn', false),
	AgenticMaxIterationsReached: t('agentic', 'warn', true),
	AgenticResumed: t('agentic', 'info', true),
	DomChangeDetected: t('agentic', 'info', false),
	SubGoalRequested: t('agentic', 'decision', true),
	SubGoalOutcome: t('agentic', 'info', true),

	// ─── HITL (execution-side AskUser / confirmation) ───
	AgenticWaitingForUser: t('hitl', 'attention', true),
	AgenticWaitingForConfirmation: t('hitl', 'attention', true),
	HitlRequested: t('hitl', 'attention', true),
	HitlResolved: t('hitl', 'info', true),
	CriticalRequestAlert: t('hitl', 'info', false),
	CriticalRequestRetired: t('hitl', 'info', false),
	VerificationRetrievalStatus: t('hitl', 'info', true),

	// ─── Agent lifecycle ───
	AgentCycleStarted: t('agent', 'info', true),
	AgentCycleCompleted: t('agent', 'info', true),
	AgentTriggered: t('agent', 'info', true),
	AgentEvent: t('agent', 'info', false),
	AgentDefinitionChanged: t('agent', 'info', true),

	// ─── Task CRUD ───
	TaskCreated: t('task', 'info', true),
	TaskUpdated: t('task', 'info', false),
	TaskDeleted: t('task', 'warn', true),

	// ─── Feed ───
	FeedItemCreated: t('feed', 'info', false),
	FeedItemUpdated: t('feed', 'info', false),
	FeedItemRemoved: t('feed', 'info', false),
	ExecutionPanelDelta: t('feed', 'info', false),

	// ─── Observability (catch-all) ───
	Heartbeat: t('observability', 'info', false),
	ObservabilityAlert: t('observability', 'warn', true),
	ChatMessageReceived: t('observability', 'info', true),
	ThinkingMapUpdated: t('observability', 'info', false),
	ThinkingMapInterpretProgress: t('observability', 'info', false),
	ProgressEvent: t('observability', 'info', false),
	ShellOutputChunk: t('observability', 'info', false),
	InteractivePtyChunk: t('observability', 'info', false),

	// ─── Activity (unified runtime activity spans) ───
	ActivityStarted: t('activity', 'info', false),
	ActivityFinished: t('activity', 'info', false),
	ActivityProgress: t('activity', 'info', false),
	ActivityCost: t('activity', 'info', false),
	DecisionShadowAgreement: t('activity', 'info', false),
	DecisionAccountingGap: t('activity', 'warn', false),

	// ─── GAUI envelope events ───
	// Pipeline
	'message.processing_started': t('pipeline', 'info', true),
	'query_analysis.completed': t('pipeline', 'info', false),
	'strategy.selected': t('pipeline', 'info', false),
	'exploration.progress': t('pipeline', 'info', false),

	// Observability (catch-all)
	'processing.error': t('observability', 'error', true),

	// Plan
	'atomic_plan.outline_started': t('plan', 'info', false),
	'atomic_plan.outline_completed': t('plan', 'info', false),
	'atomic_plan.expansion_started': t('plan', 'info', false),
	'atomic_plan.generated': t('plan', 'info', false),
	'plan.created': t('plan', 'info', true),
	'plan.revised': t('plan', 'info', true),

	// Clarification (planning HITL)
	'clarification.session_snapshot': t('clarification', 'info', false),
	'clarification.confidence_snapshot': t('clarification', 'info', false),
	'clarified_task.ready': t('clarification', 'info', true),

	// Slot extraction / parameter resolution
	'slot_graph.diff': t('slot', 'info', false),
	'slot_extraction.started': t('slot', 'info', false),
	'slot.extracted': t('slot', 'info', false),
	'slot_enrichment.started': t('slot', 'info', false),
	'slot_enrichment.completed': t('slot', 'info', false),
	'slot.confidence_updated': t('slot', 'info', false),
	'parameter_inference.attempted': t('slot', 'info', false),
	'parameter_inference.inferred': t('slot', 'info', false),
	'parameter_inference.failed': t('slot', 'warn', false),
	'parameter_discovery.attempted': t('slot', 'info', false),
	'parameter_discovery.discovered': t('slot', 'info', false),
	'parameter_discovery.failed': t('slot', 'warn', false),

	// Execution + Workflow lifecycle
	'workflow.resumed': t('execution', 'info', true),
	'workflow.stage_resumed': t('execution', 'info', false),
	'workflow.resume_failed': t('execution', 'error', true),
	'workflow.stage_started': t('execution', 'info', false),
	'workflow.stage_completed': t('execution', 'info', false),

	// LLM I/O
	'llm_analysis.started': t('llm', 'info', false),
	'llm_analysis.completed': t('llm', 'info', false),
	'llm_analysis.failed': t('llm', 'error', true),
	'llm.requested': t('llm', 'info', false),
	'llm.first_token': t('llm', 'info', false),
	'llm.succeeded': t('llm', 'info', false),
	'llm.failed': t('llm', 'error', true),
	'llm.retry_scheduled': t('llm', 'warn', false),

	// Tool matching
	'tool_matching_tier.started': t('tool', 'info', false),
	'tool_matching_tier.completed': t('tool', 'info', false),
	'tool.started': t('tool', 'info', false),
	'tool.succeeded': t('tool', 'info', true),
	'tool.failed': t('tool', 'error', true),

	// Agentic loop
	'agentic.execution_started': t('agentic', 'info', true),
	'agentic.iteration_started': t('agentic', 'info', false),
	'agentic.decision_made': t('agentic', 'decision', false),
	'agentic.step_stuck_warning': t('agentic', 'warn', true),
	'agentic.execution_completed': t('agentic', 'info', true),
	'agentic.resumed': t('agentic', 'info', true),
	'max_iterations_reached': t('agentic', 'warn', true),
	'sub_goal.requested': t('agentic', 'info', false),
	'sub_goal.outcome': t('agentic', 'info', false),

	// Execution + Workflow lifecycle
	'step.started': t('execution', 'info', false),
	'step.completed': t('execution', 'info', false),
	'step.failed': t('execution', 'error', true),
	'step.blocked': t('execution', 'attention', true),
	'step.skipped': t('execution', 'info', false),
	'step.delegated': t('execution', 'info', false),

	// HITL (execution-side AskUser / confirmation)
	'input.requested': t('hitl', 'attention', true),
	'input.received': t('hitl', 'info', true),
	'input.resolved': t('hitl', 'info', true),
	'input.promoted_to_task_scope': t('hitl', 'info', false),
	'waiting_for_confirmation': t('hitl', 'attention', true),

	// Execution + Workflow lifecycle
	'child.spawned': t('execution', 'info', true),
	'child.completed': t('execution', 'info', true),
	'child.failed': t('execution', 'error', true),
	'child.output_available': t('execution', 'info', false),
	'delegation.launch_authorized': t('execution', 'info', false),
	'delegation.launch_dispatched': t('execution', 'info', false),
	'delegation.launch_failed': t('execution', 'error', true),
	'delegation.child_linked': t('execution', 'info', false),
	'delegation.child_attach_failed': t('execution', 'warn', true),
	'delegation.results_ready': t('execution', 'info', true),
	'delegation.unmatched_child_dispatched': t('execution', 'warn', false),
	'parent.resume_authorized': t('execution', 'info', false),
	'parent.resume_dispatched': t('execution', 'info', false),
	'execution.started': t('execution', 'info', true),
	'execution.runtime_handoff': t('execution', 'info', false),
	'execution.runtime_resume_handoff': t('execution', 'info', false),
	'execution.discovered': t('execution', 'info', false),
	'execution.waiting': t('execution', 'attention', true),
	'execution.waiting_on_children': t('execution', 'info', false),

	// HITL (execution-side AskUser / confirmation)
	'execution.waiting_for_user': t('hitl', 'attention', true),

	// Execution + Workflow lifecycle
	'execution.resumed': t('execution', 'info', true),
	'execution.outcome_observed': t('execution', 'info', false),
	'execution.completed': t('execution', 'info', true),
	'execution.failed': t('execution', 'error', true),
	'execution.cancelled': t('execution', 'info', true),
	'recipe.cancellation_requested': t('execution', 'info', true),
	'execution.deferred': t('execution', 'info', false),
	'execution.status_changed': t('execution', 'info', false),
	'execution.responsibility_changed': t('execution', 'info', false),
	'execution.progress': t('execution', 'info', false),
	'execution_finalizer.started': t('execution', 'info', false),
	'execution_finalizer.completed': t('execution', 'info', false),
	'execution_finalizer.failed': t('execution', 'error', true),

	// Task CRUD
	'task_agent_finalizer.started': t('task', 'info', false),
	'task_agent_finalizer.completed': t('task', 'info', false),
	'task_user_finalizer.started': t('task', 'info', false),
	'task_user_finalizer.completed': t('task', 'info', false),

	// Execution + Workflow lifecycle
	'output.created': t('execution', 'info', true),
	'output.available': t('execution', 'info', true),
	'output_candidate.proposed': t('execution', 'info', false),
	'summary.updated': t('execution', 'info', false),
	'artifact.created': t('execution', 'info', true),
	'artifact.create_failed': t('execution', 'error', true),
	'artifact.referenced': t('execution', 'info', false),
	'artifact.used': t('execution', 'info', false),
	'import.resolved': t('execution', 'info', false),

	// Agent lifecycle
	'memory.episode_recorded': t('agent', 'info', false),

	// Plan
	'plan.snapshot': t('plan', 'info', true),
	'plan.step.started': t('plan', 'info', false),
	'plan.step.finished': t('plan', 'info', false),

	// LLM I/O
	'reasoning.start': t('llm', 'info', false),
	'reasoning.content': t('llm', 'info', false),
	'reasoning.end': t('llm', 'info', false),

	// Tool matching
	'tool.call.started': t('tool', 'info', true),
	'tool.call.args': t('tool', 'info', false),
	'tool.call.finished': t('tool', 'info', true),
	'tool.result.projected': t('tool', 'info', false),
	'tool.result.read': t('tool', 'info', false),

	// Agent lifecycle
	'agent.cycle.started': t('agent', 'info', true),
	'agent.cycle.completed': t('agent', 'info', true),

	// HITL (execution-side AskUser / confirmation)
	'hitl.requested': t('hitl', 'warn', true),
	'hitl.resolved': t('hitl', 'info', true),

	// Execution + Workflow lifecycle
	'verification.lifecycle': t('execution', 'info', true),

	// Tool matching
	'recipe.replay': t('tool', 'info', true),
	'recipe.replay.started': t('tool', 'info', true),
	'recipe.replay.step.completed': t('tool', 'info', false),
	'recipe.replay.step.failed': t('tool', 'warn', true),
	'recipe.replay.auth.healed': t('tool', 'info', true),
	'recipe.replay.transport.downgraded': t('tool', 'info', true),

	// HITL (execution-side AskUser / confirmation)
	'recipe.replay.approval.requested': t('hitl', 'attention', true),
	'recipe.replay.approval.resolved': t('hitl', 'info', true),

	// Tool matching
	'recipe.replay.fallback.handoff': t('tool', 'warn', true),
	'recipe.replay.completed': t('tool', 'info', true),
	'recipe.replay.recompiled': t('tool', 'info', true),

	// Agent lifecycle
	'agent.cycle.failed': t('agent', 'error', true),
	'agent.cycle.paused': t('agent', 'attention', true),
	'agent.goal.completed': t('agent', 'info', true),
	'agent.goal.failed': t('agent', 'error', true),
	'agent.goal.recovered': t('agent', 'info', true),
	'agent.circuit.opened': t('agent', 'warn', true),
	'agent.circuit.recovered': t('agent', 'info', true),
	'agent.created': t('agent', 'info', true),
	'agent.updated': t('agent', 'info', false),
	'agent.deleted': t('agent', 'info', true),
	'agent.paused': t('agent', 'attention', true),
	'agent.resumed': t('agent', 'info', true),
	'agent.update': t('agent', 'info', false),
	'agent.execution.mapping': t('agent', 'info', false),
	'agent.feedback.generated': t('agent', 'info', false),
	'agent.feedback.injections': t('agent', 'info', false),
	'agent.memory.report': t('agent', 'info', false),
	'agent.ui.snapshot': t('agent', 'info', false),
	'agent.ui.delta': t('agent', 'info', false),

	// Execution + Workflow lifecycle
	'task.status_changed': t('execution', 'info', true),
	'task.action_progress': t('execution', 'info', true),
	'task.child.status_changed': t('execution', 'info', true),
	'execution.handed_over': t('execution', 'info', true),

	// Observability (catch-all)
	'loop.outbox.gap': t('observability', 'warn', false),

	// Execution + Workflow lifecycle
	'chat.delegate.status_changed': t('execution', 'info', true),
	'chat.delegate.output_ready': t('execution', 'info', true),
	'chat.delegate.output_failed': t('execution', 'warn', true),

	// Agent lifecycle
	'chat.agent_turn.status_changed': t('agent', 'info', true),

	// Execution + Workflow lifecycle
	'tutor.run.started': t('execution', 'info', true),
	'tutor.run.completed': t('execution', 'info', true),
	'tutor.run.failed': t('execution', 'warn', true),
	'tutor.step.observed': t('execution', 'info', false),
	'tutor.step.target_resolved': t('execution', 'info', false),
	'tutor.step.drawing': t('execution', 'info', false),
	'tutor.draw.shape': t('execution', 'info', true),
	'tutor.step.action_delegated': t('execution', 'info', true),
	'tutor.step.verifying': t('execution', 'info', false),
	'tutor.step.verified': t('execution', 'info', true),
	'tutor.step.failed': t('execution', 'warn', true),
	'tutor.step.recovering': t('execution', 'warn', true),
	'tutor.step.clearing': t('execution', 'info', false),

	// Realtime media + control rails
	'media.session.registered': t('media', 'info', false),
	'media.session.updated': t('media', 'info', false),
	'media.session.heartbeat': t('media', 'info', false),
	'media.session.disconnected': t('media', 'info', false),
	'media.session.revoked': t('media', 'warn', true),
	'media.capabilities.updated': t('media', 'info', false),
	'media.preferences.updated': t('media', 'info', false),
	'ui.preferences.updated': t('media', 'info', false),
	'chat.engine.updated': t('media', 'info', false),
	'media.permission.granted': t('media', 'info', false),
	'media.permission.denied': t('media', 'warn', true),
	'media.permission.revoked': t('media', 'warn', true),
	'media.tts.started': t('media', 'info', false),
	'media.tts.completed': t('media', 'info', false),
	'media.tts.cancelled': t('media', 'info', false),
	'media.tts.error': t('media', 'warn', true),
	'media.stt.started': t('media', 'info', false),
	'media.transcript.delta': t('media', 'info', false),
	'media.transcript.final': t('media', 'info', true),
	'media.stt.error': t('media', 'warn', true),
	'media.audio.engine.started': t('media', 'info', false),
	'media.audio.engine.stopped': t('media', 'info', false),
	'media.audio.engine.unhealthy': t('media', 'warn', false),
	'media.audio.model.loading': t('media', 'info', false),
	'media.audio.model.loaded': t('media', 'info', false),
	'media.audio.model.unloaded': t('media', 'info', false),
	'media.audio.profile.resolved': t('media', 'info', false),
	'media.audio.profile.degraded': t('media', 'warn', false),
	'media.audio.vad.speech_started': t('media', 'info', false),
	'media.audio.vad.speech_ended': t('media', 'info', false),
	'media.audio.provider.fallback': t('media', 'warn', false),
	'media.audio.frames.dropped': t('media', 'warn', false),
	'media.voice_note.recording_started': t('media', 'info', false),
	'media.voice_note.recording_stopped': t('media', 'info', false),
	'media.voice_note.recording_failed': t('media', 'warn', true),
	'media.voice_note.received': t('media', 'info', false),
	'media.voice_note.transcription_started': t('media', 'info', false),
	'media.voice_note.transcription_completed': t('media', 'info', true),
	'media.voice_note.transcription_failed': t('media', 'warn', true),
	'media.voice_note.transcribed': t('media', 'info', true),
	'media.voice_note.chat_submit_started': t('media', 'info', false),
	'media.voice_note.chat_submit_completed': t('media', 'info', true),
	'media.voice_note.chat_submit_failed': t('media', 'warn', true),
	'media.voice_note.submitted': t('media', 'info', true),
	'media.voice_note.error': t('media', 'warn', true),
	'media.capture.started': t('media', 'info', false),
	'media.capture.completed': t('media', 'info', true),
	'media.capture.cancelled': t('media', 'info', false),
	'media.capture.error': t('media', 'warn', true),
	'media.pointer.commanded': t('media', 'info', false),
	'media.artifact.created': t('media', 'info', true),
	'media.mascot.visible': t('media', 'info', false),
	'media.mascot.hidden': t('media', 'info', false),
	'media.mascot.invoked': t('media', 'info', true),
	'media.mascot.bubble.opened': t('media', 'info', false),
	'media.mascot.bubble.closed': t('media', 'info', false),
	'media.mascot.state.changed': t('media', 'info', false),
	'media.mascot.quiet.changed': t('media', 'info', true),
	'media.config.updated': t('media', 'info', false),
	'media.voice.session.minted': t('media', 'info', false),
	'media.voice.session.rotated': t('media', 'info', false),
	'media.voice.surface.resolved': t('media', 'info', false),
	'media.voice.session.reconnect_attempt': t('media', 'info', false),
	'media.voice.session.reconnect_failed': t('media', 'warn', false),
	'media.voice.session.compaction': t('media', 'info', false),
	'media.voice.local_transcript.state': t('media', 'info', false),
	'media.voice.local_transcript.queue': t('media', 'warn', false),
	'media.voice.local_transcript.turn': t('media', 'info', false),
	'media.voice.local_transcript.fallback': t('media', 'warn', false),
	'media.voice.bridge.connected': t('media', 'info', false),
	'media.voice.bridge.disconnected': t('media', 'info', false),
	'media.voice.client_message': t('media', 'info', false),
	'media.voice.client_audio': t('media', 'info', false),
	'media.voice.controller_command': t('media', 'info', true),
	'media.voice.bridge.error': t('media', 'warn', true),
	'media.tray.bridge.connected': t('media', 'info', false),
	'media.tray.bridge.disconnected': t('media', 'info', false),
	'media.tray.frame.received': t('media', 'info', false),
	'media.tray.audio.received': t('media', 'info', false),
	'media.tray.pointer.command': t('media', 'info', true),
	'media.tray.bridge.error': t('media', 'warn', true),
});

/** Lookup with safe fallback for events that haven't been categorised yet. */
export function taxonomyFor(eventType: string): EventTaxonomy {
	return (
		EVENT_TAXONOMY[eventType] ?? {
			category: 'observability',
			severity: 'info',
			user_relevant: false
		}
	);
}

/** All event_type strings that exist in the taxonomy. Useful for filter chip options. */
export const KNOWN_EVENT_TYPES: readonly string[] = Object.freeze(Object.keys(EVENT_TAXONOMY));
