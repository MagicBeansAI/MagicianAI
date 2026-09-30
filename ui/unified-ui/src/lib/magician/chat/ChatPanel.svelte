<script context="module" lang="ts">
	import type {
		HitlInputType,
		HitlOpenTarget,
		HitlSource
	} from '$lib/hitl/types';

	export const CLARIFICATION_RESPONDER_REQUEST_PREFIX = 'clarification_responder:';

	const CHAT_HITL_INPUT_TYPES = new Set<HitlInputType>([
		'text',
		'password',
		'choice',
		'multi_choice',
		'confirmation',
		'external_action',
		'file_path',
		'guidance',
		'tool_authorization',
		'sandbox_override',
		'diff_approval',
		'form'
	]);

	export interface PersistedChatHitlTargetInput {
		executionId: string;
		pauseStateId: string;
		escalationType?: string;
		requestId?: string;
		inputType?: string;
		question?: string;
		options?: Array<{ id: string; label: string; requires_input?: boolean }>;
		principal: string;
		workspace: string;
	}

	export function clarificationResponderId(
		escalationType: string | undefined,
		requestId: string | undefined,
		executionId: string
	): string | undefined {
		if (escalationType !== 'clarification') return undefined;
		const persisted = requestId?.trim();
		if (persisted?.startsWith(CLARIFICATION_RESPONDER_REQUEST_PREFIX)) {
			return persisted.slice(CLARIFICATION_RESPONDER_REQUEST_PREFIX.length).trim() || undefined;
		}
		// Older persisted clarification cards stored only execution_id. For
		// legacy AskLoop that id is also the workflow responder key.
		return persisted || executionId.trim() || undefined;
	}

	function compatibilityInputType(input: PersistedChatHitlTargetInput): HitlInputType {
		const explicit = input.inputType?.trim() as HitlInputType | undefined;
		if (explicit && CHAT_HITL_INPUT_TYPES.has(explicit)) return explicit;
		if (input.escalationType === 'cannot_proceed' || input.escalationType === 'loop_detected') {
			return 'external_action';
		}
		if (input.escalationType === 'tool_authorization') return 'tool_authorization';
		if (input.escalationType === 'sandbox_override') return 'sandbox_override';
		if (input.escalationType === 'clarification') return 'text';
		if (input.options?.some((option) => option.requires_input)) return 'guidance';
		return 'confirmation';
	}

	export function buildPersistedChatHitlTarget(
		input: PersistedChatHitlTargetInput
	): HitlOpenTarget | null {
		const executionId = input.executionId.trim();
		const pauseStateId = input.pauseStateId.trim();
		const responderId = clarificationResponderId(
			input.escalationType,
			input.requestId,
			executionId
		);
		const serviceRequestId = input.escalationType === 'clarification'
			? undefined
			: input.requestId?.trim() || undefined;
		const source: HitlSource = input.escalationType === 'clarification'
			? 'clarification'
			: serviceRequestId
				? 'user_request'
				: 'escalation';
		const correlationId = source === 'user_request' ? serviceRequestId : pauseStateId;
		if (
			!correlationId ||
			!input.principal.trim() ||
			!input.workspace.trim() ||
			(source === 'clarification' && !responderId) ||
			(source !== 'clarification' && source !== 'user_request' && !executionId)
		) {
			return null;
		}

		return {
			id: correlationId,
			source,
			input_type: compatibilityInputType(input),
			prompt: input.question?.trim() || 'Response required',
			input_schema: {
				options: input.options?.map((option) => ({
					id: option.id,
					label: option.label,
					requires_input: option.requires_input
				}))
			},
			identifiers: source === 'user_request'
				? { correlation_id: correlationId, request_id: correlationId }
				: source === 'clarification'
					? { correlation_id: correlationId }
					: { correlation_id: correlationId, pause_state_id: correlationId },
			scope: {
				principal: input.principal.trim(),
				workspace: input.workspace.trim(),
				workflow_id: source === 'clarification' ? responderId : undefined,
				task_id: source === 'clarification' ? responderId : undefined,
				execution_id: executionId || undefined
			}
		};
	}

	export function chatHitlContinuationIsCurrent(
		startedScopeKey: string,
		currentScopeKey: string,
		startedGeneration: number,
		currentGeneration: number
	): boolean {
		return startedScopeKey === currentScopeKey && startedGeneration === currentGeneration;
	}

	export interface ChatScopeSessionTarget {
		ui_thread_id?: string | null;
	}

	export interface ChatScopeSessionLoader<T extends ChatScopeSessionTarget> {
		openSession(sessionId: string): Promise<T | null>;
		loadActiveSession(threadId: string): Promise<T | null>;
	}

	/**
	 * A server-staged attachment is bound to an exact session. Open that
	 * session directly instead of resolving the generic active session, which
	 * may create or select a different history lane and orphan the composer
	 * chip. Fall back only when the seed is stale or belongs to another thread.
	 */
	export async function loadSeedSessionOrActive<T extends ChatScopeSessionTarget>(
		threadId: string,
		seedSessionId: string | null,
		loader: ChatScopeSessionLoader<T>
	): Promise<T | null> {
		if (seedSessionId) {
			const seeded = await loader.openSession(seedSessionId);
			const seededThread = seeded?.ui_thread_id?.trim() || 'general';
			if (seeded && seededThread === threadId) return seeded;
		}
		return loader.loadActiveSession(threadId);
	}
</script>

