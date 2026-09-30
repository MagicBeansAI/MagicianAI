// Central constants for all MagicianV2 prompts
//
// This file serves as the single source of truth for prompt names and versions
// used throughout the MagicianV2 system. All components should reference these
// constants rather than hardcoding strings.

/// Prompt names used in MagicianV2
pub mod names {
    /// Local channel message distillation system prompt.
    pub const CHANNEL_INGEST_DISTILL_SYSTEM: &str = "channel_ingest_distill_system";
    /// Local channel message distillation user prompt.
    pub const CHANNEL_INGEST_DISTILL_USER: &str = "channel_ingest_distill_user";
    /// Targeted repair instruction after an invalid distillation response.
    pub const CHANNEL_INGEST_DISTILL_REPAIR_USER: &str = "channel_ingest_distill_repair_user";
    /// Body-blind channel thread classification system prompt.
    pub const CHANNEL_CLASSIFY_SYSTEM: &str = "channel_classify_system";
    /// Body-blind channel thread classification user prompt.
    pub const CHANNEL_CLASSIFY_USER: &str = "channel_classify_user";
    /// Local channel reply-draft system prompt.
    pub const CHANNEL_REPLY_DRAFT_SYSTEM: &str = "channel_reply_draft_system";
    /// Local channel reply-draft user prompt.
    pub const CHANNEL_REPLY_DRAFT_USER: &str = "channel_reply_draft_user";
    /// Passive channel pattern synthesis system prompt.
    pub const CHANNEL_PATTERN_SYNTHESIS_SYSTEM: &str = "channel_pattern_synthesis_system";
    /// Passive channel pattern synthesis user prompt.
    pub const CHANNEL_PATTERN_SYNTHESIS_USER: &str = "channel_pattern_synthesis_user";
    /// Proactive resurfacing curation system prompt.
    pub const RESURFACING_CURATE_SYSTEM: &str = "resurfacing_curate_system";
    /// Proactive resurfacing curation user prompt.
    pub const RESURFACING_CURATE_USER: &str = "resurfacing_curate_user";
    /// Explicit local deeper-summary system prompt.
    pub const RESURFACING_DEEP_SUMMARY_SYSTEM: &str = "resurfacing_deep_summary_system";
    /// Explicit local deeper-summary user prompt.
    pub const RESURFACING_DEEP_SUMMARY_USER: &str = "resurfacing_deep_summary_user";

    /// Unified query analysis prompt
    pub const UNIFIED_ANALYSIS: &str = "unified_analysis";
    /// Unified query analysis system prompt
    pub const UNIFIED_ANALYSIS_SYSTEM: &str = "unified_analysis_system";

    /// Task decomposition prompt
    pub const TASK_DECOMPOSITION: &str = "task_decomposition";
    /// Task decomposition system prompt
    pub const TASK_DECOMPOSITION_SYSTEM: &str = "task_decomposition_system";

    /// Tool matching prompt for LLM-based candidate disambiguation
    pub const TOOL_MATCHING: &str = "tool_matching";

    /// Parameter elicitation prompt (future use)
    pub const PARAMETER_ELICITATION: &str = "parameter_elicitation";

    /// Response generation prompt (future use)
    pub const RESPONSE_GENERATION: &str = "response_generation";

    /// Error handling prompt (future use)
    pub const ERROR_HANDLING: &str = "error_handling";

    /// Atomic composition strategy prompt
    pub const ATOMIC_COMPOSITION: &str = "atomic_composition";
    /// Atomic composition strategy system prompt
    pub const ATOMIC_COMPOSITION_SYSTEM: &str = "atomic_composition_system";
    /// Atomic composition outline prompt
    pub const ATOMIC_COMPOSITION_OUTLINE: &str = "atomic_composition_outline";
    /// Atomic composition outline system prompt
    pub const ATOMIC_COMPOSITION_OUTLINE_SYSTEM: &str = "atomic_composition_outline_system";

    /// Entity mapping prompt for strategy execution
    pub const ENTITY_MAPPING: &str = "entity_mapping";

    /// Ask loop clarifier templates for parameter elicitation
    pub const ASK_LOOP_CLARIFIER: &str = "ask_loop_clarifier_templates";

    /// Slot extraction prompt for slot graph bootstrap
    pub const SLOT_EXTRACTION: &str = "slot_extraction";

    /// Question rewriting prompt for clarified task synthesis
    pub const QUESTION_REWRITING: &str = "question_rewriting";
    /// Clarification question curation prompt
    pub const QUESTION_CURATION: &str = "question_curation";

    /// Progressive elicitation - parameter extraction from user message
    pub const PARAM_EXTRACTION: &str = "param_extraction";

    /// Progressive elicitation - context-aware default inference
    pub const PARAM_DEFAULT_INFERENCE: &str = "param_default_inference";

    /// Progressive elicitation - discovery plan generation
    pub const DISCOVERY_PLANNING: &str = "discovery_planning";
    /// Progressive elicitation - discovery plan system prompt
    pub const DISCOVERY_PLANNING_SYSTEM: &str = "discovery_planning_system";

    /// Progressive elicitation - safety evaluation for discovery actions
    pub const DISCOVERY_SAFETY_EVAL: &str = "discovery_safety_eval";

    /// Progressive elicitation - value extraction from discovery command output
    pub const DISCOVERY_RESULT_EXTRACTION: &str = "discovery_result_extraction";

    /// Progressive elicitation - parameter priority classification
    pub const PARAM_PRIORITY_CLASSIFICATION: &str = "param_priority_classification";

    /// Answer interpretation for clarification responses
    pub const ANSWER_INTERPRETATION: &str = "answer_interpretation";

    /// Chat system prompt for conversational mode (legacy chat path)
    pub const CHAT_SYSTEM: &str = "chat_system";
    /// Chat outer-loop system prompt for the synchronous-Responses chat path.
    /// Used by the new chat refactor (v0.6.403+) where chat is a direct LLM
    /// call with tool dispatch and `tool_choice: auto`. The LLM picks between
    /// a text reply or a tool call per turn; the framework no longer
    /// pre-creates a task on every chat send.
    pub const CHAT_OUTER_LOOP_SYSTEM: &str = "chat_outer_loop_system";
    /// Distils a finished session into candidate standing directives for the
    /// owner to approve. Local-only by binding; see `taste_capture`.
    pub const TASTE_PROFILE_DISTILL: &str = "taste_profile_distill";
    /// Slice 3 stage-2: decides which preferences govern the task at hand.
    pub const MEMORY_APPLICABILITY_JUDGE: &str = "memory_applicability_judge";
    /// Public bot ordinary-chat addendum for the least-privilege envoy lane.
    pub const PUBLIC_ENVOY_CHAT_ONLY_INSTRUCTION: &str = "public_envoy_chat_only_instruction";
    /// Legacy compatibility stub for the former combined Personal Tutor/App Copilot prompt.
    pub const PERSONAL_TUTOR_CHAT_INSTRUCTIONS: &str = "personal_tutor_chat_instructions";
    /// Shared visual storyboard runtime contract for Personal Tutor and App Copilot.
    pub const VISUAL_STORYBOARD_RUNTIME: &str = "visual_storyboard_runtime";
    /// Personal Tutor rail policy appended after explicit tutor invoke words.
    pub const PERSONAL_TUTOR_POLICY: &str = "personal_tutor_policy";
    /// App Copilot rail policy appended after explicit copilot invoke words.
    pub const APP_COPILOT_POLICY: &str = "app_copilot_policy";
    /// Answer interpretation system prompt
    pub const ANSWER_INTERPRETATION_SYSTEM: &str = "answer_interpretation_system";