<script lang="ts">
	import { originalAnswerHref, messageTargetFromQuery } from '$lib/stores/chatMessageLinks';
	import { browser } from '$app/environment';
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/stores';
	import { createEventDispatcher, onDestroy, onMount, setContext, tick } from 'svelte';
	import { fly } from 'svelte/transition';
	import { get } from 'svelte/store';
	import {
		CHAT_TASK_PANEL_OPENER,
		type ChatTaskPanelOpener,
	} from '$lib/magician/chat/taskPanelContext';
	import {
		createMap as createThinkingMap,
		interpret as interpretThinkingMap
	} from '$lib/thinkingMaps/api';
	import { flip } from 'svelte/animate';
	import { fadeUp } from '$lib/motion';
	import ChatEmptyState from '$lib/magician/chat/components/ChatEmptyState.svelte';
	import SystemPill from '$lib/magician/chat/components/SystemPill.svelte';
	import AttachmentBubble from '$lib/magician/chat/components/AttachmentBubble.svelte';
	import EscalationCard from '$lib/magician/chat/components/EscalationCard.svelte';
	import TaskStatusCard from '$lib/magician/chat/components/TaskStatusCard.svelte';
	import PlannerDock from '$lib/magician/chat/components/PlannerDock.svelte';
	import PlanReplyBanner from '$lib/magician/chat/components/PlanReplyBanner.svelte';
	import { isRunExpanded, isTerminalTaskStatus } from '$lib/magician/chat/components/taskStatus';
	import {
		plannerDockTasks,
		taskPendingQuestions,
		titleCase
	} from '$lib/magician/chat/components/planHelpers';
	import {
		beginPlanReply,
		finishPlanReply,
		type PlanReplyComposerState,
		type PlanReplyIntent
	} from '$lib/magician/chat/planReplyState';
	import { latestActiveTaskTurnId } from '$lib/magician/chat/liveTurn';
	/*
	 * Both chat gestures open **the** task panel, in the shared drawer.
	 *
	 * A durable task reference, a task-status card and the PlannerDock's execute
	 * all hold a real store `Task`, so they go through `toTaskPanelModel` — the
	 * same adapter `/tasks`, `/today` and the thread workspace use, unchanged.
	 * An activity card's *Inspect run* holds an execution instead, and goes
	 * through `toExecutionPanelModel`. Two adapters, one panel, one drawer; the
	 * two panels that used to answer these gestures are gone.
	 */
	import ExportMenu from '$lib/magician/components/ExportMenu.svelte';
	import TaskPanelDrawer from '$lib/magician/tasks/TaskPanelDrawer.svelte';
	import {
		fetchExecutionPanelState,
		toExecutionPanelModel,
		type ExecutionPanelTarget
	} from '$lib/magician/tasks/executionPanelModel';
	import { streamExecutionPanelState } from '$lib/magician/tasks/executionPanelStream';
	import {
		fetchTaskOutputFiles,
		openAuthenticatedTaskOutput,
		taskOutputUrl
	} from '$lib/magician/tasks/taskOutputs';
	import { readTaskRunState } from '$lib/magician/tasks/taskAttention';
	import {
		createTaskPanelPoll,
		panelPollCadence,
		type PanelPollTarget
	} from '$lib/magician/tasks/taskPanelPoll';
	import { toTaskPanelModel, type PanelOutputFile } from '$lib/magician/tasks/taskPanelModel';
	import type { ExecutionPanelState } from '$lib/types/executionPanel';
	// Task-scoped despite the module name: both post to
	// `/v3/tasks/{id}/outputs/open-file|open-folder`.
	import {
		openInternalTaskOutputFile,
		revealInternalTaskOutputFile
	} from '$lib/internalTasks/api';
	// ExecutionPlanInspector lives with the route's chrome (under
	// /routes/(app)/); from $lib the path traverses back to it.
	import ExecutionPlanInspector from '../../../routes/(app)/ExecutionPlanInspector.svelte';
	import {
		attachInTurnActivityToAssistantMessages,
		attachedActivityMessageIds,
		collapseTaskProgressMessages,
		isHiddenTranscriptMessage as isHiddenTranscriptMessageHelper,
		chatStore,
		chatTurnIdByMessageIdStore,
		getChatTurnIdForMessage,
		getMessageContentBlocks,
		getMessageText,
	getStructuredResponsePlainText,
		normalizeMessages,
		type ChatMessageContent,
		type ChatMessage,
		type ChatRenderMessage,
		type ChatRenderTaskExecutionGroup,
		type EscalationOption,
		type UploadedAttachment
	} from '$lib/stores/chatStore';
	import { chatProfileStore } from '$lib/stores/chatProfileStore';
	import { chatHarnessPreferenceStore } from '$lib/stores/chatHarnessPreferenceStore';
	import { fetchEngineAvailability, type EngineRoster } from '$lib/plane/terminalGrants';
	import {
		codingChoiceFromSelection,
		codingProfileStore
	} from '$lib/stores/codingProfileStore';
	import { thinkingModeForTurn } from '$lib/stores/thinkingModeStore';
	import { planModeStore, type PlanComposerMode } from '$lib/stores/planModeStore';
	import { taskStore, type Task } from '$lib/stores/taskStore';
	import {
		shouldEnableStructuredResponseRollout,
		parseRolloutPercent
	} from '$lib/magician/structuredResponse/rollout';
	import { threadStore } from '$lib/stores/threadStore';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import RequestActivityCard from '$lib/magician/components/RequestActivityCard.svelte';
	import { claimPendingActivityForMessage } from '$lib/stores/chatTurnActivityStore';
	import ChatContentBlocks from '$lib/magician/components/chat/ChatContentBlocks.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import StructuredResponseRenderer from '$lib/magician/structuredResponse/StructuredResponseRenderer.svelte';
	import {
		classifyRef,
		refOpenMode,
		resolveHref
	} from '$lib/magician/links/classifyRef';
	import { isStructuredResponseV1, validateStructuredResponse } from '$lib/magician/structuredResponse/schema';
	import type { StructuredResponseActionV1, StructuredResponseV1 } from '$lib/magician/structuredResponse/types';
	import SpeakButton from '$lib/magician/components/chat/SpeakButton.svelte';
	import { putSeed, buildChatSeedContent } from '$lib/stores/vibeSeedStore';
	import VoiceAutoSendBanner from '$lib/magician/components/chat/VoiceAutoSendBanner.svelte';
	import { maybeAutoSpeakTail } from '$lib/media/tts/autoSpeak';
	import { speak as speakBrowserTts } from '$lib/media/tts/browserTts';
	import { ttsStore } from '$lib/media/tts/store';
	import {
		captureVoiceGuidedFlowScreen,
		discardVoiceGuidedFlowCapture,
		lockedVoiceGuidedFlowMessage,
		parseVoiceGuidedFlowInvocation,
		voiceGuidedFlowCaptureStillCurrent,
		type VoiceGuidedFlowAttachment,
		type VoiceGuidedFlowFeature
	} from '$lib/media/voice/guidedFlow';
	import {
		isScreenLocked,
		startScreenLockMonitoring
	} from '$lib/media/voice/screenLock';
	import QueueInspector from '$lib/magician/chat/QueueInspector.svelte';
	import ConcurrentVoiceRequests from '$lib/media/voice/ConcurrentVoiceRequests.svelte';
	import { concurrentVoiceStore, markConcurrentVoiceResultRead, submitConcurrentVoiceRequest, settleConcurrentVoiceInput, setConcurrentVoiceForegroundBusy, selectConcurrentVoiceContext } from '$lib/media/voice/concurrentVoice';
	import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';
	import { focusTrap } from '$lib/shared/focusTrap';
	import { createMenuKeydown } from '$lib/shared/menuKeydown';
	import { resolveAttentionPrompt } from '$lib/stores/attentionPromptStore';
	import {
		pendingHitlByChatTurnId,
		type HitlPendingEntry
	} from '$lib/stores/pendingHitlStore';
	import {
		hitlOpenTargetFromPendingEntry,
		openHitlPrompt
	} from '$lib/attention';
	import { postHitlResponse } from '$lib/hitl/adapters';
	import type { HitlRequest } from '$lib/hitl/types';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	// v5 modern-shell primitives (the only shell).
	// Legacy shell branch keeps the original markup byte-identical.
	import ContextPill from '$lib/shell/ContextPill.svelte';
	import FloatingComposer from '$lib/shell/FloatingComposer.svelte';
	import DesktopVoiceCenterStage from '$lib/media/voice/DesktopVoiceCenterStage.svelte';
	import TrayLiveVoiceBanner from '$lib/media/voice/TrayLiveVoiceBanner.svelte';
	import { voiceCallStore } from '$lib/media/voice/realtimeVoiceClient';

	// Live voice controls share the composer banner. Typed messages remain
	// available and keep the displayed chat session's queue semantics.
	$: voiceCallLive =
		$voiceCallStore.state === 'connecting'
		|| $voiceCallStore.state === 'connected'
		|| $voiceCallStore.state === 'reconnecting'
		// `rotating` fires during periodic session refresh (token
		// rotation). Include it so the pill doesn't unmount + remount
		// every refresh — that was causing the panel to disappear
		// and the timer to reset every ~few seconds.
		|| $voiceCallStore.state === 'rotating';
	import WorkbenchColumn from '$lib/shell/WorkbenchColumn.svelte';
	import { writeInteractiveSessionStdin } from '$lib/shell/interactiveSessionApi';
	import { openHistoryDrawer } from '$lib/shell/shellState';
	import { timedFetch } from '$lib/shared/fetch';
	import { fetchResurfacingDetail } from '$lib/today/resurfacingQueries';
	import {
		buildResurfacingChatContext,
		resurfacingChatContextFilename
	} from '$lib/today/resurfacingChatContext';
	import {
		type ComposerMentionItem,
		buildTaskMentionItems,
		buildFeatureMentionItems
	} from '$lib/magician/chat/composerMentions';
	import {
		isComposerReferenceRequestCurrent,
		type ComposerReferenceRequest
	} from '$lib/magician/chat/composerReferenceGuard';

	export let threadId: string | null = null;
	export let embedded = false;
	export let pageTitle = 'magican · Chat';
	// SERVER-staged attachments to surface as composer chips (screen
	// capture-and-ask: the backend uploads the capture — one screenshot, or
	// a clip's frames + mp4 — against a session before this panel ever sees
	// it). Applied once the panel's active session matches the attachments'
	// session — the staged-chips clear on session change (below) makes any
	// earlier application moot.
	export let seedStagedAttachments: (UploadedAttachment & { session_id: string })[] | null = null;
	type FireAndForgetMode = 'never' | 'tutor' | 'always';
	// `brainstorm` is a reason but never a MODE: `@brainstorm` routes to the
	// thinking-map surface on its own detection, not by host configuration.
	type FireAndForgetReason = Exclude<FireAndForgetMode, 'never'> | 'brainstorm';
	export let fireAndForget: FireAndForgetMode = 'never';
	/** This ChatPanel is mounted inside the desktop HUD overlay (`/hud`). Gates the
	 *  `@copilot` feature mention (HUD-only; copilot needs a screenshot to ground on).
	 *  tutor + tutor_quick are offered regardless. Set explicitly by the HUD page. */
	export let hud = false;

	const dispatch = createEventDispatcher<{
		/** The send routed to a surface OUTSIDE this panel (tutor / copilot
		 *  overlay rails, the thinking-map page), so the host may close. Fired
		 *  only once the dispatch has actually succeeded — a send that fails
		 *  validation keeps its error visible in a still-open host. */
		'fire-and-forget-send': {
			sessionId: string;
			text: string;
			reason: FireAndForgetReason;
		};
		/** The reply is going to stream into THIS panel, so the host may need
		 *  to make room for it (the HUD expands out of its collapsed
		 *  command-bar state). Fired before the turn resolves. */
		'inline-send': {
			sessionId: string;
			text: string;
		};
	}>();

	// The v5 ("modern") shell is the only shell — the legacy chat layout
	// (left ChatSessionSidebar + bottom chat-input-area) was removed.

	let messagesContainerEl: HTMLDivElement;
	let messagesEndEl: HTMLDivElement;
	let attachmentInputEl: HTMLInputElement;
	let composerEl: FloatingComposer | null = null;
	let inputText = '';
	let stagedAttachments: UploadedAttachment[] = [];
	// Retain provenance after the voice countdown is cancelled (for example,
	// when capture fails or the user presses Send early).
	let voiceComposerTurnOrigin = false;
	let isUploading = false;
	let voiceGuidedCaptureInFlight = false;
	let voiceGuidedCaptureGeneration = 0;
	// Covers the user-gesture permission await that happens before upload/send
	// guards can engage. The generation prevents a completion from an old chat
	// scope from clearing or reviving admission in the replacement scope.
	let voiceGuidedAdmissionInFlight = false;
	let voiceGuidedAdmissionGeneration = 0;
	let stagedAttachmentSessionId: string | null = null;
	let uploadBatchVersion = 0;
	let profileDropdownOpen = false;
	let engineRoster: EngineRoster = { engines: [{ name: 'magician', installed: true }], current: 'magician', chat_current: 'magician' };
	let selectedHarnessEngine = 'magician';
	let selectedHarnessModel = 'default';
	$: selectedHarnessEngine = $chatHarnessPreferenceStore.engine;
	$: selectedHarnessModel = $chatHarnessPreferenceStore.model;
	function harnessSendOptions(_sessionId: string): { harnessEngine?: string; harnessModel?: string } {
		// A browser with no choice of its own sends none, so the server default
		// applies — the same rule every other surface follows.
		if (!chatHarnessPreferenceStore.isChosen()) return {};
		return { harnessEngine: selectedHarnessEngine, harnessModel: selectedHarnessModel };
	}
	function selectHarnessEngine(engine: string): void {
		chatHarnessPreferenceStore.select(engine);
	}
	function selectHarnessModel(model: string): void {
		chatHarnessPreferenceStore.select(selectedHarnessEngine, model);
		closeProfileDropdown(true);
	}
	// Picker mechanics: ArrowUp/Down roam choice buttons,
	// Enter/Space activate natively (they're <button>s), Escape closes and
	// hands focus back to the composer's trigger chip.
	let profileDropdownEl: HTMLDivElement | null = null;

	async function toggleProfileDropdown(): Promise<void> {
		profileDropdownOpen = !profileDropdownOpen;
		if (!profileDropdownOpen) return;
		await tick();
		const options = Array.from(
			profileDropdownEl?.querySelectorAll<HTMLElement>('.chat-profile-option') ?? []
		);
		(options.find((el) => el.getAttribute('aria-pressed') === 'true') ?? options[0])?.focus();
	}

	function closeProfileDropdown(refocusTrigger: boolean): void {
		profileDropdownOpen = false;
		if (refocusTrigger) composerEl?.focusProfileTrigger();
	}

	const handleProfileListboxKeydown = createMenuKeydown({
		getMenuEl: () => profileDropdownEl,
		close: closeProfileDropdown,
		itemSelector: '.chat-profile-option:not(:disabled)'
	});
	let inspectPanelOpen = false;
	/**
	 * The run an activity card asked to inspect — a task id and the execution
	 * under it, which is all that gesture ever had. It used to be widened into a
	 * synthesized `Task` so a task-shaped panel would accept it; the panel takes
	 * a `TaskPanelModel` now, and an execution is one of the things that maps
	 * onto it, so nothing has to pretend.
	 */
	let inspectPanelTarget: ExecutionPanelTarget | null = null;
	let taskPanelOpen = false;
	let taskPanelTask: Task | null = null;

	/*
	 * The task panel's inputs, on the same discipline every other surface uses:
	 * the outputs list and the run behind the panel are fetched per opened task
	 * and each is stored beside the id it describes, so a slow reply for one task
	 * can never render under another. See `TasksWorkspace.svelte`.
	 */
	let panelOutputs: PanelOutputFile[] | null = null;
	let panelOutputsTaskId: string | null = null;
	let panelOutputsRequestId = 0;
	let panelRunState: ExecutionPanelState | null = null;
	let panelRunStateTaskId: string | null = null;
	/**
	 * Which of the task's runs the reader asked to read, or `null` for the one the
	 * task record points at. Same shape and same reason as `TasksWorkspace.svelte`
	 * — reader intent rather than loaded state, keyed on the task it was chosen
	 * about so it cannot be read under another one.
	 */
	let panelRunSelectionId: string | null = null;
	let panelRunSelectionTaskId: string | null = null;
	/*
	 * The inspected run's state and its outputs, under the same rule keyed on the
	 * execution rather than the task — two runs of one task are two different
	 * things to inspect, and the key has to tell them apart.
	 */
	let inspectState: ExecutionPanelState | null = null;
	let inspectStateKey: string | null = null;
	let inspectOutputs: PanelOutputFile[] | null = null;
	let inspectOutputsKey: string | null = null;
	let inspectRequestId = 0;
	/** Why the inspected run could not be read. The drawer renders it. */
	let inspectLoadError: string | null = null;
	/**
	 * The live subscription behind the run drawer, and the run it is for.
	 *
	 * Keyed rather than boolean so switching from one run's card to another's
	 * tears the first subscription down — an unkeyed flag would leave the old one
	 * running and both would write `inspectState`.
	 */
	let inspectStreamStop: (() => void) | null = null;
	let inspectStreamKey: string | null = null;
	let panelNow = Date.now();
	let panelClockHandle: ReturnType<typeof setInterval> | null = null;
	const PANEL_CLOCK_INTERVAL_MS = 60_000;
	/** When the task list this panel reads was last refreshed — its "as of" line. */
	let panelLastLoadedAt: number | null = null;
	let taskPanelLoadFailure: string | null = null;
	let inlinePlanActionKey: string | null = null;
	let isPlanSheetOpen = false;
	let planSheetTaskId: string | null = null;
	let devWorkbenchSession: { id: string; program: string | null; nonce: number; threadId: string | null } | null = null;
	let devWorkbenchFocusNonce = 0;
	let devWorkbenchActiveSessionId: string | null = null;
	let planReplyIntent: PlanReplyIntent<UploadedAttachment> | null = null;
	let respondingEscalationKeys = new Set<string>();
	let hitlContinuationGeneration = 0;
	// Task-card progressive disclosure — per-run expansion OVERRIDES,
	// keyed by the stable execution-group id (ChatRenderTaskExecutionGroup.id).
	// Absent = use the default (live runs and a card's lone content-bearing
	// run start open; everything else starts collapsed). A Map rather than
	// a plain Set so both override directions survive the run's own state
	// transitions — e.g. a live run the user collapsed must not pop back
	// open when its default flips at the terminal transition. View-local
	// only: reset with the rest of the page-local state, never persisted.
	let runExpansionOverrides = new Map<string, boolean>();
	let mounted = false;
	let lastScopeKey = '';
	let loadedTargetThreadId = '';
	let reloadingTargetThreadId: string | null = null;
	let resurfacingContextStageKey = '';
	let composerSkills: ComposerSkillEntry[] = [];
	let composerReferenceAgents: ComposerReferenceAgentEntry[] = [];
	let lastComposerReferenceKey = '';
	let composerReferenceGeneration = 0;
	type StructuredResponseRolloutKind =
		'tool_call_executed' | 'rich_tool_result' | 'attachment' | 'text' | 'escalation_resolved' | 'task_status_update' |
		'task_status_update_live';
	type StructuredResponseRolloutControl = boolean | null;
	let structuredResponseRollout: Record<StructuredResponseRolloutKind, StructuredResponseRolloutControl> = {
		tool_call_executed: null,
		rich_tool_result: null,
		attachment: null,
		text: null,
		escalation_resolved: null,
		task_status_update: null,
		task_status_update_live: null
	};

	const STRUCTURED_RESPONSE_ROLLOUT_STORAGE_PREFIX = 'magician.structured_response_rollout.v1.';
	const STRUCTURED_RESPONSE_ROLLOUT_QUERY_PREFIX = 'sr_';
	const STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SUFFIX = '_pct';
	const STRUCTURED_RESPONSE_ROLLOUT_ALL_KIND: StructuredResponseRolloutKind | 'all' = 'all';

	function readRolloutPctFromStorage(kind: StructuredResponseRolloutKind | 'all'): number | null {
		if (!browser) return null;
		const storageKey = `${STRUCTURED_RESPONSE_ROLLOUT_STORAGE_PREFIX}${kind}${STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SUFFIX}`;
		const raw = localStorage.getItem(storageKey);
		if (raw === null) return null;
		const parsed = parseRolloutPercent(raw);
		// Default behavior is full structured-response rollout for migrated kinds.
		// Explicit values remain available via query/localStorage for rollback windows.
		return parsed;
	}

	function parseRolloutBoolean(value: string | null): boolean | null {
		if (!value) return null;
		const normalized = value.trim().toLowerCase();
		if (normalized === '1' || normalized === 'true' || normalized === 'on' || normalized === 'enabled') {
			return true;
		}
		if (normalized === '0' || normalized === 'false' || normalized === 'off' || normalized === 'disabled') {
			return false;
		}
		return null;
	}

	function readRolloutFlagFromStorage(kind: StructuredResponseRolloutKind | 'all'): StructuredResponseRolloutControl {
		if (!browser) return null;
		const storageKey = `${STRUCTURED_RESPONSE_ROLLOUT_STORAGE_PREFIX}${kind}`;
		const raw = localStorage.getItem(storageKey);
		if (raw === null) return null;
		const parsed = parseRolloutBoolean(raw);
		return parsed === null ? null : parsed;
	}

	function readRolloutPercentFromSearchParams(
		params: URLSearchParams,
		kind: StructuredResponseRolloutKind | 'all'
	): number | null {
		const queryKey = `${STRUCTURED_RESPONSE_ROLLOUT_QUERY_PREFIX}${kind}${STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SUFFIX}`;
		const queried = parseRolloutPercent(params.get(queryKey));
		if (queried !== null) return queried;
		return readRolloutPctFromStorage(kind);
	}

	function readRolloutFromSearchParams(
		params: URLSearchParams,
		kind: StructuredResponseRolloutKind | 'all'
	): StructuredResponseRolloutControl {
		const queryKey = `${STRUCTURED_RESPONSE_ROLLOUT_QUERY_PREFIX}${kind}`;
		const queried = parseRolloutBoolean(params.get(queryKey));
		if (queried !== null) return queried;
		return readRolloutFlagFromStorage(kind);
	}

	function resolveStructuredResponseRollout(
		params: URLSearchParams
	): Record<StructuredResponseRolloutKind, StructuredResponseRolloutControl> {
		return {
			tool_call_executed: readRolloutFromSearchParams(params, 'tool_call_executed'),
			rich_tool_result: readRolloutFromSearchParams(params, 'rich_tool_result'),
			attachment: readRolloutFromSearchParams(params, 'attachment'),
			text: readRolloutFromSearchParams(params, 'text'),
			escalation_resolved: readRolloutFromSearchParams(params, 'escalation_resolved'),
			task_status_update: readRolloutFromSearchParams(params, 'task_status_update'),
			task_status_update_live: readRolloutFromSearchParams(params, 'task_status_update_live')
		};
	}

	function isStructuredResponseRolloutEnabledForKind(
		kind: StructuredResponseRolloutKind,
		message: ChatMessage
	): boolean {
		if (structuredResponseRollout[kind] !== null) return structuredResponseRollout[kind] as boolean;

		const globalBoolean = readRolloutFromSearchParams($page.url.searchParams, STRUCTURED_RESPONSE_ROLLOUT_ALL_KIND);
		if (globalBoolean !== null) return globalBoolean;
		const globalPercent = readRolloutPercentFromSearchParams($page.url.searchParams, STRUCTURED_RESPONSE_ROLLOUT_ALL_KIND);
		if (globalPercent !== null) {
			if (globalPercent <= 0) return false;
			const scope = get(scopeIdentityStore);
			const seed = `${scope.principal}:${scope.workspace}:${kind}:${message.session_id ?? message.id}`;
			return shouldEnableStructuredResponseRollout({ seed, percent: globalPercent });
		}

		const percent = readRolloutPercentFromSearchParams($page.url.searchParams, kind);
		const scope = get(scopeIdentityStore);
		const seed = `${scope.principal}:${scope.workspace}:${kind}:${message.session_id ?? message.id}`;
		if (percent !== null) return percent > 0
			? shouldEnableStructuredResponseRollout({ seed, percent })
			: false;
		return shouldEnableStructuredResponseRollout({ seed, percent: 1 });
	}

	function resolveStructuredResponseForMessage(
		message: ChatMessage,
		kind: keyof typeof structuredResponseRollout
	): StructuredResponseV1 | null {
		if (message.id.startsWith('streaming-')) return null;
		return resolveStructuredResponseFromMessagePresentation(message, kind);
	}

	function normalizePresentationText(value: string): string {
		return value.trim().replace(/\s+/g, ' ');
	}

	function resolveStructuredResponseFromMessagePresentation(
		message: ChatMessage,
		_kind: keyof typeof structuredResponseRollout
	): StructuredResponseV1 | null {
		const presentation = message.presentation;
		if (!presentation) return null;
		if (!isStructuredResponseV1(presentation)) return null;
		const validation = validateStructuredResponse(presentation);
		if (!validation.ok) return null;
		const canonicalText = normalizePresentationText(
			getStructuredResponsePlainText(message.content)
		);
		const presentedText = normalizePresentationText(presentation.plain_text);
		if (canonicalText !== presentedText) return null;
		return presentation;
	}

	function resolveStructuredResponseForText(message: ChatMessage): StructuredResponseV1 | null {
		if (message.direction !== 'assistant') return null;
		return resolveStructuredResponseForMessage(message, 'text');
	}

	/** The text owned by the compact playback control in a message header. */
	function messageSpeechText(message: ChatMessage): string {
		return resolveStructuredResponseForText(message)?.plain_text ?? getMessageText(message.content);
	}

	function resolveStructuredResponseForTerminalTaskStatusUpdate(message: ChatMessage): StructuredResponseV1 | null {
		if (!isTerminalTaskStatus(message.content?.status)) {
			return null;
		}
		return resolveStructuredResponseForMessage(message, 'task_status_update');
	}

	function resolveStructuredResponseForLiveTaskStatusUpdate(message: ChatMessage): StructuredResponseV1 | null {
		if (isTerminalTaskStatus(message.content?.status)) {
			return null;
		}
		return resolveStructuredResponseForMessage(message, 'task_status_update_live');
	}

	function resolveStructuredResponseForTaskStatusUpdate(message: ChatMessage): StructuredResponseV1 | null {
		// Task cards retain their domain controls (watching, run expansion, and
		// output inspection). Do not replace those semantics with the generic
		// renderer until its contract includes every task interaction.
		void message;
		return null;
	}

	type ComposerSkillKind = 'procedure' | 'personality-mode' | 'compiled';

	interface ComposerSkillEntry {
		name: string;
		description?: string;
		kind: ComposerSkillKind;
		layer?: string;
		route?: 'direct' | 'delegate';
		owner_agent_id?: string | null;
		owner_agent_name?: string | null;
	}

	interface ComposerReferenceAgentEntry {
		agent_id: string;
		name?: string;
		description?: string;
		route?: 'self' | 'delegate';
	}

	// Phase 3.5a follow-up — per-session tailed-task id. Drives
	// the Watch live / Stop watching affordances on TaskStatusUpdate
	// cards. Fetched on session change, refreshed after each
	// subscribe/unsubscribe action so the buttons reflect server
	// truth without a polling loop.
	let tailedTaskId: string | null = null;

	async function refreshTailedTaskId(sessionId: string | null): Promise<void> {
		if (!sessionId) {
			tailedTaskId = null;
			return;
		}
		const scope = get(scopeIdentityStore);
		if (!scope.principal || !scope.workspace) {
			tailedTaskId = null;
			return;
		}
		const url = `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/tailed-task`;
		try {
			const resp = await timedFetch(url, { method: 'GET' });
			if (!resp.ok) return;
			const data = (await resp.json()) as { task_id: string | null };
			tailedTaskId = data.task_id ?? null;
		} catch {
			// best-effort; leave the previous value in place
		}
	}

	async function watchLive(sessionId: string, taskId: string): Promise<void> {
		const scope = get(scopeIdentityStore);
		if (!scope.principal || !scope.workspace) return;
		const url = `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/tailed-task`;
		try {
			await timedFetch(url, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ task_id: taskId }),
			});
			await refreshTailedTaskId(sessionId);
		} catch {
			// silent — the user can retry
		}
	}

	async function stopWatching(sessionId: string): Promise<void> {
		const scope = get(scopeIdentityStore);
		if (!scope.principal || !scope.workspace) return;
		const url = `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/tailed-task`;
		try {
			await timedFetch(url, { method: 'DELETE' });
			await refreshTailedTaskId(sessionId);
		} catch {
			// silent
		}
	}

	// Subscribe to store
	$: activeSessionId = $chatStore.activeSessionId;
	// Refresh tailed-task whenever the active session changes.
	$: void refreshTailedTaskId(activeSessionId);
	$: sessions = $chatStore.sessions;
	$: messages = $chatStore.messages;
	$: visibleMessages = attachInTurnActivityToAssistantMessages(
		collapseTaskProgressMessages(
			normalizeMessages(messages).filter((message) => !isHiddenTranscriptMessage(message)),
            focusedAnswerId
		)
	);
	// Set of message ids that have been folded into an assistant
	// message's `attachedActivityMessages`. The chat-page render loop
	// skips standalone rendering for these so they appear ONLY inside
	// the assistant bubble's `<AssistantActivitySection />` dropdown.
	$: attachedActivityIds = attachedActivityMessageIds(visibleMessages);
	$: isLoading = $chatStore.isLoading;
	$: canBuildInVibe = visibleMessages.some(({ message }) =>
		(message.direction === 'user' || message.direction === 'assistant')
		&& getMessageText(message.content).trim().length > 0
	);

	// M4 — seed a VibeDev build from this chat thread's recent turns.
	function buildInVibe(): void {
		const turns = visibleMessages
			.map((vm) => vm.message)
			.filter((m) => m && (m.direction === 'user' || m.direction === 'assistant'))
			.map((m) => ({ role: m.direction, text: getMessageText(m.content) }))
			.filter((t) => t.text.trim().length > 0);
		if (turns.length === 0) return;
		const id = putSeed({
			source: 'chat',
			label: 'Chat thread',
			sourceId: activeSessionId ?? undefined,
			content: buildChatSeedContent(turns, 12),
			suggestedPrompt: ''
		});
		void goto('/vibe?seed=' + id);
	}
	let textAdmissionInFlight = false;
	$: isSendingMessage = $chatStore.isSendingMessage;
	$: composerBusy = isSendingMessage || (!!activeSessionId && $chatStore.activeRunSessionId === activeSessionId);
	$: setConcurrentVoiceForegroundBusy(composerBusy || !!inputText.trim() || stagedAttachments.length > 0 || isUploading || concurrentAdmissionInFlight);
	let lastConcurrentSessionId: string | null | undefined;
	function concurrentSessionChanged(sessionId: string | null): void {
		if (lastConcurrentSessionId !== undefined && sessionId !== lastConcurrentSessionId) {
			const focus = get(concurrentVoiceStore).focus;
			if (focus && focus.parent_session_id !== sessionId) selectConcurrentVoiceContext(null);
		}
		lastConcurrentSessionId = sessionId;
	}
	$: concurrentSessionChanged(activeSessionId);
	let lastConcurrentFocusId: string | null = null;
	$: if ($concurrentVoiceStore.focus && $concurrentVoiceStore.focus.id !== lastConcurrentFocusId && !isSendingMessage && !inputText.trim() && stagedAttachments.length === 0 && !isUploading && !concurrentAdmissionInFlight && !isReadOnly) {
		lastConcurrentFocusId = $concurrentVoiceStore.focus.id;
		if ($concurrentVoiceStore.focus.parent_session_id !== activeSessionId) void chatStore.openSession($concurrentVoiceStore.focus.parent_session_id);
	}
	let concurrentAdmissionInFlight = false;

	/**
	 * The chat_turn_id of the currently-in-flight request, if any.
	 * Used to give `<RequestActivityCard live>` to exactly that user
	 * message's card. All other (historical) cards REST-fetch their
	 * events once on mount — no SSE per old turn.
	 *
	 * Heuristic: when `isSendingMessage` is true, the in-flight turn
	 * is the most recent user message with a `chat_turn_id`. (Multiple
	 * pending sends in one session is rare and only the latest is
	 * actively being processed; queued ones have no events yet.)
	 */
	$: inFlightChatTurnId = isSendingMessage
		? (() => {
				for (let i = visibleMessages.length - 1; i >= 0; i -= 1) {
					const m = visibleMessages[i]?.message;
					if (m?.direction === 'user') {
						return getChatTurnIdForMessage(m, $chatTurnIdByMessageIdStore);
					}
				}
				return null;
			})()
		: null;

	// A task spawned by a chat turn can keep emitting after the assistant's
	// hand-off reply lands. Keep that exact spawning turn subscribed, using
	// the correlation persisted on TaskStatusUpdate messages. Never infer that
	// an uncorrelated historical task belongs to the session's newest user
	// message: old planning/paused cards otherwise make every later completed
	// voice/chat turn render a permanent Steps spinner.
	$: activeTaskTurnId = latestActiveTaskTurnId(
		visibleMessages.map((vm) => vm.message),
		(message) => getChatTurnIdForMessage(message, $chatTurnIdByMessageIdStore)
	);
	function visibleTurnHasAssistantResponse(turnId: string, turnMap: Map<string, string>): boolean {
		return visibleMessages.some((vm) => {
			const m = vm.message;
			if (m.direction !== 'assistant') return false;
			if (m.id.startsWith('streaming-')) return false;
			return getChatTurnIdForMessage(m, turnMap) === turnId;
		});
	}
	$: chatTurnsWithTerminalTask = (() => {
		const turnMap = $chatTurnIdByMessageIdStore;
		const set = new Set<string>();
		for (const vm of visibleMessages) {
			const m = vm.message;
			if (m.content.type !== 'task_status_update') continue;
			if (!isTerminalTaskStatus(m.content.status)) continue;
			const tid = getChatTurnIdForMessage(m, turnMap);
			if (tid) set.add(tid);
		}
		return set;
	})();
	$: latestOpenUserTurnId = (() => {
		const turnMap = $chatTurnIdByMessageIdStore;
		for (let i = visibleMessages.length - 1; i >= 0; i -= 1) {
			const m = visibleMessages[i]?.message;
			if (m?.direction !== 'user') continue;
			const tid = getChatTurnIdForMessage(m, turnMap);
			if (!tid || tid.startsWith('voice-request-') || visibleTurnHasAssistantResponse(tid, turnMap)) continue;
			if (chatTurnsWithTerminalTask.has(tid)) continue;
			// Keep only the current live-ish no-response turn subscribed. This
			// covers HUD/Tauri attachment sends where the optimistic user row
			// exists before global sending state or activity events catch up.
			if (Date.now() - m.created_at > 10 * 60 * 1000) continue;
			return tid;
		}
		return null;
	})();
	$: liveTurnId =
		inFlightChatTurnId ?? activeTaskTurnId ?? latestOpenUserTurnId;

	/**
	 * Per-turn thinking-mode state for the chat-input chip. The chip
	 * appears the moment the backend emits `ThinkingModeActivated` for
	 * the in-flight turn (LLM self-escalated from the fast variant of
	 * an adaptive profile to the thinking variant) and clears on
	 * `ThinkingModeCompleted` at turn end. The chip is bound to the
	 * specific in-flight turn id, so when the next user message starts
	 * a fresh turn the chip resets automatically — the new turn begins
	 * on the fast variant with no escalation event yet.
	 */
	$: thinkingModeStore = thinkingModeForTurn(inFlightChatTurnId);
	$: thinkingModeForActiveTurn = $thinkingModeStore.active;
	$: thinkingModeReasonForActiveTurn = $thinkingModeStore.reason;

	/**
	 * Set of chat_turn_ids that already have an assistant response in
	 * the visible message list. Used to anchor the activity card to
	 * the assistant bubble (like the old "What happened" pattern)
	 * once the reply lands. While the reply is still in-flight
	 * (turn_id not in this set), the card floats under the user
	 * message instead. One card per turn, never duplicated.
	 */
	$: chatTurnsWithResponse = (() => {
		const turnMap = $chatTurnIdByMessageIdStore;
		const set = new Set<string>();
		for (const vm of visibleMessages) {
			const m = vm.message;
			if (m.direction !== 'assistant') continue;
			if (m.id.startsWith('streaming-')) continue;
			const tid = getChatTurnIdForMessage(m, turnMap);
			if (tid) set.add(tid);
		}
		return set;
	})();

	/**
	 * Last user-message id per chat_turn_id. With attachments, one send
	 * produces multiple user messages (N attachment rows + 1 text row),
	 * all stamped with the same `chat_turn_id`. Without this map we'd
	 * render the floating activity card N+1 times, all subscribed to the
	 * same SSE. We keep only the latest one in visible order so the card
	 * sits right above the composer like a "live status under what you
	 * just sent".
	 */
	$: lastUserMessageIdByTurnId = (() => {
		const turnMap = $chatTurnIdByMessageIdStore;
		const map = new Map<string, string>();
		for (const vm of visibleMessages) {
			const m = vm.message;
			if (m.direction !== 'user') continue;
			const tid = getChatTurnIdForMessage(m, turnMap);
			if (tid) map.set(tid, m.id);
		}
		return map;
	})();

	// Claim pending activity rows for the latest non-streaming assistant
	// message. Deferred via `tick()` because the same reactivity tick that
	// surfaces the new assistant message also unmounts <ChatTurnProgress>;
	// without the `await tick()`, this block fires before `onDestroy`
	// runs (Svelte applies $: statements before DOM updates / lifecycle
	// hooks) and pending is still empty. After `tick()`, the unmount has
	// completed and `captureCompletedTurnActivity` has populated pending.
	let lastClaimedAssistantId: string | null = null;
	let lastClaimedSessionId: string | null = null;
	$: if (activeSessionId !== lastClaimedSessionId) {
		// Reset the page-scoped claim guard on every session switch so
		// the next reactive tick re-claims pending rows for the new
		// session even when the new session's latest assistant id
		// happens to match the prior session's. Without this, the
		// guard could no-op a legitimate claim after an A→B→A switch
		// where both sessions' latest assistant ids happen to collide
		// (unlikely but possible — server-generated IDs aren't
		// session-scoped). Also lets us pick up freshly-arrived
		// pending rows that landed while we were on a different
		// session's view.
		lastClaimedAssistantId = null;
		lastClaimedSessionId = activeSessionId;
	}
	$: void scheduleAssistantActivityClaim(activeSessionId, visibleMessages);

	async function scheduleAssistantActivityClaim(
		sessionId: string | null,
		msgs: typeof visibleMessages
	): Promise<void> {
		if (!sessionId || msgs.length === 0) return;
		await tick();
		const latestAssistant = [...msgs]
			.reverse()
			.find(
				(entry) =>
					entry.message.direction !== 'user' &&
					!entry.message.id.startsWith('streaming-')
			);
		if (!latestAssistant) return;
		if (latestAssistant.message.id === lastClaimedAssistantId) return;
		lastClaimedAssistantId = latestAssistant.message.id;
		claimPendingActivityForMessage(sessionId, latestAssistant.message.id);
	}
	$: viewingArchivedId = $chatStore.viewingArchivedId;
	$: isReadOnly = viewingArchivedId !== null;

	/**
	 * Open the canonical HITL response modal for the typing-bubble pill.
	 * Rebuilds a full `HitlRequest` from the raw event the
	 * `pendingHitlStore` captured, hands it to the shared direct-open primitive
	 * modal + same POST `/attention` uses) so chat and attention resolve
	 * through one bearer-authenticated path without depending on the request also
	 * being present in a loaded feed page.
	 */
	async function openHitlForTurn(entry: HitlPendingEntry): Promise<void> {
		const target = hitlOpenTargetFromPendingEntry(entry);
		if (!target) {
			showError('Unable to open response: HITL event shape not recognized.');
			return;
		}
		const result = await openHitlPrompt(target);
		if (result.status === 'error') showError(result.error);
	}
	$: if (activeSessionId !== stagedAttachmentSessionId) {
		stagedAttachmentSessionId = activeSessionId;
		stagedAttachments = [];
		voiceComposerTurnOrigin = false;
		resurfacingContextStageKey = '';
		uploadBatchVersion += 1;
		isUploading = false;
		voiceGuidedCaptureInFlight = false;
		voiceGuidedCaptureGeneration += 1;
		voiceGuidedAdmissionGeneration += 1;
		voiceGuidedAdmissionInFlight = false;
	}
	// Surface the server-staged attachments once the active session matches.
	// `seededAttachmentIds` makes each seed apply-once: without it this
	// reactive would resurrect a chip every time the user removes it.
	let seededAttachmentIds = new Set<string>();
	$: if (seedStagedAttachments && seedStagedAttachments.length > 0) {
		const pending = seedStagedAttachments.filter(
			(seed) =>
				activeSessionId === seed.session_id
				&& !seededAttachmentIds.has(seed.attachment_id)
				&& !stagedAttachments.some(
					(attachment) => attachment.attachment_id === seed.attachment_id
				)
		);
		if (pending.length > 0) {
			for (const seed of pending) {
				seededAttachmentIds.add(seed.attachment_id);
			}
			stagedAttachments = [
				...stagedAttachments,
				...pending.map((seed) => ({
					attachment_id: seed.attachment_id,
					filename: seed.filename,
					mime_type: seed.mime_type,
					size: seed.size,
					server_registered_capture: seed.server_registered_capture,
					label: seed.label ?? seed.filename
				}))
			];
		}
	}
	// A NEW seed can target a session this panel hasn't resolved yet — same
	// thread, different active session (the backend rotates the dated session
	// at day boundaries). Reload the scope once per such seed so the panel
	// catches up to the session the capture was staged on.
	let seedReloadRequestedFor: string | null = null;
	$: {
		const seedSessionId = seedStagedAttachments?.[0]?.session_id ?? null;
		if (
			browser
			&& mounted
			&& seedSessionId
			&& activeSessionId
			&& activeSessionId !== seedSessionId
			&& loadedTargetThreadId === targetThreadId
			&& !reloadingTargetThreadId
			&& seedReloadRequestedFor !== seedSessionId
		) {
			seedReloadRequestedFor = seedSessionId;
			void reloadChatScope();
		}
	}
	$: targetThreadId = normalizeThreadId(threadId);
	$: currentSessionId = viewingArchivedId ?? activeSessionId;
	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: currentSession = currentSessionId
		? sessions.find((session) => session.id === currentSessionId) ?? null
		: null;
	$: planComposerMode = $planModeStore.mode;
	$: composerPermission = $planModeStore.permission;
	$: if (browser && mounted) {
		structuredResponseRollout = resolveStructuredResponseRollout($page.url.searchParams);
	}
	$: currentThreadId = normalizeThreadId(currentSession?.ui_thread_id ?? targetThreadId);
	let lastVoiceTitleRefresh = '';
	$: if (currentSessionId && !currentSession?.title?.trim()) {
		const firstRequest = $concurrentVoiceStore.requests.find(request => request.parent_session_id === currentSessionId);
		const refreshKey = firstRequest ? `${currentScopeKey}:${firstRequest.id}` : '';
		if (refreshKey && refreshKey !== lastVoiceTitleRefresh) {
			lastVoiceTitleRefresh = refreshKey;
			void chatStore.loadSessions(currentThreadId);
		}
	}
	$: routeDevSessionId = (($page.url.searchParams.get('dev_session') || '').trim() || null);
	$: currentThreadRecord = $threadStore.threads.find((t) => t.id === currentThreadId) ?? null;
	$: currentThreadDisplayMode = (currentThreadRecord?.display_mode ?? 'chat') as 'chat' | 'dev';
	$: currentThreadPlanMode = currentThreadRecord?.plan_mode ?? false;
	$: currentThreadTasks = !isReadOnly
		? sortThreadTasks(
				$taskStore.tasks.filter((task) => normalizeThreadId(task.uiThreadId) === currentThreadId)
			)
		: [];
	$: currentPlannerDockTasks = plannerDockTasks(currentThreadTasks);
	$: planSheetTask =
		planSheetTaskId ? $taskStore.tasks.find((task) => task.id === planSheetTaskId) ?? null : null;
	$: currentPlanReplyTarget = (() => {
		if (planReplyIntent) {
			const focusedTask = currentThreadTasks.find(
				(task) => task.id === planReplyIntent?.taskId && task.planStatus === 'eliciting'
			);
			return focusedTask ?? null;
		}
		return null;
	})();
	$: currentPlanReplyQuestion = (() => {
		if (!currentPlanReplyTarget || !planReplyIntent) {
			return null;
		}
		return (
			taskPendingQuestions(currentPlanReplyTarget).find(
				(question) => question.id === planReplyIntent?.questionId
			) ?? null
		);
	})();
	$: planReplyIsStale = Boolean(
		planReplyIntent && (!currentPlanReplyTarget || !currentPlanReplyQuestion)
	);

	function handleDevWorkbenchStarted(event: CustomEvent<{ sessionId: string; program: string | null }>): void {
		devWorkbenchSession = {
			id: event.detail.sessionId,
			program: event.detail.program,
			nonce: Date.now(),
			threadId: currentThreadId
		};
		devWorkbenchFocusNonce += 1;
	}

	function focusDevWorkbench(): void {
		devWorkbenchFocusNonce += 1;
	}

	function handleDevWorkbenchSessionsChange(event: CustomEvent<{ count: number; activeSessionId: string | null }>): void {
		devWorkbenchActiveSessionId = event.detail.activeSessionId;
	}

	// Profile chooser
	$: chatProfiles = $chatProfileStore.profiles;
	$: chatProfileWarnings = $chatProfileStore.warnings;
	$: selectedProfile = $chatProfileStore.selected;
	$: selectedProfileObj = chatProfiles.find(p => p.name === selectedProfile) ?? chatProfiles[0];
	$: selectedProfileSupportsAttachments = selectedProfileObj?.supports_user_image_inputs ?? true;
	// `@copilot` needs an image on screen to ground on; in the HUD the screen-capture
	// seeds one as a STAGED attachment. Offer copilot only when such an image is
	// staged on the current compose (HUD-only — see buildComposerMentionItems).
	$: composerHasStagedImage = stagedAttachments.some((attachment) =>
		attachment.server_registered_capture === true &&
		(attachment.mime_type ?? '').toLowerCase().startsWith('image/')
	);
	$: composerMentionItems = buildComposerMentionItems(
		composerSkills,
		composerReferenceAgents,
		$taskStore.tasks.filter((task) => task.status === 'completed'),
		hud,
		composerHasStagedImage
	);

	$: if (browser && mounted) {
		const referenceKey = `${currentScopeKey}:${activeSessionId ?? ''}`;
		if (referenceKey !== lastComposerReferenceKey) {
			lastComposerReferenceKey = referenceKey;
			void loadComposerReferenceCatalogs(beginComposerReferenceRequest());
		}
	}

	function captureChatPageScopeKey(): string {
		return currentScopeKey;
	}

	function isStaleChatPageScope(scopeKey: string): boolean {
		return scopeKey !== currentScopeKey;
	}

	function resetChatPageLocalState(): void {
		hitlContinuationGeneration += 1;
		resolveAttentionPrompt(null);
		const returnMode = planReplyIntent?.returnMode ?? null;
		inputText = '';
		stagedAttachments = [];
		resurfacingContextStageKey = '';
		uploadBatchVersion += 1;
		isUploading = false;
		voiceComposerTurnOrigin = false;
		voiceGuidedCaptureInFlight = false;
		voiceGuidedCaptureGeneration += 1;
		voiceGuidedAdmissionGeneration += 1;
		voiceGuidedAdmissionInFlight = false;
		profileDropdownOpen = false;
		inspectPanelOpen = false;
		inspectPanelTarget = null;
		taskPanelOpen = false;
		taskPanelTask = null;
		inlinePlanActionKey = null;
		isPlanSheetOpen = false;
		planSheetTaskId = null;
		composerReferenceGeneration += 1;
		composerReferenceAgents = [];
		composerSkills = [];
		planReplyIntent = null;
		if (returnMode) {
			planModeStore.select(returnMode);
		}
		respondingEscalationKeys = new Set();
		runExpansionOverrides = new Map();
	}

	function beginComposerReferenceRequest(): ComposerReferenceRequest {
		composerReferenceGeneration += 1;
		// Fail closed synchronously. References from the previous session must
		// disappear before the replacement catalog request has even started.
		composerReferenceAgents = [];
		composerSkills = [];
		return {
			generation: composerReferenceGeneration,
			sessionId: activeSessionId,
			scopeKey: currentScopeKey
		};
	}

	function composerReferenceRequestIsCurrent(request: ComposerReferenceRequest): boolean {
		return isComposerReferenceRequestCurrent(
			request,
			composerReferenceGeneration,
			activeSessionId,
			currentScopeKey
		);
	}

	async function loadComposerReferenceCatalogs(request: ComposerReferenceRequest): Promise<void> {
		await loadComposerSkills(request);
	}

	async function loadComposerSkills(request: ComposerReferenceRequest): Promise<void> {
		const catalogSessionId = request.sessionId;
		if (catalogSessionId) {
			try {
				const res = await timedFetch(
					`/api/magician/v2/chat/sessions/${encodeURIComponent(catalogSessionId)}/reference-catalog`,
					{ headers: scopedChatHeaders() }
				);
				if (!res.ok) {
					throw new Error(`server returned ${res.status}`);
				}
				const data = (await res.json().catch(() => null)) as {
					agents?: ComposerReferenceAgentEntry[];
					skills?: ComposerSkillEntry[];
				} | null;
				if (!composerReferenceRequestIsCurrent(request)) return;
				composerReferenceAgents = (data?.agents ?? [])
					.filter((agent) => typeof agent?.agent_id === 'string' && agent.agent_id.trim().length > 0)
					.slice()
					.sort((a, b) => {
						const routeOrder = (a.route === 'self' ? 0 : 1) - (b.route === 'self' ? 0 : 1);
						return routeOrder || a.agent_id.localeCompare(b.agent_id);
					});
				composerSkills = (data?.skills ?? [])
					.filter((skill) => typeof skill?.name === 'string' && skill.name.trim().length > 0)
					.slice()
					.sort((a, b) => {
						const routeOrder = (a.route === 'direct' ? 0 : 1) - (b.route === 'direct' ? 0 : 1);
						const ownerOrder = (a.owner_agent_id ?? '').localeCompare(b.owner_agent_id ?? '');
						return routeOrder || ownerOrder || a.name.localeCompare(b.name);
					});
				return;
			} catch (error) {
				console.warn('[chat] Failed to load session composer references; keeping protected references hidden:', error);
			}
		}
		if (!composerReferenceRequestIsCurrent(request)) return;
		// No active session, or a scoped-catalog failure: fail closed. Global
		// agent/skill directories are management surfaces, not proof that the
		// current agent may reference or delegate to those entries.
		composerSkills = [];
		composerReferenceAgents = [];
	}

	function buildComposerMentionItems(
		skills: ComposerSkillEntry[],
		referenceAgents: ComposerReferenceAgentEntry[],
		referenceableTasks: Task[],
		isHud: boolean,
		copilotImageReady: boolean
	): ComposerMentionItem[] {
		const items: ComposerMentionItem[] = [];
		// Feature commands (tutor / tutor_quick / copilot) lead the list so a bare `@`
		// surfaces them first. tutor + tutor_quick are always offered (HUD + non-HUD);
		// copilot is HUD-only and only when an image is staged for it to ground on.
		items.push(
			...buildFeatureMentionItems({ includeCopilot: isHud && copilotImageReady })
		);
		const seenAgentIds = new Set<string>();
		for (const agent of referenceAgents) {
			const agentId = agent.agent_id?.trim();
			if (!agentId || seenAgentIds.has(agentId)) continue;
			seenAgentIds.add(agentId);
			const name = agent.name?.trim();
			const routeLabel = agent.route === 'self' ? 'Current agent' : 'Delegate target';
			items.push({
				id: `agent:${agentId}`,
				kind: 'agent',
				label: name ? `${name} · ${agentId}` : agentId,
				detail: truncateReferenceDetail(
					[routeLabel, agent.description].filter(Boolean).join(' · ')
				),
				insertText: `agent:${agentId}`,
				searchText: [agentId, name, agent.description, routeLabel].filter(Boolean).join(' ')
			});
		}
		for (const skill of skills) {
			const name = skill.name.trim();
			const isPersonality = skill.kind === 'personality-mode';
			const ownerAgentId = skill.owner_agent_id?.trim();
			const ownerAgentName = skill.owner_agent_name?.trim();
			const isDelegatedSkill = Boolean(!isPersonality && ownerAgentId && skill.route === 'delegate');
			const routeLabel = isDelegatedSkill
				? `Via ${ownerAgentName || ownerAgentId}`
				: undefined;
			const fallbackDetail = isPersonality
				? 'Personality mode'
				: skill.kind === 'compiled'
					? 'Compiled tool'
					: 'Procedure skill';
			items.push({
				id: isDelegatedSkill
					? `tool:${name}:via:${ownerAgentId}`
					: `${isPersonality ? 'personality' : 'tool'}:${name}`,
				kind: isPersonality ? 'personality' : 'tool',
				label: name,
				detail: truncateReferenceDetail(
					[routeLabel, skill.description || fallbackDetail].filter(Boolean).join(' · ')
				),
				insertText: isPersonality
					? `personality:${name}`
					: isDelegatedSkill
						? `skill:${name} via agent:${ownerAgentId}`
						: `skill:${name}`,
				searchText: [
					name,
					skill.description,
					skill.kind,
					skill.layer,
					ownerAgentId,
					ownerAgentName,
					routeLabel
				]
					.filter(Boolean)
					.join(' ')
			});
		}
		// Task references: pick a completed task to mention. The chip DISPLAYS the
		// title (chipLabel) while `insertText` keeps `task:<id>` so the model reads
		// the precise id. Source is the persistent `$taskStore.tasks` (completed) —
		// internal/VibeDev runs live in a separate feed and are intentionally NOT
		// offered here. Shared builder (composerMentions) keeps the task-chip shape
		// identical across chat, the tasks page, and the vibe cockpit.
		items.push(...buildTaskMentionItems(referenceableTasks));
		return items;
	}

	function truncateReferenceDetail(value?: string): string | undefined {
		const normalized = value?.replace(/\s+/g, ' ').trim();
		if (!normalized) return undefined;
		return normalized.length > 96 ? `${normalized.slice(0, 93)}...` : normalized;
	}

	// `isAtBottom` is a stickiness flag — true means "the user is
	// following the conversation in real time; auto-scroll to keep the
	// latest reply in view." False means "the user scrolled up to look
	// at something earlier; don't yank them back, just surface a
	// jump-to-latest pill so they can opt in." Starts true so the
	// initial page load and the first turn-on after empty both keep
	// behaving like before. `handleChatScroll` flips this every time
	// the user moves the viewport.
	let isAtBottom = true;
	// 120px slack — covers the composer height + a sentence or so of
	// breathing room. Tighter values feel jittery on touch devices
	// where momentum scroll overshoots the bottom by a few pixels.
	const STICKY_BOTTOM_THRESHOLD_PX = 120;

	function computeIsAtBottom(el: HTMLElement | null | undefined): boolean {
		if (!el) return true;
		return el.scrollHeight - el.scrollTop - el.clientHeight <= STICKY_BOTTOM_THRESHOLD_PX;
	}

	// Scroll to bottom on two distinct triggers:
	//   1. A new message lands at the tail — different `tailId`. This
	//      covers user turns, assistant turns, and the optimistic
	//      streaming placeholder when it first appears.
	//   2. The tail message's TEXT grows while keeping the same id —
	//      what happens during streaming, where each token mutates the
	//      placeholder's content in-place. Without this branch the
	//      bubble grows below the viewport and the user has to scroll
	//      by hand to watch tokens land.
	// Both triggers respect `isAtBottom`: if the user has scrolled up
	// to read something, we leave the viewport alone and let the
	// jump-to-latest pill (rendered below the messages list) handle
	// the catch-up when they want it. Prepending older messages via
	// pagination grows `visibleMessages.length` but the tail id stays
	// the same and so does the tail text — scroll-up-to-load-history
	// is untouched.
	let chatInitialScrollDone = false;
	let lastTailMessageId: string | null = null;
	let lastTailMessageTextLen = 0;
	let lastAutoSpokenMessageId: string | null = null;
	$: if (visibleMessages.length && browser) {
		const tail = visibleMessages[visibleMessages.length - 1];
		const tailId = tail?.id ?? null;
		const tailTextLen =
			tail?.message?.content && typeof tail.message.content === 'object'
				? (getMessageText(tail.message.content as ChatMessageContent) ?? '').length
				: 0;
		if (!chatInitialScrollDone) {
			tick().then(() => {
				if (messagesContainerEl) {
					messagesContainerEl.scrollTop = messagesContainerEl.scrollHeight;
				}
				chatInitialScrollDone = true;
				lastTailMessageId = tailId;
				lastTailMessageTextLen = tailTextLen;
				isAtBottom = true;
				// Establish baseline so initial backfill never auto-speaks.
				lastAutoSpokenMessageId = tailId;
			});
		} else if (tailId !== lastTailMessageId) {
			lastTailMessageId = tailId;
			lastTailMessageTextLen = tailTextLen;
			if (isAtBottom) {
				tick().then(scrollToBottom);
			}
			void maybeAutoSpeakLatest(tailId);
		} else if (tailTextLen !== lastTailMessageTextLen) {
			// Streaming append on the same message. Use the `instant`
			// scroll variant — smooth scrolling combined with rapid
			// token bursts (every 30-50ms) chains animations and the
			// viewport never catches up. Native `scrollTop` write
			// snaps the viewport every burst so the latest characters
			// stay visible. Only fires when the user is already
			// pinned to the bottom; otherwise the tokens land off-
			// screen and the pill surfaces the catch-up affordance.
			lastTailMessageTextLen = tailTextLen;
			if (isAtBottom) {
				tick().then(() => {
					if (messagesContainerEl) {
						messagesContainerEl.scrollTop = messagesContainerEl.scrollHeight;
					}
				});
			}
		}
	}

	function maybeAutoSpeakLatest(tailId: string | null): void {
		const decision = maybeAutoSpeakTail({
			tailId,
			lastAutoSpokenMessageId,
			resolveMessage: (id) => {
				const m = visibleMessages.find((v) => v.id === id)?.message;
				// Opening Review work must not create a second automatic speaker
				// for an answer already owned by the durable voice delivery queue.
				if (m && (m.chat_turn_id?.startsWith('voice-request-') || $concurrentVoiceStore.requests.some(r => r.branch_session_id === m.session_id))) return null;
				return m
					? {
							id: m.id,
							direction: m.direction,
							content: m.content,
							source_surface: m.source_surface,
							voice_origin: m.voice_origin,
							speech_segments: m.speech_segments
						}
					: null;
			},
			getMessageText: (content) =>
				getMessageText(content as ChatMessageContent) ?? null
		});
		lastAutoSpokenMessageId = decision.nextLastAutoSpokenMessageId;
	}

	function scrollToBottom() {
		if (messagesContainerEl) {
			messagesContainerEl.scrollTo({
				top: messagesContainerEl.scrollHeight,
				behavior: 'smooth'
			});
		} else if (messagesEndEl) {
			messagesEndEl.scrollIntoView({ behavior: 'smooth' });
		}
	}

	function normalizeThreadId(value: string | null | undefined): string {
		const normalized = (value || '').trim().toLowerCase();
		return normalized.length > 0 ? normalized : 'general';
	}

	type ThreadTaskBucket = 'running' | 'needs_action' | 'ready' | 'pending' | 'done';

	function taskBucket(task: Task): ThreadTaskBucket {
		if (task.status === 'running' || task.status === 'planning') return 'running';
		if (task.status === 'paused' || task.status === 'failed') return 'needs_action';
		if (task.status === 'ready') return 'ready';
		if (task.status === 'completed') return 'done';
		return 'pending';
	}

	function bucketRank(bucket: ThreadTaskBucket): number {
		switch (bucket) {
			case 'running':
				return 0;
			case 'needs_action':
				return 1;
			case 'ready':
				return 2;
			case 'pending':
				return 3;
			case 'done':
				return 4;
		}
	}

	function sortThreadTasks(tasks: Task[]): Task[] {
		return [...tasks].sort((left, right) => {
			const bucketDelta = bucketRank(taskBucket(left)) - bucketRank(taskBucket(right));
			if (bucketDelta !== 0) return bucketDelta;
			const leftUpdated = Date.parse(left.updatedAt || left.createdAt || '') || 0;
			const rightUpdated = Date.parse(right.updatedAt || right.createdAt || '') || 0;
			return rightUpdated - leftUpdated;
		});
	}

	function inlinePlanActionBusy(taskId: string, action: string): boolean {
		return inlinePlanActionKey === `${taskId}:${action}`;
	}

	function buildCurrentThreadTaskHref(taskId: string): string {
		const params = new URLSearchParams({ selected: taskId });
		return `/t/${encodeURIComponent(currentThreadId)}?${params.toString()}`;
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			handleSend();
		}
	}

	function formatBytes(size?: number): string {
		if (!size || size <= 0) return '';
		const units = ['B', 'KB', 'MB', 'GB'];
		let value = size;
		let unitIndex = 0;
		while (value >= 1024 && unitIndex < units.length - 1) {
			value /= 1024;
			unitIndex += 1;
		}
		return `${value >= 10 || unitIndex === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unitIndex]}`;
	}

	function openAttachmentPicker(): void {
		if (isReadOnly || isSendingMessage || isUploading) return;
		attachmentInputEl?.click();
	}

	function removeStagedAttachment(attachmentId: string): void {
		stagedAttachments = stagedAttachments.filter((attachment) => attachment.attachment_id !== attachmentId);
	}

	async function handleAttachmentSelection(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const files = Array.from(input.files ?? []);
		input.value = '';
		await uploadFilesAsAttachments(files);
	}

	/**
	 * Stage one or more files as chat attachments. Shared upload path for
	 * the regular file picker and the Phase 2 camera / mic capture
	 * buttons — both produce `File` blobs and route through the same
	 * `chatStore.uploadAttachment` flow so staging, scope guards, and
	 * error toasts stay identical.
	 */
	async function uploadFilesAsAttachments(files: File[]): Promise<void> {
		if (!activeSessionId || isReadOnly || isSendingMessage || isUploading || files.length === 0) return;

		const requestScopeKey = captureChatPageScopeKey();
		const uploadSessionId = activeSessionId;
		const batchVersion = ++uploadBatchVersion;
		stagedAttachmentSessionId = uploadSessionId;
		isUploading = true;
		try {
			for (const file of files) {
				try {
					const uploaded = await chatStore.uploadAttachment(uploadSessionId, file);
					if (
						isStaleChatPageScope(requestScopeKey)
						|| batchVersion !== uploadBatchVersion
						|| activeSessionId !== uploadSessionId
					) {
						break;
					}
					stagedAttachments = [...stagedAttachments, { ...uploaded, label: file.name }];
				} catch (error) {
					console.error('[chat] Attachment upload failed:', error);
					if (
						isStaleChatPageScope(requestScopeKey)
						|| batchVersion !== uploadBatchVersion
						|| activeSessionId !== uploadSessionId
					) {
						break;
					}
					showError(
						'Attachment upload failed',
						error instanceof Error ? `${file.name}: ${error.message}` : `Could not upload ${file.name}.`
					);
				}
			}
		} finally {
			if (!isStaleChatPageScope(requestScopeKey) && batchVersion === uploadBatchVersion) {
				isUploading = false;
			}
		}
	}

	/**
	 * Consume the candidate-only Ask Presto route. The selected safe brief is
	 * resolved through the scoped API and staged through the existing durable
	 * attachment path; raw original content is never copied into chat context.
	 */
	async function stageResurfacingContextFromRoute(): Promise<void> {
		const candidateId = ($page.url.searchParams.get('resurfacing_candidate') || '').trim();
		if (!candidateId || !activeSessionId || isReadOnly || isSendingMessage || isUploading) return;
		const uploadSessionId = activeSessionId;
		const stageKey = `${uploadSessionId}:${candidateId}`;
		if (resurfacingContextStageKey === stageKey) return;

		resurfacingContextStageKey = stageKey;
		const requestScopeKey = captureChatPageScopeKey();
		const batchVersion = ++uploadBatchVersion;
		stagedAttachmentSessionId = uploadSessionId;
		isUploading = true;
		try {
			const detail = await fetchResurfacingDetail(candidateId);
			if (
				isStaleChatPageScope(requestScopeKey)
				|| batchVersion !== uploadBatchVersion
				|| activeSessionId !== uploadSessionId
			) return;
			const filename = resurfacingChatContextFilename(detail);
			const file = new File([buildResurfacingChatContext(detail)], filename, {
				type: 'text/plain;charset=utf-8'
			});
			const uploaded = await chatStore.uploadAttachment(uploadSessionId, file);
			if (
				isStaleChatPageScope(requestScopeKey)
				|| batchVersion !== uploadBatchVersion
				|| activeSessionId !== uploadSessionId
			) return;
			stagedAttachments = [...stagedAttachments, { ...uploaded, label: filename }];
			if (!inputText.trim()) inputText = 'Help me with this Worth a look item.';

			const url = new URL(window.location.href);
			url.searchParams.delete('resurfacing_candidate');
			window.history.replaceState({}, '', url.toString());
			await tick();
			composerEl?.focus();
		} catch (error) {
			resurfacingContextStageKey = '';
			if (!isStaleChatPageScope(requestScopeKey) && batchVersion === uploadBatchVersion) {
				showError(
					'Could not attach Worth a look context',
					error instanceof Error ? error.message : 'The selected item is unavailable.'
				);
			}
		} finally {
			if (!isStaleChatPageScope(requestScopeKey) && batchVersion === uploadBatchVersion) {
				isUploading = false;
			}
		}
	}

	function handleCameraCapture(event: CustomEvent<{ file: File }>): void {
		void uploadFilesAsAttachments([event.detail.file]);
	}

	function handleMicCapture(event: CustomEvent<{ file: File; durationMs: number }>): void {
		void uploadFilesAsAttachments([event.detail.file]);
	}

	// ── Voice auto-send countdown ──
	// After a successful mic → STT round-trip we drop the transcript
	// into the composer and kick off a short countdown. If the user
	// stays still the message auto-sends — that's what makes the flow
	// feel conversational instead of "transcribe, then click send".
	// Any composer interaction (typing, focus, click) cancels so the
	// user can keep editing.
	// Cushion between transcript landing in the composer and the
	// auto-send firing. Dropped from 2 s to 500 ms now that the
	// composer is also showing live transcript deltas — by the time
	// the final lands, the user has already SEEN the words appear and
	// would have hit cancel if they meant to. 500 ms is just enough to
	// react with "wait, I want to edit" without feeling sluggish.
	const VOICE_AUTO_SEND_MS = 4000;
	let voiceAutoSendRemainingMs: number | null = null;
	let voiceAutoSendInterval: ReturnType<typeof setInterval> | null = null;
	let voiceAutoSendStartedAt = 0;

	// Voice-in → voice-out used to live here as a frontend-tracked
	// Set of voice-originated chat_turn_ids + a reactive capture
	// block that stamped the new user message's turn id after a
	// voice send. The backend now stamps `voice_origin: true` onto
	// both the user message and the assistant reply during
	// persist_user_turn / process_chat_inline_turn, so the
	// frontend just reads `message.voice_origin` (see
	// `maybeAutoSpeakLatest`).

	function voiceSourceLabel(message: ChatMessage): string {
		const surface = (message.source_surface ?? '').trim();
		if (surface === 'global_voice_note') return 'Sent via desktop voice note';
		if (surface === 'global_live_ptt') return 'Sent via desktop live PTT';
		if (surface === 'realtime_voice') return 'Sent via realtime voice';
		if (surface === 'mascot' || surface === 'mascot_macos') return 'Sent via mascot voice';
		if (surface.length > 0) return `Sent via voice (${surface.replaceAll('_', ' ')})`;
		return 'Voice-originated turn';
	}

	function cancelVoiceAutoSend(settleInput = true): void {
        const hadPendingDictation = voiceAutoSendInterval !== null || voiceAutoSendRemainingMs !== null;
		if (voiceAutoSendInterval !== null) {
			clearInterval(voiceAutoSendInterval);
			voiceAutoSendInterval = null;
		}
		voiceAutoSendRemainingMs = null;
		if (settleInput && hadPendingDictation) settleConcurrentVoiceInput();
	}

	// Component teardown: cancel any in-flight voice auto-send timer
	// so its `setInterval` callback doesn't fire after the page
	// unmounts and try to mutate state on a destroyed component (or
	// worse, trigger handleSend() against a stale session id).
	onDestroy(() => {
		cancelVoiceAutoSend();
		setConcurrentVoiceForegroundBusy(false);
		if (panelClockHandle !== null) clearInterval(panelClockHandle);
		panelClockHandle = null;
		taskPanelPoll.stop();
		// The run drawer's live subscription outlives the drawer's own markup —
		// nothing else here unsubscribes it, so leaving without this keeps a
		// closure writing into a destroyed component's variables.
		inspectStreamStop?.();
		inspectStreamStop = null;
		inspectStreamKey = null;
	});

	function startVoiceAutoSend(): void {
		cancelVoiceAutoSend(false);
		voiceAutoSendStartedAt = Date.now();
		voiceAutoSendRemainingMs = VOICE_AUTO_SEND_MS;
		voiceAutoSendInterval = setInterval(() => {
			const elapsed = Date.now() - voiceAutoSendStartedAt;
			const remaining = VOICE_AUTO_SEND_MS - elapsed;
			if (remaining <= 0) {
				cancelVoiceAutoSend(false);
				// `voiceOrigin: true` propagates to the chat-send
				// request body → backend stamps `voice_origin: true`
				// on both the user message and the assistant reply
				// during persist_user_turn / process_chat_inline_turn.
				// Frontend just reads the flag off the message envelope
				// (see `maybeAutoSpeakLatest`).
				void handleSend({ voiceOrigin: true });
			} else {
				voiceAutoSendRemainingMs = remaining;
			}
		}, 80);
	}

	function handleVoiceAutoSendInputCancel(): void {
		if (voiceAutoSendRemainingMs !== null) cancelVoiceAutoSend();
		if (!inputText.trim()) voiceComposerTurnOrigin = false;
	}

	// Live snapshot of what the composer looked like BEFORE the
	// streaming transcript started landing. Used to compose the
	// final composer value as `prefix + transcript` so partial
	// deltas don't clobber text the user was already typing.
	let voiceComposerPrefix: string | null = null;

	function handleMicTranscribeDelta(
		event: CustomEvent<{ transcript: string }>
	): void {
		const delta = event.detail.transcript;
		if (voiceComposerPrefix === null) {
			// First delta of this utterance — snapshot whatever was
			// already in the textarea so a user who started typing
			// before speaking doesn't lose their draft.
			voiceComposerPrefix = inputText;
		}
		inputText = voiceComposerPrefix.length > 0
			? `${voiceComposerPrefix.trim()} ${delta}`
			: delta;
	}

	function handleMicTranscribe(
		event: CustomEvent<{ transcript: string; durationMs: number }>
	): void {
		const transcript = event.detail.transcript.trim();
		// Reset the prefix tracker so the next voice note starts
		// fresh against whatever's in the composer at THAT point.
		const prefix = voiceComposerPrefix ?? inputText;
		voiceComposerPrefix = null;
		if (!transcript) return;
		voiceComposerTurnOrigin = true;
		// Final value: `prefix + transcript`. We don't trust the
		// delta-built inputText here because a delta could have been
		// dropped on the floor while the auto-send was already firing
		// for an earlier interrupted note.
		inputText = prefix.length > 0 ? `${prefix.trim()} ${transcript}` : transcript;
		// Kick off the auto-send countdown for hands-free conversational
		// flow once the composer has re-rendered with the transcript.
		tick().then(() => startVoiceAutoSend());
	}

	// Mid-flight cancel handler. Wired to FloatingComposer's `on:stop`
	// event, which fires when the user clicks the stop affordance. Hits
	// `DELETE /chat/sessions/{id}/run`; the backend per-session
	// `CancellationToken` then races the in-flight LLM call inside
	// `process_chat_inline_turn` and aborts the provider HTTP stream
	// (HTTP/2 `RST_STREAM CANCEL`), so generation stops on the provider
	// side and the user stops being billed for unproduced tokens.
	async function cancelInflightChatTurn(): Promise<void> {
		if (!activeSessionId || !composerBusy) return;
		await chatStore.cancelChatRun(activeSessionId);
	}

	function isTutorInvokeText(text: string): boolean {
		return /^\s*(?:@tut(?:or|ur)(?=$|[\s:,])|hey[\s,]+tut(?:or|ur)(?=$|[\s:,]))/i.test(text);
	}

	function isBrainstormInvokeText(text: string): boolean {
		return /^\s*(?:@brainstorm(?=$|[\s:,])|hey[\s,]+brainstorm(?=$|[\s:,]))/i.test(text);
	}

	/** The seed thought after the `@brainstorm` invoke prefix (may be empty). */
	function stripBrainstormInvoke(text: string): string {
		return text.replace(/^\s*(?:@brainstorm|hey[\s,]+brainstorm)[\s:,]*/i, '').trim();
	}

	/**
	 * `@brainstorm <messy thought>` — start a Live Thinking Map INSTEAD of a chat
	 * turn (the @tutor pattern: a send-time text detection triggers the feature's
	 * own surface, but where tutor overlays on top of the chat reply, brainstorm's
	 * home IS the map page, so nothing is sent to the chat LLM). Creates the map,
	 * folds the seed in via `/interpret` (best-effort — the map still opens and
	 * ✦ Continue works there), and navigates to the brainstorm surface.
	 */
	/**
	 * Open a path in the MAIN app window via the Tauri `open_app_at` command,
	 * which promotes the activation policy and builds/focuses that window
	 * (`desktop/src-tauri/src/tray.rs`). Outside Tauri there is no second
	 * window, so fall back to an in-page navigation.
	 */
	async function openInMainWindow(path: string): Promise<void> {
		if (!browser || typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) {
			await goto(path);
			return;
		}
		const { invoke } = await import('@tauri-apps/api/core');
		await invoke('open_app_at', { path });
	}

	async function startBrainstormFromComposer(rawText: string): Promise<void> {
		const seed = stripBrainstormInvoke(rawText);
		try {
			const title = seed
				? seed.length > 60
					? `${seed.slice(0, 57)}…`
					: seed
				: 'Brainstorm';
			const map = await createThinkingMap({ title });
			if (seed) {
				try {
					await interpretThinkingMap(map.map_id, { text: seed, intent: 'continue_thinking' });
				} catch {
					/* llm_unavailable/offline — the map opens empty, Continue works there */
				}
			}
			const path = `/thinking-maps/${map.map_id}`;
			if (hud) {
				// The HUD webview persists across hide/show, so a `goto` here
				// would BOTH render the map inside a borderless, always-on-top,
				// transparent overlay AND leave the webview parked off `/hud` —
				// the next summon would show a thinking map instead of the
				// composer. Open in the main window and let the host dismiss.
				await openInMainWindow(path);
				dispatch('fire-and-forget-send', {
					sessionId: activeSessionId ?? '',
					text: rawText,
					reason: 'brainstorm'
				});
				return;
			}
			await goto(path);
		} catch (error) {
			showError(
				`Couldn't start the brainstorm: ${error instanceof Error ? error.message : String(error)}`
			);
			// Give the text back so nothing typed is lost.
			inputText = rawText;
		}
	}

	function isAppCopilotInvokeText(text: string): boolean {
		return /^\s*(?:@app[-_]?copilot(?=$|[\s:,])|@copilot(?=$|[\s:,])|hey[\s,]+(?:app[\s,]+)?copilot(?=$|[\s:,]))/i.test(text);
	}

	function isTutorOrAppCopilotInvokeText(text: string): boolean {
		return isTutorInvokeText(text) || isAppCopilotInvokeText(text);
	}

	type TutorCanvasMode = 'screen_overlay' | 'blackboard';
	type TutorOverlayRail = 'tutor' | 'copilot';

	function tutorCanvasModeForText(text: string, hasVisualSource: boolean): TutorCanvasMode {
		if (isAppCopilotInvokeText(text)) return 'screen_overlay';
		return hasVisualSource ? 'screen_overlay' : 'blackboard';
	}

	function tutorOverlayRailForText(text: string): TutorOverlayRail {
		return isAppCopilotInvokeText(text) ? 'copilot' : 'tutor';
	}

	function fireAndForgetReasonForText(text: string): FireAndForgetReason | null {
		if (fireAndForget === 'always') return 'always';
		if (fireAndForget === 'tutor' && isTutorOrAppCopilotInvokeText(text)) return 'tutor';
		return null;
	}

	async function notifyTutorOverlayStatus(
		status: 'working' | 'idle',
		sessionId: string | null,
		canvasMode: TutorCanvasMode = 'screen_overlay',
		rail: TutorOverlayRail = 'tutor'
	): Promise<void> {
		if (!browser || typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
				await invoke('show_tutor_overlay_status', {
					status,
					sessionId,
					canvasMode,
					rail
				});
		} catch (error) {
			console.warn('[chat] show_tutor_overlay_status failed:', error);
		}
	}

	function rejectLockedGuidedFlow(
		feature: VoiceGuidedFlowFeature,
		voiceOrigin: boolean
	): void {
		const message = lockedVoiceGuidedFlowMessage(feature);
		if (voiceOrigin) {
			voiceComposerTurnOrigin = false;
			inputText = '';
		}
		showError('Unlock your screen to continue', message);
		if (!voiceOrigin) return;
		const speech = get(ttsStore);
		ttsStore.setActive('guided-flow-screen-locked');
		speakBrowserTts(
			{
				messageId: 'guided-flow-screen-locked',
				text: message,
				voiceName: speech.prefs.voiceName,
				rate: speech.prefs.rate,
				pitch: speech.prefs.pitch
			},
			() => ttsStore.setActive(null)
		);
	}

	async function handleSend(opts: { voiceOrigin?: boolean; background?: boolean; stopAndSend?: boolean } = {}) {
        const voiceOrigin = opts.voiceOrigin === true || voiceAutoSendRemainingMs !== null || voiceComposerTurnOrigin;
        const concurrent = voiceOrigin || opts.background === true;
		if (currentThreadDisplayMode === 'dev') {
			await sendDevWorkbenchInput();
			return;
		}
		if (planComposerMode === 'plan') {
			await sendPlanModeMessage();
			return;
		}
		if (
			(!inputText.trim() && stagedAttachments.length === 0) ||
			textAdmissionInFlight ||
			concurrentAdmissionInFlight ||
			isUploading ||
			voiceGuidedAdmissionInFlight ||
			isReadOnly ||
			!activeSessionId
		) return;
		// A user may press Send while the voice countdown is still visible.
		// Preserve that turn's voice provenance and cancel the pending timer so
		// the same dictated command cannot be submitted a second time later.
		if (voiceAutoSendRemainingMs !== null) cancelVoiceAutoSend(false);
		// `@brainstorm` routes the composer text into a fresh Live Thinking Map
		// and opens it — the message never becomes a chat turn. Checked before
		// anything is staged/cleared so a failure can hand the text back intact.
		if (isBrainstormInvokeText(inputText)) {
			const brainstormText = inputText;
			voiceComposerTurnOrigin = false;
			inputText = '';
			void startBrainstormFromComposer(brainstormText);
			return;
		}
		const requestScopeKey = captureChatPageScopeKey();
		const sendSessionId = activeSessionId;
		const guidedVoiceFlow = voiceOrigin
			? parseVoiceGuidedFlowInvocation(inputText)
			: null;
		const lockedGuidedFlowFeature = guidedVoiceFlow?.feature
			?? (isAppCopilotInvokeText(inputText)
				? 'app_copilot'
				: isTutorInvokeText(inputText)
					? 'tutor'
					: null);
		if (lockedGuidedFlowFeature) {
			// Called inside the send/dictation user action. Chromium requires that
			// gesture before granting exact Idle Detection; unsupported browsers
			// remain unknown and are never inferred from tab visibility.
			const admissionGeneration = voiceGuidedAdmissionGeneration + 1;
			voiceGuidedAdmissionGeneration = admissionGeneration;
			voiceGuidedAdmissionInFlight = true;
			try {
				await startScreenLockMonitoring(true);
			} finally {
				if (voiceGuidedAdmissionGeneration === admissionGeneration) {
					voiceGuidedAdmissionInFlight = false;
				}
			}
			if (
				voiceGuidedAdmissionGeneration !== admissionGeneration
				|| isStaleChatPageScope(requestScopeKey)
				|| activeSessionId !== sendSessionId
			) return;
		}
		if (lockedGuidedFlowFeature && isScreenLocked()) {
			rejectLockedGuidedFlow(lockedGuidedFlowFeature, voiceOrigin);
			return;
		}
		const text = guidedVoiceFlow?.normalizedText ?? inputText;
		let pendingAttachments = [...stagedAttachments];
		const attachmentsBeforeGuidedCapture = [...pendingAttachments];
		let capturedForThisAttempt: VoiceGuidedFlowAttachment[] = [];
		if (guidedVoiceFlow?.canvas === 'blackboard' && pendingAttachments.length > 0) {
			showError(
				'Blackboard voice flow cannot use staged attachments. Remove them, or say “Tutor screen” to teach from the visible source.'
			);
			return;
		}
		const hasReusableGuidedCapture = pendingAttachments.some((attachment) =>
			attachment.server_registered_capture === true
			&& (attachment.mime_type ?? '').toLowerCase().startsWith('image/')
		);
		if (guidedVoiceFlow?.requiresScreenCapture && !hasReusableGuidedCapture) {
			if (!selectedProfileSupportsAttachments) {
				showError('The selected chat profile cannot start a screen-guided voice flow.');
				return;
			}
			isUploading = true;
			voiceGuidedCaptureInFlight = true;
			const captureGeneration = voiceGuidedCaptureGeneration + 1;
			voiceGuidedCaptureGeneration = captureGeneration;
			const captureDraftText = inputText;
			const captureScope = get(scopeIdentityStore);
			try {
				const captured = await captureVoiceGuidedFlowScreen(sendSessionId, captureScope);
				capturedForThisAttempt = captured;
				const sessionStillOwnsCapture = !isStaleChatPageScope(requestScopeKey)
					&& activeSessionId === sendSessionId;
				if (!sessionStillOwnsCapture) {
					try {
						await discardVoiceGuidedFlowCapture(sendSessionId, captured, captureScope);
					} catch (cleanupError) {
						console.warn('[chat] failed to discard stale voice screen capture', cleanupError);
					}
					return;
				}
				// Capture is an async boundary. Refresh the detector and reject before
				// staging/sending if the actual screen locked while capture ran.
				await startScreenLockMonitoring(false);
				const captureStillOwned = !isStaleChatPageScope(requestScopeKey)
					&& activeSessionId === sendSessionId;
				if (!captureStillOwned) {
					try {
						await discardVoiceGuidedFlowCapture(sendSessionId, captured, captureScope);
					} catch (cleanupError) {
						console.warn('[chat] failed to discard capture after scope change', cleanupError);
					}
					return;
				}
				if (lockedGuidedFlowFeature && isScreenLocked()) {
					rejectLockedGuidedFlow(lockedGuidedFlowFeature, voiceOrigin);
					try {
						await discardVoiceGuidedFlowCapture(sendSessionId, captured, captureScope);
					} catch (cleanupError) {
						console.warn('[chat] failed to discard lock-rejected capture', cleanupError);
					}
					return;
				}
				if (!voiceGuidedFlowCaptureStillCurrent(
						sendSessionId,
						captureDraftText,
						activeSessionId,
						inputText
					)) {
					pendingAttachments = [...pendingAttachments, ...captured];
					stagedAttachments = pendingAttachments;
					stagedAttachmentSessionId = sendSessionId;
					showInfo(
						'Screen captured, draft preserved',
						'Your words changed during capture, so nothing was sent. Review the draft and try again.'
					);
					return;
				}
				pendingAttachments = [...pendingAttachments, ...captured];
				stagedAttachments = pendingAttachments;
				stagedAttachmentSessionId = sendSessionId;
			} catch (error) {
				if (!isStaleChatPageScope(requestScopeKey)) {
					showError(
						"Couldn't capture the screen",
						error instanceof Error ? error.message : 'The guided flow was not started.'
					);
				}
				return;
			} finally {
				if (
					voiceGuidedCaptureGeneration === captureGeneration
					&& !isStaleChatPageScope(requestScopeKey)
					&& activeSessionId === sendSessionId
				) {
					isUploading = false;
					voiceGuidedCaptureInFlight = false;
				}
			}
		}
		// No await occurs between this final exact-state check and starting the
		// send. A lock event observed after capture therefore cannot leave a
		// deferred request behind for unlock to revive.
		if (lockedGuidedFlowFeature && isScreenLocked()) {
			rejectLockedGuidedFlow(lockedGuidedFlowFeature, voiceOrigin);
			if (capturedForThisAttempt.length > 0) {
				stagedAttachments = attachmentsBeforeGuidedCapture;
				const captureScope = get(scopeIdentityStore);
				try {
					await discardVoiceGuidedFlowCapture(
						sendSessionId,
						capturedForThisAttempt,
						captureScope
					);
				} catch (cleanupError) {
					console.warn('[chat] failed to discard final lock-rejected capture', cleanupError);
				}
			}
			return;
		}
		const attachmentIds = pendingAttachments.map((attachment) => attachment.attachment_id);
		if (attachmentIds.length > 0 && !selectedProfileSupportsAttachments) {
			showError('Attachments are not supported for the selected chat profile.');
			return;
		}
		// `@copilot` (the feature mention, or a typed invoke) needs a screenshot to
		// ground on. The picker only OFFERS copilot while an image is staged, but a
		// committed `@copilot` chip can outlive that image — the user can remove the
		// staged screenshot before sending — so re-validate here and block an
		// ungrounded app-copilot invoke rather than letting it reach the backend with
		// no visual source. HUD-scoped: the HUD is the only surface copilot is offered.
		if (hud && isAppCopilotInvokeText(text)) {
			const hasStagedImage = pendingAttachments.some((attachment) =>
				attachment.server_registered_capture === true &&
				(attachment.mime_type ?? '').toLowerCase().startsWith('image/')
			);
			if (!hasStagedImage) {
				showError('Copilot needs a screenshot to ground on — re-capture the screen, or remove @copilot.');
				return;
			}
		}
		const profile = $chatProfileStore.selected;
		// Feature activation travels as a typed product surface. The visible
		// @-command remains composer syntax, but arbitrary message substrings no
		// longer authorize Tutor/App Copilot tools on the backend.
		const featureSourceSurface = isAppCopilotInvokeText(text)
			? 'app_copilot'
			: isTutorInvokeText(text)
				? 'tutor'
				: undefined;
		const codingChoice = codingChoiceFromSelection($codingProfileStore.selected);
		const sendOptions = {
			harnessEngine: selectedHarnessEngine,
			harnessModel: selectedHarnessModel,
			// `as const` on both arms: without them TypeScript widens the ternary to
			// `string`, and `ChatSendOptions.mode` is the `ChatMessageMode` union. Both
			// values are already in that union — this is a widening fix, not a change
			// to what the composer sends.
			mode: planComposerMode === 'accept_in_scope' ? ('accept_in_scope' as const) : ('ask' as const),
			...(voiceOrigin ? { voiceOrigin: true } : {}),
			...(featureSourceSurface ? { sourceSurface: featureSourceSurface } : {}),
			...(codingChoice ? { codingChoice } : {})
		};
		voiceComposerTurnOrigin = false;
		inputText = '';
		stagedAttachments = [];
			const shouldShowTutorOverlayStatus = isTutorOrAppCopilotInvokeText(text);
			const tutorCanvasMode = tutorCanvasModeForText(text, pendingAttachments.length > 0);
			const tutorOverlayRail = tutorOverlayRailForText(text);
			if (shouldShowTutorOverlayStatus) {
				void notifyTutorOverlayStatus('working', sendSessionId, tutorCanvasMode, tutorOverlayRail);
			}
		const fireAndForgetReason = fireAndForgetReasonForText(text);
		const completeSend = async () => {
			let sendFailed = false;
            let queuedAccepted = false;
			try {
				if (concurrent && attachmentIds.length === 0 && !featureSourceSurface && !planReplyIntent) {
					concurrentAdmissionInFlight = true;
					await submitConcurrentVoiceRequest(sendSessionId, text, {
						profile, harness_engine: selectedHarnessEngine, harness_model: selectedHarnessModel,
						mode: sendOptions.mode, coding_choice: codingChoice
					}, voiceOrigin);
					if (!isStaleChatPageScope(requestScopeKey) && activeSessionId === sendSessionId) {
						await chatStore.loadSessions(currentThreadId);
						if (activeSessionId === sendSessionId) await chatStore.loadMessages(sendSessionId, { preserveLive: true });
					}
                } else if (composerBusy || opts.stopAndSend) {
                    textAdmissionInFlight = true;
                    const queuedId = await chatStore.queueMessage(sendSessionId, text, profile, attachmentIds, sendOptions);
                    queuedAccepted = true;
                    if (opts.stopAndSend) await chatStore.actOnQueuedMessage(sendSessionId, queuedId, 'stop_and_send');
				} else if (attachmentIds.length > 0) {
					await chatStore.sendMessage(sendSessionId, text, profile, attachmentIds, sendOptions);
				} else {
					await chatStore.sendMessageStreaming(sendSessionId, text, profile, [], sendOptions);
				}
			} catch (error) {
				sendFailed = true;
				if (isStaleChatPageScope(requestScopeKey)) {
					return;
				}
				if (activeSessionId === sendSessionId && !queuedAccepted) {
					inputText = text;
					stagedAttachments = pendingAttachments;
					voiceComposerTurnOrigin = voiceOrigin;
				}
				console.error('[chat] Failed to send message:', error);
				showError('Failed to send message', error instanceof Error ? error.message : 'Unknown error');
			} finally {
				concurrentAdmissionInFlight = false;
                textAdmissionInFlight = false;
				if (voiceOrigin) settleConcurrentVoiceInput();
					if (shouldShowTutorOverlayStatus && sendFailed) {
						void notifyTutorOverlayStatus('idle', sendSessionId, tutorCanvasMode, tutorOverlayRail);
					}
			}
		};
		const sendPromise = completeSend();
		if (fireAndForgetReason) {
			dispatch('fire-and-forget-send', {
				sessionId: sendSessionId,
				text,
				reason: fireAndForgetReason
			});
			return;
		}
		// The reply streams into THIS panel, so the host may need to make room
		// for it. Dispatched before the await so the HUD's expansion animates
		// while the turn is in flight rather than after it lands.
		dispatch('inline-send', { sessionId: sendSessionId, text });
		await sendPromise;
	}

	async function sendDevWorkbenchInput(): Promise<void> {
		const text = inputText;
		const targetSessionId = devWorkbenchActiveSessionId;
		if (!text.trim() || !targetSessionId || isReadOnly) return;
		inputText = '';
		try {
			const response = await writeInteractiveSessionStdin(targetSessionId, `${text}\r`);
			if (!response.ok) {
				throw new Error(`terminal stdin failed with HTTP ${response.status}`);
			}
			focusDevWorkbench();
		} catch (error) {
			inputText = text;
			showError('Failed to send to terminal', error instanceof Error ? error.message : 'Unknown error');
		}
	}

	function currentPlanReplyComposerState(): PlanReplyComposerState<UploadedAttachment> {
		return {
			mode: planComposerMode,
			draft: inputText,
			attachments: [...stagedAttachments],
			intent: planReplyIntent
		};
	}

	function applyPlanReplyComposerState(state: PlanReplyComposerState<UploadedAttachment>): void {
		inputText = state.draft;
		stagedAttachments = [...state.attachments];
		planReplyIntent = state.intent;
		planModeStore.select(state.mode);
	}

	function focusActiveComposer(): void {
		void tick().then(() => {
			composerEl?.focus();
		});
	}

	/**
	 * Put the caret in the composer, on request from whoever owns the surface.
	 *
	 * The HUD needs this because Tauri keeps its WebView mounted across
	 * hide/show: `onMount` runs once ever, and hiding actively blurs the focused
	 * element, so on every later summon the command bar would open with nothing
	 * focused and the user would type into nowhere.
	 */
	export function focusComposer(): void {
		focusActiveComposer();
	}

	/**
	 * Stage files dropped OUTSIDE the composer itself (the HUD's transparent
	 * stage is a whole-window drop zone, and the composer's own handler
	 * stops propagation, so this runs only for genuine misses). Same upload
	 * rail as the picker: all guards live inside `uploadFilesAsAttachments`.
	 */
	export function attachFiles(files: File[]): void {
		void uploadFilesAsAttachments(files);
	}

	/**
	 * The HUD's attach-screen flow stages its capture into the session the
	 * panel is currently bound to, so the capture never rebinds the thread
	 * (which would clear anything already staged in this conversation).
	 */
	export function currentChatSessionId(): string | null {
		return activeSessionId || null;
	}

	function focusComposerForPlanTask(
		taskId: string,
		questionId?: string,
		notifyIfUnavailable = true
	): boolean {
		const task = currentThreadTasks.find((entry) => entry.id === taskId) ?? null;
		const pendingQuestions = taskPendingQuestions(task);
		const selectedQuestion = questionId
			? pendingQuestions.find((question) => question.id === questionId) ?? null
			: pendingQuestions[0] ?? null;
		if (!task || task.planStatus !== 'eliciting' || !selectedQuestion) {
			if (notifyIfUnavailable) {
				showError('This planning question is no longer available.');
			}
			return false;
		}

		const transition = beginPlanReply(currentPlanReplyComposerState(), {
			taskId: task.id,
			taskTitle: task.title,
			questionId: selectedQuestion.id,
			questionText: selectedQuestion.question,
			threadId: currentThreadId
		});
		if (!transition.ok) {
			showError('Finish or cancel the current planning answer before selecting another question.');
			return false;
		}

		applyPlanReplyComposerState(transition.state);
		focusActiveComposer();
		return true;
	}

	function cancelPlanReply(modeOverride?: PlanComposerMode): void {
		if (!planReplyIntent) {
			if (modeOverride) planModeStore.select(modeOverride);
			return;
		}
		applyPlanReplyComposerState(
			finishPlanReply(currentPlanReplyComposerState(), modeOverride)
		);
		focusActiveComposer();
	}

	function handleComposerModeSelect(nextMode: PlanComposerMode): void {
		if (planReplyIntent && nextMode !== 'plan') {
			cancelPlanReply(nextMode);
			return;
		}
		planModeStore.select(nextMode);
	}

	async function sendPlanModeMessage(): Promise<void> {
		if ((!inputText.trim() && stagedAttachments.length === 0) || isSendingMessage || isUploading || isReadOnly) {
			return;
		}
		const requestScopeKey = captureChatPageScopeKey();
		const text = inputText.trim();
		if (!text) {
			showError('Plan mode requires a message describing the task or answer.');
			return;
		}

		const replyTarget = currentPlanReplyTarget;
		const replyQuestion = currentPlanReplyQuestion;
		const replyIntentAtSend = planReplyIntent;
		if (replyIntentAtSend && (!replyTarget || !replyQuestion)) {
			showError('The selected planning question is no longer active. Cancel this reply to restore your previous draft.');
			return;
		}

		const sendSessionId = activeSessionId;
		if (!sendSessionId) {
			showError('No active chat session is available for this thread.');
			return;
		}

		const pendingAttachments = [...stagedAttachments];
		const attachmentIds = pendingAttachments.map((attachment) => attachment.attachment_id);
		if (attachmentIds.length > 0 && !selectedProfileSupportsAttachments) {
			showError('Attachments are not supported for the selected chat profile.');
			return;
		}
		inputText = '';
		stagedAttachments = [];

		try {
			await chatStore.sendMessage(sendSessionId, text, selectedProfile, attachmentIds, {
				mode: 'plan',
				planTaskId: replyIntentAtSend?.taskId ?? null,
				planQuestionId: replyIntentAtSend?.questionId ?? null
			});
			if (isStaleChatPageScope(requestScopeKey)) {
				return;
			}
			const sendError = get(chatStore).error;
			if (sendError) {
				throw new Error(sendError);
			}
			showSuccess(replyIntentAtSend ? 'Planning response sent.' : 'Planning started.');
			if (
				replyIntentAtSend
				&& planReplyIntent?.taskId === replyIntentAtSend.taskId
				&& planReplyIntent?.questionId === replyIntentAtSend.questionId
			) {
				applyPlanReplyComposerState(finishPlanReply(currentPlanReplyComposerState()));
			}
			await taskStore.loadTasks();
			tick().then(scrollToBottom);
		} catch (error) {
			if (isStaleChatPageScope(requestScopeKey)) {
				return;
			}
			if (activeSessionId === sendSessionId) {
				inputText = text;
				stagedAttachments = pendingAttachments;
			}
			showError(error instanceof Error ? error.message : 'Failed to send planning request');
		}
	}

	async function openTaskPanel(taskId: string, executionId?: string): Promise<void> {
		const requestScopeKey = captureChatPageScopeKey();
		let task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		if (!task) {
			await taskStore.loadTasks();
			if (isStaleChatPageScope(requestScopeKey)) return;
			task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		}
		if (!task) {
			// Internal tasks (chat/runtime-spawned work) live behind
			// /v3/tasks/internal and never enter the /tasks feed loadTasks()
			// pulls, so resolve them by id. fetchTaskRecordById returns the
			// task WITHOUT adding it to the store list, so Internal tasks
			// stay out of /tasks while still opening the chat-owned panel.
			task = await taskStore.fetchTaskRecordById(taskId);
			if (isStaleChatPageScope(requestScopeKey)) return;
		}
		if (isStaleChatPageScope(requestScopeKey)) return;
		if (!task) {
			showError('Task not found.');
			return;
		}
		// A run row still opens the full task-details surface, but preserve its
		// concrete execution so the Run act's provenance names what was clicked.
		inspectPanelOpen = false;
		inspectPanelTarget = null;
		taskPanelTask = executionId?.trim()
			? { ...task, executionId: executionId.trim() }
			: task;
		panelRunSelectionId = executionId?.trim() || null;
		panelRunSelectionTaskId = executionId?.trim() ? task.id : null;
		taskPanelOpen = true;
	}

	// Provide the task-panel opener to every ChatMarkdown rendered under this
	// panel, so a task id the LLM mentions in prose opens the chat-owned task
	// panel in place (works for Internal tasks too) instead of navigating to
	// /tasks. Must run during init, before children mount.
	setContext<ChatTaskPanelOpener>(CHAT_TASK_PANEL_OPENER, (taskId) => {
		void openTaskPanel(taskId);
	});

	type OpenAction = 'folder' | 'file';

	async function openSessionPath(
		absolutePath: string,
		action: OpenAction,
		sessionId: string | null
	): Promise<void> {
		const trimmedPath = absolutePath.trim();
		if (!trimmedPath || !sessionId) {
			showError('Cannot open this path without an active chat session.');
			return;
		}
		const endpoint =
			`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}` +
			`/outputs/open-${action}`;
		const response = await timedFetch(endpoint, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ absolute_path: trimmedPath })
		});
		if (response.ok) return;
		let fallback = action === 'folder'
			? "Couldn't open that folder (path may be outside the workspace)"
			: "Couldn't open that file (path may be outside the workspace)";
		try {
			const payload = await response.json();
			if (typeof payload?.error === 'string' && payload.error.trim()) {
				fallback = payload.error;
			}
		} catch {
			// fall through
		}
		showError(fallback);
	}

	async function openStructuredUrl(url: string, sessionId: string | null): Promise<void> {
		const trimmed = url.trim();
		if (!trimmed) {
			showError('Action URL is empty.');
			return;
		}

		const ref = classifyRef(trimmed);
		if (ref.kind === 'external') {
			window.open(ref.url, '_blank', 'noopener,noreferrer');
			return;
		}

		if (ref.kind === 'unknown') {
			showError(`Couldn't resolve "${url}" as an openable destination.`);
			return;
		}

		if (ref.kind === 'task') {
			await openTaskPanel(ref.id);
			return;
		}

		const target = resolveHref(ref);
		if (!target) {
			showError(`Unable to open "${trimmed}".`);
			return;
		}

		const mode = refOpenMode(ref);
		if (mode === 'osAction' && ref.kind === 'filePath') {
			await openSessionPath(ref.absolutePath, 'file', sessionId);
			return;
		}

		if (mode === 'newTab' || mode === 'download') {
			window.open(target, '_blank', 'noopener,noreferrer');
			return;
		}

		try {
			await goto(target);
		} catch (error) {
			showError(error instanceof Error ? error.message : `Couldn't open ${trimmed}`);
		}
	}

	async function openStructuredArtifact(
		artifactId: string,
		messageSessionId: string | null
	): Promise<void> {
		const trimmed = artifactId.trim();
		if (!trimmed) {
			showError('Action artifact id is empty.');
			return;
		}
		if (trimmed.startsWith('magician-artifact:session:')) {
			const relativePath = trimmed.slice('magician-artifact:session:'.length);
			if (relativePath) await openSessionPath(relativePath, 'file', messageSessionId);
			return;
		}
		if (trimmed.startsWith('magician-artifact:task:')) {
			const target = trimmed.slice('magician-artifact:task:'.length);
			const separator = target.indexOf(':');
			if (separator > 0 && separator < target.length - 1) {
				const taskId = target.slice(0, separator);
				const relativePath = target.slice(separator + 1);
				const scope = get(scopeIdentityStore);
				await openAuthenticatedTaskOutput(
					taskOutputUrl(taskId, relativePath, scope.principal, scope.workspace)
				);
			}
			return;
		}

		if (/^task_[0-9a-f]{32}$/i.test(trimmed)) {
			await openTaskPanel(trimmed);
			return;
		}

		const ref = classifyRef(trimmed);
		if (ref.kind === 'filePath') {
			await openSessionPath(ref.absolutePath, 'file', messageSessionId);
			return;
		}

		if (ref.kind === 'task') {
			await openTaskPanel(ref.id);
			return;
		}

		// For task outputs we prefer opening in browser preview when the
		// route is explicit (`/api/magician/v3/tasks/<task>/outputs/...`),
		// while preserving the existing safe classifier behavior for routes,
		// external URLs, and OS-file paths.
		await openStructuredUrl(trimmed, messageSessionId);
	}

	async function handleStructuredResponseAction(
		action: StructuredResponseActionV1,
		messageSessionId: string | null
	): Promise<void> {
		try {
			switch (action.kind) {
				case 'copy_text': {
					if (!navigator.clipboard?.writeText) {
						showError('Clipboard not available in this browser.');
						return;
					}
					await navigator.clipboard.writeText(action.text);
					break;
				}
				case 'open_url':
					await openStructuredUrl(action.url, messageSessionId);
					break;
				case 'open_task':
					await openTaskPanel(action.task_id);
					break;
				case 'open_artifact':
					await openStructuredArtifact(action.artifact_id, messageSessionId);
					break;
				case 'send_follow_up': {
					if (isReadOnly) {
						showError('Cannot send follow-up from read-only chat.');
						return;
					}
					if (!messageSessionId) {
						showError('Unable to send follow-up: no target chat session.');
						return;
					}
					const prompt = action.prompt.trim();
					if (!prompt) {
						showError('Unable to send follow-up: prompt is empty.');
						return;
					}
								dispatch('inline-send', { sessionId: messageSessionId, text: prompt });
								await chatStore.sendMessageStreaming(messageSessionId, prompt, get(chatProfileStore).selected, [], harnessSendOptions(messageSessionId));
								break;
							}
							case 'invoke_server_action':
								showError('Server-side structured actions are not supported.');
								break;
						}
				} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to run action.');
		}
	}

	function closeTaskPanel(): void {
		taskPanelOpen = false;
		taskPanelTask = null;
		panelRunSelectionId = null;
		panelRunSelectionTaskId = null;
	}

	/**
	 * Load the open task's outputs. The request id guards the *reply*, not the
	 * request: a fetch already in flight cannot be recalled, so a late answer for
	 * a task the reader has left is discarded rather than rendered under the one
	 * they are looking at.
	 */
	async function loadPanelOutputs(taskId: string | null): Promise<void> {
		if (taskId === null) {
			panelOutputsRequestId += 1;
			panelOutputs = null;
			panelOutputsTaskId = null;
			return;
		}

		const requestId = ++panelOutputsRequestId;
		if (panelOutputsTaskId !== taskId) {
			panelOutputs = null;
			panelOutputsTaskId = null;
		}
		const scope = get(scopeIdentityStore);
		const files = await fetchTaskOutputFiles(taskId, scope.principal, scope.workspace);
		if (requestId !== panelOutputsRequestId) return;
		panelOutputs = files;
		panelOutputsTaskId = taskId;
	}

	async function refreshTaskPanelRecord(taskId: string): Promise<void> {
		const refreshedInList = await taskStore.refreshTask(taskId);
		const fresh = refreshedInList
			? get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null
			: await taskStore.fetchTaskRecordById(taskId);
		if (fresh && taskPanelTask?.id === taskId) taskPanelTask = fresh;
	}

	const taskPanelPoll = createTaskPanelPoll<ExecutionPanelState | null>({
		read: async (target: PanelPollTarget) => {
			const scope = get(scopeIdentityStore);
			const [, run] = await Promise.all([
				refreshTaskPanelRecord(target.taskId),
				readTaskRunState(
					target.taskId,
					scope.principal,
					scope.workspace,
					target.executionId
				)
			]);
			if (!run.ok) throw new Error(run.reason);
			return run.state;
		},
		onSnapshot: (target, state, at) => {
			if (target.taskId !== (taskPanelTask?.id ?? null)) return;
			panelRunState = state;
			panelRunStateTaskId = target.taskId;
			panelLastLoadedAt = at;
			taskPanelLoadFailure = null;
		},
		onFailure: (target, message) => {
			if (target.taskId !== (taskPanelTask?.id ?? null)) return;
			taskPanelLoadFailure = message;
		}
	});

	/**
	 * The reader picked a different run to read — re-read `/execution-panel` for it.
	 * The selection is recorded before the fetch and never rolled back; a failure
	 * leaves the Run act's timeline absent, which is design §6's partial load and
	 * what the panel's Retry re-reads. See `TasksWorkspace.svelte`.
	 */
	function handlePanelSelectRun(event: CustomEvent<{ executionId: string }>): void {
		const taskId = taskPanelTask?.id ?? null;
		if (taskId === null) return;
		const executionId = event.detail?.executionId?.trim() ?? '';
		if (!executionId) return;
		panelRunSelectionId = executionId;
		panelRunSelectionTaskId = taskId;
	}

	async function handlePanelOpenFile(
		event: CustomEvent<{ file: PanelOutputFile; index: number }>
	): Promise<void> {
		const taskId = taskPanelTask?.id;
		const path = event.detail.file.path;
		if (!taskId || !path) return;
		const scope = get(scopeIdentityStore);
		try {
			await openInternalTaskOutputFile(taskId, path, scope.principal, scope.workspace);
		} catch (error) {
			showError(error instanceof Error ? error.message : `Couldn't open ${event.detail.file.name}`);
		}
	}

	async function handlePanelRevealFile(
		event: CustomEvent<{ file: PanelOutputFile; index: number }>
	): Promise<void> {
		const taskId = taskPanelTask?.id;
		const path = event.detail.file.path;
		if (!taskId || !path) return;
		const scope = get(scopeIdentityStore);
		try {
			await revealInternalTaskOutputFile(taskId, path, scope.principal, scope.workspace);
		} catch (error) {
			showError(
				error instanceof Error ? error.message : `Couldn't reveal ${event.detail.file.name}`
			);
		}
	}

	/**
	 * The task panel's Retry (design §6, case 2): re-read the task, its outputs
	 * and the ask blocking it. Clearing the two keys is what happens in the
	 * meantime — what is on screen was loaded before a failure, so it is unknown
	 * until the replies land rather than shown as current.
	 */
	async function handlePanelRetry(): Promise<void> {
		const taskId = taskPanelTask?.id;
		if (!taskId) return;
		panelOutputsTaskId = null;
		panelOutputsGuard.key = '';
		panelRunStateTaskId = null;
		taskPanelLoadFailure = null;
		taskPanelPoll.refreshNow();
		await refreshOpenTaskPanel(taskId);
	}

	/**
	 * The controls the task drawer's header carries.
	 *
	 * Deliberately the same three-verb ladder `/tasks` and the thread workspace
	 * offer, routed through the handlers the retired panel's events used to
	 * reach — so a chat card and a task card cannot mean different things by
	 * `Stop`. Pending work exposes both the safe Plan path and the explicit
	 * direct-run path, matching the task and thread surfaces.
	 */
	function taskPanelActions(task: Task): Array<{ label: string; run: () => void }> {
		if (task.status === 'running' || task.status === 'planning') {
			return [{ label: 'Stop', run: () => void handleTaskPanelAbort(task.id) }];
		}
		if (task.status === 'paused' || task.status === 'failed' || task.status === 'cancelled') {
			return task.executionId
				? [{ label: 'Reset to Ready', run: () => void handleTaskPanelReset(task.id) }]
				: [];
		}
		if (task.status === 'ready') {
			return [{ label: 'Run', run: () => void handleTaskPanelExecute(task.id) }];
		}
		if (task.status === 'pending') {
			return [
				{ label: 'Plan', run: () => void handleTaskPanelExecute(task.id) },
				{ label: 'Run now', run: () => void handleTaskPanelExecuteDirect(task.id) }
			];
		}
		return [];
	}

	async function refreshOpenTaskPanel(taskId: string): Promise<void> {
		await taskStore.loadTasks();
		const refreshed =
			get(taskStore).tasks.find((candidate) => candidate.id === taskId) ??
			(await taskStore.fetchTaskRecordById(taskId));
		if (refreshed && taskPanelTask?.id === taskId) taskPanelTask = refreshed;
	}

	/*
	 * The three verbs the task drawer's header offers.
	 *
	 * They take a task id rather than a `CustomEvent`: they used to be bound to
	 * the retired panel's `execute` / `abort` / `resetToReady` events, and the
	 * envelope was the event system's, not theirs. The drawer's header calls them
	 * directly — a shell that dispatched typed task verbs would be the surface
	 * knowledge it was extracted to stay out of.
	 */
	async function handleTaskPanelExecute(taskId: string): Promise<void> {
		try {
			const task = taskPanelTask?.id === taskId ? taskPanelTask : null;
			if (task?.status === 'pending') {
				await taskStore.planTask(taskId);
				showSuccess('Plan generated. Review and execute when ready.');
			} else {
				await taskStore.executeTask(taskId);
				showSuccess('Execution started.');
			}
			await refreshOpenTaskPanel(taskId);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to start task');
		}
	}

	async function handleTaskPanelExecuteDirect(taskId: string): Promise<void> {
		try {
			await taskStore.executeTaskDirect(taskId);
			showSuccess('Direct execution started.');
			await refreshOpenTaskPanel(taskId);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to start direct execution');
		}
	}

	async function handleTaskPanelAbort(taskId: string): Promise<void> {
		try {
			const status = await taskStore.abortTask(taskId);
			showSuccess(status === 'ready' ? 'Task aborted. Ready to re-run.' : 'Task aborted.');
			await refreshOpenTaskPanel(taskId);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to abort task');
		}
	}

	async function handleTaskPanelReset(taskId: string): Promise<void> {
		try {
			const status = await taskStore.resetTaskToReady(taskId);
			showSuccess(status === 'ready' ? 'Task reset to ready.' : 'Task reset to pending.');
			await refreshOpenTaskPanel(taskId);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to reset task');
		}
	}

	function openTaskDetailsFromStatus(content: ChatMessageContent): void {
		const taskId = content.task_id?.trim();
		if (taskId) {
			void openTaskPanel(taskId, content.execution_id);
			return;
		}
		// Defensive compatibility for historical malformed status messages that
		// carried only an execution id. They remain inspectable as a run.
		openInspectionPanel(content);
	}

	async function openPlanSheet(taskId: string): Promise<void> {
		const requestScopeKey = captureChatPageScopeKey();
		let task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		if (!task) {
			await taskStore.loadTasks();
			if (isStaleChatPageScope(requestScopeKey)) return;
			task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		}
		if (isStaleChatPageScope(requestScopeKey)) return;
		if (!task) {
			showError('Task not found.');
			return;
		}
		planSheetTaskId = task.id;
		isPlanSheetOpen = true;
	}

	function closePlanSheet(): void {
		isPlanSheetOpen = false;
		planSheetTaskId = null;
	}

	async function handleNewExecution() {
        const session = await chatStore.newExecution(currentThreadId ?? 'general');
        if (session) { focusedAnswerId = null; replaceSessionDeepLink(session.id); }
	}

	async function handleInlinePlanAction(
		task: Task,
		action: 'open' | 'approve' | 'reject' | 'replan' | 'execute'
	): Promise<void> {
		const actionKey = `${task.id}:${action}`;
		const requestScopeKey = captureChatPageScopeKey();
		inlinePlanActionKey = actionKey;
		try {
			if (action === 'open') {
				await openPlanSheet(task.id);
				return;
			}
			if (action === 'approve') {
				const planId = task.latestPlanId?.trim();
				if (!planId) throw new Error('The displayed plan has no version id.');
				await taskStore.approvePlan(task.id, planId);
				if (isStaleChatPageScope(requestScopeKey)) return;
				showSuccess('Plan approved.');
			} else if (action === 'reject') {
				const planId = task.latestPlanId?.trim();
				if (!planId) throw new Error('The displayed plan has no version id.');
				await taskStore.rejectPlan(task.id, planId);
				if (isStaleChatPageScope(requestScopeKey)) return;
				showSuccess('Plan rejected.');
			} else if (action === 'replan') {
				await taskStore.replanTask(task.id);
				if (isStaleChatPageScope(requestScopeKey)) return;
				showSuccess('Planning restarted.');
			} else if (action === 'execute') {
				await taskStore.executeTask(task.id);
				if (isStaleChatPageScope(requestScopeKey)) return;
				showSuccess('Execution started.');
				await openTaskPanel(task.id);
				if (isStaleChatPageScope(requestScopeKey)) return;
			}
			await taskStore.loadTasks();
			if (isStaleChatPageScope(requestScopeKey)) return;
		} catch (error) {
			if (isStaleChatPageScope(requestScopeKey)) return;
			showError(error instanceof Error ? error.message : 'Failed to update task plan');
		} finally {
			if (isStaleChatPageScope(requestScopeKey)) return;
			if (inlinePlanActionKey === actionKey) {
				inlinePlanActionKey = null;
			}
		}
	}

	// Shared drain — load older history pages back-to-back until
	// `has_more=false`. Used by both the scroll-to-top trigger and the
	// explicit "Load earlier messages" button.
	async function drainOlderMessages(): Promise<void> {
		if (!activeSessionId) return;
		while ($chatStore.hasMoreMessages && !$chatStore.isLoadingOlder) {
			await chatStore.loadOlderMessages(activeSessionId);
		}
	}

	async function handleChatScroll(e: Event): Promise<void> {
		const el = e.target as HTMLElement;
		if (!el) return;
		// Stickiness flag — every native scroll event re-evaluates
		// whether the viewport is pinned to the bottom. Drives both
		// the auto-scroll reactives above and the jump-to-latest pill
		// visibility below. Programmatic `scrollTo`/`scrollTop` writes
		// also fire scroll events, so the flag stays correct after
		// `scrollToBottom()` snaps us back to the tail.
		isAtBottom = computeIsAtBottom(el);
		if (!activeSessionId) return;
		// Once the user lands in the top trigger zone, drain the entire
		// history back-to-back. We can't keep re-checking `scrollTop`
		// per-iteration because browser scroll-anchoring repositions
		// us *out* of the trigger zone after the first prepend (the
		// previously-visible message stays in place; scrollTop jumps
		// to the prepended height). The `isLoadingOlder` spinner shows
		// for each page; the loop exits when the backend reports
		// `has_more=false`.
		if (el.scrollTop < 80 && $chatStore.hasMoreMessages && !$chatStore.isLoadingOlder) {
			await drainOlderMessages();
		}
	}

	// Force-scroll affordance for the jump-to-latest pill. Mirrors the
	// existing `scrollToBottom` smooth-scroll but explicitly resets
	// the stickiness flag so the pill hides immediately rather than
	// waiting for the post-scroll event to flip it.
	function jumpToLatest(): void {
		isAtBottom = true;
		scrollToBottom();
	}

    let lastAnswerRoute = '';
    let focusedAnswerId: string | null = null;
    $: answerRoute = `${currentScopeKey}:${$page.url.pathname}${$page.url.search}`;
    // Same-page navigation can address a conversation without a message (for
    // example the branch's parent link). Follow both forms on every URL change.
    $: if (browser && mounted && !reloadingTargetThreadId && $page.url.searchParams.get('session')?.trim() && answerRoute !== lastAnswerRoute) {
        lastAnswerRoute = answerRoute;
        void openAnswerRoute(answerRoute);
    }
    async function openAnswerRoute(route: string) {
        const sessionId = $page.url.searchParams.get('session')?.trim();
        const target = messageTargetFromQuery($page.url.searchParams);
        if (!sessionId || window.location.pathname + window.location.search !== $page.url.pathname + $page.url.search) return;
        focusedAnswerId = null;
        const current = get(chatStore);
        if (!target && (current.viewingArchivedId ?? current.activeSessionId) === sessionId) return;
        const opened = await chatStore.openSession(sessionId, target);
        if (!opened || route !== answerRoute) return;
        const sourceThreadId = normalizeThreadId(opened.ui_thread_id);
        if (sourceThreadId !== targetThreadId) {
            const query = new URLSearchParams($page.url.searchParams);
            await goto(`/t/${encodeURIComponent(sourceThreadId)}/chat?${query}`);
            return;
        }
        if (!target) {
            await tick();
            if (route === answerRoute) { isAtBottom = true; scrollToBottom(); }
            return;
        }
        focusedAnswerId = get(chatStore).focusedMessageId ?? null;
        if (focusedAnswerId) { chatInitialScrollDone = true; isAtBottom = false; }
        await tick();
        if (route !== answerRoute || !focusedAnswerId || (get(chatStore).viewingArchivedId ?? get(chatStore).activeSessionId) !== sessionId) return;
        chatInitialScrollDone = true;
        isAtBottom = false;
        const canonicalUrl = new URL(window.location.href);
        if (canonicalUrl.searchParams.get('session') !== sessionId) return;
        canonicalUrl.searchParams.set('message', focusedAnswerId);
        canonicalUrl.searchParams.delete('source_turn');
        canonicalUrl.searchParams.delete('source_at');
        lastAnswerRoute = `${currentScopeKey}:${canonicalUrl.pathname}${canonicalUrl.search}`;
        replaceState(canonicalUrl, $page.state);
        messagesContainerEl?.querySelector<HTMLElement>('[data-linked-answer="true"]')?.scrollIntoView({ block: 'center' });
        const receipt = get(concurrentVoiceStore).requests.find(request =>
            request.branch_session_id === sessionId && request.result_message_id === focusedAnswerId);
        // Navigation remains usable after the short-lived receipt is pruned.
        if (receipt && receipt.read_at == null) void markConcurrentVoiceResultRead(receipt.id).catch(() => {});
    }

	async function handleViewExecution(sessionId: string) {
		chatInitialScrollDone = false;
		const sessions = $chatStore.sessions;
		if (sessionId === activeSessionId && !viewingArchivedId) return;
		const session = sessions.find(s => s.id === sessionId);
		if (!session) return;

		const opened = await chatStore.openSession(sessionId);
		if (opened) replaceSessionDeepLink(sessionId);
	}

	function replaceSessionDeepLink(sessionId: string): void {
		if (!browser) return;
		const url = new URL(window.location.href);
		url.searchParams.set('session', sessionId);
        for (const key of ['message', 'source_turn', 'source_at']) url.searchParams.delete(key);
        focusedAnswerId = null;
		replaceState(url, $page.state);
	}

	async function handleReturnToActive(): Promise<void> {
		const returnSessionId = activeSessionId;
		await chatStore.returnToActive();
		if (returnSessionId && get(chatStore).viewingArchivedId === null) {
			replaceSessionDeepLink(returnSessionId);
		}
	}

	// Fill the composer with a suggested prompt and focus it so the user
	// can edit/send immediately. Exactly one of the two composers is
	// mounted per layout; the other ref is null and its call is a no-op.
	function applySuggestedPrompt(prompt: string): void {
		inputText = prompt;
		composerEl?.focus();
	}

	// Up to 3 recent sessions for the empty state. Every row opens by exact
	// session ID; active rows become writable and archived rows remain
	// read-only. The session currently on screen is excluded — switching to
	// it is a no-op. Store order isn't guaranteed, so sort by recency.
	$: recentLauncherSessions = sessions
		.filter((session) => session.id !== currentSessionId)
		.sort((a, b) => b.updated_at - a.updated_at)
		.slice(0, 3);

	async function handleDeleteChatMessage(messageIds: string[]): Promise<void> {
		const sessionId = currentSessionId;
		const ids = messageIds.map((id) => id.trim()).filter(Boolean);
		if (!sessionId || ids.length === 0) return;
		const ok = await requestConfirmation({
			title: ids.length === 1 ? 'Delete this message?' : `Delete ${ids.length} messages?`,
			message: 'This removes the selected messages from the chat history.',
			confirmLabel: 'Delete',
			destructive: true
		});
		if (!ok) return;
		try {
			await chatStore.deleteMessage(sessionId, ids);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to delete message');
		}
	}

	let isClearingAll = false;
	// ── Docked ContextPill handlers ──────────────────────────────────────
	// Named rather than inline so the pill's wiring reads in one place; it
	// now lives inside the composer's dock slot, several hundred lines from
	// the rest of the session controls.
	function handlePillOpenHistory(): void {
		openHistoryDrawer({
			threadFilter: threadId ? currentThreadId : null,
			initialTab: 'sessions'
		});
	}

	function handlePillNewSession(): void {
        void handleNewExecution();
	}

	function handlePillArchive(): void {
		if (activeSessionId) void chatStore.archiveSession(activeSessionId);
	}

	async function handleClearAllMessages(): Promise<void> {
		const sessionId = currentSessionId;
		if (!sessionId || isReadOnly) return;
		const ok = await requestConfirmation({
			title: 'Clear all messages in this chat?',
			message: 'This cannot be undone.',
			confirmLabel: 'Clear all',
			destructive: true
		});
		if (!ok) return;
		isClearingAll = true;
		try {
			await chatStore.clearMessages(sessionId);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to clear chat');
		} finally {
			isClearingAll = false;
		}
	}

	async function handleDeleteSession(): Promise<void> {
		const sessionId = activeSessionId;
		if (!sessionId || isReadOnly) return;
		const ok = await requestConfirmation({
			title: 'Delete this session?',
			message:
				'The session, its messages, and its staged attachments are permanently deleted. This cannot be undone.',
			confirmLabel: 'Delete',
			destructive: true
		});
		if (!ok) return;
		try {
			await chatStore.deleteSession(sessionId);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to delete session');
		}
	}

	function setEscalationResponding(key: string, responding: boolean): void {
		if (!key) return;
		const next = new Set(respondingEscalationKeys);
		if (responding) {
			next.add(key);
		} else {
			next.delete(key);
		}
		respondingEscalationKeys = next;
	}

	function taskStatusLabel(status: string | undefined): string {
		switch (status) {
			case 'completed':
				return 'Task completed';
			case 'failed':
				return 'Task failed';
			case 'cancelled':
				return 'Task cancelled';
			default:
				return 'No longer active';
		}
	}

	function taskForExecution(executionId: string | undefined): Task | null {
		const normalizedExecutionId = executionId?.trim();
		if (!normalizedExecutionId) return null;
		// First try the thread-filtered view (most common: chat-spawned
		// task with `ui_thread_id` set). Then fall back to the full task
		// list — tasks created without a `ui_thread_id` (or where the
		// backend hasn't yet linked them to a chat session) are still
		// live tasks, and previously the absence of a thread match
		// caused the escalation card to mislabel them as "Task deleted"
		// even when status was `running` on the backend.
		const fromThread = currentThreadTasks.find(
			(task) => task.executionId?.trim() === normalizedExecutionId,
		);
		if (fromThread) return fromThread;
		return (
			$taskStore.tasks.find(
				(task) => task.executionId?.trim() === normalizedExecutionId,
			) ?? null
		);
	}

	function escalationInactiveReason(content: ChatMessageContent): string | null {
		if (content.type !== 'escalation') return null;
		const explicitReason = content.inactive_reason?.trim();
		if (content.resolved) return explicitReason || 'Answered';
		if (content.stale) return explicitReason || 'No longer active';
		// Clarifications are keyed by question + task/workflow, not by a live
		// task execution. Older persisted rows may carry a planexec id without
		// the responder marker added by this migration, so the generic execution
		// existence check below cannot determine whether they are actionable.
		if (content.escalation_type === 'clarification') return null;
		// UserRequestService-backed escalations (memory clarifications,
		// learning consolidation questions like "should I remember that…")
		// are NOT tied to a live execution — their lifetime is the
		// `request_id` ledger, not a task. Bail out before the
		// task-existence gate below, otherwise a memory question whose
		// originating execution is missing from the local thread store
		// renders as "Task deleted" and the user can never answer it
		// even though the question is still valid server-side.
		if (content.request_id?.trim()) return null;
		const executionId = content.execution_id?.trim();
		if (!executionId) return null;
		const task = taskForExecution(executionId);
		if (task && isTerminalTaskStatus(task.status)) {
			return taskStatusLabel(task.status);
		}
		if (!isReadOnly && !$taskStore.isLoading && !task) {
			return 'Task deleted';
		}
		return null;
	}

	function isEscalationNonActionable(content: ChatMessageContent): boolean {
		return escalationInactiveReason(content) !== null;
	}

	/**
	 * Should the escalation card render with the dim/grayed "this is done"
	 * styling? Only `true` for escalations the user already answered
	 * (`resolved`) or that were superseded by a newer one (`stale`).
	 *
	 * `Task deleted` and terminal-task labels DO render an inactive_reason
	 * footer (so the user knows acting on it is moot), but the card stays
	 * full-opacity because the agent's message inside (often a `cannot_proceed`
	 * with a substantive "here's why I couldn't…" explanation) is still
	 * worth reading. Dimming it makes the agent's reply look thrown-away
	 * even though it carries useful context.
	 */
	function shouldDimEscalation(content: ChatMessageContent): boolean {
		if (content.type !== 'escalation') return false;
		if (content.resolved) return true;
		if (content.stale) return true;
		return false;
	}

	function escalationStatusLabel(content: ChatMessageContent): string {
		if (content.type !== 'escalation') return '';
		const inactiveReason = escalationInactiveReason(content);
		if (inactiveReason === 'Task deleted') return 'Task Deleted';
		if (content.resolved) return 'Resolved';
		if (inactiveReason) return 'No Longer Active';
		return 'Action Required';
	}

	function shouldShowEscalationActions(content: ChatMessageContent): boolean {
		return content.type === 'escalation'
			&& Boolean(content.options?.length)
			&& !isEscalationNonActionable(content);
	}

	function scopedChatHeaders(headers?: HeadersInit): Headers {
		return scopedRequestHeaders(headers);
	}

	function pendingUserRequestIds(payload: unknown): string[] {
		const record = payload && typeof payload === 'object' && !Array.isArray(payload)
			? payload as Record<string, unknown>
			: {};
		const requests = Array.isArray(record.requests) ? record.requests : [];
		return requests
			.map((entry) => {
				if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return '';
				const value = (entry as Record<string, unknown>).id;
				return typeof value === 'string' ? value.trim() : '';
			})
			.filter(Boolean);
	}

	async function reconcilePendingUserRequestEscalations(): Promise<void> {
		const scopeKey = captureChatPageScopeKey();
		try {
			const response = await timedFetch('/api/magician/v2/user-requests', {
				headers: scopedChatHeaders()
			});
			if (isStaleChatPageScope(scopeKey) || !response.ok) return;
			const payload = await response.json();
			if (isStaleChatPageScope(scopeKey)) return;
			// Clarification responder ids use the compatibility request_id slot
			// but are not UserRequestService ledger ids. Preserve those cards
			// while reconciling actual service-backed requests.
			const clarificationResponderMarkers = get(chatStore).messages
				.filter((message) => message.content.type === 'escalation')
				.filter((message) => message.content.escalation_type === 'clarification')
				.map((message) => message.content.request_id?.trim() || '')
				.filter(Boolean);
			chatStore.reconcilePendingUserRequests([
				...pendingUserRequestIds(payload),
				...clarificationResponderMarkers
			]);
		} catch {
			// Best effort only: stale buttons can still be resolved by live events or response attempts.
		}
	}

	/**
	 * The run a status card is pointing at, or `null` when it names none.
	 *
	 * **The task id is required now, and that is a behaviour change worth
	 * naming.** This used to fall back to a synthetic `agent-cycle:<execution>`
	 * id when a card carried only an execution — and that id routes to
	 * `/v3/executions/{id}/execution-panel`, which is *not* populated for the
	 * task-backed delegate runs these cards describe, so the fallback opened a
	 * panel that could only say `execution panel state not found`. A card with no
	 * task id now offers nothing rather than a dead end, which is what
	 * `RequestActivityCard` already decided for its own *Inspect run* — it
	 * requires both ids before showing the control at all.
	 */
	function inspectionTargetOf(content: ChatMessageContent): ExecutionPanelTarget | null {
		const executionId = content.execution_id?.trim();
		const taskId = content.task_id?.trim();
		if (!executionId || !taskId) return null;
		return { taskId, executionId };
	}

	function openInspectionPanel(content: ChatMessageContent): void {
		const target = inspectionTargetOf(content);
		if (!target) return;
		taskPanelOpen = false;
		taskPanelTask = null;
		inspectPanelTarget = target;
		inspectPanelOpen = true;
	}

	// Open the run panel directly from a RequestActivityCard's "Inspect run →"
	// affordance, using the (task_id, execution_id) the card derived from its
	// events. Goes through the same target builder as a status card, so a run is
	// inspectable even when no standalone task-status card was rendered for the
	// turn — and both gestures agree about which run they mean.
	function openInspectionPanelFromIds(
		detail: { taskId: string; executionId: string } | undefined
	): void {
		const executionId = detail?.executionId?.trim();
		const taskId = detail?.taskId?.trim();
		if (!executionId || !taskId) return;
		openInspectionPanel({
			type: 'task_status_update',
			task_id: taskId,
			execution_id: executionId,
			status: 'running'
		} as ChatMessageContent);
	}

	function closeInspectionPanel(): void {
		inspectPanelOpen = false;
		inspectPanelTarget = null;
	}

	/** One run, as a key: two executions of one task must not share an answer. */
	function inspectionKey(target: ExecutionPanelTarget): string {
		return `${target.taskId}|${target.executionId}`;
	}

	/**
	 * Read the inspected run: its panel state, and the files it produced.
	 *
	 * Both are keyed on the run rather than the task, and both replies are
	 * guarded by one request id — a single counter for the pair, because they
	 * describe the same run and a model built from one run's state and another
	 * run's files would be wrong in a way nothing downstream could catch.
	 *
	 * The outputs request is the ordinary task-scoped one: the id these cards
	 * carry is a real task id, so it answers. `null` from it is a failed read and
	 * the Output act is then absent, never empty.
	 */
	async function loadInspection(target: ExecutionPanelTarget | null): Promise<void> {
		if (target === null) {
			inspectRequestId += 1;
			inspectState = null;
			inspectStateKey = null;
			inspectOutputs = null;
			inspectOutputsKey = null;
			inspectLoadError = null;
			return;
		}

		const key = inspectionKey(target);
		const requestId = ++inspectRequestId;
		inspectState = null;
		inspectStateKey = null;
		inspectOutputs = null;
		inspectOutputsKey = null;
		inspectLoadError = null;

		const scope = get(scopeIdentityStore);
		const [state, files] = await Promise.all([
			fetchExecutionPanelState(target, scope.principal, scope.workspace),
			fetchTaskOutputFiles(target.taskId, scope.principal, scope.workspace)
		]);
		if (requestId !== inspectRequestId) return;

		inspectState = state;
		inspectStateKey = state === null ? null : key;
		inspectOutputs = files;
		inspectOutputsKey = files === null ? null : key;
		// The one thing the drawer cannot work out for itself. Without a state
		// there is no verdict to report, and a drawer that rendered its skeleton
		// forever would claim the read was still in flight.
		inspectLoadError =
			state === null
				? "This run's activity is no longer available — its task may have been cleared from history."
				: null;
	}

	/**
	 * Follow the inspected run live.
	 *
	 * The fetch beside this answers once; a run being watched moves. The panel
	 * itself stays a pure render of a prop — the subscription belongs here, where
	 * the fetch already is, and it writes the same three variables the fetch
	 * writes so there is one path into the drawer rather than two.
	 *
	 * Idempotent on the run: called from a reactive block, so it runs on every
	 * unrelated re-render and must not tear down and re-establish the socket
	 * subscription each time.
	 */
	function syncInspectionStream(target: ExecutionPanelTarget | null): void {
		const key = target === null ? null : inspectionKey(target);
		if (key === inspectStreamKey) return;
		inspectStreamStop?.();
		inspectStreamStop = null;
		inspectStreamKey = key;
		if (target === null) return;

		const scope = get(scopeIdentityStore);
		inspectStreamStop = streamExecutionPanelState(target, scope, (state) => {
			// The drawer may have moved on between the push and this callback. The
			// same guard the fetch's request id is, against the same failure: one
			// run's state rendered under another run's drawer.
			if (inspectStreamKey !== key) return;
			inspectState = state;
			inspectStateKey = key;
			// A push is a successful read, so it clears the staleness line the same
			// way a successful fetch does — and the `as of` instant moves with it,
			// or the drawer would report live state as minutes old.
			inspectLoadError = null;
			panelLastLoadedAt = Date.now();
		});
	}

	/** Re-read the inspected run, which is all its Retry can do. */
	async function handleInspectionRetry(): Promise<void> {
		await loadInspection(inspectPanelTarget);
	}

	/** The path the run drawer's Nth output row names, or `null`. */
	function inspectionOutputPath(index: number): string | null {
		if (inspectPanelTarget === null) return null;
		if (inspectOutputsKey !== inspectionKey(inspectPanelTarget)) return null;
		return inspectOutputs?.[index]?.path ?? null;
	}

	async function handleInspectionOpenFile(
		event: CustomEvent<{ file: { name: string }; index: number }>
	): Promise<void> {
		const taskId = inspectPanelTarget?.taskId;
		const path = inspectionOutputPath(event.detail.index);
		if (!taskId || !path) return;
		const scope = get(scopeIdentityStore);
		try {
			await openInternalTaskOutputFile(taskId, path, scope.principal, scope.workspace);
		} catch (error) {
			showError(error instanceof Error ? error.message : `Couldn't open ${event.detail.file.name}`);
		}
	}

	async function handleInspectionRevealFile(
		event: CustomEvent<{ file: { name: string }; index: number }>
	): Promise<void> {
		const taskId = inspectPanelTarget?.taskId;
		const path = inspectionOutputPath(event.detail.index);
		if (!taskId || !path) return;
		const scope = get(scopeIdentityStore);
		try {
			await revealInternalTaskOutputFile(taskId, path, scope.principal, scope.workspace);
		} catch (error) {
			showError(
				error instanceof Error ? error.message : `Couldn't reveal ${event.detail.file.name}`
			);
		}
	}

	// Re-exported wrapper around `chatStore::isHiddenTranscriptMessage`
	// so the page-local rendering keeps the same call shape but the
	// filter logic lives in one place. Add page-specific overrides
	// here if a future filter doesn't generalize to other surfaces.
	const isHiddenTranscriptMessage = isHiddenTranscriptMessageHelper;

	function toggleRunExpanded(group: ChatRenderTaskExecutionGroup, total: number): void {
		const next = new Map(runExpansionOverrides);
		next.set(group.id, !isRunExpanded(runExpansionOverrides, group, total));
		runExpansionOverrides = next;
	}

	async function handleEscalationResponse(
		executionId: string,
		pauseStateId: string,
		option: EscalationOption,
		escalationType?: string,
		requestId?: string,
		pauseInputType?: string,
		question?: string,
		options?: EscalationOption[]
	) {
		const taskResponderId = escalationType === 'clarification' && !requestId?.trim()
			? currentThreadTasks.find((task) =>
				taskPendingQuestions(task).some((question) => question.id === pauseStateId)
			)?.id
			: undefined;
		const responderId = taskResponderId
			?? clarificationResponderId(escalationType, requestId, executionId);
		// V3 planning clarifications persist their task responder separately
		// from the durable planexec id. Prefer the composer flow when that task
		// is loaded; legacy AskLoop workflows use the canonical modal below.
		if (
			escalationType === 'clarification'
			&& responderId
			&& focusComposerForPlanTask(responderId, pauseStateId, false)
		) {
			return;
		}
		const responseKey = pauseStateId.trim() || requestId?.trim() || executionId.trim();
		const requestScopeKey = captureChatPageScopeKey();
		const continuationGeneration = hitlContinuationGeneration;
		const continuationIsCurrent = () => chatHitlContinuationIsCurrent(
			requestScopeKey,
			currentScopeKey,
			continuationGeneration,
			hitlContinuationGeneration
		);
		const serviceRequestId = escalationType === 'clarification' ? undefined : requestId;
		try {
			setEscalationResponding(responseKey, true);

			// Keep Trying has a dedicated continuation endpoint. Stopping still
			// uses the shared canonical responder with a confirmation value.
			if (escalationType === 'max_iterations') {
				if (option.id === 'continue') {
					const continueResponse = await timedFetch(
						`/api/magician/v2/executions/${encodeURIComponent(executionId)}/execution/agentic-continue`,
						{
							method: 'POST',
							headers: scopedChatHeaders({ 'Content-Type': 'application/json' }),
							body: JSON.stringify({ pause_state_id: pauseStateId })
						}
					);
					if (!continuationIsCurrent()) return;
					if (continueResponse.ok || continueResponse.status === 404) {
						chatStore.markEscalationResolved(executionId, undefined, pauseStateId);
					}
					return;
				}

				const stopRequest: HitlRequest = {
					id: pauseStateId,
					source: 'escalation',
					input_type: 'confirmation',
					schema: {},
					prompt: question?.trim() || 'Stop this execution?',
					scope: { execution_id: executionId },
					identifiers: {
						correlation_id: pauseStateId,
						pause_state_id: pauseStateId
					}
				};
				const outcome = await postHitlResponse(
					stopRequest,
					{ type: 'confirmation', confirmed: false },
					scopedChatHeaders({ 'Content-Type': 'application/json' })
				);
				if (!continuationIsCurrent()) return;
				if (outcome.ok) {
					chatStore.markEscalationResolved(executionId, undefined, pauseStateId);
				} else if (!('cancelled' in outcome)) {
					showError(outcome.message);
				}
				return;
			}

			const scope = get(scopeIdentityStore);
			const target = buildPersistedChatHitlTarget({
				executionId,
				pauseStateId,
				escalationType,
				requestId: escalationType === 'clarification' && responderId
					? `${CLARIFICATION_RESPONDER_REQUEST_PREFIX}${responderId}`
					: requestId,
				inputType: pauseInputType,
				question,
				options,
				principal: scope.principal,
				workspace: scope.workspace
			});
			if (!target) {
				showError('Unable to respond: the persisted HITL target is incomplete.');
				return;
			}
			const result = await openHitlPrompt(target);
			if (!continuationIsCurrent()) return;
			if (result.status === 'resolved') {
				chatStore.markEscalationResolved(executionId, serviceRequestId, pauseStateId);
			} else if (result.status === 'error') {
				showError(result.error);
			}
		} catch (e) {
			console.error('[Chat] Failed to respond to escalation:', e);
		} finally {
			if (!continuationIsCurrent()) return;
			setEscalationResponding(responseKey, false);
		}
	}

	function formatTimestamp(ts: number): string {
		const date = new Date(ts);
		const now = new Date();
		const isToday =
			date.getDate() === now.getDate() &&
			date.getMonth() === now.getMonth() &&
			date.getFullYear() === now.getFullYear();

		if (isToday) {
			return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
		}
		return date.toLocaleDateString([], { month: 'short', day: 'numeric' }) +
			' ' +
			date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	async function reloadChatScope(): Promise<void> {
		const requestedThreadId = targetThreadId;
		// A `?session=` deep link seeds the load: the requested chat opens first
		// and renders, instead of the active one loading (with every turn's
		// events) before the session list, tasks and thread, and only then the
		// requested chat — a History click waited ~0.5 s on that chain, and
		// seconds behind a slow request. Outside its thread it falls back to
		// the active session, exactly as `applySessionDeepLink` would.
		const deepLinkSessionId = ($page.url.searchParams.get('session') || '').trim() || null;
		const requestedSeedSessionId =
			seedStagedAttachments?.[0]?.session_id ?? deepLinkSessionId;
		if (planReplyIntent && planReplyIntent.threadId !== requestedThreadId) {
			const returnMode = planReplyIntent.returnMode;
			planReplyIntent = null;
			inputText = '';
			stagedAttachments = [];
			planModeStore.select(returnMode);
		}
		reloadingTargetThreadId = requestedThreadId;
		try {
			await loadSeedSessionOrActive(
				requestedThreadId,
				requestedSeedSessionId,
				chatStore
			);
			await Promise.all([
				chatStore.loadSessions(threadId ? requestedThreadId : null),
				taskStore.loadTasks(),
				threadStore.ensureThread(requestedThreadId)
			]);
			loadedTargetThreadId = requestedThreadId;
			await reconcilePendingUserRequestEscalations();
			await applySessionDeepLink(requestedThreadId);
			await tick();
			scrollToBottom();
		} finally {
			if (reloadingTargetThreadId === requestedThreadId) {
				reloadingTargetThreadId = null;
			}
		}
	}

	async function applySessionDeepLink(requestedThreadId: string): Promise<void> {
		const requestedSessionId = ($page.url.searchParams.get('session') || '').trim();
		if (!requestedSessionId) return;

		const state = get(chatStore);
		if (requestedSessionId === state.activeSessionId && !state.viewingArchivedId) return;
		if (requestedSessionId === state.viewingArchivedId) return;

		let session = state.sessions.find((candidate) => candidate.id === requestedSessionId) ?? null;
		if (!session) {
			await chatStore.loadSessions(threadId ? requestedThreadId : null);
			session =
				get(chatStore).sessions.find((candidate) => candidate.id === requestedSessionId) ?? null;
		}
		if (!session || normalizeThreadId(session.ui_thread_id ?? requestedThreadId) !== requestedThreadId) {
			return;
		}

		await chatStore.openSession(requestedSessionId);
	}

	$: if (browser && mounted && lastScopeKey && currentScopeKey !== lastScopeKey) {
		lastScopeKey = currentScopeKey;
		resetChatPageLocalState();
		void reloadChatScope();
	}

	onMount(async () => {
		if (!browser) return;
		mounted = true;
		// The Do-mode permission posture is stored per principal+workspace on the
		// backend, not in this browser, so ask for it. Until it answers the
		// composer shows `Ask`, which is the posture that still prompts.
		void planModeStore.hydrate();
		// One clock for both drawers. The verdict's durations and its stall
		// threshold are read against it, so a panel whose clock never moved would
		// report `stalled` a minute late and then never again. Started here rather
		// than when a drawer opens, the same way every other task surface does it:
		// one timer for the page's life is cheaper than the state that would tell
		// two drawers apart.
		panelNow = Date.now();
		panelClockHandle = setInterval(() => (panelNow = Date.now()), PANEL_CLOCK_INTERVAL_MS);
		lastScopeKey = `${get(scopeIdentityStore).principal}:${get(scopeIdentityStore).workspace}`;
		void chatProfileStore.load();
		void fetchEngineAvailability().then((roster) => {
			engineRoster = roster;
			chatHarnessPreferenceStore.reconcileWithRoster(roster);
		}).catch(() => { /* Keep Magician available while offline. */ });
		void codingProfileStore.load();
		await reloadChatScope();
		await tick();
		await stageResurfacingContextFromRoute();

		// If landed with ?q= param (from landing page), auto-send the message
		const queryParam = $page.url.searchParams.get('q');
		if (queryParam) {
			// Clear the query param from the URL so it doesn't re-send on refresh
			const url = new URL(window.location.href);
			url.searchParams.delete('q');
			window.history.replaceState({}, '', url.toString());

			const sessionId = activeSessionId;
			if (sessionId) {
				await chatStore.sendMessageStreaming(sessionId, queryParam, selectedProfile, [], harnessSendOptions(sessionId));
			}
		}
	});

	$: if (
		browser
		&& mounted
		&& targetThreadId
		&& targetThreadId !== loadedTargetThreadId
		&& reloadingTargetThreadId !== targetThreadId
		&& !viewingArchivedId
	) {
		void reloadChatScope();
	}

	/* ── the two task panels ──────────────────────────────────────────────
	   Both drawers read from here. The task drawer refreshes its task record and
	   selected run on one poll, while outputs reload only when the task or its
	   terminal state changes. */

	// The task panel's "as of" line. Off the store rather than any one call
	// site, because the tasks behind it are replaced by the store's own poll too.
	$: $taskStore.tasks, (panelLastLoadedAt = Date.now());

	const panelOutputsGuard = { key: '' };
	$: {
		const taskId = browser && taskPanelOpen ? (taskPanelTask?.id ?? null) : null;
		const key = taskId === null ? '' : `${taskId}:${taskPanelTask?.status ?? ''}`;
		if (key !== panelOutputsGuard.key) {
			panelOutputsGuard.key = key;
			void loadPanelOutputs(taskId);
		}
	}
	$: taskPanelPollTarget =
		browser && taskPanelOpen && taskPanelTask
			? {
					taskId: taskPanelTask.id,
					executionId:
						panelRunSelectionTaskId === taskPanelTask.id ? panelRunSelectionId : null
				}
			: null;
	$: taskPanelPoll.aim(
		taskPanelPollTarget,
		panelPollCadence(taskPanelTask?.status ?? null)
	);
	$: if (browser) void loadInspection(inspectPanelOpen ? inspectPanelTarget : null);
	// Beside the fetch, on the same key, so the drawer's one read path opens and
	// closes as one thing.
	$: if (browser) syncInspectionStream(inspectPanelOpen ? inspectPanelTarget : null);

	/**
	 * The task drawer's whole input, through the adapter every task surface
	 * shares. The two id checks are what keep an output list or an ask belonging
	 * to the task the reader just left from rendering under this one.
	 */
	$: taskPanelModel =
		taskPanelTask === null
			? null
			: toTaskPanelModel(
					taskPanelTask,
					panelOutputsTaskId === taskPanelTask.id ? panelOutputs : null,
					panelRunStateTaskId === taskPanelTask.id ? panelRunState : null,
					// The same id check, third time: a run id is meaningless against
					// another task.
					panelRunSelectionTaskId === taskPanelTask.id ? panelRunSelectionId : null,
					(path) =>
						taskOutputUrl(
							taskPanelTask!.id,
							path,
							$scopeIdentityStore.principal,
							$scopeIdentityStore.workspace
						)
				);

	/**
	 * The run drawer's input, through the execution adapter. Same shape of
	 * guard, keyed on the run: a state or a file list read for another execution
	 * of the same task is exactly the mix-up nothing downstream could catch.
	 */
	$: inspectPanelModel =
		inspectPanelTarget === null || inspectState === null
			|| inspectStateKey !== inspectionKey(inspectPanelTarget)
			? null
			: toExecutionPanelModel(
					inspectState,
					inspectOutputsKey === inspectionKey(inspectPanelTarget) ? inspectOutputs : null
				);

	/** The run drawer's title. The run's own name, falling back to its id. */
	$: inspectPanelTitle =
		inspectState?.overview?.title?.trim()
		|| (inspectPanelTarget ? `Run ${inspectPanelTarget.executionId}` : null);

	/**
	 * Whether Escape is a drawer's to take. The plan sheet stacks over both, and
	 * the innermost thing wins — the window handler above already closes the
	 * sheet, so the drawers must not also close underneath it.
	 */
	$: escapeBelongsToPanel = !isPlanSheetOpen;

</script>

<svelte:window
	on:keydown={(event) => {
		if (event.key === 'Escape' && isPlanSheetOpen) {
			closePlanSheet();
		}
	}}
/>

<svelte:head>
	<title>{pageTitle}</title>
</svelte:head>

<div
	class="presto-gaui-page chat-page chat-page-v5"
	class:chat-page--workbench={currentThreadDisplayMode === 'dev' && !!currentThreadId}
	class:chat-page--embedded={embedded}
>
	<!-- v5 surfaces sessions through the global HistoryDrawer + the ContextPill,
	     which now docks into the composer's chrome row rather than floating over
	     the transcript. See the `dock` slot on <FloatingComposer /> below. -->
	<!-- Main Chat Area -->
	<main class="chat-main">
		<!-- Read-only banner for archived executions -->
		{#if isReadOnly}
			<div class="chat-readonly-banner">
				<span>Viewing archived execution (read-only)</span>
				<Button label="Return to active" size="sm" variant="outline" on:click={() => void handleReturnToActive()} />
			</div>
		{/if}

		<!-- Voice call panel is now a thin one-row pill rendered just
		     above the composer (further down in this template) rather
		     than a big center-stage block at the top. See the
		     `<DesktopVoiceCenterStage />` mount near the composer. -->


		<!-- Messages Area.
		     Live region: `role="log"` + polite/additions so screen readers
		     announce messages as they land, without re-reading the list.
		     The streaming bubble is NOT a separate container (it renders
		     inside the same keyed each-block), so it carries `aria-busy`
		     while tokens stream — AT defers that subtree until the final
		     message replaces it (new node ⇒ announced once, as an
		     addition), instead of narrating every token burst. -->
		<div
			class="chat-messages-area"
			class:chat-messages-area--empty={visibleMessages.length === 0 && !isLoading && currentPlannerDockTasks.length === 0}
			role="log"
			aria-live="polite"
			aria-relevant="additions"
			bind:this={messagesContainerEl}
			on:scroll={handleChatScroll}
		>
			{#if $chatStore.isLoadingOlder}
				<div style="text-align: center; padding: 0.5rem;">
					<span style="font-size: 0.75rem; color: var(--text-muted);">Loading older messages...</span>
				</div>
			{:else if $chatStore.hasMoreMessages && visibleMessages.length > 0}
				<button class="load-older-btn" on:click={() => void drainOlderMessages()} type="button">
					Load earlier messages
				</button>
			{/if}
			{#if visibleMessages.length === 0 && !isLoading && currentPlannerDockTasks.length === 0}
				<!-- Capability launcher: greeting + suggested-prompt chips +
				     recent sessions. Chips only FILL the composer (no
				     auto-send), so the ?q= deep-link auto-send path in
				     onMount is untouched. -->
				<ChatEmptyState
					{isReadOnly}
					recentSessions={recentLauncherSessions}
					onSuggest={applySuggestedPrompt}
					onOpenSession={(sessionId) => void handleViewExecution(sessionId)}
				/>
			{:else}
				{#each visibleMessages.filter((m) => !attachedActivityIds.has(m.message.id)) as visibleMessage (visibleMessage.id)}
					{@const message = visibleMessage.message}
					{@const spokenText = messageSpeechText(message)}
					<div
						class="chat-message-row"
                        class:chat-message-row--forwarded={!!originalAnswerHref(message)}
                        data-linked-answer={visibleMessage.messageIds.includes(focusedAnswerId ?? '')}
						class:chat-message-row--own={message.direction === 'user'}
						aria-busy={message.id.startsWith('streaming-') ? true : undefined}
						animate:flip={{
							/* Svelte's FLIP fires on every bounding-rect change of a
							   keyed each-block item — INCLUDING height growth, not
							   just reorder. With a 260ms duration and streaming
							   tokens arriving every 5–10ms on a fast streaming model,
							   25+ flip animations queue up in flight per second,
							   each interpolating `transform: translate()` from the
							   old bounding-rect to the new one. The visible effect
							   is the streaming bubble appearing to "collapse to 0
							   and re-expand" on every token because flip's transform
							   animation runs from a non-zero delta back to zero
							   continuously. Fix: drop the duration to 0 on the
							   streaming bubble so its height grows naturally with
							   no transform interpolation. Completed messages keep
							   the original 260ms duration so add/remove/reorder
							   transitions still feel polished. */
							duration: message.id.startsWith('streaming-') ? 0 : 260,
						}}
						in:fadeUp={{ y: 10, duration: 220 }}
					>
					{#if !message.id.startsWith('streaming-')}
						<button
							type="button"
							class="chat-message-delete"
							class:chat-message-delete--own={message.direction === 'user'}
							aria-label="Delete message"
							title="Delete message"
							on:click={() => void handleDeleteChatMessage(visibleMessage.messageIds)}
						>
							<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/></svg>
						</button>
					{/if}
						{#if message.content.type === 'tool_call_executed'}
							{@const structuredResponse = resolveStructuredResponseForMessage(message, 'tool_call_executed')}
							{#if structuredResponse}
								<div class="chat-action-card-wrap">
									<div class="chat-action-card chat-structured-response">
								<StructuredResponseRenderer
									response={structuredResponse}
									sessionId={message.session_id}
									onAction={(action) => void handleStructuredResponseAction(action, message.session_id)}
								/>
									</div>
								</div>
							{:else}
								<!-- Tool call executed: system-style completion card -->
								<div class="chat-action-card-wrap">
									<div class="chat-executed-alert">
										<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>
										<div class="chat-executed-content">
											<span class="chat-executed-label">Action completed: {message.content.tool_name}</span>
											{#if message.content.summary}
												<div class="chat-executed-summary">
													<ChatMarkdown content={message.content.summary} sessionId={message.session_id} />
												</div>
											{/if}
										</div>
									</div>
								</div>
							{/if}
						{:else if message.content.type === 'rich_tool_result'}
							{@const structuredResponse = resolveStructuredResponseForMessage(message, 'rich_tool_result')}
							{#if structuredResponse}
								<div class="chat-action-card-wrap">
									<div class="chat-action-card chat-structured-response">
								<StructuredResponseRenderer
									response={structuredResponse}
									sessionId={message.session_id}
									onAction={(action) => void handleStructuredResponseAction(action, message.session_id)}
								/>
									</div>
								</div>
							{:else}
								<div class="chat-action-card-wrap">
									<div class="chat-rich-result-card">
										<div class="chat-rich-result-header">
											<span class="chat-rich-result-label">Action completed: {message.content.tool_name}</span>
											{#if message.content.summary}
												<div class="chat-rich-result-summary">
													<ChatMarkdown content={message.content.summary} sessionId={message.session_id} />
												</div>
											{/if}
										</div>
										<ChatContentBlocks
											sessionId={message.session_id}
											blocks={getMessageContentBlocks(message.content)}
										/>
									</div>
								</div>
							{/if}
						{:else if message.content.type === 'task_status_update'}
							{@const structuredResponse = resolveStructuredResponseForTaskStatusUpdate(message)}
							{#if structuredResponse}
								<div class="chat-action-card-wrap">
									<div class="chat-action-card chat-structured-response">
										<StructuredResponseRenderer
											response={structuredResponse}
											sessionId={message.session_id}
											onAction={(action) => void handleStructuredResponseAction(action, message.session_id)}
										/>
									</div>
								</div>
							{:else}
								<div class="chat-action-card-wrap">
									<TaskStatusCard
										{message}
										taskExecutionGroups={visibleMessage.taskExecutionGroups}
										{runExpansionOverrides}
										{activeSessionId}
										{tailedTaskId}
										onToggleRun={toggleRunExpanded}
										onOpenTask={openTaskDetailsFromStatus}
										onInspectFromIds={openInspectionPanelFromIds}
										onWatchLive={watchLive}
										onStopWatching={stopWatching}
									/>
								</div>
							{/if}
						{:else if message.content.type === 'escalation'}
						<!-- Escalation: action-required card with buttons -->
						{@const escalationActionKey = message.content.pause_state_id ?? message.content.request_id ?? message.content.execution_id ?? message.id}
						<div class="chat-action-card-wrap">
							<EscalationCard
								{message}
								statusLabel={escalationStatusLabel(message.content)}
								dimmed={shouldDimEscalation(message.content)}
								showActions={shouldShowEscalationActions(message.content)}
								inactiveReason={escalationInactiveReason(message.content)}
								disabled={isReadOnly || respondingEscalationKeys.has(escalationActionKey)}
								onRespond={(executionId, pauseStateId, option, escalationType, requestId, inputType) =>
									handleEscalationResponse(
										executionId,
										pauseStateId,
										option,
										escalationType,
										requestId,
										inputType,
										message.content.question,
										message.content.options
									)}
							/>
						</div>
						{:else if message.content.type === 'escalation_resolved'}
							{@const structuredResponse = resolveStructuredResponseForMessage(message, 'escalation_resolved')}
							{#if structuredResponse}
								<div class="chat-action-card-wrap">
									<div class="chat-action-card chat-structured-response">
								<StructuredResponseRenderer
									response={structuredResponse}
									sessionId={message.session_id}
									onAction={(action) => void handleStructuredResponseAction(action, message.session_id)}
								/>
									</div>
								</div>
							{:else}
								<div class="chat-action-card-wrap">
									<EscalationCard {message} />
								</div>
							{/if}
						{:else if message.content.type === 'attachment'}
							{@const structuredResponse = resolveStructuredResponseForMessage(message, 'attachment')}
							{#if structuredResponse}
								<div class="chat-action-card-wrap">
									<div class="chat-action-card chat-structured-response">
									<StructuredResponseRenderer
										response={structuredResponse}
										sessionId={message.session_id}
										onAction={(action) => void handleStructuredResponseAction(action, message.session_id)}
									/>
									</div>
								</div>
							{:else}
								<AttachmentBubble {message} timeLabel={formatTimestamp(message.created_at)} />
							{/if}
						{:else if message.direction === 'system'}
						<SystemPill {message} />
					{:else if message.direction === 'assistant' && !getMessageText(message.content).trim() && visibleMessage.attachedActivityMessages.length === 0 && visibleMessage.taskExecutionGroups.length === 0}
						<!-- Empty assistant turn with no folded activity — skip the
						     empty bubble. Two cases:
						     (1) a streaming placeholder before the first token
						         (`<ChatTurnProgress />` below renders the "Working…"
						         indicator, so a typing-dots bubble would be a
						         redundant second in-flight state); and
						     (2) a FINAL turn that produced only a tool call — a voice
						         turn where the model "responded" by acting, with no
						         spoken text — which otherwise renders as a bare empty
						         bubble.
						     A turn that DID fold a task card (attachedActivityMessages
						     / taskExecutionGroups non-empty) falls through to the
						     normal branch so the card still renders; and once a
						     streaming turn's first token arrives it no longer matches
						     and renders normally. -->
					{:else}
						{@const assistantTurnId =
							message.direction === 'assistant' && !message.id.startsWith('streaming-')
								? getChatTurnIdForMessage(message, $chatTurnIdByMessageIdStore)
								: null}
						<div class="chat chat-{message.direction === 'user' ? 'end' : 'start'}">
							<div class="chat-header">
								{message.direction === 'user' ? 'You' : 'Assistant'}
                                {#if originalAnswerHref(message)}
                                    <a class="chat-original-link" href={originalAnswerHref(message)} title="Open the original answer in its conversation">
                                        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M10 13a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-2 2M14 11a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l2-2"/></svg>
                                        View original answer
                                    </a>
                                {/if}
								{#if message.voice_origin === true}
									<span class="chat-voice-origin" title={voiceSourceLabel(message)}>
										<svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
											<path
												d="M8 1.5a2 2 0 0 0-2 2v4.5a2 2 0 1 0 4 0V3.5a2 2 0 0 0-2-2z M4.5 7.5v1a3.5 3.5 0 0 0 7 0v-1 M8 12v2 M5.5 14h5"
												fill="none"
												stroke="currentColor"
												stroke-width="1.4"
												stroke-linejoin="round"
												stroke-linecap="round"
											/>
										</svg>
									</span>
								{/if}
								<time class="chat-msg-time">{formatTimestamp(message.created_at)}</time>
								{#if spokenText.trim()}
									<span class="chat-header-speak">
										<SpeakButton
											messageId={message.id}
											text={spokenText}
											speechSegments={message.speech_segments}
											compact
										/>
									</span>
								{/if}
							</div>
							<div
								class="chat-bubble chat-bubble-{message.direction === 'user' ? 'primary' : 'neutral'}"
								class:chat-bubble--streaming={message.id.startsWith('streaming-')}
							>
								{#if message.direction === 'user' && message.content.type === 'text' && message.content.plan_reply}
									<div class="chat-plan-reply-context">
										<span>Planning answer · {message.content.plan_reply.task_title}</span>
										<p>{message.content.plan_reply.question_text}</p>
									</div>
								{/if}
								<!-- Streaming placeholder bubbles used to render a
								     `<span class="chat-typing-dots">` here when text
								     was empty. That stacked on top of `<ChatTurnProgress />`
								     (mounted below), giving the user TWO in-flight
								     indicators for the same state. The empty-placeholder
								     branch is now skipped at the outer {:else if} guard
								     a few lines up — by the time control reaches here,
								     either there's real streamed text or this is a final
								     assistant message worth rendering.
								     The `chat-bubble--streaming` class on the parent
								     bubble pins the bubble width (via the rule near
								     the bottom of the style block) so the daisyUI
								     `fit-content` default doesn't shrink-wrap on
								     every token — that's what made the bubble feel
								     like it was reloading + re-laying-out per token.
								     The markdown parser runs every token; with
								     bubble width pinned, only height changes as new
								     text wraps inside the stable-width bubble.
								     The body and header playback control render only when
								     there IS text, so a turn that produced only a tool call (a
								     voice turn where the model "responded" by acting,
								     no spoken text) doesn't leave an empty text area —
								     any folded activity below still renders. -->
										{#if message.direction === 'assistant'}
											{@const structuredTextResponse = resolveStructuredResponseForText(message)}
											{#if structuredTextResponse}
												<StructuredResponseRenderer
													response={structuredTextResponse}
													sessionId={message.session_id}
													onAction={(action) => void handleStructuredResponseAction(action, message.session_id)}
												/>
											{:else if getMessageText(message.content).trim()}
												<ChatMarkdown
													content={getMessageText(message.content)}
													sessionId={message.session_id}
												/>
											{/if}
										{:else if getMessageText(message.content).trim()}
											<ChatMarkdown
												content={getMessageText(message.content)}
												sessionId={message.session_id}
												/>
										{/if}
								{#if assistantTurnId}
									<!-- Activity for this turn rides *inside*
									     the assistant bubble so it reads as
									     "this response is still working / has
									     finished working". Replaces the old
									     `<AssistantActivitySection />` "What
									     happened" dropdown and the previous
									     separate-row placement. The card's
									     border-top flips info → success when
									     the request completes (see embedded
									     styling). Works for both live SSE turns
									     and historical REST-loaded turns. -->
									<RequestActivityCard
										sessionId={message.session_id}
										chatTurnId={assistantTurnId}
										live={assistantTurnId === liveTurnId}
										embedded={true}
										on:inspect={(e) => openInspectionPanelFromIds(e.detail)}
									/>
								{/if}
							</div>
						</div>
						{#if message.direction === 'user'}
							{@const messageTurnId = getChatTurnIdForMessage(message, $chatTurnIdByMessageIdStore)}
							{#if messageTurnId && !chatTurnsWithResponse.has(messageTurnId) && lastUserMessageIdByTurnId.get(messageTurnId) === message.id && (messageTurnId === liveTurnId || !!$pendingHitlByChatTurnId.get(messageTurnId))}
								{@const turnIsLive = messageTurnId === liveTurnId}
								<!-- No assistant response yet for this turn →
								     render a "typing" assistant bubble with
								     the activity stream embedded inside.
								     Visually reads as "the assistant is
								     responding, here's what they're doing".
								     Once the actual assistant message lands
								     with the same chat_turn_id, the guard
								     above (`chatTurnsWithResponse`) flips
								     true and this bubble unmounts — the
								     real assistant bubble (which embeds the
								     same activity card) takes over. One
								     card per turn, never duplicated. -->
								{@const pendingHitl = $pendingHitlByChatTurnId.get(messageTurnId)}
								<div class="chat chat-start chat-turn-typing">
									<div class="chat-header">Assistant</div>
									<div class="chat-bubble chat-bubble-neutral chat-turn-typing__bubble">
										{#if turnIsLive && pendingHitl}
											<!-- "Waiting on you" pill — flips from working
											     dots to a CTA the moment a HITL request
											     fires for this chat turn. Click opens the
											     same modal as /attention; the HITL is
											     handled in one canonical place. -->
											<button
												type="button"
												class="chat-turn-typing__waiting"
												on:click={() => openHitlForTurn(pendingHitl)}
												title="Respond to this request"
											>
												<span class="chat-turn-typing__waiting-dot" aria-hidden="true"></span>
												<span>Waiting on you — click to respond</span>
											</button>
										{:else if turnIsLive}
											<span class="chat-typing-dots" aria-label="Working">
												<span></span><span></span><span></span>
											</span>
										{/if}
										<RequestActivityCard
											sessionId={message.session_id}
											chatTurnId={messageTurnId}
											live={turnIsLive}
											embedded={true}
											on:inspect={(e) => openInspectionPanelFromIds(e.detail)}
										/>
									</div>
								</div>
							{/if}
						{/if}
					{/if}
					</div>
				{/each}

			{/if}
			<div bind:this={messagesEndEl}></div>
		</div>

		<!-- Jump-to-latest pill. Surfaces only when the user has
		     scrolled up off the tail; clicking snaps them back to the
		     latest message and re-arms auto-scroll. Sits over the
		     scroll container's bottom-right via `position: absolute`
		     (chat-main is the positioning context), tucked above where
		     the composer overlays so it never overlaps the input. -->
		{#if !isAtBottom && visibleMessages.length > 0}
			<button
				type="button"
				class="chat-jump-to-latest"
				on:click={jumpToLatest}
				aria-label="Jump to latest message"
				title="Jump to latest"
			>
				<svg
					xmlns="http://www.w3.org/2000/svg"
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2.2"
					stroke-linecap="round"
					stroke-linejoin="round"
					aria-hidden="true"
				>
					<path d="M6 9l6 6 6-6" />
				</svg>
			</button>
		{/if}

		<!-- Always-mounted hidden file input so both legacy and v5 composers
		     can call openAttachmentPicker() — the function clicks attachmentInputEl. -->
		<input
			bind:this={attachmentInputEl}
			class="chat-attachment-input chat-attachment-input--hidden"
			type="file"
			multiple
			on:change={handleAttachmentSelection}
			aria-hidden="true"
			tabindex="-1"
		/>

			{#if currentThreadDisplayMode === 'dev' && currentThreadId}
				<div class="chat-dev-workbench-main">
					<!-- Visible exit: the pill's chat/dev toggle is gone, and a
					     thread stuck in dev mode (e.g. flipped via /devsessions)
					     must not depend on knowing the hidden keyboard chord. -->
					<div class="chat-dev-exit-row">
						<span class="chat-dev-exit-label">Developer Mode — this thread renders its workbench instead of chat.</span>
						<button
							type="button"
							class="chat-dev-exit-btn"
							on:click={() => {
								if (currentThreadId) {
									void threadStore.updateThread(currentThreadId, { display_mode: 'chat' });
								}
							}}
						>Exit Dev Mode</button>
					</div>
					<WorkbenchColumn
						threadId={currentThreadId}
						planMode={currentThreadPlanMode}
						placement="main"
						startedSession={devWorkbenchSession?.threadId === currentThreadId ? devWorkbenchSession : null}
						selectedSessionId={routeDevSessionId}
						focusNonce={devWorkbenchFocusNonce}
						on:sessionschange={handleDevWorkbenchSessionsChange}
						on:started={handleDevWorkbenchStarted}
						on:focus={focusDevWorkbench}
					/>
				</div>
			{/if}
			<!-- Pending-replay queue inspector — renders only when the
			     backend has queued messages for the active session.
			     Mounted above the composer so users see what they typed
			     during the in-flight turn AND can act on it (copy /
			     delete / clear-all / stop) before the drain runs. -->

			<!-- v5 shell — floating glass composer.
			     Dev mode hides it entirely: the user types directly into the
			     xterm pane and the launcher controls live in the workbench
			     header, so a composer would just be visual noise. -->
			{#if currentThreadDisplayMode !== 'dev'}
			{@const composerDisplayMode = currentThreadDisplayMode as string}
			{#if currentPlannerDockTasks.length > 0}
				<PlannerDock
					tasks={currentPlannerDockTasks}
					activeReplyTaskId={planReplyIntent?.taskId ?? null}
					activeReplyQuestionId={planReplyIntent?.questionId ?? null}
					disabled={isReadOnly || isSendingMessage}
					isActionBusy={inlinePlanActionBusy}
					onAction={handleInlinePlanAction}
					onReplyToQuestion={focusComposerForPlanTask}
					onOpenThread={(taskId) => void goto(buildCurrentThreadTaskHref(taskId))}
				/>
			{/if}
			<FloatingComposer
				bind:this={composerEl}
				bind:value={inputText}
				placeholder={
					isReadOnly
						? 'Viewing archived execution…'
						: composerDisplayMode === 'dev'
							? devWorkbenchActiveSessionId
								? 'Send input to active terminal'
								: 'Start CLI to begin'
						: planComposerMode === 'plan'
							? planReplyIntent
								? 'Type your planning answer…'
								: 'Describe what to plan…'
							: 'Message or describe a task'
				}
				mode={composerDisplayMode === 'dev' ? 'ask' : planComposerMode}
				permission={composerPermission}
				planReplyActive={Boolean(planReplyIntent)}
				isReadOnly={isReadOnly}
				disabled={textAdmissionInFlight || voiceGuidedAdmissionInFlight || voiceGuidedCaptureInFlight || (composerDisplayMode === 'dev' && !devWorkbenchActiveSessionId)}
				isSending={composerDisplayMode === 'dev' ? false : composerBusy}
				isUploading={isUploading}
				canSend={
					composerDisplayMode === 'dev'
						? !isReadOnly && !!devWorkbenchActiveSessionId && inputText.trim().length > 0
						: !isLoading && !isUploading && !isReadOnly && !planReplyIsStale && !(planComposerMode !== 'plan' && !activeSessionId) && (inputText.trim().length > 0 || stagedAttachments.length > 0)
				}
				hasContent={inputText.trim().length > 0 || stagedAttachments.length > 0}
				supportsAttachments={selectedProfileSupportsAttachments}
				engineName={titleCase(selectedHarnessEngine.replaceAll('_', ' '))}
				profileName={selectedHarnessEngine === 'magician' || selectedHarnessEngine === 'pi' ? selectedProfileObj?.name ?? 'Profile' : selectedHarnessModel === 'default' ? 'Default' : selectedHarnessModel}
				profileModel={selectedHarnessEngine === 'magician' || selectedHarnessEngine === 'pi' ? selectedProfileObj?.model ?? null : null}
				profileIsAdaptive={(selectedHarnessEngine === 'magician' || selectedHarnessEngine === 'pi') && (selectedProfileObj?.is_adaptive ?? false)}
				profileAdaptiveTier={selectedHarnessEngine === 'magician' || selectedHarnessEngine === 'pi' ? selectedProfileObj?.adaptive_tier ?? null : null}
				profileMenuOpen={profileDropdownOpen}
				thinkingModeActive={thinkingModeForActiveTurn}
				thinkingModeReason={thinkingModeReasonForActiveTurn}
				mentionItems={composerMentionItems}
				codingProfiles={$codingProfileStore.profiles}
				blockedCodingProfiles={$codingProfileStore.blocked}
				selectedCodingProfileId={$codingProfileStore.selected}
				variant={composerDisplayMode === 'dev' ? 'dev' : 'chat'}
				dock={composerDisplayMode !== 'dev'}
				maxWidth="var(--chat-col)"
                allowParallel={!!activeSessionId && stagedAttachments.length === 0 && planComposerMode !== 'plan' && !planReplyIntent}
                on:parallel={() => handleSend({ background: true })}
                on:stopAndSend={() => handleSend({ stopAndSend: true })}
				on:send={() => handleSend()}
				on:stop={() => void cancelInflightChatTurn()}
				on:setMode={(e) => handleComposerModeSelect(e.detail)}
				on:selectCodingProfile={(e) => codingProfileStore.select(e.detail.id)}
				on:attach={openAttachmentPicker}
				on:attachFiles={(e) => uploadFilesAsAttachments(e.detail.files)}
				on:cameraCapture={(e) => uploadFilesAsAttachments([e.detail.file])}
				on:micCapture={(e) => uploadFilesAsAttachments([e.detail.file])}
				on:micTranscribe={handleMicTranscribe}
				on:micTranscribeDelta={handleMicTranscribeDelta}
				on:input={handleVoiceAutoSendInputCancel}
				on:toggleProfile={() => void toggleProfileDropdown()}
			>
				<svelte:fragment slot="top">
                    {#if activeSessionId}<QueueInspector sessionId={activeSessionId} />{/if}
					<ConcurrentVoiceRequests />
                    {#if currentSession?.internal_voice?.kind === 'branch'}
                        <a class="concurrent-parent-link" href={`/t/${encodeURIComponent(currentSession.ui_thread_id)}/chat?session=${encodeURIComponent(currentSession.internal_voice.parent_session_id)}`}>
                            Concurrent · Started from original conversation ↗
                        </a>
                    {/if}
				</svelte:fragment>
				<svelte:fragment slot="dock">
					<ContextPill
						inline
						threadId={currentThreadId}
						sessionTitle={currentSession?.title ?? null}
						updatedAt={currentSession?.updated_at ?? null}
						isReadOnly={isReadOnly}
						canClear={!isReadOnly && visibleMessages.length > 0}
						canBuild={canBuildInVibe}
						on:open-history={handlePillOpenHistory}
						on:new-session={handlePillNewSession}
						on:build={buildInVibe}
						on:clear={handleClearAllMessages}
						on:archive={handlePillArchive}
						on:delete={handleDeleteSession}
						on:return-to-active={() => void handleReturnToActive()}
					/>
				</svelte:fragment>
				<svelte:fragment slot="dock-actions">
					<!-- Host-supplied chrome pinned right of the pill. The Tauri
					     HUD puts its theme + expand controls here; /chat leaves
					     it empty and the pill spans the full row. -->
					<slot name="composer-dock" />
				</svelte:fragment>
				<svelte:fragment slot="banner">
					{#if planReplyIntent}
						<PlanReplyBanner
							taskTitle={planReplyIntent.taskTitle}
							questionText={currentPlanReplyQuestion?.question ?? planReplyIntent.questionText}
							stale={planReplyIsStale}
							on:cancel={() => cancelPlanReply()}
						/>
					{/if}
					{#if voiceCallLive}
						<!-- Voice-call panel rendered INSIDE composer-shell
						     as a banner — becomes part of the composer's
						     chrome (top-aligned, same width, shares the
						     composer's rounded border). No positioning
						     math; flex inside composer-shell handles it. -->
						<DesktopVoiceCenterStage />
					{:else}
						<TrayLiveVoiceBanner />
					{/if}
					<VoiceAutoSendBanner
						remainingMs={voiceAutoSendRemainingMs}
						totalMs={VOICE_AUTO_SEND_MS}
						on:cancel={() => cancelVoiceAutoSend()}
					/>
				</svelte:fragment>
				<svelte:fragment slot="attachments">
					{#if stagedAttachments.length > 0}
						<div class="chat-staged-attachments chat-staged-attachments--v5">
						{#each stagedAttachments as attachment (attachment.attachment_id)}
							<button
								type="button"
								class="chat-staged-attachment"
								in:fly={{ y: 10, duration: 200 }}
								on:click={() => removeStagedAttachment(attachment.attachment_id)}
								aria-label={`Remove ${attachment.label || attachment.filename}`}
							>
									<span class="chat-staged-attachment-name">
										{attachment.label || attachment.filename}
									</span>
									<span class="chat-staged-attachment-meta">
										{attachment.mime_type}
										{#if formatBytes(attachment.size)}
											<span>&middot; {formatBytes(attachment.size)}</span>
										{/if}
									</span>
								</button>
							{/each}
						</div>
					{/if}
				</svelte:fragment>
				<svelte:fragment slot="warnings">
					{#if chatProfileWarnings.length > 0}
						<div class="chat-profile-warning chat-profile-warning--v5" role="status">
							{#each chatProfileWarnings as warning}
								<div>{warning.message}</div>
							{/each}
						</div>
					{/if}
					{#if !selectedProfileSupportsAttachments}
						<div class="chat-profile-warning chat-profile-warning--v5" role="status">
							Attachments are disabled for this profile.
						</div>
					{/if}
				</svelte:fragment>
			</FloatingComposer>
			{/if}

			{#if currentThreadDisplayMode !== 'dev' && profileDropdownOpen}
				<!-- Scrim: a real (untabbable) button so pointer-close needs no
				     a11y suppressions; keyboard users close via Escape on the
				     picker, which also refocuses the composer trigger chip. -->
				<button
					type="button"
					class="chat-profile-backdrop chat-profile-backdrop--v5"
					tabindex="-1"
					aria-label="Close profile picker"
					on:click={() => closeProfileDropdown(false)}
				></button>
				<div
					class="chat-profile-dropdown chat-profile-dropdown--v5"
					role="dialog"
					aria-label="Chat engine and profile or model"
					tabindex="-1"
					bind:this={profileDropdownEl}
					on:keydown={handleProfileListboxKeydown}
				>
					<div class="chat-profile-group-label">Engine</div>
					{#each engineRoster.engines.filter(engine => engine.installed) as engine (engine.name)}
						<button type="button" class="chat-profile-option" class:selected={engine.name === selectedHarnessEngine}
							aria-pressed={engine.name === selectedHarnessEngine}
							on:click={() => selectHarnessEngine(engine.name)}>
							<span class="chat-profile-option-name">{titleCase(engine.name.replaceAll('_', ' '))}</span>
						</button>
					{/each}
					<div class="chat-profile-group-divider"></div>
					{#if selectedHarnessEngine === 'magician' || selectedHarnessEngine === 'pi'}
						<div class="chat-profile-group-label">API profile</div>
					{#if chatProfiles.some(p => p.is_adaptive)}
						<div class="chat-profile-group-label">Adaptive</div>
						{#each chatProfiles.filter(p => p.is_adaptive) as profile (profile.name)}
							<button
								class="chat-profile-option adaptive"
								class:selected={profile.name === selectedProfile}
								aria-pressed={profile.name === selectedProfile}
								on:click={() => { chatProfileStore.select(profile.name); closeProfileDropdown(true); }}
								type="button"
							>
								<span class="chat-profile-option-name">
									{profile.name}{profile.is_default ? ' \u2605' : ''}
									<span class="chat-profile-adaptive-badge inline">Adaptive</span>
								</span>
								<span class="chat-profile-option-model">{profile.model}</span>
								{#if profile.adaptive_description}
									<span class="chat-profile-option-desc">{profile.adaptive_description}</span>
								{/if}
							</button>
						{/each}
						<div class="chat-profile-group-divider"></div>
						<div class="chat-profile-group-label">Standard</div>
					{/if}
					{#each chatProfiles.filter(p => !p.is_adaptive) as profile (profile.name)}
						<button
							class="chat-profile-option"
							class:selected={profile.name === selectedProfile}
							aria-pressed={profile.name === selectedProfile}
								on:click={() => { chatProfileStore.select(profile.name); closeProfileDropdown(true); }}
							type="button"
						>
							<span class="chat-profile-option-name">{profile.name}{profile.is_default ? ' \u2605' : ''}</span>
							<span class="chat-profile-option-model">{profile.model}</span>
						</button>
					{/each}
					{:else}
						<div class="chat-profile-group-label">Harness model</div>
						{#each engineRoster.engines.find(engine => engine.name === selectedHarnessEngine)?.models ?? ['default'] as model (model)}
							<button type="button" class="chat-profile-option" class:selected={model === selectedHarnessModel}
								aria-pressed={model === selectedHarnessModel}
								on:click={() => selectHarnessModel(model)}>
								<span class="chat-profile-option-name">{model}</span>
							</button>
						{/each}
					{/if}
				</div>
			{/if}
	</main>
</div>

{#if isPlanSheetOpen && planSheetTaskId}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="thread-plan-sheet-backdrop" on:click={closePlanSheet}>
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<!-- focusTrap: initial focus lands on the first focusable (the
		     header Close button), Tab/Shift+Tab cycle inside the sheet,
		     and on close focus returns to the invoker. Escape-close stays
		     with the svelte:window handler — the trap only touches Tab. -->
		<div
			class="thread-plan-sheet"
			role="dialog"
			aria-modal="true"
			aria-label="Task plan inspector"
			tabindex="-1"
			use:focusTrap
			on:click|stopPropagation
		>
			<header class="thread-plan-sheet__header">
				<div class="thread-plan-sheet__copy">
					<span class="thread-plan-sheet__eyebrow">Plan Inspector</span>
					<h2>{planSheetTask?.title || 'Task plan'}</h2>
					<p>
						{#if planSheetTask?.planStatus}
							{titleCase(planSheetTask.planStatus)} plan for #{currentThreadId}.
						{:else}
							Graph, waterfall, history, and persisted task plan for #{currentThreadId}.
						{/if}
					</p>
				</div>
				<Button label="Close" variant="outline" size="sm" on:click={closePlanSheet} />
			</header>
			<div class="thread-plan-sheet__body">
				<ExecutionPlanInspector taskId={planSheetTaskId} modal={true} />
			</div>
		</div>
	</div>
{/if}

<!--
	The run an activity card asked to inspect, in the shared drawer. The chrome —
	scrim, dialog role, focus capture and restore, Escape, loading skeleton,
	header — is `TaskPanelDrawer`'s and is not restated here.

	It carries **no action slot**: this gesture inspects a run, and every verb
	that could act on it belongs to the task the run is under, which the card's
	own Stop control and the task drawer below already offer. A second Stop here
	would be a second answer to one question.
-->
{#if inspectPanelOpen && inspectPanelTarget}
	<TaskPanelDrawer
		task={inspectPanelModel}
		title={inspectPanelTitle}
		loadError={inspectLoadError}
		lastLoadedAt={panelLastLoadedAt}
		now={panelNow}
		outputActions={true}
		closeOnEscape={escapeBelongsToPanel}
		on:close={closeInspectionPanel}
		on:openFile={handleInspectionOpenFile}
		on:revealFile={handleInspectionRevealFile}
		on:retry={handleInspectionRetry}
	/>
{/if}

<!--
	The chat-owned task panel: a durable task reference, a task-status card's
	*Inspect run*, and the PlannerDock's execute all land here. The action ladder
	in the slot is the same three verbs every other task surface offers, routed
	through the handlers the retired panel's events used to reach.
-->
{#if taskPanelOpen && taskPanelTask}
	{@const drawerTask = taskPanelTask}
	<TaskPanelDrawer
		task={taskPanelModel}
		title={drawerTask.title}
		description={drawerTask.description ?? null}
		threadId={drawerTask.uiThreadId ?? 'general'}
		loadError={$taskStore.error ?? taskPanelLoadFailure}
		lastLoadedAt={panelLastLoadedAt}
		now={panelNow}
		outputActions={true}
		closeOnEscape={escapeBelongsToPanel}
		on:close={closeTaskPanel}
		on:openFile={handlePanelOpenFile}
		on:revealFile={handlePanelRevealFile}
		on:retry={handlePanelRetry}
		on:selectRun={handlePanelSelectRun}
	>
		<svelte:fragment slot="actions">
			{#each taskPanelActions(drawerTask) as action (action.label)}
				<button type="button" class="task-panel__action" on:click={action.run}>
					{action.label}
				</button>
			{/each}
			<ExportMenu taskId={drawerTask.id} />
		</svelte:fragment>
	</TaskPanelDrawer>
{/if}

<style>
    .concurrent-parent-link {
        display: flex;
        align-items: center;
        min-height: 30px;
        padding: 0 12px;
        border-bottom: 1px solid var(--border-soft);
        color: var(--text-secondary);
        font-size: 12px;
    }
    .concurrent-parent-link:first-child {
        border-radius: calc(var(--radius-lg, 14px) - 1px) calc(var(--radius-lg, 14px) - 1px) 0 0;
    }
    .concurrent-parent-link:hover, .concurrent-parent-link:focus-visible {
        background: var(--accent-primary-soft);
        color: var(--text-primary);
    }

    .chat-original-link { display: inline-flex; align-items: center; gap: 4px; margin-inline: 8px; color: var(--accent-primary); font-size: 11px; text-decoration: none; }
    .chat-original-link:hover { text-decoration: underline; }
    .chat-original-link:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; border-radius: 3px; }
    .chat-message-row--forwarded {
        --chat-reply-border: color-mix(in srgb, var(--accent-primary) 45%, transparent);
    }

	.chat-page {
		display: flex;
		height: 100%;
		min-height: 0;
		padding: 0;
		max-width: none;
		/* Transparent so the AtmosphereLayer (paper grid + stipple) shows
		   through. Chat bubbles bring their own --bg-card so readability is
		   preserved. Was painting var(--bg-base) solid which covered the
		   atmosphere across the whole route. */
		background: transparent;
		overflow: hidden;
	}

	.chat-page--embedded {
		flex: 1 1 auto;
		width: 100%;
		min-height: 0;
	}

	/* v5 shell — inherit the legacy `.chat-page` flex layout (display:flex
	   from the rule above) so `.chat-main` gets a proper height via flex:1
	   and its inner `.chat-messages-area` scroll container actually scrolls.
	   We only:
	     1. Center the chat column at a deliberately NARROW reading width.
	        Chat is the one intentional exception to the app-wide content
	        cap (--app-content-max: 1320px): a conversation reads better in
	        a tighter column, so we hold it at `--chat-col` — the single
	        width token shared by messages, composer, and context pill —
	        and auto-margin to centre.
	     2. Reserve bottom padding inside `.chat-messages-area` so the last
	        message clears the FloatingComposer. */
	.chat-page-v5 :global(.chat-main) {
		max-width: var(--chat-col);
		margin-left: auto;
		margin-right: auto;
	}

	/* Developer Mode: the terminal workbench occupies the chat canvas and the
	   composer becomes the workbench launcher. The transcript remains mounted
	   behind it so state and scroll positions survive mode switches. */
	.chat-page-v5.chat-page--workbench {
		/* Layout's .presto-gaui-page adds 5rem bottom padding; in workbench mode
		   the absolutely-positioned terminal needs the full viewport height so
		   the bottom anchor (composer + any legacy bottom offset) is the real floor. */
		padding-top: 0;
		padding-bottom: 0;
		max-width: none;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}

	.chat-page-v5.chat-page--workbench :global(.chat-main) {
		position: relative;
		flex: 1 1 auto;
		max-width: none;
		margin-left: 0;
		margin-right: 0;
		min-height: 0;
	}

	.chat-page-v5.chat-page--workbench :global(.chat-messages-area) {
		opacity: 0;
		pointer-events: none;
	}

	/* Workbench is anchored to the viewport so it fills the entire
	   visible area below the fixed ContextPill (top bar 48px + pill
	   ~32px + 16px breathing = ~104px) and above any legacy bottom offset.
	   Composer is hidden in dev mode so no need to reserve composer
	   height — only the legacy bottom offset + a 12px breathing strip.

	   Width:
	   - `width: 1320px` is the explicit target.
	   - `max-width: calc(100vw - 32px)` only kicks in on narrow
	     viewports (<1352px), shrinking the wrapper to fit while
	     keeping a 16px gutter on each side.
	   - `min-width: 0` lets descendants shrink instead of forcing
	     the wrapper wider than its computed width.

	   This is intentionally NOT `width: 100%` or `min()` — both
	   patterns can be pushed past `max-width` by descendants with
	   intrinsic min-content width (the launcher's cwd input +
	   button row) in some browser engines. A concrete `width:
	   1320px` is the source of truth that the cascade can't drift. */
	.chat-dev-workbench-main {
		position: fixed;
		top: 104px;
		bottom: calc(var(--attention-bar-offset, 0px) + 12px);
		left: 50%;
		transform: translateX(-50%);
		width: 1320px;
		max-width: calc(100vw - 32px);
		min-width: 0;
		box-sizing: border-box;
		z-index: 20;
		display: flex;
		/* Column: a slim exit bar above the workbench (the pill's chat/dev
		   toggle is gone; this is the visible way out of dev mode). */
		flex-direction: column;
		gap: 6px;
		min-height: 0;
	}

	.chat-dev-exit-row {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.6rem;
	}

	.chat-dev-exit-label {
		font-size: 0.75rem;
		color: var(--text-muted, #8f9799);
	}

	.chat-dev-exit-btn {
		flex: 0 0 auto;
		padding: 0.25rem 0.7rem;
		border-radius: 999px;
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		background: var(--bg-base, #fff);
		color: var(--text-primary, #1a1a1a);
		font-size: 0.75rem;
		font-weight: 600;
		cursor: pointer;
	}

	.chat-dev-exit-btn:hover {
		border-color: var(--accent-primary, #c2502a);
		color: var(--accent-primary, #c2502a);
	}

	.chat-dev-workbench-main > :global(*) {
		flex: 1 1 auto;
		min-width: 0;
		max-width: 100%;
	}

	@media (max-width: 720px) {
		.chat-dev-workbench-main {
			top: 96px;
			max-width: calc(100vw - 16px);
		}
	}

	.chat-page-v5 :global(.chat-messages-area) {
		/* Composer is a static flex child of chat-main (not fixed), so
		   no big bottom reserve is needed — the composer just sits
		   below the messages area in the flex column. Keep a tiny gap
		   for visual breathing and the AttentionBar offset. Width comes
		   from the base `.chat-messages-area` rule (`--chat-col`). */
		padding-top: 64px !important;
		padding-bottom: calc(var(--attention-bar-offset, 0px) + 8px) !important;
	}

	.chat-page-v5 :global(.composer-wrap) {
		margin-bottom: 8px;
	}

	.chat-attachment-input--hidden {
		position: absolute;
		left: -9999px;
		width: 1px;
		height: 1px;
		opacity: 0;
		pointer-events: none;
	}

	.chat-staged-attachments--v5 {
		padding: 8px 14px 0;
	}

	.chat-profile-warning--v5 {
		padding: 6px 14px 8px;
		font-size: var(--text-2xs);
		color: var(--text-muted, #888);
	}

	/* Keep the profile picker above floating shell chrome.
	   Also: the base `.chat-profile-dropdown` rule lives further down the
	   file with `position: absolute` + `bottom: calc(100% + 6px)`, which at
	   equal specificity wins by source order and shoves the dropdown off
	   the top of the viewport. Combined-class selectors (.chat-profile-dropdown.chat-profile-dropdown--v5)
	   bump specificity to (0,2,0) so v5 always wins. */
	.chat-profile-backdrop.chat-profile-backdrop--v5 {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.05);
		z-index: 935;
	}

	.chat-profile-dropdown.chat-profile-dropdown--v5 {
		position: fixed;
		bottom: calc(140px + var(--attention-bar-offset, 0px));
		left: 50%;
		transform: translateX(-50%);
		width: auto;
		min-width: 280px;
		max-width: 480px;
		max-height: 320px;
		overflow-y: auto;
		z-index: 940;
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: var(--radius-md, 10px);
		box-shadow: var(--shadow-lg, 0 12px 28px rgba(0, 0, 0, 0.18));
		padding: 6px;
	}

	/* ===== Main Chat Area ===== */
	.chat-main {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-width: 0;
		max-width: 1320px;
		overflow: hidden;
		/* Positioning context for the jump-to-latest pill that hovers
		   over the messages list when the user has scrolled up off the
		   tail. Without this the pill would escape to the nearest
		   positioned ancestor (often the viewport root) and float in
		   the wrong place. */
		position: relative;
	}

	.chat-jump-to-latest {
		position: absolute;
		/* Sit above the composer — the composer occupies ~9–11rem at
		   the bottom of `.chat-main` depending on attachments / mode
		   row. 6.5rem keeps the pill clear of the input on every
		   resting state without hugging the bottom of the messages
		   area. Tightened on the mobile breakpoint via the media
		   query below. */
		bottom: 6.5rem;
		right: 1.25rem;
		z-index: 12;
		width: 2.25rem;
		height: 2.25rem;
		border-radius: 50%;
		border: 1px solid var(--border-soft, #eee4dc);
		background: var(--bg-card, #ffffff);
		color: var(--text-primary, #2d3436);
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.12);
		transition: transform 120ms ease, box-shadow 120ms ease, background 120ms ease;
	}

	.chat-jump-to-latest:hover {
		transform: translateY(-1px);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.16);
		background: var(--bg-soft, #f6f1e8);
	}

	.chat-jump-to-latest:focus-visible {
		outline: 2px solid var(--accent-primary, #ff6b6b);
		outline-offset: 2px;
	}

	.chat-jump-to-latest svg {
		display: block;
	}

	@media (max-width: 768px) {
		.chat-jump-to-latest {
			bottom: 5rem;
			right: 0.75rem;
		}
	}

	.chat-readonly-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.45rem 1rem;
		background: var(--bg-soft, #f6f1e8);
		border-bottom: 1px solid var(--border-soft, #eee4dc);
		font-size: var(--text-xs);
		color: var(--text-secondary, #5f6668);
	}

	/* ===== Messages Area ===== */
	.chat-messages-area {
		flex: 1;
		overflow-y: auto;
		padding: 1.25rem 1.5rem;
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		max-width: var(--chat-col);
		width: 100%;
		margin: 0 auto;
		/* Token-stream scroll pinning needs scrollTop writes to be
		   instant. The global `html { scroll-behavior: smooth }` in
		   app.css would otherwise animate every `scrollTop = scrollHeight`
		   assignment, chaining one smooth animation per token burst —
		   the viewport never catches up and the message bubble visibly
		   "jumps" with each token (worst near ~150 tok/s). Setting
		   `scroll-behavior: auto`
		   here makes raw scrollTop writes snap. User-triggered
		   `scrollTo({ behavior: 'smooth' })` calls (jump-to-bottom button
		   etc.) still smooth-scroll because the per-call option
		   overrides the CSS. */
		scroll-behavior: auto;
	}

	/* When the chat is empty (no messages, no streaming, no inline plan
	   cards), suppress the scrollbar entirely. The empty-state launcher
	   uses flex:1 to fill the area; combined with the bottom padding that
	   reserves space for the floating composer, the layout can spill by a
	   few sub-pixels on some viewport sizes and `overflow-y: auto` would
	   render a scrollbar with nothing to scroll. On short/narrow
	   viewports the launcher (chips + recent sessions) can genuinely
	   exceed the area, so scrolling comes back there — clipping content
	   is worse than a scrollbar. */
	.chat-messages-area--empty {
		overflow-y: hidden;
	}

	@media (max-height: 700px), (max-width: 640px) {
		.chat-messages-area--empty {
			overflow-y: auto;
		}
	}

	.load-older-btn {
		display: block;
		margin: 0 auto 0.5rem;
		padding: 0.3rem 0.8rem;
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: var(--radius-full, 9999px);
		background: var(--bg-card, #ffffff);
		color: var(--text-muted, #8f9799);
		font-family: var(--font-primary);
		font-size: var(--text-2xs);
		font-weight: 600;
		cursor: pointer;
		transition: all 0.15s;
	}

	.load-older-btn:hover {
		border-color: var(--accent-primary, #ff6b6b);
		color: var(--text-primary, #2d3436);
	}

	.chat-message-row {
		position: relative;
		display: block;
	}

	/* Typing-bubble shell for an in-flight (or response-less) turn.
	 * Embeds the activity card inside an assistant-style chat bubble
	 * with optional typing dots, so the UI reads as "the assistant
	 * is responding, here's what they're doing" instead of a floating
	 * card under the user message. Caps width to keep the embedded
	 * activity strip readable on wide viewports. */
	.chat-turn-typing__bubble {
		max-width: min(640px, 100%);
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	/* Typing dots, duplicated from ChatBubble.svelte's scoped styles
	 * because Svelte's CSS scoping means each file needs its own
	 * `.chat-typing-dots` rules to actually animate. The mark-up is
	 * `<span class="chat-typing-dots"><span></span>×3</span>`. */
	.chat-turn-typing__bubble .chat-typing-dots {
		display: inline-flex;
		align-items: center;
		gap: 0.22rem;
		min-width: 1.45rem;
		min-height: 0.8rem;
	}

	.chat-turn-typing__bubble .chat-typing-dots span {
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--text-muted, #8a847a);
		animation: chat-turn-typing-bounce 1.2s infinite;
	}

	.chat-turn-typing__bubble .chat-typing-dots span:nth-child(2) {
		animation-delay: 0.15s;
	}

	.chat-turn-typing__bubble .chat-typing-dots span:nth-child(3) {
		animation-delay: 0.3s;
	}

	@keyframes chat-turn-typing-bounce {
		0%, 60%, 100% { opacity: 0.35; transform: translateY(0); }
		30% { opacity: 1; transform: translateY(-2px); }
	}

	/* "Waiting on you" pill — flips from the dots when a HITL request
	 * lands for the in-flight chat turn. Themed against
	 * `--accent-primary` so it picks up the active theme's primary
	 * tint (same token as the user-bubble background — visually
	 * reads as "your turn"). Pulsing dot draws the eye without the
	 * full bouncing-dots animation cost. */
	.chat-turn-typing__waiting {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.3rem 0.7rem;
		border-radius: 999px;
		background: var(--accent-primary-soft, color-mix(in srgb, var(--accent-primary) 14%, transparent));
		color: var(--accent-primary);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 35%, transparent);
		font-size: var(--text-xs);
		font-weight: 600;
		cursor: pointer;
		align-self: flex-start;
		line-height: 1;
		transition: background 120ms ease, transform 120ms ease;
	}

	.chat-turn-typing__waiting:hover {
		background: color-mix(in srgb, var(--accent-primary) 22%, transparent);
		transform: translateY(-1px);
	}

	.chat-turn-typing__waiting-dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
		background: var(--accent-primary);
		animation: chat-turn-waiting-pulse 1.6s ease-in-out infinite;
	}

	@keyframes chat-turn-waiting-pulse {
		0%, 100% { opacity: 0.4; transform: scale(0.85); }
		50% { opacity: 1; transform: scale(1.1); }
	}

	.chat-message-delete {
		position: absolute;
		top: 6px;
		right: 6px;
		width: 24px;
		height: 24px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		border: 1px solid transparent;
		border-radius: 6px;
		background: transparent;
		color: var(--text-muted, #8f9799);
		opacity: 0;
		z-index: 2;
		cursor: pointer;
		transition: opacity 120ms ease, background 120ms ease, color 120ms ease, border-color 120ms ease;
	}

	.chat-message-row--own .chat-message-delete {
		right: auto;
		left: 6px;
	}

	.chat-message-row:hover .chat-message-delete,
	.chat-message-row:focus-within .chat-message-delete,
	.chat-message-delete:focus-visible {
		opacity: 1;
	}

	/* Touch has no hover — keep the delete affordance discoverable at
	   reduced opacity (same pattern as the studio rail's hidden row
	   affordances). Hover/focus still lift to 1 via the rules above. */
	@media (pointer: coarse) {
		.chat-message-delete {
			opacity: 0.5;
		}
	}

	.chat-message-delete svg {
		width: 14px;
		height: 14px;
	}

	.chat-message-delete:hover,
	.chat-message-delete:focus-visible {
		color: var(--color-error, #d63031);
		background: var(--color-error-soft, rgba(214, 48, 49, 0.1));
		border-color: var(--color-error-soft, rgba(214, 48, 49, 0.2));
	}

	.chat-plan-reply-context {
		display: grid;
		gap: 0.18rem;
		margin-bottom: 0.55rem;
		padding-bottom: 0.5rem;
		border-bottom: 1px solid color-mix(in srgb, currentColor 30%, transparent);
	}

	.chat-plan-reply-context span {
		font-size: var(--text-2xs);
		font-weight: 700;
	}

	.chat-plan-reply-context p {
		margin: 0;
		font-size: var(--text-xs);
		line-height: 1.35;
		opacity: 0.86;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}
	/* DaisyUI v5 chat component customizations */
	.chat-page :global(.chat) {
		padding-top: 0.35rem;
		padding-bottom: 0.35rem;
	}

	.chat-page :global(.chat-header) {
		font-size: var(--text-2xs);
		font-weight: 600;
		color: var(--text-muted, #8f9799);
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.chat-msg-time {
		font-weight: 400;
		opacity: 0.7;
		font-size: var(--text-2xs);
	}

	/* Playback is message metadata, not a second content row. Housing the
	 * compact control in the header keeps the bubble's text edge authoritative
	 * and removes the otherwise-empty footer beneath every message. */
	.chat-header-speak {
		display: inline-flex;
		align-items: center;
		margin-left: -0.16rem;
	}

	.chat-header-speak :global(.speak-btn--compact) {
		width: 18px;
		height: 18px;
		border-color: transparent;
		border-radius: 4px;
	}

	.chat-header-speak :global(.speak-btn--compact:hover),
	.chat-header-speak :global(.speak-btn--compact:focus-visible),
	.chat-header-speak :global(.speak-btn--active) {
		border-color: currentColor;
	}

	.chat-voice-origin {
		display: inline-flex;
		align-items: center;
		opacity: 0.6;
		color: currentColor;
	}
	.chat-voice-origin svg {
		width: 11px;
		height: 11px;
		display: block;
	}

	.chat-page :global(.chat-bubble) {
		font-size: 0.82rem;
		line-height: 1.55;
		max-width: 70%;
		min-width: 0;
		/* Svelte preserves source-level whitespace (newlines + tabs)
		 * between sibling elements/conditionals inside the bubble as
		 * actual text nodes. With `white-space: pre-wrap` (the prior
		 * value), those text nodes rendered as visible line breaks —
		 * one phantom blank line per Svelte `{#if}` block inside the
		 * bubble, regardless of whether the conditional body
		 * rendered anything. Adding the v0.0.299 AssistantActivitySection
		 * conditional made the regression visible: every chat bubble
		 * grew by one extra line because of the new whitespace
		 * between the content `{#if}` and the activity-section
		 * `{#if}`.
		 *
		 * Switch to `normal`: source whitespace collapses (back to
		 * one space max), real message newlines still render because
		 * ChatMarkdown converts `\n` → `<br>` internally before the
		 * content reaches this surface. Same change applied to the
		 * thread page (`t/[name]/+page.svelte`). */
		white-space: normal;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-page :global(.chat-bubble-neutral) {
		background-color: var(--bg-card, #ffffff) !important;
		color: var(--text-primary, #2d3436);
		box-shadow: 0 0 0 1px var(--chat-reply-border, var(--border-soft, #eee4dc));
		/* Give the assistant's reply text room to breathe. daisyUI's
		 * default padding leaves the markdown right up against the
		 * bubble edges and (when present) the embedded activity strip,
		 * which makes the actual answer look squeezed and turns the
		 * tiny grey strip into the visual focus. Bump the inner
		 * padding so the reply reads as the primary element. */
		padding: 0.9rem 1.05rem;
	}

	/* While the message is still the optimistic streaming placeholder
	   (id starts with `streaming-`), pin the bubble width to the same
	   max-width the final bubble would max out at. daisyUI's default
	   `width: fit-content` makes the bubble shrink-wrap to whatever
	   the widest text line currently is, so every new token nudges
	   the bubble width — and any token that pushes a line past
	   `max-width: 70%` causes a sudden wrap-and-grow that reads as
	   "the chat just reloaded." With width pinned, content only
	   changes the bubble's HEIGHT as text wraps inside; once the
	   final non-streaming message lands the class drops and the
	   bubble snaps back to shrink-wrap for the polished resting
	   state. */
	.chat-page :global(.chat-bubble--streaming) {
		width: 100%;
		max-width: 70%;
		/* Pre-allocate ~5 lines of vertical space so the bubble doesn't
		   visibly "open like a book" from a one-line height as the
		   first tokens stream in. Almost every chat reply exceeds 5
		   lines, so this min-height is invisible for real replies (the
		   text just keeps growing past it). Short single-line replies
		   look slightly roomy for a moment then snap back to fit-content
		   at stream end when `chat-bubble--streaming` drops. */
		min-height: 6rem;
		/* Isolate streaming-bubble reflow so any layout work caused by
		   token-appends stays inside this element — the surrounding
		   chat column does not re-layout per token. `layout` alone is
		   enough; `paint` adds a transparent clip we don't want. */
		contain: layout;
	}

	.chat-page :global(.chat-bubble-primary) {
		background-color: var(--accent-primary, #ff6b6b) !important;
		color: var(--text-on-accent, #ffffff);
	}

	/* DaisyUI v5 tail color override for user bubble. */
	.chat-page :global(.chat-end .chat-bubble-primary)::before {
		background-color: var(--accent-primary, #ff6b6b) !important;
	}

	/* Neutral bubble: DaisyUI's mask tail is invisible (same color as page bg).
	   Suppress it and use a custom bordered tail that matches the bubble outline. */
	.chat-page :global(.chat-start .chat-bubble-neutral)::before {
		display: none !important;
	}

	.chat-page :global(.chat-start .chat-bubble-neutral) {
		overflow: visible;
	}

	.chat-page :global(.chat-start .chat-bubble-neutral)::after {
		content: '';
		position: absolute;
		left: -0.32rem;
		bottom: 0.4rem;
		width: 0.6rem;
		height: 0.6rem;
		background: var(--bg-card, #ffffff);
		box-shadow: -1px 1px 0 0 var(--chat-reply-border, var(--border-soft, #eee4dc));
		transform: rotate(45deg);
	}

	.chat-rich-result-card {
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		max-width: min(100%, 500px);
		min-width: 0;
		padding: 0.95rem 1rem;
		border-radius: 1rem;
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 10%, transparent);
		background: color-mix(in srgb, var(--bg-elevated, #fff) 96%, #f8fafc);
	}

	.chat-rich-result-header {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.chat-rich-result-label {
		font-size: 0.8rem;
		font-weight: 700;
		color: var(--text-primary, #2d3436);
	}

	.chat-rich-result-summary {
		font-size: var(--text-xs);
		color: var(--text-secondary, #5f6668);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-attachment-input {
		display: none;
	}

	.chat-staged-attachments {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin-bottom: 0.75rem;
	}

	.chat-staged-attachment {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.15rem;
		padding: 0.55rem 0.7rem;
		border-radius: 0.85rem;
		border: 1px solid var(--border-soft, #eee4dc);
		background: var(--bg-soft, #f6f1e8);
		color: inherit;
		text-align: left;
		cursor: pointer;
	}

	.chat-staged-attachment-name {
		font-size: var(--text-xs);
		font-weight: 600;
		word-break: break-word;
	}

	.chat-staged-attachment-meta {
		font-size: var(--text-2xs);
		color: var(--text-muted, #8f9799);
		word-break: break-word;
	}

	.chat-profile-warning {
		margin-top: 0.55rem;
		font-size: 0.8rem;
		line-height: 1.35;
		/* Warning tone, themed: mix the palette's warning hue toward the
		   text color so it stays readable as body copy in every theme. */
		color: color-mix(in srgb, var(--color-warning) 60%, var(--text-primary));
	}

	/* Typing-dots styles were here previously, but the empty-streaming-
	   placeholder branch they targeted no longer renders — `<ChatTurnProgress />`
	   handles the in-flight indicator. The dots component itself still
	   lives inside ChatTurnProgress.svelte with its own scoped styles, so
	   nothing here needs to fall back to a shared rule. */

	/* ===== Responsive ===== */
	@media (max-width: 768px) {
		.chat-messages-area {
			padding: 1rem;
		}

		.chat-page :global(.chat-bubble) {
			max-width: 85%;
		}
	}

	/* ===== Action Cards (tool proposals, executed, status updates) ===== */
	.chat-action-card-wrap {
		display: flex;
		justify-content: flex-start;
		padding: 0.35rem 0;
	}

	.chat-executed-alert {
		display: flex;
		max-width: min(100%, 500px);
		min-width: 0;
		font-size: 0.8rem;
		border-radius: var(--radius-sm);
		padding: 0.6rem 0.85rem;
		gap: 0.5rem;
		align-items: flex-start;
		background: var(--color-success-soft);
		border: 1px solid var(--color-success);
		color: var(--text-primary, #2d3436);
	}

	.chat-executed-content {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	.chat-executed-label {
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--text-primary, #2d3436);
	}

	.chat-executed-summary {
		font-size: var(--text-2xs);
		color: var(--text-secondary, #5f6668);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	/* ===== Profile Picker (custom dropdown) ===== */
	/* Keep the profile picker above floating shell chrome.
	   The backdrop is a <button> (pointer-close without a11y suppressions;
	   tabindex="-1" keeps it out of the tab order) — reset the UA button
	   chrome so it stays a bare scrim. */
	.chat-profile-backdrop {
		position: fixed;
		inset: 0;
		z-index: 935;
		border: 0;
		padding: 0;
		background: transparent;
		appearance: none;
		cursor: default;
	}

	.chat-profile-dropdown {
		position: absolute;
		bottom: calc(100% + 6px);
		left: 0;
		z-index: 940;
		min-width: 180px;
		width: min(360px, calc(100vw - 3rem));
		max-height: min(420px, calc(100vh - 9rem));
		overflow-y: auto;
		overscroll-behavior: contain;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: var(--radius-sm);
		box-shadow: var(--shadow-md);
		padding: 0.3rem;
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	.chat-profile-option {
		display: flex;
		flex-direction: column;
		gap: 0.05rem;
		padding: 0.4rem 0.6rem;
		border: none;
		background: none;
		border-radius: 7px;
		cursor: pointer;
		text-align: left;
		font-family: var(--font-primary);
		transition: background 0.1s;
	}

	.chat-profile-option:hover {
		background: var(--bg-soft, #f6f1e8);
	}

	.chat-profile-option.selected {
		background: var(--accent-primary-soft);
	}

	.chat-profile-adaptive-badge {
		display: inline-block;
		margin-left: 0.4rem;
		padding: 0.05rem 0.4rem;
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		border-radius: 999px;
		background: var(--accent-soft, rgba(194, 80, 42, 0.12));
		color: var(--accent-primary, #c2502a);
		border: 1px solid var(--accent-border, rgba(194, 80, 42, 0.28));
		vertical-align: middle;
	}

	.chat-profile-adaptive-badge.inline {
		font-size: var(--text-2xs);
		padding: 0.02rem 0.3rem;
		margin-left: 0.35rem;
	}

	.chat-profile-group-label {
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted, #8f9799);
		padding: 0.4rem 0.6rem 0.2rem;
	}

	.chat-profile-group-divider {
		height: 1px;
		margin: 0.25rem 0.5rem;
		background: var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.chat-profile-option-desc {
		font-size: var(--text-2xs);
		color: var(--text-muted, #8f9799);
		margin-top: 0.15rem;
		line-height: 1.25;
	}

	.chat-profile-option.adaptive {
		border-left: 2px solid var(--accent-primary, #c2502a);
		padding-left: calc(0.6rem - 2px);
	}

	.chat-profile-option-name {
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-primary, #2d3436);
		overflow-wrap: anywhere;
	}

	.chat-profile-option-model {
		font-size: var(--text-2xs);
		color: var(--text-muted, #8f9799);
		font-family: var(--font-mono);
		overflow-wrap: anywhere;
	}

	.thread-plan-sheet-backdrop {
		position: fixed;
		inset: 0;
		z-index: 980;
		background: rgba(33, 37, 41, 0.34);
		backdrop-filter: blur(2px);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 1rem;
	}

	.thread-plan-sheet {
		width: min(1040px, calc(100vw - 1.8rem));
		max-height: calc(100dvh - 2rem);
		display: flex;
		flex-direction: column;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: 1.2rem;
		box-shadow: 0 22px 56px rgba(26, 32, 44, 0.18);
		overflow: hidden;
	}

	.thread-plan-sheet__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		padding: 1rem 1.05rem 0.85rem;
		border-bottom: 1px solid var(--border-soft, #eee4dc);
		/* Theme-aware: previously hardcoded `white` and `rgba(255,255,255,0.98)`
		   here, which forced a near-white header even in dark mode and made
		   the title/eyebrow text effectively invisible. Use theme tokens so
		   the gradient adapts to the active theme. */
		background:
			linear-gradient(
				180deg,
				color-mix(
					in srgb,
					var(--accent-secondary-soft, rgba(78, 205, 196, 0.12)) 72%,
					var(--bg-card, #ffffff)
				) 0%,
				var(--bg-card, #ffffff) 100%
			);
	}

	.thread-plan-sheet__copy {
		min-width: 0;
	}

	.thread-plan-sheet__eyebrow {
		display: inline-block;
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted, #8f9799);
		margin-bottom: 0.25rem;
	}

	.thread-plan-sheet__copy h2 {
		margin: 0;
		font-size: 1rem;
		font-weight: 700;
		color: var(--text-primary, #2d3436);
	}

	.thread-plan-sheet__copy p {
		margin: 0.25rem 0 0;
		font-size: 0.76rem;
		line-height: 1.35;
		color: var(--text-muted, #7f8c8d);
	}

	.thread-plan-sheet__body {
		flex: 1;
		min-height: 0;
		overflow: auto;
		padding: 0.9rem 1rem 1rem;
		background: var(--bg-base, #fffdf8);
	}

	@media (max-width: 900px) {
		.thread-plan-sheet-backdrop {
			padding: 0.4rem;
		}

		.thread-plan-sheet {
			width: 100%;
			max-height: calc(100dvh - 0.8rem);
			border-radius: 1rem;
		}

		.thread-plan-sheet__header {
			padding: 0.85rem 0.9rem 0.75rem;
		}

		.thread-plan-sheet__body {
			padding: 0.75rem 0.85rem 0.85rem;
		}
	}

</style>