    /// Placeholder resolution prompt for native action runtime resolution
    pub const PLACEHOLDER_RESOLUTION: &str = "placeholder_resolution";

    /// Agentic decision prompt for observe-decide-execute loop
    pub const AGENTIC_DECISION: &str = "agentic_decision";

    /// Agentic decision system prompt for non-SoM / non-browser fallback flow
    pub const AGENTIC_DECISION_SYSTEM: &str = "agentic_decision_system";

    /// Agentic input interpretation for validating user responses
    pub const AGENTIC_INPUT_INTERPRETATION: &str = "agentic_input_interpretation";
    /// Agentic input interpretation system prompt
    pub const AGENTIC_INPUT_INTERPRETATION_SYSTEM: &str = "agentic_input_interpretation_system";

    // Inner-loop system prompt: agent-browser pack
    // Generic inner-loop system prompt fallback used by inner-loop packs
    // that do not have a dedicated prompt mapping.

    // NOTE: OVERLAY_DETECTION_VISION removed - overlay detection is now integrated
    // into the agentic decision loop. The LLM detects overlays directly from the
    // screenshot, with DOM-detected overlays shown as hints in the prompt.

    // NOTE: SEMANTIC_TEXT_MATCH_SYSTEM/USER removed - feature was never implemented

    // Memory consolidation prompts
    /// Memory entity extraction user prompt
    pub const MEMORY_EXTRACT_ENTITIES: &str = "memory_extract_entities";
    /// Memory entity extraction system prompt
    pub const MEMORY_EXTRACT_ENTITIES_SYSTEM: &str = "memory_extract_entities_system";
    /// Memory activity summarization user prompt
    pub const MEMORY_SUMMARIZE_ACTIVITY: &str = "memory_summarize_activity";
    /// Memory activity summarization system prompt
    pub const MEMORY_SUMMARIZE_ACTIVITY_SYSTEM: &str = "memory_summarize_activity_system";
    /// Memory insight extraction user prompt
    pub const MEMORY_EXTRACT_INSIGHTS: &str = "memory_extract_insights";
    /// Memory insight extraction system prompt
    pub const MEMORY_EXTRACT_INSIGHTS_SYSTEM: &str = "memory_extract_insights_system";
    /// Memory insight distillation user prompt
    pub const MEMORY_DISTILL_INSIGHTS: &str = "memory_distill_insights";
    /// Memory insight distillation system prompt
    pub const MEMORY_DISTILL_INSIGHTS_SYSTEM: &str = "memory_distill_insights_system";
    /// Memory user-level promotion user prompt
    pub const MEMORY_PROMOTE_TO_USER: &str = "memory_promote_to_user";
    /// Memory user-level promotion system prompt
    pub const MEMORY_PROMOTE_TO_USER_SYSTEM: &str = "memory_promote_to_user_system";
    /// Memory episode archival user prompt
    pub const MEMORY_ARCHIVE_EPISODES: &str = "memory_archive_episodes";
    /// Memory episode archival system prompt
    pub const MEMORY_ARCHIVE_EPISODES_SYSTEM: &str = "memory_archive_episodes_system";
    /// Memory environment knowledge extraction user prompt
    pub const MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE: &str = "memory_extract_environment_knowledge";
    /// Memory environment knowledge extraction system prompt
    pub const MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE_SYSTEM: &str =
        "memory_extract_environment_knowledge_system";

    /// Post-run learning reflection prompt
    pub const LEARNING_REFLECTION: &str = "learning_reflection";
    /// Post-run learning reflection system prompt
    pub const LEARNING_REFLECTION_SYSTEM: &str = "learning_reflection_system";

    /// Autonomous execution planning prompt
    pub const AUTONOMOUS_EXECUTION: &str = "autonomous_execution";
    /// Autonomous execution planning system prompt
    pub const AUTONOMOUS_EXECUTION_SYSTEM: &str = "autonomous_execution_system";

    /// V3 execution-scoped agent output synthesis prompt
    pub const EXECUTION_OUTPUT_SYNTHESIZE: &str = "execution_output_synthesize";
    /// V3 execution-scoped agent output synthesis system prompt
    pub const EXECUTION_OUTPUT_SYNTHESIZE_SYSTEM: &str = "execution_output_synthesize_system";
    /// V3 task-scoped agent output synthesis prompt
    pub const TASK_AGENT_OUTPUT_SYNTHESIZE: &str = "task_agent_output_synthesize";
    /// V3 task-scoped agent output synthesis system prompt
    pub const TASK_AGENT_OUTPUT_SYNTHESIZE_SYSTEM: &str = "task_agent_output_synthesize_system";
    /// V3 task-scoped user output synthesis prompt
    pub const TASK_USER_OUTPUT_SYNTHESIZE: &str = "task_user_output_synthesize";
    /// V3 task-scoped user output synthesis system prompt
    pub const TASK_USER_OUTPUT_SYNTHESIZE_SYSTEM: &str = "task_user_output_synthesize_system";
    /// System prompt for the task-result summarization op (`task_summary`,
    /// config-bound to a small local model). Used at finalize to condense a
    /// long/noisy deliverable into a readable cached result summary.
    pub const TASK_SUMMARY_SYSTEM: &str = "task_summary_system";
    /// API Mining Phase 2 workflow compilation system prompt.
    /// Teaches the LLM how to merge N captured CapabilitySequences into one
    /// replayable WorkflowGraph.
    pub const WORKFLOW_COMPILATION_SYSTEM: &str = "workflow_compilation_system";
    /// Shape-only refinement for backward-compiled Task Recipes.
    pub const RECIPE_COMPILE_SYSTEM: &str = "recipe_compile_system";
    /// Task-shape confirmation and input extraction for recipe lookup.
    pub const RECIPE_MATCH_SYSTEM: &str = "recipe_match_system";

    /// Audio modality addendum appended to `CHAT_OUTER_LOOP_SYSTEM`
    /// when the agent is running on a realtime voice substrate. Shapes
    /// delivery (speak conversationally, acknowledge before slow
    /// tools, no raw data aloud, confirm destructive actions) while
    /// keeping the brain identity unified with the text chat agent.
    /// See `docs/archive/plans/2026-05-19-voice-as-chat-agent.md` (Phase A1b).
    pub const VOICE_MODALITY_ADDENDUM: &str = "voice_modality_addendum";

    /// GPT-Live-1 `session.instructions` mouth prompt. Live owns
    /// turn-taking; Magician is the delegated backend. Isolated from
    /// `VOICE_MODALITY_ADDENDUM` / chat outer-loop used by GPT Realtime
    /// and Gemini Live.
    pub const VOICE_LIVE_MOUTH_SYSTEM: &str = "voice_live_mouth_system";

    /// Meeting-join Presto realtime session.instructions. Isolated from
    /// personal GPT Realtime / Gemini / GPT Live 1 prompts.
    pub const VOICE_MEETING_PRESTO_SYSTEM: &str = "voice_meeting_presto_system";

    /// Voice context compaction prompt — used by
    /// `voice_context_compactor.rs` to roll older voice turns into a
    /// short factual summary before each upstream rotation.
    /// Provider-neutral: same prompt for OpenAI Realtime and the
    /// future Gemini Live transport. Two variables — the optional
    /// prior summary and the new turns to fold in.
    pub const VOICE_CONTEXT_COMPACTION_SYSTEM: &str = "voice_context_compaction_system";
    pub const VOICE_CONTEXT_COMPACTION: &str = "voice_context_compaction";

    /// Voice task-completion announcement template — rendered
    /// backend-side when a task this voice call dispatched reaches
    /// a terminal state. The control WS pushes the rendered string
    /// to the frontend, which injects it as a system message into
    /// the realtime data channel.
    pub const VOICE_TASK_COMPLETION_ANNOUNCEMENT: &str = "voice_task_completion_announcement";

    /// Spoken verification clauses, appended to the task-completion
    /// announcement above. **One template per state that has something true to
    /// say, and no template for `unknown`** — which is the default and the
    /// common case, because the verification controller is inert unless
    /// `MAGICIAN_VERIFICATION_CONTROLLER=enforce`. Absence is the phrasing
    /// there: a task nothing checked must not be spoken as verified, and it
    /// must not be spoken as broken either. Every other state (`repairing`,
    /// `verifying`, `unavailable`, `cancelled`, `blocked_partial`) is
    /// unsettled and equally gets no clause.
    pub const VOICE_VERIFICATION_VERIFIED: &str = "voice_verification_verified";
    pub const VOICE_VERIFICATION_UNVERIFIED: &str = "voice_verification_unverified";
    pub const VOICE_VERIFICATION_EXHAUSTED: &str = "voice_verification_exhausted";

    /// Spoken nudge for a run that has staged a code diff and is waiting for
    /// the user to approve it. Rendered from the derived proposal state, not
    /// from a HITL event, and spoken at most once per proposal.
    pub const VOICE_DIFF_APPROVAL_WAITING: &str = "voice_diff_approval_waiting";

    /// Voice-origin speech-tag instruction — appended to the chat
    /// outer-loop system prompt when the user message arrived via
    /// voice (mic → STT). Teaches the model the `<speech>` protocol
    /// and the optional delivery attributes (`emotion`, `style`,
    /// `pace`, `voice`, `emphasis`) that the TTS pipeline translates
    /// into provider-native hints.
    pub const VOICE_ORIGIN_SPEECH_INSTRUCTION: &str = "voice_origin_speech_instruction";

    /// Screen describe (the agent's one-shot "look and answer") system prompt
    pub const SCREEN_DESCRIBE_SYSTEM: &str = "screen_describe_system";

    /// Screen grounding (pixel click targeting) system prompt
    pub const SCREEN_GROUNDING_SYSTEM: &str = "screen_grounding_system";

    /// Screen observation per-frame narrator system prompt
    pub const SCREEN_OBSERVATION_SYSTEM: &str = "screen_observation_system";

    /// Stable-screen deep observation system prompt
    pub const SCREEN_DEEP_OBSERVATION_SYSTEM: &str = "screen_deep_observation_system";

    /// Meeting/observation transcript summarizer system prompt
    pub const MEETING_SUMMARY_SYSTEM: &str = "meeting_summary_system";

    /// VibeDev coding-coordinator execution-policy footer (relocated server-side
    /// from the cockpit's client-composed `buildCodingTaskDescription`, RCA fix #5).
    pub const VIBEDEV_EXECUTION_POLICY: &str = "vibedev_execution_policy";

    /// VibeDev scaffold directive — prepended to a Pi coding run when the bound
    /// project repo is an empty/uninitialized isolated dir, so Pi scaffolds a
    /// starter (stack named in the build request, else generic) before building.
    pub const VIBEDEV_SCAFFOLD_DIRECTIVE: &str = "vibedev_scaffold_directive";

    /// `@vibedev` chat rail — prose half of the VibeDev project-context block.
    /// The machine-read lines of that block are assembled in Rust; see
    /// `magician_v2::vibedev::rail`.
    pub const VIBEDEV_RAIL_PROJECT_CONTEXT: &str = "vibedev_rail_project_context";

    /// `@vibedev #discuss` planning directive (read-only, plan-as-deliverable).
    pub const VIBEDEV_RAIL_PLAN_DIRECTIVE: &str = "vibedev_rail_plan_directive";

    /// `@vibedev` follow-up continuation — the prose half of the block a rail
    /// turn carries when it continues the conversation's previous run. The
    /// machine-read `Parent task:` line is assembled in Rust and deliberately
    /// NOT stored here; see `magician_v2::vibedev::rail`.
    pub const VIBEDEV_RAIL_FOLLOW_UP_CONTEXT: &str = "vibedev_rail_follow_up_context";

    /// `@vibedev` chat replies. The rail answers in the user's own thread rather
    /// than through the model, so its user-facing prose lives in the store too.
    pub const VIBEDEV_RAIL_REPLY_STARTED: &str = "vibedev_rail_reply_started";
    pub const VIBEDEV_RAIL_REPLY_NO_PROJECT: &str = "vibedev_rail_reply_no_project";
    pub const VIBEDEV_RAIL_REPLY_EMPTY_PROMPT: &str = "vibedev_rail_reply_empty_prompt";
    pub const VIBEDEV_RAIL_REPLY_NO_CODING_LEAD: &str = "vibedev_rail_reply_no_coding_lead";
    pub const VIBEDEV_RAIL_REPLY_START_FAILED: &str = "vibedev_rail_reply_start_failed";
    /// The run is **durably admitted** but this turn did not get it started.
    /// Deliberately neither of its neighbours: `start_failed` promises "nothing
    /// was left running", which is false here, and `started` says the task is
    /// running, which is also false. Both were being used for this case, so the
    /// user was told the build was gone and then told it was going.
    pub const VIBEDEV_RAIL_REPLY_ADMITTED_NOT_STARTED: &str =
        "vibedev_rail_reply_admitted_not_started";
    /// The turn's server-derived idempotency key was reused for a *different*
    /// request. A refusal, not a failure: the run that key already names is
    /// untouched and still going.
    pub const VIBEDEV_RAIL_REPLY_KEY_CONFLICT: &str = "vibedev_rail_reply_key_conflict";
    /// Several live VibeDev projects and nothing pointing at one of them. A
    /// question rather than a refusal: it names the candidates and the one move
    /// that answers it (pick a project in the cockpit, then send the turn
    /// again).
    pub const VIBEDEV_RAIL_REPLY_AMBIGUOUS_PROJECT: &str = "vibedev_rail_reply_ambiguous_project";

    /// Recurring Monitors Phase 2 — versioned MONITOR_CONTEXT_V1 execution
    /// context. Appended (server-side, marker-idempotent) to the goal of
    /// every execution whose task manifest carries a `monitor_spec`: renders
    /// the spec (objective, sources, include/exclude rules, match mode) and
    /// the REQUIRED output contract instructing the agent to end the run by
    /// emitting a `monitor_run_result` JSON artifact matching
    /// `MonitorRunResultV1`. Generic (non-monitor) executions never see it.
    pub const MONITOR_EXECUTION_CONTEXT_V1: &str = "monitor_execution_context_v1";

    /// Routed `agentic_ledger_compaction` operation prompt. Renders the
    /// itemized compactable window (stable ids and protection metadata),
    /// non-compactable goal/open-requirement context, and target budget. It
    /// carries the strict §3.2 JSON patch output contract and neutral,
    /// extractive summary contract. No compiled fallback: a missing template
    /// fails the compaction call loudly
    /// (`CompactionCallFailure::TemplateMissing`) and the run falls back to
    /// deterministic eviction (§3.4). See
    /// `docs/archive/plans/2026-07-23-agentic-llm-led-compaction-design-plan.md`.
    pub const AGENTIC_COMPACTOR_V1: &str = "agentic_compactor_v1";
}

/// Prompt versions used in MagicianV2
pub mod versions {
    /// Legacy compatibility system contract for channel distillation.
    pub const CHANNEL_INGEST_DISTILL_SYSTEM_LEGACY: &str = "1.0.0";
    /// Legacy compatibility user contract with explicit truncation state.
    pub const CHANNEL_INGEST_DISTILL_USER_LEGACY: &str = "1.0.1";
    /// Information-complete V2 system contract for channel distillation.
    pub const CHANNEL_INGEST_DISTILL_SYSTEM: &str = "1.1.0";
    /// Information-complete V2 user contract with explicit truncation state.
    pub const CHANNEL_INGEST_DISTILL_USER: &str = "1.1.1";
    /// Targeted channel distillation response repair prompt.
    pub const CHANNEL_INGEST_DISTILL_REPAIR_USER: &str = "1.0.0";
    /// Body-blind channel thread classification contract.
    pub const CHANNEL_CLASSIFY: &str = "1.1.0";
    /// Local channel reply-draft contract.
    pub const CHANNEL_REPLY_DRAFT: &str = "1.0.0";
    /// Passive channel pattern synthesis contract.
    pub const CHANNEL_PATTERN_SYNTHESIS: &str = "1.0.0";
    /// Rich proactive resurfacing curation contract.
    pub const RESURFACING_CURATE: &str = "1.2.0";
    /// Explicit local deeper-summary contract.
    pub const RESURFACING_DEEP_SUMMARY: &str = "1.0.0";

    /// Version for unified query analysis prompt
    /// v3.2.0: Added required_capabilities extraction for LLM-driven capability
    /// completeness checking
    /// v3.3.0: Added task_clarity classification for progressive planning
    pub const UNIFIED_ANALYSIS: &str = "3.3.0";
    /// Version for unified query analysis system prompt
    pub const UNIFIED_ANALYSIS_SYSTEM: &str = "1.0.1";

    /// Version for task decomposition prompt
    pub const TASK_DECOMPOSITION: &str = "1.0.0";
    /// Version for task decomposition system prompt
    pub const TASK_DECOMPOSITION_SYSTEM: &str = "1.0.0";

    /// Version for tool matching prompt
    pub const TOOL_MATCHING: &str = "1.0.0";

    /// Version for parameter elicitation prompts (future use)
    pub const PARAMETER_ELICITATION: &str = "1.0.0";

    /// Version for response generation prompts (future use)
    pub const RESPONSE_GENERATION: &str = "1.0.0";

    /// Version for error handling prompts (future use)
    pub const ERROR_HANDLING: &str = "1.0.0";

    /// Version for atomic composition prompt
    pub const ATOMIC_COMPOSITION: &str = "1.5.0";
    /// Version for atomic composition system prompt
    pub const ATOMIC_COMPOSITION_SYSTEM: &str = "1.0.1";
    /// Version for atomic composition outline prompt
    pub const ATOMIC_COMPOSITION_OUTLINE: &str = "1.3.0";
    /// Version for atomic composition outline system prompt
    pub const ATOMIC_COMPOSITION_OUTLINE_SYSTEM: &str = "1.0.0";

    /// Version for entity mapping prompt
    pub const ENTITY_MAPPING: &str = "1.0.0";

    /// Version for ask loop clarifier templates
    pub const ASK_LOOP_CLARIFIER: &str = "1.0.1";

    /// Version for slot extraction prompt
    pub const SLOT_EXTRACTION: &str = "1.0.0";

    /// Version for question rewriting prompt
    pub const QUESTION_REWRITING: &str = "1.0.1";
    /// Version for clarification question curation prompt
    pub const QUESTION_CURATION: &str = "1.0.0";

    /// Version for parameter extraction prompt
    pub const PARAM_EXTRACTION: &str = "1.0.0";

    /// Version for parameter default inference prompt
    pub const PARAM_DEFAULT_INFERENCE: &str = "1.0.0";

    /// Version for discovery planning prompt
    pub const DISCOVERY_PLANNING: &str = "1.0.0";
    /// Version for discovery planning system prompt
    pub const DISCOVERY_PLANNING_SYSTEM: &str = "1.0.0";

    /// Version for discovery safety evaluation prompt
    pub const DISCOVERY_SAFETY_EVAL: &str = "1.0.0";

    /// Version for discovery result extraction prompt
    pub const DISCOVERY_RESULT_EXTRACTION: &str = "1.0.0";

    /// Version for parameter priority classification prompt
    pub const PARAM_PRIORITY_CLASSIFICATION: &str = "1.0.0";

    /// Version for answer interpretation prompt
    pub const ANSWER_INTERPRETATION: &str = "1.0.0";

    /// Version for chat system prompt (legacy)
    /// v1.0.0: Initial release with agent persona injection for identity-aware conversations
    pub const CHAT_SYSTEM: &str = "1.0.0";
    /// Version for chat outer-loop system prompt (synchronous-Responses path)
    /// v0.0.1: Initial release for the chat refactor — direct LLM call, tool_choice: auto,
    ///         no automatic task creation, chat-orchestrator framing, layered persona/personality/tools.
    /// v0.0.2: Adds active reusable procedure retrieval beside memory for per-turn operating guidance.
    /// v0.0.3: Adds the Response shape section — bias toward ~3–5 sentence replies, skip prefaces and
    ///         closing offers, match the user's depth signal. Chat-scoped only; agentic / memory /
    ///         mining prompts are untouched (they use separate operation profiles + prompts). Pairs
    ///         with the tightened `max_output_tokens` caps on chat-fast / chat-thinking in
    ///         `magician-config.yaml` — chat-perceived latency scales with output volume at
    ///         ~50–60 tok/s visible streaming, so terser replies are the highest-leverage
    ///         no-quality-drop perf lever.
    /// v0.0.4: Static/dynamic split — moves `memory_tiers_block`, `procedure_memory_block`,
    ///         `environment_knowledge`, `recent_session_files_block`, and `delegate_targets_block`
    ///         out of the system prompt into a separate first-user-turn `<context>` block so the
    ///         system-prompt prefix is stable across the session. Anthropic / OpenAI / Gemini
    ///         prefix caches now hit reliably turn-over-turn, cutting per-turn input token cost
    ///         on warm chats. Implements docs/plans/2026-04-10-prompt-cache-optimization.md.
    /// v0.0.6: Carries the owner's taste profile after the persona. The slot
    ///         is empty when no profile note exists, so a deployment without
    ///         one renders byte-identically to v0.0.5 and keeps the same
    ///         prefix-cache behaviour described above.
    pub const CHAT_OUTER_LOOP_SYSTEM: &str = "0.0.6";
    /// v1.0.0: initial capture distiller. Strict over recall — a wrong
    /// directive shapes every future session until the owner notices it.
    /// v1.1.0: adds retraction proposals; the profile is passed in so a
    /// contradicted directive is surfaced as a question. Silence is
    /// explicitly not contradiction.
    pub const TASTE_PROFILE_DISTILL: &str = "1.2.0";
    /// v1.0.0: biased toward inclusion — an over-included preference costs
    /// budget, an under-included one silently withholds what the owner asked for.
    pub const MEMORY_APPLICABILITY_JUDGE: &str = "1.0.0";
    /// Version for the public bot ordinary-chat addendum.
    /// v1.0.0: Moves the public-envoy chat-only policy out of a Rust-only
    ///         constant and pins the Presto/Magican identity without asserting
    ///         that the speaker is or is not the owner.
    /// v1.1.0 resolves product and primary-agent presentation identity at runtime
    /// and keeps the backend service name out of ordinary public conversation.
    pub const PUBLIC_ENVOY_CHAT_ONLY_INSTRUCTION: &str = "1.2.0";
    /// Version for the legacy compatibility stub of the former combined tutor/copilot prompt.
    pub const PERSONAL_TUTOR_CHAT_INSTRUCTIONS: &str = "1.0.0";
    /// Version for shared visual storyboard runtime contract.
    pub const VISUAL_STORYBOARD_RUNTIME: &str = "1.1.0";
    /// Version for Personal Tutor rail policy.
    pub const PERSONAL_TUTOR_POLICY: &str = "1.0.0";
    /// Version for App Copilot rail policy.
    pub const APP_COPILOT_POLICY: &str = "1.1.0";

    /// Version for answer interpretation system prompt
    pub const ANSWER_INTERPRETATION_SYSTEM: &str = "1.0.0";

    /// Version for placeholder resolution prompt
    pub const PLACEHOLDER_RESOLUTION: &str = "1.0.0";

    /// Version for agentic decision prompt
    /// v1.0.1: Added popup/modal/overlay handling guidance
    /// v1.0.2: Added page-level browser examples for the now-retired NAV-only browser path.
    /// v1.0.3: Added {delegation_section} variable for cross-agent delegation targets.
    /// v1.0.4: Added {capabilities_section} variable for dynamic capability tool awareness.
    /// v1.0.5: Added {task_context} variable and defer_execution guidance for deferred task execution.
    /// v1.0.6: Added rule CHECK GOAL BEFORE ACTING
    /// v1.0.7: Added artifact format documentation to goal_reached — use artifacts for result data instead of file writes.
    /// v1.0.8: Added {task_state_section} — injects persisted task state from prior runs.
    /// v1.0.9: Added artifact_type field documentation for dashboardable artifacts (custom:metric_set, custom:record_table, custom:activity_feed, custom:summary_note).
    /// v1.2.0: Adds {procedure_memory_section} for active reusable procedure retrieval with rationale.
    /// v1.3.0: Cache-layout reorder — moves `{task_context}` and `{hint_section}` above the
    ///         `<!--MAGICIAN_CACHE_BREAKPOINT-->` sentinel (truly stable per task), and pushes the
    ///         five memory sections (`user_memory_section`, `agent_memory_section`,
    ///         `agent_goal_memory_section`, `procedure_memory_section`,
    ///         `environment_knowledge_section`) BELOW it. Memory ranking can shift between
    ///         iterations (vector retrieval is non-deterministic across reranks) and was
    ///         invalidating the prefix cache every iteration. Now the stable prefix
    ///         (goal, criteria, capabilities, delegation, task_context, hint) survives memory
    ///         reranks turn-over-turn. Pure content reorder — no semantic / template changes.
    /// v1.3.1: Adds `{call_frequency_section}` between memory sections and `## CURRENT STATE`.
    ///         Surfaces a frequency rollup of `(call, args)` signatures across the entire
    ///         execution history (only repeated ones, count ≥ 2, capped at 15 unique signatures).
    ///         The windowed `EXECUTION HISTORY` below only shows ~10 recent iterations — without
    ///         this section, a planner that has called `list_tasks(status=completed)` 600× can't
    ///         see that repetition from its local window. Lets the LLM self-correct *before* the
    ///         loop detector's pressure threshold fires (≥5 identical control calls), and gives
    ///         a hard fact even when the threshold doesn't fire (e.g. interleaved repeats).
    /// v1.3.2: Adds `{artifact_guidance_section}` between `{input_context}` and `## TASK STATE
    ///         ACTION`. Centralises the "Persisting tangible deliverables" + "Output format
    ///         guidance" + optional "Producing dashboards" boilerplate so agent personas
    ///         don't duplicate ~2k chars each. Builder in
    ///         `decision.rs::build_artifact_output_guidance_section` — universal text always
    ///         injected for outer-loop executions, dashboard guidance conditionally appended
    ///         when `create_dashboard` is in the agent's tool catalog.
    /// v1.3.3: Keeps the full mutation contract once in the decision prompt while teaching
    ///         the common no-change response to emit only `action` + `reason`. This pairs
    ///         with the compact open extension schema carried by each native tool, avoiding
    ///         per-tool duplication without introducing a second task-state tool call.
    /// v1.3.4: Makes omission the canonical no-op task-state action. The full mutation
    ///         envelope remains inline and prompt-defined once, while the runtime defaults
    ///         a missing field to `none` and validates every present envelope strictly.
    /// v1.3.5: Consolidates six sparse execution signals under one optional
    ///         `decision_metadata` sidecar. The prompt defines the contract once while
    ///         runtime lowering accepts both nested and historical flat representations.
    /// v1.3.6: Stops advertising the retired `goal_reached` / `cannot_proceed`
    ///         LLM tools. Complete, partial, and blocked terminal reports now use
    ///         `yield` exclusively; compatibility lowering remains unchanged.
    /// v1.3.7: Removes legacy text-JSON decision-envelope examples and their
    ///         anti-JSON correction. Provider-native tool schemas are the sole
    ///         invocation-shape authority; behavioral and defer guidance remain.
    pub const AGENTIC_DECISION: &str = "1.3.7";

    /// Version for agentic decision system prompt.
    /// v1.0.0: Extracted from embedded constant in decision.rs.
    /// v1.0.1: Changed '4 categories' to 'these categories' for dynamic capability tool awareness.
    /// v1.0.2: Removed hardcoded tool category list and added {capabilities_section}
    /// variable for runtime capability injection in system prompt.
    /// v1.0.3: Added PARTIAL SUCCESS (prefer goal_reached(partial) over cannot_proceed) and
    /// STUCK ON A FAILING ACTION guidance (switch strategy after 2-3 same-class errors).
    /// v1.0.4: Introduces `yield` as the preferred unified terminal outcome-report tool —
    /// LLM describes outcome in structured fields and the orchestrator classifies. Legacy
    /// `goal_reached` / `cannot_proceed` remain accepted; `need_user_input` stays distinct.
    /// See docs/plans/2026-05-27-yield-decision-migration.md.
    /// v1.0.5: Removes model-facing references to the retired legacy terminal tools;
    /// compatibility aliases remain internal for persisted-history replay.
    /// v1.0.6: Uses direct native-tool language (`call`) and identifies the
    /// provider-native catalog as the invocation-shape authority.
    /// v1.0.7: Carries the owner's taste profile, rendered after the agent
    /// persona so an operator boundary always outranks owner taste. The
    /// section is empty when no profile note exists, which leaves the
    /// rendered prompt byte-identical to v1.0.6.
    pub const AGENTIC_DECISION_SYSTEM: &str = "1.0.7";

    /// Version for agentic input interpretation prompt
    pub const AGENTIC_INPUT_INTERPRETATION: &str = "1.0.0";
    /// Version for agentic input interpretation system prompt
    pub const AGENTIC_INPUT_INTERPRETATION_SYSTEM: &str = "1.0.0";

    // Version for the agent-browser inner-loop system prompt.
    // v0.0.1: Initial version.
    // v0.0.2: Added front-load-grounding philosophy, permissive eval, iframe-origin
    //         handling, coordinate-widget guidance, evidence-based strategy switching.
    // v0.0.3: Reframed operating philosophy from front-load-grounding (which the
    //         model over-applied as exhaustive script-source reading) to "simplest
    //         plausible action first; measure narrowly when needed; diagnose only
    //         after a failed action". Eval scoped to this-turn's needs.
    // v0.0.4: Added explicit scroll-into-view reflex bullet. v0.0.3 had only the
    //         consistency rule "keep measurement and action in the same scroll
    //         state"; v0.0.4 adds the prerequisite "scroll first before measuring
    //         or acting on viewport coordinates" because CDP mouse events at
    //         off-viewport coords silently fail to dispatch.
    // v0.0.5: Sharpened cross-origin / sandboxed iframe guidance. Adds explicit
    //         detection that `frame <selector>` may silently report success
    //         without switching for sandboxed iframes; adds explicit srcdoc-as-
    //         parent-readable reach-through for canvas/coord/handler-constant
    //         extraction; adds explicit parent-observable verification path
    //         (postMessage → parent DOM updates).
    // v0.0.6: Adds the viewport-screenshot-only rule for coordinate-based
    //         interaction. Yutori N1.5 normalises coordinates against the
    //         IMAGE it sees; magicllm denormalises against the VIEWPORT.
    //         The round-trip only works when image_dim == viewport_dim, so
    //         a click derived from a `screenshot --full` image lands in
    //         the wrong place (was observed on the SoTA-35 canvas test —
    //         agent took a 4800×9348 full-page screenshot, then every
    //         click landed at random positions in the page header). Rule:
    //         `screenshot --full` is for content reading only, never for
    //         click-targeting; for coord-based actions use viewport
    //         screenshots after `scrollintoview`.

    // Version for the generic inner-loop system prompt (fallback for inner-loop
    // packs without a dedicated mapping).
    // v0.0.1: Initial generic inner-loop dispatch prompt.
    // v0.0.2: Carries the reusable browser-loop disciplines into generic CLI
    //         packs: simplest useful action first, exact argv rules,
    //         meaningful-boundary verification, ledger use, raw/help restraint,
    //         and read-before-mutate safety.
    // v0.0.3: Adds the manifest-and-fetch guidance: tool results may be
    //         projected as compact manifests, the preview is a hint not the
    //         data, and `read_artifact` is the way to dereference the full
    //         content on demand.
    // v0.0.4: Introduces `yield` as the preferred terminal outcome-report
    //         tool (mirrors outer-loop v1.0.4). LLM describes outcome in
    //         structured fields and the outer loop classifies. Legacy
    //         `goal_reached` / `cannot_proceed` remain accepted; the inner
    //         loop's `need_user_input` stays as the interactive-ask primitive.

    // NOTE: OVERLAY_DETECTION_VISION version removed - see names module comment

    // Version for semantic text match system prompt
    // NOTE: SEMANTIC_TEXT_MATCH versions removed - feature was never implemented

    /// Version for impact review prompt
    /// v1.1.0: Removed expected_outcome to avoid misleading LLM with speculative guesses.
    /// Now focuses on observable action-effect correlation. Added concrete examples for dialog-target matching.
    /// v1.2.0: Added goal_context and action_reasoning for semantic verification.
    /// LLM now checks if action target matches goal requirements (catches row/column mismatches).
    /// v1.3.0: Enriched data pipeline — scroll_detail, dom_changes_detail, value_detail.
    /// LLM now sees container positions, viewport visibility, slider values, detailed DOM arrays.
    /// Added progress_type response field for tri-state outcome (goal_reached/partial_progress/no_effect).
    // Memory consolidation prompt versions
    /// Version for memory entity extraction user prompt.
    /// v1.2.0: Makes every entity field conditional on the authoritative
    /// target item schema while retaining durable-evidence filtering.
    pub const MEMORY_EXTRACT_ENTITIES: &str = "1.2.0";
    /// Version for memory entity extraction system prompt.
    /// v1.2.0: Removes the fixed entity shape and forbids undeclared fields.
    pub const MEMORY_EXTRACT_ENTITIES_SYSTEM: &str = "1.2.0";
    /// Version for memory activity summarization user prompt
    pub const MEMORY_SUMMARIZE_ACTIVITY: &str = "1.0.0";
    /// Version for memory activity summarization system prompt
    pub const MEMORY_SUMMARIZE_ACTIVITY_SYSTEM: &str = "1.0.0";
    /// Version for memory insight extraction user prompt
    pub const MEMORY_EXTRACT_INSIGHTS: &str = "1.0.0";
    /// Version for memory insight extraction system prompt
    pub const MEMORY_EXTRACT_INSIGHTS_SYSTEM: &str = "1.0.0";
    /// Version for memory insight distillation user prompt.
    /// v1.1.0: Makes evidence and confidence handling conditional on the
    /// authoritative target tier schema.
    pub const MEMORY_DISTILL_INSIGHTS: &str = "1.1.0";
    /// Version for memory insight distillation system prompt.
    /// v1.1.0: Forbids source-only fields in consolidated insight output.
    pub const MEMORY_DISTILL_INSIGHTS_SYSTEM: &str = "1.1.0";
    /// Version for memory user-level promotion user prompt.
    /// v1.1.0: Treats memory candidates as evidence and blocks promotion of
    /// single-run task/test/internal artifacts.
    pub const MEMORY_PROMOTE_TO_USER: &str = "1.1.0";
    /// Version for memory user-level promotion system prompt.
    /// v1.1.0: Adds memory_candidate source type and stricter user-memory
    /// promotion thresholds.
    pub const MEMORY_PROMOTE_TO_USER_SYSTEM: &str = "1.1.0";
    /// Version for memory episode archival user prompt
    pub const MEMORY_ARCHIVE_EPISODES: &str = "1.0.0";
    /// Version for memory episode archival system prompt
    pub const MEMORY_ARCHIVE_EPISODES_SYSTEM: &str = "1.0.0";
    /// Version for memory environment knowledge extraction user prompt.
    /// v1.1.0: Prioritizes memory candidates/final outputs and skips
    /// one-off local/test artifacts unless they reveal reusable behavior.
    pub const MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE: &str = "1.1.0";
    /// Version for memory environment knowledge extraction system prompt.
    /// v1.1.0: Allows object-wrapped `environments` and adds evidence
    /// priority for memory candidates, explicit failures, and action/result
    /// pairs.
    pub const MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE_SYSTEM: &str = "1.1.0";

    /// Version for post-run learning reflection user prompt.
    /// v1.1.0: adds routeable memory payload fields for Phase 3 memory
    /// candidate promotion.
    /// v1.2.0: adds routeable evaluation payload fields for eval backlog
    /// routing.
    /// v1.3.0: adds routeable capability-evolution payload fields for
    /// generic capability backlog routing.
    /// v1.4.0: adds routeable skill/workflow payload fields and
    /// workflow-signature gating for Phase 5 skill evolution.
    /// v1.5.0: adds structured meta-harness diagnosis and executable
    /// eval-worker handoff fields.
    /// v1.6.0: adds routeable program-state update fields for OPC runtime
    /// state.
    /// v1.7.0: adds existing procedure context and memory_procedure draft
    /// procedure extraction rules.
    /// v1.8.0: adds retrieved-procedure feedback, update, and deprecation
    /// proposal rules.
    /// v1.9.0: requires one evidence-based procedure feedback judgement per
    /// retrieved procedure.
    /// v1.10.0: program-state candidates use the focus-area program document
    /// path supplied in the run's program context (no hardcoded `program.md`
    /// default) and are reserved for harness-enabled autonomous agents.
    pub const LEARNING_REFLECTION: &str = "1.10.0";
    /// Version for post-run learning reflection system prompt.
    /// v1.1.0: distinguishes auto-promotable explicit user memory from
    /// review-required inferred memory.
    /// v1.2.0: distinguishes routeable eval candidates from direct
    /// promotion.
    /// v1.3.0: distinguishes routeable capability/tool candidates from
    /// direct promotion.
    /// v1.4.0: distinguishes skill/workflow candidates and treats workflow
    /// signatures as hints requiring an LLM reusable-procedure gate.
    /// v1.5.0: asks the reflection judge to classify failure causes and
    /// attach diagnoses to candidates.
    /// v1.6.0: adds routeable program-state update payload instructions.
    /// v1.7.0: routes reusable procedural knowledge to draft procedures
    /// instead of semantic memory or direct skill mutation.
    /// v1.8.0: reviews retrieved procedure usage and reserves runtime-only
    /// bookkeeping for the feedback bridge.
    /// v1.9.0: requires one evidence-based procedure feedback judgement per
    /// retrieved procedure.
    /// v1.10.0: program-state candidates target the focus-area program document
    /// path supplied in the run's program context (no hardcoded `program.md`
    /// default) and are reserved for harness-enabled autonomous agents.
    pub const LEARNING_REFLECTION_SYSTEM: &str = "1.10.1";

    /// Version for autonomous execution planning prompt
    pub const AUTONOMOUS_EXECUTION: &str = "1.0.0";
    /// Version for autonomous execution planning system prompt
    pub const AUTONOMOUS_EXECUTION_SYSTEM: &str = "1.0.0";

    /// Version for V3 execution-scoped agent output synthesis prompt
    pub const EXECUTION_OUTPUT_SYNTHESIZE: &str = "1.0.0";
    /// Version for V3 execution-scoped agent output synthesis system prompt
    pub const EXECUTION_OUTPUT_SYNTHESIZE_SYSTEM: &str = "1.0.1";
    /// Version for V3 task-scoped agent output synthesis prompt
    pub const TASK_AGENT_OUTPUT_SYNTHESIZE: &str = "1.0.0";
    /// Version for V3 task-scoped agent output synthesis system prompt
    pub const TASK_AGENT_OUTPUT_SYNTHESIZE_SYSTEM: &str = "1.0.1";
    /// Version for V3 task-scoped user output synthesis prompt
    pub const TASK_USER_OUTPUT_SYNTHESIZE: &str = "1.0.0";
    /// Version for V3 task-scoped user output synthesis system prompt.
    /// v1.3.0: rebalances DEFAULT format selection — text/html is now the
    /// default for substantive deliverables; text/markdown is reserved for
    /// extremely rudimentary output; application/json (MUI-JSON dashboards)
    /// for data-rich results. Prior versions defaulted to markdown for any
    /// prose-heavy result, so nearly every output landed as .md.
    /// v1.2.0: adds explicit chart-selection guidance (BarChart vs
    /// LineChart vs PieChart vs Heatmap vs Table vs MetricCard) and a
    /// default dashboard recipe so the LLM stops defaulting to plain
    /// tables when charts would tell the story.
    /// v1.1.0: teach the LLM about rich media types (MUI-JSON dashboards
    /// with live `dataSource` bindings, HTML with `data-magician-source`
    /// placeholders, markdown with KPI auto-extract) and the dashboard
    /// theme registry (`editorial` / `brutalist` / `refined` / `terminal`
    /// / `studio`). See `data/magician_v2/prompts/task_user_output_synthesize_system_v1.2.0.json`.
    pub const TASK_USER_OUTPUT_SYNTHESIZE_SYSTEM: &str = "1.4.1";
    /// Version for the task-result summarization system prompt.
    pub const TASK_SUMMARY_SYSTEM: &str = "1.0.0";
    /// Version for API Mining Phase 2 workflow compilation system prompt.
    /// v1.0.1: consumes privacy-safe structural sequence projections so raw
    /// request, response, browser, and authentication values never enter an
    /// operation-routed LLM request. See
    /// `data/magician_v2/prompts/workflow_compilation_system_v1.0.1.json`.
    /// v1.0.0: initial release with linear-graph compilation and two worked
    /// examples (simple login flow + 2FA branch with skip_if). See
    /// `data/magician_v2/prompts/workflow_compilation_system_v1.0.0.json`.
    pub const WORKFLOW_COMPILATION_SYSTEM: &str = "1.0.1";
    pub const RECIPE_COMPILE_SYSTEM: &str = "1.0.0";
    pub const RECIPE_MATCH_SYSTEM: &str = "1.0.0";

    /// Version for the voice modality addendum.
    /// v1.1.0: Adds a runtime-supplied memory-authority block. Owner-only
    /// realtime surfaces may answer direct personal-memory questions after
    /// scoped retrieval, while shared meeting threads retain a non-disclosure
    /// boundary. See
    /// `data/magician_v2/prompts/voice_modality_addendum_v1.1.0.json`.
    /// v1.0.0: Initial static content. See
    /// `data/magician_v2/prompts/voice_modality_addendum_v1.0.0.json`.
    pub const VOICE_MODALITY_ADDENDUM: &str = "1.1.0";

    /// Version for the GPT-Live-1 mouth prompt.
    /// v1.0.0: OpenAI live-prompting template. See
    /// `data/magician_v2/prompts/voice_live_mouth_system_v1.0.0.json`.
    pub const VOICE_LIVE_MOUTH_SYSTEM: &str = "1.0.0";

    /// Version for the meeting-join Presto mouth prompt.
    /// v1.0.0: Same text as the former compiled `PRESTO_INSTRUCTIONS`.
    /// See `data/magician_v2/prompts/voice_meeting_presto_system_v1.0.0.json`.
    pub const VOICE_MEETING_PRESTO_SYSTEM: &str = "1.0.0";

    /// Version for the voice context compaction system prompt and
    /// its user template.
    /// v1.0.0: Initial release. Folds the prior summary (optional)
    /// plus new voice turns into a short factual prose summary.
    /// See `data/magician_v2/prompts/voice_context_compaction_*.json`.
    pub const VOICE_CONTEXT_COMPACTION_SYSTEM: &str = "1.0.0";
    pub const VOICE_CONTEXT_COMPACTION: &str = "1.0.0";

    /// Version for the voice task-completion announcement template.
    /// v1.0.0: Initial release. Three variables (title, verb,
    /// summary_suffix). See
    /// `data/magician_v2/prompts/voice_task_completion_announcement_v1.0.0.json`.
    pub const VOICE_TASK_COMPLETION_ANNOUNCEMENT: &str = "1.0.0";

    /// Versions for the spoken verification clauses.
    /// v1.0.0: Initial release. No variables — each is one settled sentence
    /// about a state, and there is deliberately no `unknown` template.
    pub const VOICE_VERIFICATION_VERIFIED: &str = "1.0.0";
    pub const VOICE_VERIFICATION_UNVERIFIED: &str = "1.0.0";
    pub const VOICE_VERIFICATION_EXHAUSTED: &str = "1.0.0";

    /// Version for the spoken pending diff-approval nudge.
    /// v1.0.0: Initial release. Two variables (title, file_phrase); names no
    /// task id, because this one is only ever read aloud.
    pub const VOICE_DIFF_APPROVAL_WAITING: &str = "1.0.0";

    /// Version for the voice-origin speech-tag instruction.
    /// v1.0.0: Initial release covering the `<speech>` block protocol
    /// plus the optional `emotion`, `style`, `pace`, `voice`,
    /// `emphasis` delivery attributes consumed by the TTS provider
    /// chain. See
    /// `data/magician_v2/prompts/voice_origin_speech_instruction_v1.0.0.json`.
    pub const VOICE_ORIGIN_SPEECH_INSTRUCTION: &str = "1.0.0";

    /// Version for screen describe system prompt
    pub const SCREEN_DESCRIBE_SYSTEM: &str = "1.0.0";

    /// Version for screen grounding system prompt
    pub const SCREEN_GROUNDING_SYSTEM: &str = "1.0.0";

    /// Version for screen observation narrator system prompt
    pub const SCREEN_OBSERVATION_SYSTEM: &str = "1.0.0";

    /// Version for stable-screen deep observation system prompt
    pub const SCREEN_DEEP_OBSERVATION_SYSTEM: &str = "1.0.0";

    /// Version for meeting summary system prompt
    pub const MEETING_SUMMARY_SYSTEM: &str = "1.0.0";

    /// Version for the VibeDev execution-policy footer
    pub const VIBEDEV_EXECUTION_POLICY: &str = "1.0.1";

    /// Version for the VibeDev scaffold directive
    pub const VIBEDEV_SCAFFOLD_DIRECTIVE: &str = "1.0.0";

    /// Versions for the `@vibedev` chat rail's prose.
    pub const VIBEDEV_RAIL_PROJECT_CONTEXT: &str = "1.0.0";
    pub const VIBEDEV_RAIL_PLAN_DIRECTIVE: &str = "1.0.0";
    pub const VIBEDEV_RAIL_FOLLOW_UP_CONTEXT: &str = "1.0.0";
    pub const VIBEDEV_RAIL_REPLY_STARTED: &str = "1.0.0";
    pub const VIBEDEV_RAIL_REPLY_NO_PROJECT: &str = "1.0.0";
    /// v1.1.0: names the spoken invoke as well as the typed marker. This
    /// refusal is read aloud on the hands-free road, where "add `#discuss` on
    /// the same line" is advice a caller cannot follow.
    pub const VIBEDEV_RAIL_REPLY_EMPTY_PROMPT: &str = "1.1.0";
    pub const VIBEDEV_RAIL_REPLY_NO_CODING_LEAD: &str = "1.0.0";
    pub const VIBEDEV_RAIL_REPLY_START_FAILED: &str = "1.0.0";
    pub const VIBEDEV_RAIL_REPLY_ADMITTED_NOT_STARTED: &str = "1.0.0";
    pub const VIBEDEV_RAIL_REPLY_KEY_CONFLICT: &str = "1.0.0";
    pub const VIBEDEV_RAIL_REPLY_AMBIGUOUS_PROJECT: &str = "1.0.0";

    /// Version for the Recurring Monitors MONITOR_CONTEXT_V1 execution
    /// context.
    /// v1.0.0: Initial release. Seven variables (objective,
    /// monitor_revision, match_mode, sources_block, query_seeds_block,
    /// include_rules_block, exclude_rules_block) formatted server-side from
    /// the typed `MonitorSpecV1`; carries the mandatory
    /// `monitor_run_result` output contract with varied domain-neutral
    /// examples (status page, docs changelog, public dashboard). The
    /// backend recomputes all fingerprints/classifications at acceptance —
    /// the contract tells the model that explicitly so it never invents
    /// canonical hashes.
    pub const MONITOR_EXECUTION_CONTEXT_V1: &str = "1.0.0";

    /// Version for the agentic ledger compactor prompt.
    /// v1.1.0: Adds the real target character budget and per-item protected
    /// metadata so the model can produce a patch that fits without losing
    /// runtime-protected evidence.
    /// v1.0.0: Initial release for compaction Phase B (routed operation).
    /// Eight variables (goal_line, open_requirements_block, items_block,
    /// item_count, target_char_budget, summary_max_chars,
    /// rationale_max_chars, prior_rejection)
    /// rendered server-side from `CompactionWindow`; carries the strict
    /// `{"patches":[{id, action: keep|summarize|discard, summary?}],
    /// "rationale"}` output contract with domain-neutral examples (research
    /// run, ops run, coding run — deliberately mixed).
    pub const AGENTIC_COMPACTOR_V1: &str = "1.1.0";
}

// NOTE: System prompts are loaded from separate JSON files via PromptManager.
