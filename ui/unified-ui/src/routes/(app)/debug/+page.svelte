<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { get } from 'svelte/store';
	import type { MuijComponent, MuijInteractionEventDetail } from '$lib/stores/muijStore';
	import ExecutionResponsibilityPanel from '$lib/magician/components/ExecutionResponsibilityPanel.svelte';
	import MuijRenderer from '$lib/magician/components/generative/MuijRenderer.svelte';
	import { v2Events, type V2WebSocketEvent, getV2EventSequence } from '$lib/realtime/v2-websocket';
	import type { ExecutionResponsibilitySnapshot } from '$lib/types/executionResponsibility';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import {
		SOTA_FIXTURES as SHARED_SOTA_FIXTURES,
		SOTA_GOAL as SHARED_SOTA_GOAL,
		sotaDebugGoalForFixture,
		sotaEnvModeForFixture,
		sotaRegionCaptureSelectorForFixture
	} from '$lib/data/sotaFixtures';
	import {
		DESKTOP_APP_TUTOR_CONTRACT,
		DESKTOP_APP_TUTOR_TESTS
	} from '$lib/data/desktopAppTutorTests';
	import { DESKTOP_GROUNDING_TESTS, GROUNDING_RECIPE_PREAMBLE } from '$lib/data/desktopGroundingTests';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		coordinateExecutionControl,
		requireExecutionControlResponse
	} from '$lib/magician/execution/controlClient';

	type ExecutionMode = 'agentic' | 'direct' | 'sota-tests';
	type AgenticEnvMode = string;

	interface SkillActionParam {
		name: string;
		type: string;
		description?: string;
		required: boolean;
		default?: unknown;
		enum?: string[];
		items?: { type?: string };
	}
	interface SkillActionDef {
		name: string;
		description: string;
		argv: string[];
		parameters: SkillActionParam[];
	}
	interface SkillSchema {
		name: string;
		description: string;
		topLevelParams: SkillActionParam[];
		actions: SkillActionDef[];
	}

	interface SotaTest {
		id: string;
		name: string;
		phase: string;
		description: string;
		file: string;
		suggestedGoal: string;
		group: string;
		groups: string[];
		/// Live-app desktop test: no fixture URL and no browser navigate prefix.
		/// `envMode` decides whether the run is shell-scoped or uses the full
		/// agent tool set.
		desktop?: boolean;
		/// Allowlisted macOS app to launch/focus before Desktop App Copilot chat.
		desktopApp?: 'Notes' | 'Calculator' | 'TextEdit' | 'Music';
		/// Tool-scope override for the run. `null` means full agent tool set.
		envMode?: AgenticEnvMode | null;
		/// Same-origin selector inside a SOTA fixture whose visible screen rect
		/// should be captured and attached before the tutor/copilot chat prompt is sent.
		regionCaptureSelector?: string | null;
	}

	interface SotaChatRunResult {
		sessionId: string;
		chatTurnId: string;
		threadId: string;
		prompt: string;
		attachmentIds: string[];
		captureRect: ScreenRegionRect | null;
		assistantText: string;
		queued: boolean;
		messageCount: number;
	}

	interface SotaTutorCaptureResult {
		sessionId: string;
		threadId: string;
		attachmentIds: string[];
		captureRect: ScreenRegionRect | null;
	}

	interface ScreenRegionRect {
		x: number;
		y: number;
		width: number;
		height: number;
	}

	interface DebugExecution {
		id: string;
		goal: string;
	}

	interface DebugExecutionApiRecord {
		id: string;
		title?: string;
		updated_at?: number;
		created_at?: number;
	}

	const API_BASE = '/api/magician/v2';
	const TASKS_API_BASE = '/api/magician/v3';
	const TUTOR_SOTA_THREAD_ID = 'screens';
	// Skill action runner — replaces the now-defunct Magicutor direct-action
	// path. Drives the `Direct Actions` panel: pick a skill → action → fill
	// schema-driven form → POST to `/api/magician/v2/skills/{name}/run`.
	const VEIL_SCHEMA_VERSION = 'presto.veil-v1';

	interface DebugScopeSnapshot {
		principal: string;
		workspace: string;
	}

	// SOTA goal text is shared with the command palette so both surfaces
	// dispatch identical agentic prompts. Source: `$lib/data/sotaFixtures.ts`.
	const SOTA_TEST_GOAL = SHARED_SOTA_GOAL;
	// SOTA fixture list is shared with the command palette so adding a new
	// fixture in one place updates both surfaces. Source:
	// `$lib/data/sotaFixtures.ts`.
	const SOTA_TEST_FIXTURES = SHARED_SOTA_FIXTURES;
	const SOTA_TEST_OVERRIDES: Record<
		string,
		Pick<SotaTest, 'name' | 'phase' | 'description' | 'group'> & Partial<Pick<SotaTest, 'groups'>>
		> = {
			'01-cross-origin-iframe.html': {
				name: 'Cross-Origin Iframe',
				phase: 'Cross-Origin Iframes',
				description: 'Elements inside cross-origin iframe contexts.',
				group: 'Iframes & Cross-Origin'
			},
			'02-codesandbox-only.html': {
				name: 'CodeSandbox Iframe Only',
				phase: 'Cross-Origin Editors',
				description: 'Isolated CodeSandbox typing inside a cross-origin iframe editor.',
				group: 'Input & Editors',
				groups: ['Input & Editors', 'Iframes & Cross-Origin']
			},
			'03-dense-grid.html': {
				name: 'Dense Grid / SoM Scalability',
				phase: 'Element Density',
			description: 'Pages with dense interactive layouts.',
			group: 'Detection & Visual'
		},
		'07-loading-states.html': {
			name: 'Temporal Context / Loading',
			phase: 'Loading States',
			description: 'Loading transitions and readiness checks.',
			group: 'Resilience & Loading'
		},
		'12-checkout-flow.html': {
			name: 'Checkout Flow',
			phase: 'E2E Integration',
			description: 'Multi-step real-world browser flow.',
			group: 'Navigation & Flow'
		},
		'17-drag-and-drop.html': {
			name: 'Drag and Drop',
			phase: 'Drag and Drop',
			description: 'Element-to-element drag interactions.',
			group: 'Drag & Interaction'
		},
		'23-download-upload-cycles.html': {
			name: 'Downloads and Uploads',
			phase: 'File Transfer Workflows',
			description: 'Browser downloads, uploads, and download-upload round-trip cycles.',
			group: 'Input & Editors',
			groups: ['Input & Editors', 'FileHandling']
		},
		'24-credential-leak-detection.html': {
			name: 'Credential Leak Detection',
			phase: 'Security',
			description: 'Tests whether passwords and OTPs are leaked to LLM agents via DOM, console, or network.',
			group: 'Security'
		},
		'25-diagram-editor-canvas.html': {
			name: 'Diagram Editor Spatial Workflows',
			phase: 'Canvas Mode Benchmarks',
			description: 'Diagram lane placement and connector-path gestures on dense spatial surfaces.',
			group: 'Canvas Mode',
			groups: ['Canvas Mode', 'Drag & Interaction']
		},
		'26-whiteboard-spatial-mode.html': {
			name: 'Whiteboard Spatial Mode',
			phase: 'Canvas Mode Benchmarks',
			description: 'Canvas drawing and text-tool entry on a whiteboard surface.',
			group: 'Canvas Mode',
			groups: ['Canvas Mode', 'Input & Editors']
		},
		'27-map-pan-zoom-canvas.html': {
			name: 'Map Pan and Zoom Canvas',
			phase: 'Canvas Mode Benchmarks',
			description: 'Viewport panning, zoom, and marker targeting on a spatial map surface.',
			group: 'Canvas Mode',
			groups: ['Canvas Mode', 'Scroll', 'Detection & Visual']
		},
		'28-chart-inspector-spatial.html': {
			name: 'Chart Inspector Spatial Workflows',
			phase: 'Canvas Mode Benchmarks',
			description: 'Tooltip hover and brush-zoom gestures on a chart-local surface.',
			group: 'Canvas Mode',
			groups: ['Canvas Mode', 'Detection & Visual']
		},
		'29-cross-origin-sandboxed-canvas.html': {
			name: 'Cross-Origin Sandboxed Canvas',
			phase: 'Canvas Mode Benchmarks',
			description: 'Opaque sandboxed iframes that only verify via postMessage bridge events.',
			group: 'Canvas Mode',
			groups: ['Canvas Mode', 'Iframes & Cross-Origin']
		},
			'30-collaborative-noisy-board.html': {
				name: 'Collaborative Noisy Board',
				phase: 'Canvas Mode Benchmarks',
				description: 'Drag and edit flows under moving cursors and collaboration toasts.',
				group: 'Canvas Mode',
				groups: ['Canvas Mode', 'Drag & Interaction', 'Input & Editors']
			},
			'31-hybrid-diagram-connectors.html': {
				name: 'Hybrid Diagram Connectors',
				phase: 'Canvas Mode Benchmarks',
				description: 'Broad card drags and precise handle drags on a DOM-over-SVG connector surface.',
				group: 'Canvas Mode',
				groups: ['Canvas Mode', 'Drag & Interaction']
			},
			'32-vision-only-targets.html': {
				name: 'Vision-Only Canvas Targets',
				phase: 'Canvas Mode Benchmarks',
				description: 'Canvas with randomized colored circle / package-and-depot positions; coordinates kept in closure scope so screenshot vision is the only viable path.',
				group: 'Canvas Mode',
				groups: ['Canvas Mode', 'Detection & Visual']
			},
			'33-parent-occlusion.html': {
				name: 'Parent-Frame Occlusion Detection',
				phase: 'Phase 0.5',
				description: 'Detection when a modal in the parent frame covers iframe content.',
				group: 'Iframes & Cross-Origin'
			},
			'34-same-origin-iframe-routing.html': {
				name: 'Same-Origin Iframe Routing (Phase 4)',
				phase: 'Phase 4',
				description: 'Deterministic same-origin iframe (srcdoc) fixture for the OOPIF iframe-session-routing plan. Verifies frame-switch + click + type + snapshot flows. Cross-origin OOPIF coverage stays in 01-cross-origin-iframe.html.',
				group: 'Iframes & Cross-Origin'
			},
			'35-screenshot-only-canvas.html': {
				name: 'Screenshot-Only Canvas Targets',
				phase: 'Phase 4',
				description: 'Three canvas-only test cases that force screenshot observation (the canvases expose nothing to the DOM, so snapshot is useless). Designed to exercise the Yutori N1.5 image-turn route end-to-end through the agent_browser flat-loop dispatch.',
				group: 'Canvas Mode',
				groups: ['Canvas Mode', 'Detection & Visual']
			},
			'36-live-concept-tutor-math.html': {
				name: 'Live Concept Tutor · Math Graph',
				phase: 'Live Concept Tutor',
				description: 'Static graph fixture for stepwise tutor overlays grounded on axes, line, rise/run labels, and formula text.',
				group: 'Live Concept Tutor'
			},
			'37-live-concept-tutor-physics.html': {
				name: 'Live Concept Tutor · Physics Forces',
				phase: 'Live Concept Tutor',
				description: 'Static free-body fixture for tutor overlays grounded on body, force arrows, axes, and net/equilibrium explanation.',
				group: 'Live Concept Tutor'
			},
			'38-live-concept-tutor-cs.html': {
				name: 'Live Concept Tutor · CS Recursion',
				phase: 'Live Concept Tutor',
				description: 'Static code/stack/heap fixture for tutor overlays grounded on code, stack frames, heap object, and pointer relation.',
				group: 'Live Concept Tutor'
			},
			'39-cropped-capture-slope.html': {
				name: 'Cropped Capture - Slope',
				phase: 'Cropped Capture Visual',
				description: 'Stages only the graph panel as a rect-aware crop, then verifies tutor overlays map crop-local coordinates back to the live screen.',
				group: 'Cropped Capture Visual'
			},
			'40-cropped-capture-app-help.html': {
				name: 'Cropped Capture - App Help',
				phase: 'Cropped Capture Visual',
				description: 'Stages only a toolbar crop and asks App Copilot to point at the create-note control without mutating the fixture.',
				group: 'Cropped Capture Visual'
			},
			'41-cropped-capture-code-memory.html': {
				name: 'Cropped Capture - Code Memory',
				phase: 'Cropped Capture Visual',
				description: 'Stages only a code/stack/heap crop and verifies Live Concept Tutor can ground a pointer explanation from the cropped attachment.',
				group: 'Cropped Capture Visual'
			}
		};

	function getSotaTestGroups(test: Pick<SotaTest, 'group' | 'groups'>): string[] {
		return test.groups.length > 0 ? test.groups : [test.group];
	}

	function toTitleCase(value: string): string {
		return value
			.split(/\s+/)
			.filter(Boolean)
			.map((word) => word.charAt(0).toUpperCase() + word.slice(1))
			.join(' ');
	}

	function fallbackSotaGroup(id: string): string {
		const numericId = Number(id);
		if (numericId === 1 || numericId === 2 || numericId === 15) return 'Iframes & Cross-Origin';
		if (numericId === 17 || numericId === 18) return 'Drag & Interaction';
		if (numericId === 16 || numericId === 19) return 'Scroll';
		if (numericId === 3 || numericId === 5 || numericId === 6 || numericId === 10) return 'Detection & Visual';
		if (numericId === 4 || numericId === 7 || numericId === 8) return 'Resilience & Loading';
		if (numericId === 9 || numericId === 14) return 'Input & Editors';
		if (numericId === 24) return 'Security';
		if (numericId >= 25) return 'Canvas Mode';
		if (numericId >= 20) return 'CAPTCHA';
		return 'Navigation & Flow';
	}

		const sotaTests: SotaTest[] = SOTA_TEST_FIXTURES.map((file) => {
			const id = file.replace(/\.html$/, '');
			const numericId = file.slice(0, 2);
			const base = file.replace(/^\d+-/, '').replace(/\.html$/, '');
			const fallbackName = toTitleCase(base.replace(/-/g, ' '));
			const override = SOTA_TEST_OVERRIDES[file];
			const primaryGroup = override?.group ?? fallbackSotaGroup(numericId);
			const groups = override?.groups ?? [primaryGroup];
			return {
			id,
			name: override?.name ?? fallbackName,
			phase: override?.phase ?? fallbackName,
			description: override?.description ?? `SOTA fixture: ${fallbackName}.`,
			file,
			suggestedGoal: sotaDebugGoalForFixture(file),
			group: primaryGroup,
			groups,
			regionCaptureSelector: sotaRegionCaptureSelectorForFixture(file),
			envMode: sotaEnvModeForFixture(file)
		};
	});
	// Desktop Grounding suite — live macOS apps as fixtures (Freeform /
	// Maps / Stocks content areas are AX-empty; Chess = calibration).
	// Goals embed the grounding recipe so the raw shell-env loop needs
	// no skill activation. Source: $lib/data/desktopGroundingTests.ts.
	const desktopGroundingTests: SotaTest[] = DESKTOP_GROUNDING_TESTS.map((test) => ({
		id: test.id,
		name: test.name,
		phase: 'Desktop Grounding',
		description: test.description,
		file: '',
		suggestedGoal: `${test.goal} ${GROUNDING_RECIPE_PREAMBLE}`,
		group: 'Desktop Grounding',
		groups: ['Desktop Grounding'],
		desktop: true
	}));
	sotaTests.push(...desktopGroundingTests);
	const desktopAppTutorTests: SotaTest[] = DESKTOP_APP_TUTOR_TESTS.map((test) => ({
		id: test.id,
		name: test.name,
		phase: 'Desktop App Copilot',
		description: test.description,
		file: '',
		suggestedGoal: `${test.goal} ${DESKTOP_APP_TUTOR_CONTRACT}`,
		group: 'Desktop App Copilot',
		groups: ['Desktop App Copilot'],
		desktop: true,
		desktopApp: test.app,
		envMode: null
	}));
	sotaTests.push(...desktopAppTutorTests);
	const sotaGroups = [
		'Desktop Grounding',
		'Desktop App Copilot',
		'Cropped Capture Visual',
		'Live Concept Tutor',
		'Canvas Mode',
		'Drag & Interaction',
		'Scroll',
		'Navigation & Flow',
		'Iframes & Cross-Origin',
		'Detection & Visual',
		'Resilience & Loading',
		'FileHandling',
		'Input & Editors',
		'CAPTCHA',
		'Security',
		'All'
	] as const;
	const sotaGroupCounts: Record<string, number> = (() => {
		const counts: Record<string, number> = {};
		for (const group of sotaGroups) {
			if (group === 'All') {
				counts[group] = sotaTests.length;
				continue;
			}
			counts[group] = sotaTests.filter((test) => getSotaTestGroups(test).includes(group)).length;
		}
		return counts;
	})();
	const DEFAULT_AGENTIC_MAX_ITERATIONS = 2000;
	// UI guard rail. The server-side default is 400
	// (`DEFAULT_MAX_ITERATIONS`); long-running canvas / multi-iframe
	// fixtures can legitimately need more. Cap is intentionally generous
	// so it doesn't block real runs while still catching typos in the
	// number field (e.g. an extra zero).
	const MAX_AGENTIC_MAX_ITERATIONS = 2000;

	let executionMode: ExecutionMode = 'agentic';
	let agenticEnvMode: AgenticEnvMode = 'browser';
	let maxIterations = DEFAULT_AGENTIC_MAX_ITERATIONS;
	let goal = '';

	// Dynamic capability packs fetched from backend
	let capabilityPacks: { name: string; description?: string }[] = [];
	const DEFAULT_ENV_MODES = ['browser', 'http', 'file', 'bash'];

	async function loadCapabilityPacks() {
		try {
			const scope = get(scopeIdentityStore);
			const headers: Record<string, string> = {};
			const res = await timedFetch('/api/magician/v2/skills', { headers });
			if (res.ok) {
				const data = await res.json();
				capabilityPacks = (data.skills ?? [])
					.filter((s: { kind: string }) => s.kind !== 'personality-mode')
					.map((s: { name: string; description?: string }) => ({
						name: s.name,
						description: s.description,
					}))
					.sort((a: { name: string }, b: { name: string }) => {
						// Put default modes first, then alphabetical
						const aDefault = DEFAULT_ENV_MODES.indexOf(a.name);
						const bDefault = DEFAULT_ENV_MODES.indexOf(b.name);
						if (aDefault >= 0 && bDefault >= 0) return aDefault - bDefault;
						if (aDefault >= 0) return -1;
						if (bDefault >= 0) return 1;
						return a.name.localeCompare(b.name);
					});
			}
		} catch {
			// Fallback to defaults if API unavailable
		}
	}

	// Skill action runner state. `selectedSkill` tracks the picked
	// skill (procedure/compiled — anything dispatchable). `skillSchema`
	// is the parsed `tool_schema.yaml` for that skill. `selectedAction`
	// is a key in `skillSchema.actions` (or empty for single-action
	// packs). `skillParamValues` holds the form input keyed by parameter
	// name. `skillRunResult` is the most recent run response.
	let selectedSkill = '';
	let selectedAction = '';
	let skillSchema: SkillSchema | null = null;
	let skillSchemaLoading = false;
	let skillSchemaError: string | null = null;
	// Per-action parameter form values (one entry per `native_action_schemas
	// .<action>.parameters` field).
	let skillParamValues: Record<string, string> = {};
	// Top-level skill `parameters` values (browser uses these for
	// `connection_mode` / `cdp_url` / `url` session config; other skills
	// currently ignore them server-side).
	let skillSessionParamValues: Record<string, string> = {};
	interface SkillRunStep {
		label: string;
		argv: string[];
		exit_code: number | null;
		success: boolean;
		stdout: string;
		stderr: string;
		duration_ms: number;
		parsed_json: unknown;
	}
	let skillRunResult: {
		prelude: SkillRunStep[];
		argv: string[];
		exit_code: number | null;
		success: boolean;
		stdout: string;
		stderr: string;
		duration_ms: number;
		parsed_json: unknown;
	} | null = null;

	let isRunning = false;
	let isPaused = false;
	let isClearing = false;
	let executionId: string | null = null;
	let activeSotaChatSessionId: string | null = null;
	let sotaChatRunResult: SotaChatRunResult | null = null;
	let events: V2WebSocketEvent[] = [];
	let observations: Array<Record<string, unknown>> = [];
	let pauseState: Record<string, unknown> | null = null;
	let currentStatus = 'idle';
	let error: string | null = null;
	let activeTab: 'events' | 'observations' | 'pause' | 'durable' = 'events';
	let isLoadingArtifacts = false;
	let artifactRefreshRequestId = 0;

	// Durable artifacts state
	interface DurableArtifactEntry {
		namespace: string;
		name: string;
		content_type: string | null;
		last_updated: string | null;
		last_updated_by: string | null;
	}
	interface DurableArtifactContent {
		namespace: string;
		name: string;
		content_type: string | null;
		last_updated: string;
		last_updated_by: string;
		created_by: string;
		content: string;
	}
	let durableArtifacts: DurableArtifactEntry[] = [];
	let durableArtifactContent: DurableArtifactContent | null = null;
	let isLoadingDurableArtifacts = false;
	let durableArtifactsFetchId = 0;
	let isLoadingDurableContent = false;
	let durableContentError: string | null = null;
	let lastProcessedEventSequence = 0;
	let eventUnsubscribe: (() => void) | null = null;
	let refreshInterval: ReturnType<typeof setInterval> | null = null;
	let fetchedScreenshotIds = new Set<string>();

	let debugExecutions: DebugExecution[] = [];
	let selectedSotaTest: SotaTest | null = null;
	let activeGroup = 'Drag & Interaction';
	let sotaServerStatus: 'unknown' | 'running' | 'stopped' = 'unknown';
	let sotaTestBox = '';
	let components: MuijComponent[] = [];
	let currentDebugScopeKey = '';
	let lastDebugScopeKey = '';
	let debugMounted = false;
	let debugScopeGeneration = 0;
	let durableArtifactContentFetchId = 0;

	function parseExecutionMode(raw: string | null): ExecutionMode {
		return raw === 'direct' || raw === 'sota-tests' ? raw : 'agentic';
	}

	function scopedDebugValue(key: 'principal' | 'workspace'): string {
		const routeValue = (get(page).url.searchParams.get(key) || '').trim();
		if (routeValue.length > 0) return routeValue;
		const scope = get(scopeIdentityStore);
		const storeValue = (key === 'principal' ? scope.principal : scope.workspace).trim();
		return storeValue;
	}

	function currentDebugScopeSnapshot(): DebugScopeSnapshot {
		return {
			principal: scopedDebugValue('principal'),
			workspace: scopedDebugValue('workspace')
		};
	}

	function captureDebugScopeToken(): { generation: number; scopeKey: string } {
		return {
			generation: debugScopeGeneration,
			scopeKey: currentDebugScopeKey
		};
	}

	function isStaleDebugScope(token: { generation: number; scopeKey: string }): boolean {
		return token.generation !== debugScopeGeneration || currentDebugScopeKey !== token.scopeKey;
	}

	function applyScopedDebugQuery(
		params: URLSearchParams,
		scope: DebugScopeSnapshot = currentDebugScopeSnapshot()
	): void {
		const principal = scope.principal;
		const workspace = scope.workspace;
	}

	function scopedDebugHeaders(
		headers?: HeadersInit,
		scope: DebugScopeSnapshot = currentDebugScopeSnapshot()
	): Headers {
		const next = new Headers(headers);
		const principal = scope.principal;
		const workspace = scope.workspace;
		return scopedRequestHeaders(next);
	}

	async function scopedDebugFetch(
		path: string,
		init: RequestInit = {},
		scope: DebugScopeSnapshot = currentDebugScopeSnapshot()
	): Promise<Response> {
		const params = new URLSearchParams();
		applyScopedDebugQuery(params, scope);
		const query = params.toString();
		const url = query ? `${path}${path.includes('?') ? '&' : '?'}${query}` : path;
		return fetch(url, {
			...init,
			headers: scopedDebugHeaders(init.headers, scope)
		});
	}

	function clearScopedDebugState(): void {
		debugScopeGeneration += 1;
		artifactRefreshRequestId += 1;
		durableArtifactsFetchId += 1;
		durableArtifactContentFetchId += 1;
		stopAutoRefresh();
		executionId = null;
		activeSotaChatSessionId = null;
		sotaChatRunResult = null;
		events = [];
		observations = [];
		pauseState = null;
		currentStatus = 'idle';
		error = null;
		activeTab = 'events';
		isLoadingArtifacts = false;
		durableArtifacts = [];
		durableArtifactContent = null;
		isLoadingDurableArtifacts = false;
		isLoadingDurableContent = false;
		durableContentError = null;
		lastProcessedEventSequence = 0;
		fetchedScreenshotIds = new Set();
		isRunning = false;
		isPaused = false;
		isClearing = false;
		debugExecutions = [];
		skillRunResult = null;
	}

	$: urlExecutionMode = parseExecutionMode($page.url.searchParams.get('mode'));
	$: if (executionMode !== urlExecutionMode) {
		executionMode = urlExecutionMode;
	}
	$: SOTA_TEST_BASE_URL = browser ? `${window.location.origin}/tests/sota-tests` : '/tests/sota-tests';
	$: filteredSotaTests =
		activeGroup === 'All' ? sotaTests : sotaTests.filter((test) => getSotaTestGroups(test).includes(activeGroup));
	$: components = buildVeilSurface({
		executionMode,
		agenticEnvMode,
		capabilityPacks,
		maxIterations,
		goal,
		selectedSkill,
		selectedAction,
		skillSchema,
		skillSchemaLoading,
		skillSchemaError,
		skillParamValues,
		skillRunResult,
		isRunning,
		isPaused,
		isClearing,
		executionId,
		events,
		observations,
		pauseState,
		currentStatus,
		error,
		activeTab,
		isLoadingArtifacts,
		debugExecutions,
		selectedSotaTest,
		activeGroup,
		sotaServerStatus,
		filteredSotaTests,
		sotaTestBox,
		durableArtifacts,
		durableArtifactContent,
		isLoadingDurableArtifacts,
		isLoadingDurableContent,
		durableContentError,
	});

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') return String(value);
		return '';
	}

	function asRecord(value: unknown): Record<string, unknown> {
		return value != null && typeof value === 'object' && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: {};
	}

	function readChangedValue(payload: Record<string, unknown>): unknown {
		if (Object.prototype.hasOwnProperty.call(payload, 'value')) {
			return payload.value;
		}
		if (Object.prototype.hasOwnProperty.call(payload, 'checked')) {
			return payload.checked;
		}
		const nestedValues = asRecord(payload.values);
		if (Object.prototype.hasOwnProperty.call(nestedValues, 'value')) {
			return nestedValues.value;
		}
		if (Object.prototype.hasOwnProperty.call(nestedValues, 'checked')) {
			return nestedValues.checked;
		}
		if (Object.prototype.hasOwnProperty.call(payload, 'text')) {
			return payload.text;
		}
		return undefined;
	}

	function asNumber(value: unknown, fallback: number): number {
		const parsed = Number(asString(value));
		return Number.isFinite(parsed) ? parsed : fallback;
	}

	function asBoolean(value: unknown, fallback = false): boolean {
		if (typeof value === 'boolean') return value;
		const normalized = asString(value).trim().toLowerCase();
		if (normalized === 'true' || normalized === '1' || normalized === 'yes') return true;
		if (normalized === 'false' || normalized === '0' || normalized === 'no') return false;
		return fallback;
	}

	function formatTimestamp(epochMs: number | null | undefined): string {
		if (!epochMs || !Number.isFinite(epochMs)) return 'n/a';
		try {
			return new Date(epochMs).toLocaleString();
		} catch {
			return 'n/a';
		}
	}

	function formatRetentionMs(value: number | null | undefined): string {
		if (!value || !Number.isFinite(value)) return 'n/a';
		const totalMinutes = Math.round(value / 60_000);
		const hours = Math.floor(totalMinutes / 60);
		const minutes = totalMinutes % 60;
		return minutes === 0 ? `${hours}h` : `${hours}h ${minutes}m`;
	}

	function formatRegionRect(rect: ScreenRegionRect | null | undefined): string {
		if (!rect) return 'none';
		return `x=${rect.x}, y=${rect.y}, w=${rect.width}, h=${rect.height}`;
	}

	function summarizeText(value: string | undefined, fallback: string): string {
		const trimmed = value?.trim();
		if (!trimmed) return fallback;
		return trimmed.length > 220 ? `${trimmed.slice(0, 217)}...` : trimmed;
	}

	function isTutorSotaTest(test: SotaTest): boolean {
		return getSotaTestGroups(test).some(
			(group) =>
				group === 'Live Concept Tutor'
				|| group === 'Desktop App Copilot'
				|| group === 'Cropped Capture Visual'
		);
	}

	function isLiveConceptTutorSotaTest(test: SotaTest): boolean {
		return getSotaTestGroups(test).includes('Live Concept Tutor');
	}

	function buildSotaDispatchGoal(test: SotaTest, effectiveGoal: string): string {
		if (test.desktop) {
			return effectiveGoal;
		}
		return `Navigate to ${SOTA_TEST_BASE_URL}/${test.file} and then ${effectiveGoal}`;
	}

	function waitForTutorFixtureFocus(): Promise<void> {
		return new Promise((resolve) => {
			window.setTimeout(resolve, 700);
		});
	}

	function waitMs(ms: number): Promise<void> {
		return new Promise((resolve) => {
			window.setTimeout(resolve, ms);
		});
	}

	async function openSotaFixtureForTutorRun(test: SotaTest): Promise<Window | null> {
		if (!browser || test.desktop) {
			return null;
		}
		const fixtureWindow = window.open(`${SOTA_TEST_BASE_URL}/${test.file}`, '_blank');
		if (!fixtureWindow) return null;
		fixtureWindow.focus();
		await waitForTutorFixtureFocus();
		return fixtureWindow;
	}

	async function waitForFixtureElement(
		fixtureWindow: Window,
		selector: string
	): Promise<Element> {
		let lastError: unknown = null;
		for (let attempt = 0; attempt < 30; attempt += 1) {
			try {
				if (fixtureWindow.closed) {
					throw new Error('Fixture window was closed before capture');
				}
				const doc = fixtureWindow.document;
				if (doc.readyState !== 'loading') {
					const element = doc.querySelector(selector);
					if (element) return element;
				}
			} catch (err) {
				lastError = err;
			}
			await waitMs(150);
		}
		throw new Error(
			`Could not resolve cropped-capture target selector ${selector}${lastError instanceof Error ? `: ${lastError.message}` : ''}`
		);
	}

	async function scrollSotaTutorFixtureIntoCaptureView(test: SotaTest, fixtureWindow: Window): Promise<void> {
		if (!isLiveConceptTutorSotaTest(test)) return;
		const selectors = [
			'.workspace-surface.concept-stage',
			'.workspace-card',
			'[data-goal]',
			'.test-case',
			'main',
			'body'
		];
		let target: Element | null = null;
		for (const selector of selectors) {
			try {
				target = fixtureWindow.document.querySelector(selector);
			} catch {
				target = null;
			}
			if (target) break;
		}
		if (!target) return;
		target.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
		await waitMs(250);
	}

	async function launchDesktopAppForTutorRun(test: SotaTest, scope: DebugScopeSnapshot): Promise<void> {
		if (!test.desktop || !test.desktopApp) return;
		currentStatus = `launching ${test.desktopApp}...`;
		const response = await scopedDebugFetch(
			`${API_BASE}/screen/desktop-app/launch`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ app: test.desktopApp })
			},
			scope
		);
		if (!response.ok) {
			const detail = await response.text().catch(() => '');
			throw new Error(`Failed to launch ${test.desktopApp}: ${detail || response.statusText}`);
		}
		await waitMs(1_200);
	}

	function elementScreenRect(fixtureWindow: Window, element: Element, padding = 10): ScreenRegionRect {
		const rect = element.getBoundingClientRect();
		if (rect.width <= 0 || rect.height <= 0) {
			throw new Error('Cropped-capture target has zero size');
		}
		const sideChrome = Math.max(0, (fixtureWindow.outerWidth - fixtureWindow.innerWidth) / 2);
		const topChrome = Math.max(0, fixtureWindow.outerHeight - fixtureWindow.innerHeight - sideChrome);
		const left = fixtureWindow.screenX + sideChrome + rect.left - padding;
		const top = fixtureWindow.screenY + topChrome + rect.top - padding;
		const right = fixtureWindow.screenX + sideChrome + rect.right + padding;
		const bottom = fixtureWindow.screenY + topChrome + rect.bottom + padding;
		return {
			x: Math.floor(left),
			y: Math.floor(top),
			width: Math.max(1, Math.ceil(right - left)),
			height: Math.max(1, Math.ceil(bottom - top))
		};
	}

	function attachmentIdsFromCaptureResponse(payload: Record<string, unknown>): string[] {
		const attachments = Array.isArray(payload.attachments)
			? payload.attachments.filter((attachment): attachment is Record<string, unknown> => {
					return attachment != null && typeof attachment === 'object' && !Array.isArray(attachment);
				})
			: [];
		return attachments
			.map((attachment) => asString(attachment.attachment_id))
			.filter((id) => id.length > 0);
	}

	async function stageSotaTutorCapture(
		test: SotaTest,
		fixtureWindow: Window | null,
		scope: DebugScopeSnapshot,
		sessionId?: string | null
	): Promise<SotaTutorCaptureResult | null> {
		const selector = test.regionCaptureSelector?.trim();
		if (selector && (!fixtureWindow || fixtureWindow.closed)) {
			throw new Error('Cropped-capture SOTA requires an open same-origin fixture window');
		}
		let captureRect: ScreenRegionRect | null = null;
		let captureMode = 'screenshot';
		if (selector && fixtureWindow) {
			currentStatus = 'capturing fixture crop...';
			fixtureWindow.focus();
			await waitMs(200);
			const element = await waitForFixtureElement(fixtureWindow, selector);
			element.scrollIntoView({ block: 'center', inline: 'center' });
			await waitMs(200);
			captureRect = elementScreenRect(fixtureWindow, element);
			captureMode = 'region';
		} else if (!test.desktop && fixtureWindow && !fixtureWindow.closed) {
			currentStatus = 'capturing fixture screenshot...';
			fixtureWindow.focus();
			await scrollSotaTutorFixtureIntoCaptureView(test, fixtureWindow);
			await waitMs(250);
		} else if (test.desktop) {
			currentStatus = `capturing ${test.desktopApp ?? 'desktop app'} screenshot...`;
			await waitMs(300);
		} else {
			return null;
		}
		const response = await scopedDebugFetch(
			`${API_BASE}/screen/capture`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					mode: captureMode,
					...(captureRect ? { region_rect: captureRect } : {}),
					...(sessionId ? { session_id: sessionId } : {})
				})
			},
			scope
		);
		if (!response.ok) {
			const detail = await response.text().catch(() => '');
			throw new Error(`Failed to stage fixture capture: ${detail || response.statusText}`);
		}
		const payload = asRecord(await response.json());
		const attachmentIds = attachmentIdsFromCaptureResponse(payload);
		if (attachmentIds.length === 0) {
			throw new Error('Fixture capture response did not include attachment ids');
		}
		const sessionIdFromCapture = asString(payload.session_id);
		const threadIdFromCapture = asString(payload.thread_id);
		if (!sessionIdFromCapture) {
			throw new Error('Fixture capture response did not include a session id');
		}
		return {
			sessionId: sessionIdFromCapture,
			threadId: threadIdFromCapture || TUTOR_SOTA_THREAD_ID,
			attachmentIds,
			captureRect
		};
	}

	function chatMessageText(message: Record<string, unknown>): string {
		const content = asRecord(message.content);
		const type = asString(content.type);
		if (type === 'text') {
			return asString(content.text);
		}
		if (type === 'tool_call_executed') {
			const toolName = asString(content.tool_name) || 'tool';
			const summary = asString(content.summary);
			return summary ? `Action completed: ${toolName}\n${summary}` : `Action completed: ${toolName}`;
		}
		if (type === 'rich_tool_result') {
			const summary = asString(content.summary);
			if (summary) return summary;
		}
		if (type === 'task_status_update') {
			return [asString(content.status), asString(content.summary)].filter(Boolean).join('\n');
		}
		return '';
	}

	function assistantSummaryFromChatResponse(payload: Record<string, unknown>): {
		text: string;
		messageCount: number;
	} {
		const responseMessages = Array.isArray(payload.messages)
			? payload.messages.filter((message): message is Record<string, unknown> => {
					return message != null && typeof message === 'object' && !Array.isArray(message);
				})
			: [];
		const fallbackAssistant = asRecord(payload.assistant_message);
		const messages = fallbackAssistant.id ? [...responseMessages, fallbackAssistant] : responseMessages;
		const assistantText = messages
			.filter((message) => asString(message.direction) === 'assistant')
			.map(chatMessageText)
			.filter((text) => text.trim().length > 0)
			.at(-1);
		return {
			text: assistantText ?? '',
			messageCount: messages.length
		};
	}

	function asEpochMs(value: unknown): number | null {
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value !== 'string' || !value.trim()) return null;
		const parsed = Date.parse(value);
		return Number.isFinite(parsed) ? parsed : null;
	}

	function setExecutionMode(mode: ExecutionMode): void {
		executionMode = mode;
		if (!browser) return;
		const url = new URL(window.location.href);
		if (mode === 'agentic') {
			url.searchParams.delete('mode');
		} else {
			url.searchParams.set('mode', mode);
		}
		void goto(url.pathname + url.search, { replaceState: true, noScroll: true });
		if (mode === 'sota-tests') {
			void checkSotaServer();
		}
	}

	async function checkSotaServer(): Promise<void> {
		try {
			const response = await timedFetch(`${SOTA_TEST_BASE_URL}/01-cross-origin-iframe.html`, { method: 'HEAD' });
			sotaServerStatus = response.ok ? 'running' : 'stopped';
		} catch {
			sotaServerStatus = 'stopped';
		}
	}

	function isDebugExecutionRecord(value: unknown): value is DebugExecutionApiRecord {
		if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
		const record = value as Record<string, unknown>;
		return typeof record.id === 'string';
	}

	function debugExecutionGoalFromTitle(title: string | undefined): string {
		const trimmed = title?.trim() || '';
		if (!trimmed) return '';
		return trimmed.startsWith('Debug: ') ? trimmed.slice('Debug: '.length).trim() : trimmed;
	}

	async function refreshDebugExecutions(): Promise<DebugExecution[]> {
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		try {
			const response = await scopedDebugFetch(`${API_BASE}/executions?limit=200&offset=0`, {}, scope);
			if (!response.ok) {
				throw new Error(`Failed to load executions (${response.status})`);
			}
			const payload = asRecord(await response.json());
			if (isStaleDebugScope(token)) return [];
			const items = Array.isArray(payload.items)
				? payload.items.filter(isDebugExecutionRecord)
				: [];
			const nextExecutions = items
				.filter((item) => (item.title || '').trim().startsWith('Debug: '))
				.sort((left, right) => (right.updated_at ?? right.created_at ?? 0) - (left.updated_at ?? left.created_at ?? 0))
				.map((item) => ({
					id: item.id,
					goal: debugExecutionGoalFromTitle(item.title)
				}));
			if (isStaleDebugScope(token)) return [];
			debugExecutions = nextExecutions;
			return nextExecutions;
		} catch (err) {
			console.error('Failed to load debug executions:', err);
			if (isStaleDebugScope(token)) return [];
			debugExecutions = [];
			return [];
		}
	}

	async function clearDebugData(): Promise<void> {
		if (debugExecutions.length === 0) {
			error = 'No debug executions to clear';
			return;
		}
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		isClearing = true;
		error = null;
		let cleared = 0;
		let failed = 0;
		for (const dt of debugExecutions) {
			if (isStaleDebugScope(token)) return;
			try {
				const response = await scopedDebugFetch(`${API_BASE}/executions/${dt.id}`, {
					method: 'DELETE'
				}, scope);
				if (isStaleDebugScope(token)) return;
				if (response.ok || response.status === 404) {
					cleared += 1;
				} else {
					failed += 1;
				}
			} catch {
				failed += 1;
			}
		}
		if (isStaleDebugScope(token)) return;
		await refreshDebugExecutions();
		if (isStaleDebugScope(token)) return;
		currentStatus = `Cleared ${cleared} debug execution(s)${failed > 0 ? `, ${failed} failed` : ''}`;
		if (!isStaleDebugScope(token)) {
			isClearing = false;
		}
	}

	async function deleteTrackedExecution(idToDelete: string): Promise<void> {
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		currentStatus = `deleting execution ${idToDelete.substring(0, 8)}...`;
		try {
			await scopedDebugFetch(`${API_BASE}/executions/${idToDelete}/execution`, {
				method: 'DELETE'
			}, scope).catch(() => {});
			if (isStaleDebugScope(token)) return;
			const response = await scopedDebugFetch(`${API_BASE}/executions/${idToDelete}`, {
				method: 'DELETE'
			}, scope);
			if (isStaleDebugScope(token)) return;
			if (response.ok || response.status === 404) {
				await refreshDebugExecutions();
				if (isStaleDebugScope(token)) return;
				if (executionId === idToDelete) {
					clearAndReset();
				}
				currentStatus = 'execution deleted';
			} else {
				throw new Error(`Failed to delete: ${response.statusText}`);
			}
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			error = err instanceof Error ? err.message : 'Failed to delete execution';
			currentStatus = 'error';
		}
	}

	async function runGoal(): Promise<void> {
		if (!goal.trim()) {
			error = 'Please enter a goal';
			return;
		}
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		isRunning = true;
		isPaused = false;
		error = null;
		events = [];
		observations = [];
		pauseState = null;
		fetchedScreenshotIds = new Set();
		activeSotaChatSessionId = null;
		sotaChatRunResult = null;
		currentStatus = 'creating execution...';
		try {
			const requestBody: Record<string, unknown> = {
				title: `Debug: ${goal.substring(0, 50)}...`,
				initial_message: goal,
				// v0.6.655 — every run launched from the debug page is
				// test scaffolding (SOTA fixtures, skill action probes,
				// agentic experiments), not work the user wants tracked.
				// Backend hides the resulting V3 task from `/tasks`
				// via `created_by: __system__`.
				debug: true
			};
			if (executionMode === 'agentic' || executionMode === 'sota-tests') {
				requestBody.skip_planning = true;
				requestBody.max_iterations = maxIterations;
				// Scope the run to the selected capability env (e.g. 'browser' for
				// SOTA tests) so the backend restricts the execution to that single
				// tool — runs it directly, no delegation. Was silently dropped before.
				const trimmedEnvMode = agenticEnvMode.trim();
				if (trimmedEnvMode) {
					requestBody.env_mode = trimmedEnvMode;
				}
			}
			const response = await scopedDebugFetch(`${API_BASE}/executions`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify(requestBody)
			}, scope);
			if (!response.ok) {
				throw new Error(`Failed to create execution: ${response.statusText}`);
			}
			if (isStaleDebugScope(token)) return;
			const result = await response.json();
			if (isStaleDebugScope(token)) return;
			executionId = result.execution_id;
			if (executionId) {
				await refreshDebugExecutions();
				if (isStaleDebugScope(token)) return;
				v2Events.connect(executionId);
				window.postMessage(
					{ type: 'magicutor_register_execution', executionId: executionId },
					'*'
				);
				startAutoRefresh();
				currentStatus = 'connected, waiting for execution...';
			}
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			error = err instanceof Error ? err.message : 'Unknown error';
			isRunning = false;
			currentStatus = 'error';
		}
	}

	function newChatTurnId(): string {
		const random = browser && typeof crypto !== 'undefined' && 'randomUUID' in crypto
			? crypto.randomUUID()
			: `${Date.now()}-${Math.random().toString(16).slice(2)}`;
		return `debug-sota-${random}`;
	}

	async function resolveTutorSotaChatSession(
		scope: DebugScopeSnapshot
	): Promise<{ sessionId: string; threadId: string }> {
		const params = new URLSearchParams({
			ui_thread_id: TUTOR_SOTA_THREAD_ID,
			channel: 'web',
			channel_address: 'debug-sota'
		});
		const response = await scopedDebugFetch(`${API_BASE}/chat/active?${params.toString()}`, {}, scope);
		if (!response.ok) {
			const detail = await response.text().catch(() => '');
			throw new Error(`Failed to resolve tutor/copilot chat session: ${detail || response.statusText}`);
		}
		const payload = asRecord(await response.json());
		const session = asRecord(payload.session);
		const sessionId = asString(session.id);
		if (!sessionId) {
			throw new Error('Tutor chat session response did not include a session id');
		}
		return {
			sessionId,
			threadId: asString(session.ui_thread_id) || TUTOR_SOTA_THREAD_ID
		};
	}

	async function sendSotaTutorChatTurn(
		sessionId: string,
		prompt: string,
		attachmentIds: string[],
		chatTurnId: string,
		scope: DebugScopeSnapshot
	): Promise<Record<string, unknown>> {
		const response = await scopedDebugFetch(
			`${API_BASE}/chat/sessions/${encodeURIComponent(sessionId)}/messages`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					text: prompt,
					mode: 'ask',
					chat_turn_id: chatTurnId,
					source_surface: 'screen',
					attachment_ids: attachmentIds
				})
			},
			scope
		);
		if (!response.ok) {
			const detail = await response.text().catch(() => '');
			throw new Error(`Tutor chat run failed: ${detail || response.statusText}`);
		}
		return asRecord(await response.json());
	}

	async function runSotaTutorChatTest(test: SotaTest, prompt: string): Promise<void> {
		const fixtureWindow = await openSotaFixtureForTutorRun(test);
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		isRunning = true;
		isPaused = false;
		error = null;
		events = [];
		observations = [];
		pauseState = null;
		fetchedScreenshotIds = new Set();
		executionId = null;
		sotaChatRunResult = null;
		activeSotaChatSessionId = null;
		currentStatus = fixtureWindow
			? 'opened fixture, preparing screen capture...'
			: 'resolving #screens tutor/copilot chat session...';
		try {
			await launchDesktopAppForTutorRun(test, scope);
			if (isStaleDebugScope(token)) return;
			const stagedCapture = await stageSotaTutorCapture(test, fixtureWindow, scope);
			if (isStaleDebugScope(token)) return;
			const sessionTarget = stagedCapture ?? await resolveTutorSotaChatSession(scope);
			if (isStaleDebugScope(token)) return;
			const chatTurnId = newChatTurnId();
			const sessionId = sessionTarget.sessionId;
			const threadId = sessionTarget.threadId;
			const attachmentIds = stagedCapture?.attachmentIds ?? [];
			const captureRect = stagedCapture?.captureRect ?? null;
			activeSotaChatSessionId = sessionId;
			sotaChatRunResult = {
				sessionId,
				chatTurnId,
				threadId,
				prompt,
				attachmentIds,
				captureRect,
				assistantText: 'Tutor chat is running. Open the session to inspect live progress, or watch the visible app/fixture for overlays.',
				queued: false,
				messageCount: 0
			};
			currentStatus = 'running tutor/copilot chat runtime...';
			if (fixtureWindow && !fixtureWindow.closed) {
				fixtureWindow.focus();
				await waitMs(100);
			}
			const payload = await sendSotaTutorChatTurn(sessionId, prompt, attachmentIds, chatTurnId, scope);
			if (isStaleDebugScope(token)) return;
			const summary = assistantSummaryFromChatResponse(payload);
			const queued = !!payload.queued;
			sotaChatRunResult = {
				sessionId,
				chatTurnId,
				threadId,
				prompt,
				attachmentIds,
				captureRect,
				assistantText: summarizeText(summary.text, queued ? 'Message queued behind an active chat turn.' : 'No assistant text returned.'),
				queued,
				messageCount: summary.messageCount
			};
			currentStatus = queued ? 'visual chat queued' : `visual chat completed: ${test.name}`;
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			error = err instanceof Error ? err.message : 'Unknown tutor/copilot chat error';
			currentStatus = 'error';
		} finally {
			if (!isStaleDebugScope(token)) {
				isRunning = false;
			}
		}
	}

	async function cancelExecution(): Promise<void> {
		if (!executionId && activeSotaChatSessionId) {
			await cancelSotaTutorChatRun();
			return;
		}
		if (!executionId) return;
		const targetExecutionId = executionId;
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		try {
			const response = await coordinateExecutionControl(targetExecutionId, () =>
				scopedDebugFetch(
					`${TASKS_API_BASE}/executions/${encodeURIComponent(targetExecutionId)}/cancel`,
					{ method: 'POST' },
					scope
				)
			);
			if (isStaleDebugScope(token)) return;
			await requireExecutionControlResponse(response, 'Could not cancel the execution.');
			if (isStaleDebugScope(token)) return;
			currentStatus = 'cancelled';
			isRunning = false;
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			currentStatus = `cancel error: ${err}`;
		}
	}

	async function cancelSotaTutorChatRun(): Promise<void> {
		if (!activeSotaChatSessionId) return;
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		const sessionId = activeSotaChatSessionId;
		currentStatus = 'cancelling tutor/copilot chat...';
		try {
			const response = await scopedDebugFetch(
				`${API_BASE}/chat/sessions/${encodeURIComponent(sessionId)}/run`,
				{ method: 'DELETE' },
				scope
			);
			if (isStaleDebugScope(token)) return;
			if (response.ok) {
				currentStatus = 'visual chat cancelled';
				isRunning = false;
			}
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			currentStatus = `cancel error: ${err}`;
		}
	}

	async function resumeExecution(): Promise<void> {
		if (!executionId) return;
		const targetExecutionId = executionId;
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		isPaused = false;
		isRunning = true;
		currentStatus = 'resuming...';
		window.postMessage(
			{ type: 'magicutor_register_execution', executionId: targetExecutionId },
			'*'
		);
		try {
			const response = await coordinateExecutionControl(targetExecutionId, () =>
				scopedDebugFetch(
					`${API_BASE}/executions/${encodeURIComponent(targetExecutionId)}/execution/agentic-continue`,
					{
						method: 'POST',
						headers: { 'Content-Type': 'application/json' },
						body: JSON.stringify({})
					},
					scope
				)
			);
			if (isStaleDebugScope(token)) return;
			if (!response.ok) {
				const text = await response.text();
				if (isStaleDebugScope(token)) return;
				currentStatus = `resume failed: ${text.slice(0, 100)}`;
				isPaused = true;
				isRunning = false;
			}
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			currentStatus = `resume error: ${err}`;
			isPaused = true;
			isRunning = false;
		}
	}

	async function cancelPausedExecution(): Promise<void> {
		if (!executionId) return;
		const targetExecutionId = executionId;
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		const previousPaused = isPaused;
		const previousRunning = isRunning;
		isPaused = false;
		currentStatus = 'cancelling...';
		try {
			const response = await coordinateExecutionControl(targetExecutionId, () =>
				scopedDebugFetch(
					`${TASKS_API_BASE}/executions/${encodeURIComponent(targetExecutionId)}/cancel`,
					{ method: 'POST' },
					scope
				)
			);
			if (isStaleDebugScope(token)) return;
			await requireExecutionControlResponse(response, 'Could not cancel the paused execution.');
			if (isStaleDebugScope(token)) return;
			currentStatus = 'cancelled';
			isRunning = false;
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			currentStatus = `cancel error: ${err}`;
			isPaused = previousPaused;
			isRunning = previousRunning;
		}
	}

	function scopeHeaders(): Record<string, string> {
		const scope = get(scopeIdentityStore);
		const headers: Record<string, string> = {};
		return headers;
	}

	function paramsFromYaml(raw: unknown): SkillActionParam[] {
		if (!Array.isArray(raw)) return [];
		return raw
			.map((entry) => {
				const p = asRecord(entry);
				return {
					name: asString(p.name),
					type: asString(p.param_type) || 'string',
					description: typeof p.description === 'string' ? p.description : undefined,
					required: p.required === true,
					default: p.default,
					enum: Array.isArray(p.enum_values) ? p.enum_values.map(String) : undefined,
				} as SkillActionParam;
			})
			.filter((p) => p.name);
	}

	function actionParamsFromOverrides(
		paramNames: unknown,
		overrides: unknown,
		required: unknown,
	): SkillActionParam[] {
		if (!Array.isArray(paramNames)) return [];
		const overrideMap = asRecord(overrides);
		const requiredSet = new Set(
			Array.isArray(required) ? required.map(String) : [],
		);
		return paramNames.map((rawName) => {
			const name = String(rawName);
			const ov = asRecord(overrideMap[name]);
			const items = asRecord(ov.items);
			return {
				name,
				type: typeof ov.type === 'string' ? ov.type : 'string',
				description: typeof ov.description === 'string' ? ov.description : undefined,
				required: requiredSet.has(name),
				default: ov.default,
				enum: Array.isArray(ov.enum) ? ov.enum.map(String) : undefined,
				items: typeof items.type === 'string' ? { type: items.type } : undefined,
			};
		});
	}

	async function loadSkillSchema(name: string): Promise<void> {
		if (!name) {
			skillSchema = null;
			selectedAction = '';
			skillParamValues = {};
			return;
		}
		skillSchemaLoading = true;
		skillSchemaError = null;
		try {
			const res = await timedFetch(`/api/magician/v2/skills/${encodeURIComponent(name)}/schema`, {
				headers: scopeHeaders(),
			});
			if (!res.ok) {
				const detail = await res.text().catch(() => '');
				throw new Error(`schema fetch failed (${res.status}): ${detail || res.statusText}`);
			}
			const raw = await res.json();
			const rawRecord = asRecord(raw);
			const actionsRaw = asRecord(rawRecord.native_action_schemas);
			const actions: SkillActionDef[] = Object.keys(actionsRaw)
				.sort()
				.map((key) => {
					const def = asRecord(actionsRaw[key]);
					return {
						name: key,
						description: typeof def.description === 'string' ? def.description : '',
						argv: Array.isArray(def.argv) ? def.argv.map(String) : [],
						parameters: actionParamsFromOverrides(
							def.parameters,
							def.parameter_overrides,
							def.required,
						),
					};
				});
			const topLevelParams = paramsFromYaml(rawRecord.parameters);
			skillSchema = {
				name: asString(rawRecord.name) || name,
				description: typeof rawRecord.description === 'string' ? rawRecord.description : '',
				topLevelParams,
				actions,
			};
			selectedAction = actions.length > 0 ? actions[0].name : '';
			skillParamValues = {};
			// Seed session-param defaults from the YAML so the form
			// shows e.g. `connection_mode: cdp` without the operator
			// having to retype it. Empty values still mean "skip" on
			// the backend (browser session prelude only fires when
			// `connection_mode` is non-empty).
			const seeded: Record<string, string> = {};
			for (const param of topLevelParams) {
				if (param.default !== undefined && param.default !== null) {
					seeded[param.name] = String(param.default);
				}
			}
			skillSessionParamValues = seeded;
		} catch (err) {
			skillSchemaError = err instanceof Error ? err.message : String(err);
			skillSchema = null;
		} finally {
			skillSchemaLoading = false;
		}
	}

	function buildArgvFromAction(action: SkillActionDef): string[] {
		const args: string[] = [];
		for (const param of action.parameters) {
			const raw = skillParamValues[param.name];
			if (raw === undefined || raw === '') continue;
			if (param.type === 'array') {
				// One arg per non-empty line.
				const tokens = raw
					.split('\n')
					.map((s) => s.trim())
					.filter((s) => s.length > 0);
				args.push(...tokens);
			} else {
				args.push(raw);
			}
		}
			return args;
		}

	async function runSkillAction(): Promise<void> {
		if (!selectedSkill) {
			error = 'Pick a skill first.';
			return;
		}
		const action = skillSchema?.actions.find((a) => a.name === selectedAction);
		const token = captureDebugScopeToken();
		isRunning = true;
		error = null;
		skillRunResult = null;
		currentStatus = `running ${selectedSkill}${action ? ` ${action.name}` : ''}…`;
		try {
			const args = action ? buildArgvFromAction(action) : [];
			const sessionParams: Record<string, string> = {};
			for (const [key, value] of Object.entries(skillSessionParamValues)) {
				if (value !== undefined && value !== '') sessionParams[key] = value;
			}
			const res = await timedFetch(
				`/api/magician/v2/skills/${encodeURIComponent(selectedSkill)}/run`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json', ...scopeHeaders() },
					body: JSON.stringify({
						action: action?.name || null,
						args,
						session_params: sessionParams,
					}),
				},
			);
			if (isStaleDebugScope(token)) return;
			const result = await res.json().catch(() => null);
			if (!res.ok) {
				const reason = result?.reason || result?.message || res.statusText;
				throw new Error(`run failed (${res.status}): ${reason}`);
			}
			skillRunResult = result;
			currentStatus = result?.success ? 'action completed' : `exit ${result?.exit_code ?? '?'}`;
			if (!result?.success && result?.stderr) {
				error = result.stderr.split('\n').slice(0, 3).join(' / ');
			}
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			error = err instanceof Error ? err.message : 'Unknown error';
			currentStatus = 'error';
		} finally {
			if (isStaleDebugScope(token)) return;
			isRunning = false;
		}
	}

	function clearSkillRunState(): void {
		skillRunResult = null;
		error = null;
		currentStatus = 'idle';
	}

	function startAutoRefresh(): void {
		stopAutoRefresh();
		refreshInterval = setInterval(() => {
			if (executionId) {
				void refreshArtifacts();
			}
		}, 2000);
	}

	function stopAutoRefresh(): void {
		if (refreshInterval) {
			clearInterval(refreshInterval);
			refreshInterval = null;
		}
	}

	async function fetchScreenshotForObservation(
		observation: Record<string, unknown>,
		scope: DebugScopeSnapshot = currentDebugScopeSnapshot()
	): Promise<void> {
		if (!executionId) return;
		const activeExecutionId = executionId;
		const observationId = asString(observation.observation_id);
		if (!observationId) return;
		try {
			const response = await scopedDebugFetch(
				`${API_BASE}/executions/${activeExecutionId}/observations/${observationId}/screenshot`,
				{},
				scope
			);
			if (!response.ok || executionId !== activeExecutionId) return;
			const data = await response.json();
			if (executionId !== activeExecutionId) return;
			observations = observations.map((entry) => {
				if (asString(entry.observation_id) !== observationId) return entry;
				return { ...entry, screenshot_url: `data:image/png;base64,${data.screenshot}` };
			});
		} catch {
			// best effort
		}
	}

	async function refreshArtifacts(): Promise<void> {
		if (!executionId) return;
		const activeExecutionId = executionId;
		const requestId = ++artifactRefreshRequestId;
		const scope = currentDebugScopeSnapshot();
		isLoadingArtifacts = true;
		try {
			const [obsRes, pauseRes] = await Promise.all([
				scopedDebugFetch(`${API_BASE}/executions/${activeExecutionId}/observations`, {}, scope),
				scopedDebugFetch(`${API_BASE}/executions/${activeExecutionId}/pause-state`, {}, scope)
			]);

			if (obsRes.ok) {
				const data = await obsRes.json();
				if (requestId !== artifactRefreshRequestId || executionId !== activeExecutionId) return;
				const newObs = (Array.isArray(data.observations) ? data.observations : []) as Array<Record<string, unknown>>;
				const oldScreenshotUrls = new Map(
					observations
						.filter((entry) => asString(entry.screenshot_url))
						.map((entry) => [asString(entry.observation_id), asString(entry.screenshot_url)])
				);
				observations = newObs.map((entry) => {
					const observationId = asString(entry.observation_id);
					return {
						...entry,
						screenshot_url: oldScreenshotUrls.get(observationId) || asString(entry.screenshot_url)
					};
				});
				for (const observation of observations) {
					const observationId = asString(observation.observation_id);
					if (!observationId) continue;
					if (observation.has_screenshot === false) continue;
					if (fetchedScreenshotIds.has(observationId)) continue;
					fetchedScreenshotIds.add(observationId);
					void fetchScreenshotForObservation(observation, scope);
				}
			}

			if (pauseRes.ok) {
				const nextPauseState = await pauseRes.json();
				if (requestId === artifactRefreshRequestId && executionId === activeExecutionId) {
					pauseState = nextPauseState;
				}
			}
		} catch {
			// best effort
		} finally {
			if (requestId === artifactRefreshRequestId && executionId === activeExecutionId) {
				isLoadingArtifacts = false;
			}
		}
	}

	async function fetchDurableArtifacts(): Promise<void> {
		const requestId = ++durableArtifactsFetchId;
		const scope = currentDebugScopeSnapshot();
		isLoadingDurableArtifacts = true;
		try {
			const res = await scopedDebugFetch(`${API_BASE}/artifacts/durable`, {}, scope);
			if (requestId === durableArtifactsFetchId && res.ok) {
				const data = await res.json();
				durableArtifacts = Array.isArray(data.artifacts) ? data.artifacts : [];
			}
		} catch (e) {
			console.error('Failed to fetch durable artifacts:', e);
		} finally {
			if (requestId === durableArtifactsFetchId) {
				isLoadingDurableArtifacts = false;
			}
		}
	}

	async function fetchDurableArtifactContent(namespace: string, name: string): Promise<void> {
		const requestId = ++durableArtifactContentFetchId;
		const scope = currentDebugScopeSnapshot();
		isLoadingDurableContent = true;
		durableArtifactContent = null;
		durableContentError = null;
		try {
			const res = await scopedDebugFetch(
				`${API_BASE}/artifacts/durable/${encodeURIComponent(namespace)}/${encodeURIComponent(name)}`,
				{},
				scope
			);
			if (requestId !== durableArtifactContentFetchId) return;
			if (res.ok) {
				durableArtifactContent = await res.json();
			} else {
				const errData = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
				durableContentError = errData.error || `Failed to load artifact (${res.status})`;
			}
		} catch (e) {
			console.error('Failed to fetch durable artifact content:', e);
			if (requestId === durableArtifactContentFetchId) {
				durableContentError = 'Network error loading artifact';
			}
		} finally {
			if (requestId === durableArtifactContentFetchId) {
				isLoadingDurableContent = false;
			}
		}
	}

	function handleEvents(allEvents: V2WebSocketEvent[]): void {
		const newEvents = allEvents.filter((event) => getV2EventSequence(event) > lastProcessedEventSequence);
		if (newEvents.length > 0) {
			lastProcessedEventSequence = getV2EventSequence(newEvents[newEvents.length - 1]);
		}

		const myEvents = executionId
			? newEvents.filter((event) => 'execution_id' in (event.data as Record<string, unknown>) && (event.data as Record<string, unknown>).execution_id === executionId)
			: newEvents;
		if (myEvents.length === 0) return;

		events = [...events, ...myEvents];
		for (const event of myEvents) {
			const data = event.data as Record<string, unknown>;
			switch (event.event_type) {
				case 'AgenticExecutionStarted':
					currentStatus = 'executing...';
					break;
				case 'AgenticIterationStarted':
					currentStatus = `iteration ${asString(data.iteration)}...`;
					break;
				case 'AgenticPageUnderstanding':
					currentStatus = `analyzing page (${asString(data.page_stage)})...`;
					break;
				case 'AgenticDecisionMade':
					currentStatus = `decided: ${asString(data.action_summary) || asString(data.decision_type)}`;
					break;
				case 'AgenticActionExecuted':
					currentStatus = data.success ? 'action succeeded' : `action failed: ${asString(data.error)}`;
					break;
					case 'AgenticExecutionCompleted':
						currentStatus = `completed: ${asString(data.outcome)}`;
						isRunning = false;
						stopAutoRefresh();
						void refreshArtifacts();
						void fetchDurableArtifacts();
						break;
				case 'AgenticWaitingForUser':
					currentStatus = `waiting for input: ${asString(data.question)}`;
					isPaused = true;
					isRunning = false;
					void refreshArtifacts();
					void fetchDurableArtifacts();
					break;
				case 'AgenticMaxIterationsReached':
					currentStatus = `max iterations (${asString(data.iterations_used)}) reached — paused`;
					isPaused = true;
					isRunning = false;
					void refreshArtifacts();
					void fetchDurableArtifacts();
					break;
				case 'ExecutionStatusChanged':
					if (asString(data.new_status) === 'Executing') {
						isPaused = false;
						isRunning = true;
					}
					if (asString(data.new_status) === 'WaitingUser' || asString(data.new_status) === 'WaitingChildren' || asString(data.new_status) === 'Paused') {
						isPaused = true;
						isRunning = false;
					}
					break;
					case 'ExecutionCancelled':
						currentStatus = 'cancelled';
						isRunning = false;
						stopAutoRefresh();
						break;
				}
			}
		}

	function clearAndReset(): void {
		executionId = null;
		activeSotaChatSessionId = null;
		sotaChatRunResult = null;
		events = [];
		observations = [];
		pauseState = null;
		currentStatus = 'idle';
		error = null;
		isRunning = false;
		isPaused = false;
		lastProcessedEventSequence = 0;
		fetchedScreenshotIds = new Set();
		v2Events.clear();
		v2Events.disconnect();
		v2Events.connectGlobal();
		stopAutoRefresh();
	}

	async function reattachExecution(tid: string): Promise<void> {
		if (tid === executionId) {
			activeTab = 'observations';
			await refreshArtifacts();
			return;
		}
		events = [];
		observations = [];
		pauseState = null;
		error = null;
		isRunning = false;
		isPaused = false;
		lastProcessedEventSequence = 0;
		fetchedScreenshotIds = new Set();
		activeSotaChatSessionId = null;
		sotaChatRunResult = null;
		executionId = tid;
		currentStatus = `reattached to ${tid.substring(0, 8)}...`;
		activeTab = 'observations';
		await refreshArtifacts();
	}

	async function deleteCurrentExecution(): Promise<void> {
		if (!executionId) return;
		const idToDelete = executionId;
		const token = captureDebugScopeToken();
		const scope = currentDebugScopeSnapshot();
		isRunning = true;
		currentStatus = 'deleting execution...';
		try {
			await scopedDebugFetch(`${API_BASE}/executions/${idToDelete}/execution`, {
				method: 'DELETE'
			}, scope).catch(() => {});
			if (isStaleDebugScope(token)) return;
			const response = await scopedDebugFetch(`${API_BASE}/executions/${idToDelete}`, {
				method: 'DELETE'
			}, scope);
			if (isStaleDebugScope(token)) return;
			if (response.ok || response.status === 404) {
				await refreshDebugExecutions();
				if (isStaleDebugScope(token)) return;
				clearAndReset();
				currentStatus = 'execution deleted';
			} else {
				throw new Error(`Failed to delete: ${response.statusText}`);
			}
		} catch (err) {
			if (isStaleDebugScope(token)) return;
			error = err instanceof Error ? err.message : 'Failed to delete execution';
			currentStatus = 'error';
		} finally {
			if (isStaleDebugScope(token)) return;
			isRunning = false;
		}
	}

	async function runSotaTestAgentic(
		test: SotaTest,
		options: { useSuggestedGoal?: boolean } = {}
	): Promise<void> {
		selectedSotaTest = test;
		if (options.useSuggestedGoal || !sotaTestBox.trim()) {
			sotaTestBox = test.suggestedGoal;
		}
		const effectiveGoal = options.useSuggestedGoal
			? test.suggestedGoal
			: sotaTestBox.trim() || test.suggestedGoal;
		const dispatchGoal = buildSotaDispatchGoal(test, effectiveGoal);
		if (isTutorSotaTest(test)) {
			goal = effectiveGoal;
			await runSotaTutorChatTest(test, effectiveGoal);
			return;
		}
		goal = dispatchGoal;
		if (test.desktop) {
			// Live-app desktop case: goal is self-contained — no fixture URL.
			// Desktop grounding defaults to shell-only; app tutor cases set
			// envMode=null so tutor/screen-draw/mac-operator tools remain available.
			agenticEnvMode = test.envMode === undefined ? 'shell' : (test.envMode ?? '');
		} else {
			agenticEnvMode = test.envMode ?? '';
		}
		await runGoal();
	}

	function formatEventPreview(event: V2WebSocketEvent): string {
		const data = event.data as Record<string, unknown>;
		const keys = Object.keys(data).slice(0, 5);
		if (keys.length === 0) return '';
		return keys
			.map((key) => {
				const value = data[key];
				if (typeof value === 'object') return `${key}: ${JSON.stringify(value).slice(0, 120)}`;
				return `${key}: ${String(value).slice(0, 120)}`;
			})
			.join(' | ');
	}

	function stringify(value: unknown): string {
		try {
			return JSON.stringify(value, null, 2);
		} catch {
			return String(value);
		}
	}

	function buildModeButtons(): MuijComponent[] {
		return [
			{
				id: 'veil-mode-agentic',
				component_type: 'Button',
				label: 'Agentic (LLM Loop)',
				props: {
					interactive: true,
					variant: 'outline',
					size: 'sm',
					className: executionMode === 'agentic' ? 'veil-pill-btn is-active' : 'veil-pill-btn'
				}
			},
			{
				id: 'veil-mode-direct',
				component_type: 'Button',
				label: 'Direct Actions',
				props: {
					interactive: true,
					variant: 'outline',
					size: 'sm',
					className: executionMode === 'direct' ? 'veil-pill-btn is-active' : 'veil-pill-btn'
				}
			},
			{
				id: 'veil-mode-sota',
				component_type: 'Button',
				label: `SOTA tests (${sotaTests.length})`,
				props: {
					interactive: true,
					variant: 'outline',
					size: 'sm',
					className: executionMode === 'sota-tests' ? 'veil-pill-btn is-active' : 'veil-pill-btn'
				}
			}
		];
	}

	const CAPABILITY_ICONS: Record<string, string> = {
		browser: '🌐', http: '🔗', file: '🗂️', bash: '💻', shell: '💻',
		gmail: '📧', calendar: '📅', sheets: '📊', search: '🔍', websearch: '🔍',
		duckdb: '🦆', jq: '⚙️', grep: '🔎', rg: '🔎', sed: '✂️', awk: '✂️',
		ocr: '👁️', pdftotext: '📄', htmltotext: '📝', csvkit: '📊',
		telegram: '✈️', whatsapp: '💬', treasurer: '🔐', files: '🗂️',
	};

	function buildAgenticEnvironmentButtons(): MuijComponent[] {
		// Use dynamic capability packs if loaded, otherwise fall back to defaults
		const packs = capabilityPacks.length > 0
			? capabilityPacks
			: DEFAULT_ENV_MODES.map(name => ({ name }));

		return packs.map(pack => ({
			id: `veil-env-${pack.name}`,
			component_type: 'Button' as const,
			label: `${CAPABILITY_ICONS[pack.name] || '🔧'} ${pack.name}`,
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				className: agenticEnvMode === pack.name ? 'veil-env-btn is-active' : 'veil-env-btn'
			}
		}));
	}

	function buildFormFieldFromParam(
		param: SkillActionParam,
		current: string,
	): Record<string, unknown> {
		const labelSuffix = param.required ? ' *' : '';
		const description = param.description ? param.description : undefined;
		if (param.enum && param.enum.length > 0) {
			return {
				id: param.name,
				label: `${param.name}${labelSuffix}`,
				type: 'select',
				required: param.required,
				value: current || (param.default != null ? String(param.default) : ''),
				options: param.enum.map((v) => ({ value: v, label: v })),
				placeholder: description,
			};
		}
		if (param.type === 'array') {
			return {
				id: param.name,
				label: `${param.name}${labelSuffix}`,
				type: 'textarea',
				required: param.required,
				value: current,
				rows: 4,
				placeholder: description ?? 'one argv token per line',
			};
		}
		if (param.type === 'integer' || param.type === 'number') {
			return {
				id: param.name,
				label: `${param.name}${labelSuffix}`,
				type: 'number',
				required: param.required,
				value: current,
				placeholder: description ?? (param.default != null ? String(param.default) : ''),
			};
		}
		return {
			id: param.name,
			label: `${param.name}${labelSuffix}`,
			type: 'text',
			required: param.required,
			value: current,
			placeholder: description ?? (param.default != null ? String(param.default) : ''),
		};
	}

	function buildSkillSessionFields(): Array<Record<string, unknown>> {
		const params = skillSchema?.topLevelParams ?? [];
		// Skip names already declared on the selected action. Some
		// skills (codex) repeat e.g. `working_dir` at both top-level
		// (cwd template) and per-action (arg_mapping) so we'd otherwise
		// render two identical inputs that point at the same value.
		const action = skillSchema?.actions.find((a) => a.name === selectedAction);
		const dupeNames = new Set((action?.parameters ?? []).map((p) => p.name));
		return params
			.filter((p) => !dupeNames.has(p.name))
			.map((param) =>
				buildFormFieldFromParam(param, skillSessionParamValues[param.name] ?? ''),
			);
	}

	function buildSkillActionFields(): Array<Record<string, unknown>> {
		const action = skillSchema?.actions.find((a) => a.name === selectedAction);
		const params = action?.parameters ?? [];
		return params.map((param) =>
			buildFormFieldFromParam(param, skillParamValues[param.name] ?? ''),
		);
	}

	function applySkillSessionFormValues(values: Record<string, unknown>): void {
		const next: Record<string, string> = { ...skillSessionParamValues };
		for (const [key, raw] of Object.entries(values)) {
			next[key] = asString(raw);
		}
		skillSessionParamValues = next;
	}

	function applySkillFormValues(values: Record<string, unknown>): void {
		const nextAction: Record<string, string> = { ...skillParamValues };
		const nextSession: Record<string, string> = { ...skillSessionParamValues };
		for (const [key, raw] of Object.entries(values)) {
			if (key.startsWith('__session__:')) {
				nextSession[key.slice('__session__:'.length)] = asString(raw);
			} else {
				nextAction[key] = asString(raw);
			}
		}
		skillParamValues = nextAction;
		skillSessionParamValues = nextSession;
	}

	function buildVeilSurface(_deps: Record<string, unknown>): MuijComponent[] {
		void _deps;
		const modeShellChildren: MuijComponent[] = [
			{
				id: 'veil-mode-buttons',
				component_type: 'Stack',
				props: {
					className: 'veil-mode-tabs',
					direction: 'row',
					gap: '0.65rem',
					wrap: true
				},
				children: buildModeButtons()
			}
		];

		if (executionMode === 'agentic') {
			modeShellChildren.push(
				{
					id: 'veil-agentic-env-row',
					component_type: 'Stack',
					props: {
						className: 'veil-env-row',
						direction: 'row',
						align: 'center',
						gap: '0.65rem',
						wrap: true
					},
					children: [
						{
							id: 'veil-agentic-env-label',
							component_type: 'Text',
							props: {
								children: 'Environment:',
								variant: 'body'
							}
						},
						{
							id: 'veil-agentic-env-buttons',
							component_type: 'Stack',
							props: {
								direction: 'row',
								gap: '0.45rem',
								wrap: true
							},
							children: buildAgenticEnvironmentButtons()
						}
					]
				},
				{
					id: 'veil-agentic-config-row',
					component_type: 'Stack',
					props: {
						className: 'veil-sota-config-row',
						direction: 'row',
						align: 'end',
						gap: '0.7rem',
						wrap: true
					},
					children: [
						{
							id: 'veil-agentic-goal-wrap',
							component_type: 'Stack',
							props: {
								className: 'veil-sota-test-box-wrap',
								direction: 'column',
								gap: '0.25rem'
							},
							children: [
								{
									id: 'veil-agentic-goal-box',
									component_type: 'TextArea',
									label: 'Goal',
									props: {
										value: goal,
										rows: 2,
										placeholder: "Enter your goal, e.g., 'Navigate to google.com and search for weather'"
									}
								}
							]
						},
						{
							id: 'veil-agentic-right-controls',
							component_type: 'Stack',
							props: {
								className: 'veil-sota-right-controls',
								direction: 'column',
								align: 'end',
								gap: '0.45rem'
							},
							children: [
								{
									id: 'veil-agentic-max-iterations-wrap',
									component_type: 'Stack',
									props: {
										className: 'veil-sota-max-iterations-wrap',
										direction: 'column',
										gap: '0.25rem'
									},
									children: [
										{
											id: 'veil-agentic-max-iterations',
											component_type: 'NumberField',
											label: 'Max Iterations',
											props: {
												value: maxIterations,
												min: 1,
												max: MAX_AGENTIC_MAX_ITERATIONS,
												step: 1
											}
										}
									]
								},
								{
									id: 'veil-agentic-run',
									component_type: 'Button',
									label: isRunning ? 'Running...' : 'Run Agentic',
									props: {
										interactive: true,
										variant: 'primary',
										size: 'sm',
										disabled: isRunning || !goal.trim(),
										className: 'veil-sota-run-selected-btn'
									}
								}
							]
							}
						]
					}
				);
			}

		if (executionMode === 'direct') {
			// Truncate option labels — native <select> popups auto-size to
			// the longest option text and CSS can't constrain that.
			// Cap at 26 chars so the popup never overflows the 18rem cell.
			const truncate = (s: string, n = 26) =>
				s.length <= n ? s : `${s.slice(0, n - 1)}…`;
			const skillOptions = capabilityPacks.map((pack) => ({
				value: pack.name,
				label: `${CAPABILITY_ICONS[pack.name] || '🔧'} ${truncate(pack.name, 24)}`,
			}));
			const actionOptions = (skillSchema?.actions ?? []).map((a) => ({
				value: a.name,
				label: truncate(a.name),
			}));

			modeShellChildren.push({
				id: 'veil-skill-action-select-row',
				component_type: 'Stack',
				props: {
					className: 'veil-direct-action-select-row',
					direction: 'row',
					align: 'end',
					gap: '0.7rem',
					wrap: true,
				},
				children: [
					{
						id: 'veil-skill-select',
						component_type: 'Select',
						label: 'Skill',
						props: {
							interactive: true,
							value: selectedSkill,
							options: skillOptions,
							placeholder: 'Pick a skill',
						},
					},
					...(actionOptions.length > 0
						? [{
								id: 'veil-action-select',
								component_type: 'Select' as const,
								label: 'Action',
								props: {
									interactive: true,
									value: selectedAction,
									options: actionOptions,
									placeholder: 'Pick an action',
								},
							}]
						: []),
				],
			});

			if (skillSchemaLoading) {
				modeShellChildren.push({
					id: 'veil-skill-schema-loading',
					component_type: 'Text',
					props: { text: 'Loading schema…', size: 'sm' },
				});
			} else if (skillSchemaError) {
				modeShellChildren.push({
					id: 'veil-skill-schema-error',
					component_type: 'Text',
					props: { text: `Schema error: ${skillSchemaError}`, size: 'sm', tone: 'error' },
				});
			} else if (selectedSkill && skillSchema) {
				const action = skillSchema.actions.find((a) => a.name === selectedAction);
				const description = action?.description || skillSchema.description || '';
				if (description) {
					modeShellChildren.push({
						id: 'veil-skill-action-description',
						component_type: 'Text',
						props: { text: description, size: 'sm' },
					});
				}
				// Compose one form with session params first (browser
				// uses connection_mode / cdp_url / url as session
				// config — backend translates into agent-browser prelude
				// steps before the primitive runs) followed by the
				// per-action params. Session params are name-prefixed
				// with `__session__:` so the submit handler can split
				// them back out cleanly.
				const sessionFields = buildSkillSessionFields().map((f) => ({
					...f,
					id: `__session__:${(f as { id: string }).id}`,
				}));
				const actionFields = buildSkillActionFields();
				const fields = [...sessionFields, ...actionFields];
				modeShellChildren.push({
					id: 'veil-skill-form',
					component_type: 'Form',
					label: '',
					props: {
						title: '',
						showSubmit: true,
						submitLabel: isRunning ? 'Running…' : 'Run',
						disabled: isRunning || !selectedSkill,
						idBase: 'veil-skill-form',
						fields,
					},
				});
			}
		}

		if (executionMode === 'sota-tests') {
			const fixtureStatusText =
				sotaServerStatus === 'running'
					? 'Available via Vite'
					: sotaServerStatus === 'stopped'
						? 'Unavailable'
						: 'Checking...';
			const fixtureStatusClass =
				sotaServerStatus === 'running'
					? 'is-running'
					: sotaServerStatus === 'stopped'
						? 'is-stopped'
						: 'is-unknown';

			modeShellChildren.push(
				{
					id: 'veil-sota-config-row',
					component_type: 'Stack',
					props: {
						className: 'veil-sota-config-row',
						direction: 'row',
						align: 'end',
						gap: '0.7rem',
						wrap: true
					},
					children: [
						{
							id: 'veil-sota-test-box-wrap',
							component_type: 'Stack',
							props: {
								className: 'veil-sota-test-box-wrap',
								direction: 'column',
								gap: '0.25rem'
							},
							children: [
								{
									id: 'veil-sota-goal-box',
									component_type: 'TextArea',
									label: 'Test box',
									props: {
										value: sotaTestBox,
										rows: 2,
										placeholder: 'Test execution objective'
									}
								}
							]
						},
						{
							id: 'veil-sota-right-controls',
							component_type: 'Stack',
							props: {
								className: 'veil-sota-right-controls',
								direction: 'column',
								align: 'end',
								gap: '0.45rem'
							},
							children: [
								{
									id: 'veil-sota-max-iterations-wrap',
									component_type: 'Stack',
									props: {
										className: 'veil-sota-max-iterations-wrap',
										direction: 'column',
										gap: '0.25rem'
									},
									children: [
										{
											id: 'veil-sota-max-iterations',
											component_type: 'NumberField',
											label: 'Max Iterations',
											props: {
												value: maxIterations,
												min: 1,
												max: MAX_AGENTIC_MAX_ITERATIONS,
												step: 1
											}
										}
									]
								},
									{
										id: 'veil-sota-run-selected',
										component_type: 'Button',
										label: isRunning
											? 'Running...'
											: selectedSotaTest && isTutorSotaTest(selectedSotaTest)
												? 'Run Visual Chat'
												: 'Run Agentic',
										props: {
											interactive: true,
											variant: 'primary',
										size: 'sm',
										disabled: isRunning || !sotaTestBox.trim() || filteredSotaTests.length === 0,
										className: 'veil-sota-run-selected-btn'
									}
								}
							]
						}
					]
				},
				{
					id: 'veil-sota-fixture-row',
					component_type: 'Stack',
					props: {
						className: 'veil-sota-fixture-row',
						direction: 'row',
						justify: 'space-between',
						align: 'center',
						gap: '0.55rem'
					},
					children: [
						{
							id: 'veil-sota-fixture-status-wrap',
							component_type: 'Stack',
							props: {
								direction: 'row',
								align: 'center',
								gap: '0.42rem',
								className: 'veil-sota-fixture-status-wrap'
							},
							children: [
								{
									id: 'veil-sota-fixture-dot',
									component_type: 'Text',
									props: {
										children: '●',
										variant: 'body',
										className: `veil-sota-fixture-dot ${fixtureStatusClass}`
									}
								},
								{
									id: 'veil-sota-fixture-status',
									component_type: 'Text',
									props: {
										children: `Test Fixtures: ${fixtureStatusText}`,
										variant: 'body',
										className: 'veil-sota-fixture-status'
									}
								}
							]
						},
						{
							id: 'veil-sota-check-server',
							component_type: 'Button',
							label: 'Refresh',
							props: {
								interactive: true,
								variant: 'outline',
								size: 'sm',
								className: 'veil-sota-fixture-refresh'
							}
						}
					]
				},
				{
					id: 'veil-sota-actions',
					component_type: 'Stack',
					props: {
						direction: 'row',
						gap: '0.5rem',
						wrap: true,
						className: 'veil-sota-groups-row'
					},
					children: [
						...sotaGroups.map((group) => ({
							id: `veil-sota-group:${encodeURIComponent(group)}`,
							component_type: 'Button' as const,
							label: `${group} (${sotaGroupCounts[group] ?? 0})`,
							props: {
								interactive: true,
								variant: activeGroup === group ? 'primary' : 'outline',
								size: 'sm',
								className: activeGroup === group ? 'veil-sota-group-btn is-active' : 'veil-sota-group-btn'
							}
						}))
					]
				},
				{
					id: 'veil-sota-test-grid',
					component_type: 'Stack',
					props: {
						className: 'veil-sota-test-grid',
						direction: 'row',
						gap: '0.7rem',
						wrap: true
					},
					children: filteredSotaTests.map((test) => ({
						id: `veil-sota-card-${test.id}`,
						component_type: 'Card',
						props: {
							title: '',
							subtitle: '',
							body: '',
							className: 'veil-sota-test-card'
						},
						children: [
							{
								id: `veil-sota-meta-${test.id}`,
								component_type: 'Stack',
								props: {
									className: 'veil-sota-test-meta-row',
									direction: 'row',
									justify: 'space-between',
									align: 'center'
								},
								children: [
									{
										id: `veil-sota-phase-chip-${test.id}`,
										component_type: 'Tag',
										props: {
											text: test.phase,
											color: 'info',
											className: 'veil-sota-test-phase-chip'
										}
									},
									{
										id: `veil-sota-id-${test.id}`,
										component_type: 'Text',
										props: {
											children: `#${test.id}`,
											variant: 'caption',
											className: 'veil-sota-test-id'
										}
									}
								]
							},
							{
								id: `veil-sota-title-${test.id}`,
								component_type: 'Text',
								props: {
									children: test.name,
									variant: 'heading',
									className: 'veil-sota-test-title'
								}
							},
							{
								id: `veil-sota-desc-${test.id}`,
								component_type: 'Text',
								props: {
									children: test.description,
									variant: 'body',
									className: 'veil-sota-test-description'
								}
							},
							{
								id: `veil-sota-actions-row-${test.id}`,
								component_type: 'Stack',
								props: {
									className: 'veil-sota-test-actions',
									direction: 'row',
									gap: '0.5rem'
								},
								children: [
									{
										id: `veil-sota-open:${test.id}`,
										component_type: 'Button',
										label: test.desktop ? 'Live app' : 'Open ↗',
										props: {
											interactive: true,
											variant: 'primary',
											size: 'sm',
											disabled: test.desktop,
											className: 'veil-sota-test-action-btn'
										}
									},
										{
											id: `veil-sota-run:${test.id}`,
											component_type: 'Button',
											label: isTutorSotaTest(test) ? 'Run Visual Chat' : 'Run Agentic',
											props: {
												interactive: true,
												variant: 'primary',
												size: 'sm',
												disabled: isRunning || (!test.desktop && sotaServerStatus !== 'running'),
												className: 'veil-sota-test-action-btn'
											}
									}
								]
							}
						]
					}))
				}
			);
		}

		const components: MuijComponent[] = [
			{
				id: 'veil-header',
				component_type: 'Card',
				props: {
					title: '',
					subtitle: '',
					body: '',
					className: 'veil-header-card'
				},
				children: [
					{
						id: 'veil-header-top-row',
						component_type: 'Stack',
						props: {
							direction: 'row',
							align: 'center',
							justify: 'space-between',
							gap: '1rem'
						},
						children: [
							{
								id: 'veil-header-title-stack',
								component_type: 'Stack',
								props: {
									direction: 'column',
									gap: '0.2rem'
								},
								children: [
									{
										id: 'veil-header-title',
										component_type: 'Text',
										props: {
											children: 'Agentic Execution Debug',
											className: 'veil-header-title-text'
										}
									},
									{
										id: 'veil-header-subtitle',
										component_type: 'Text',
										props: {
											children: 'Test browser automation - agentic loop or direct single actions',
											variant: 'caption'
										}
									}
								]
							},
							{
								id: 'veil-status-pill',
								component_type: 'Tag',
								props: {
									text: `● Status: ${currentStatus}`,
									color: isRunning ? 'success' : 'default',
									className: 'veil-status-pill'
								}
							}
						]
					}
				]
			},
			{
				id: 'veil-mode-shell',
				component_type: 'Card',
				props: {
					title: '',
					subtitle: '',
					body: '',
					className: 'veil-mode-shell-card'
				},
				children: modeShellChildren
			},
			{
				id: 'veil-tutor-cursive-preview-card',
				component_type: 'Card',
				props: {
					title: 'Tutor cursive preview',
					subtitle: 'Vector glyph draft',
					body: 'Review path-backed cursive letters, joins, and common words before wiring them into tutor cursive_text.',
					className: 'veil-debug-tool-card'
				},
				children: [
					{
						id: 'veil-open-tutor-cursive-preview',
						component_type: 'Button',
						label: 'Open Cursive Preview',
						props: {
							interactive: true,
							variant: 'secondary',
							size: 'sm'
						}
					}
				]
			}
		];

			if (error) {
				components.push({
					id: 'veil-error',
					component_type: 'Alert',
					props: {
					type: 'error',
					message: error,
					closable: false
				}
				});
			}

			if (sotaChatRunResult) {
				components.push({
					id: 'veil-sota-chat-result',
					component_type: 'Card',
					props: {
						title: 'Tutor/Copilot chat runtime run',
						subtitle: `${sotaChatRunResult.threadId} · ${sotaChatRunResult.sessionId}`,
						body: sotaChatRunResult.queued
							? 'The message was queued behind an active chat turn.'
							: 'Executed through the same chat-session tutor/copilot runtime used by HUD/screen prompts.',
						className: 'veil-sota-chat-result-card'
					},
					children: [
						{
							id: 'veil-sota-chat-result-actions',
							component_type: 'Stack',
							props: { direction: 'row', gap: '0.5rem', wrap: true },
							children: [
								{
									id: 'veil-sota-chat-open-session',
									component_type: 'Button',
									label: 'Open Chat Session',
									props: {
										interactive: true,
										variant: 'secondary',
										size: 'sm'
									}
								},
								{
									id: 'veil-sota-chat-cancel',
									component_type: 'Button',
									label: 'Cancel Chat Run',
									props: {
										interactive: true,
										variant: 'outline',
										size: 'sm',
										disabled: !isRunning
									}
								}
							]
						},
						{
							id: 'veil-sota-chat-result-meta',
							component_type: 'Text',
							props: {
								text: `turn: ${sotaChatRunResult.chatTurnId} · messages: ${sotaChatRunResult.messageCount} · attachments: ${sotaChatRunResult.attachmentIds.length} · crop: ${formatRegionRect(sotaChatRunResult.captureRect)}`,
								size: 'sm'
							}
						},
						{
							id: 'veil-sota-chat-result-summary',
							component_type: 'Text',
							props: {
								text: sotaChatRunResult.assistantText,
								size: 'sm'
							}
						}
					]
				});
			}

			if (executionMode === 'direct' && skillRunResult) {
			const exitLabel =
				skillRunResult.exit_code === null
					? '(no exit code)'
					: `exit ${skillRunResult.exit_code}`;
			// Browser session prelude steps (connection_mode + url)
			// run before the primary primitive. Surface them so the
			// operator sees what was set up.
			for (const [idx, step] of (skillRunResult.prelude ?? []).entries()) {
				const stepExit =
					step.exit_code === null ? '(no exit code)' : `exit ${step.exit_code}`;
				components.push({
					id: `veil-skill-run-prelude-${idx}`,
					component_type: 'Text',
					props: {
						text: `prelude · ${step.label} · ${stepExit} · ${step.duration_ms}ms · ${step.argv.join(' ')}`,
						size: 'sm',
					},
				});
			}
			components.push({
				id: 'veil-skill-run-meta',
				component_type: 'Text',
				props: {
					text: `${exitLabel} · ${skillRunResult.duration_ms}ms · argv: ${skillRunResult.argv.join(' ')}`,
					size: 'sm',
				},
			});
			if (skillRunResult.stdout) {
				components.push({
					id: 'veil-skill-run-stdout',
					component_type: 'CodeBlock',
					label: 'stdout',
					props: {
						language: skillRunResult.parsed_json ? 'json' : 'text',
						code: skillRunResult.parsed_json
							? stringify(skillRunResult.parsed_json)
							: skillRunResult.stdout,
						showLineNumbers: true,
					},
				});
			}
			if (skillRunResult.stderr) {
				components.push({
					id: 'veil-skill-run-stderr',
					component_type: 'CodeBlock',
					label: 'stderr',
					props: {
						language: 'text',
						code: skillRunResult.stderr,
						showLineNumbers: true,
					},
				});
			}
		}

		components.push({
			id: 'veil-debug-executions',
			component_type: 'Card',
			props: {
				title: 'Tracked debug executions',
				subtitle: `${debugExecutions.length} tracked execution(s)`,
				body: 'Reattach to previous debug sessions or purge stored executions.',
				className: 'veil-debug-executions-card'
			},
			children: [
				{
					id: 'veil-debug-execution-actions',
					component_type: 'Stack',
					props: { direction: 'row', gap: '0.5rem', wrap: true },
					children: [
							{ id: 'veil-clear-view', component_type: 'Button', label: 'Clear', props: { interactive: true, variant: 'outline', size: 'sm' } },
							{ id: 'veil-debug-clear-all', component_type: 'Button', label: isClearing ? 'Clearing...' : 'Clear All', props: { interactive: true, variant: 'outline', size: 'sm', disabled: isClearing || debugExecutions.length === 0 } },
							{ id: 'veil-agentic-cancel', component_type: 'Button', label: 'Cancel Current Run', props: { interactive: true, variant: 'outline', size: 'sm', disabled: !isRunning || (!executionId && !activeSotaChatSessionId) } },
							{ id: 'veil-delete-current-execution', component_type: 'Button', label: 'Delete Execution', props: { interactive: true, variant: 'outline', size: 'sm', disabled: !executionId || isRunning } },
						{
							id: 'veil-refresh-artifacts',
							component_type: 'Button',
							label: isLoadingArtifacts ? 'Loading...' : 'Refresh Artifacts',
							props: {
								interactive: true,
								variant: 'secondary',
								size: 'sm',
								disabled: !executionId || isLoadingArtifacts
							}
						}
					]
				},
				...(debugExecutions.length === 0
					? [
							{
								id: 'veil-debug-executions-empty',
								component_type: 'EmptyState',
								props: {
									title: 'No tracked executions',
									description: 'Run agentic execution to create and track debug executions.'
								}
							}
						]
					: debugExecutions.map((dt) => ({
							id: `veil-debug-execution-${dt.id}`,
							component_type: 'Card',
							props: {
								title: dt.id,
								subtitle: dt.goal || 'no goal stored',
								body: executionId === dt.id ? 'active execution' : 'inactive execution'
							},
							children: [
								{ id: `veil-debug-reattach:${dt.id}`, component_type: 'Button', label: 'Reattach', props: { interactive: true, variant: 'secondary', size: 'sm' } },
								{ id: `veil-debug-delete:${dt.id}`, component_type: 'Button', label: 'Delete', props: { interactive: true, variant: 'outline', size: 'sm' } }
							]
						}))
				)
			]
		});

		components.push({
			id: 'veil-tab-actions',
			component_type: 'Stack',
			props: { direction: 'row', gap: '0.9rem', wrap: true, className: 'veil-runtime-tabs' },
			children: [
				{
					id: 'veil-tab-events',
					component_type: 'Button',
					label: `Events (${events.length})`,
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						className: activeTab === 'events' ? 'veil-runtime-tab-btn is-active' : 'veil-runtime-tab-btn'
					}
				},
					{
						id: 'veil-tab-observations',
						component_type: 'Button',
						label: `Observations (${observations.length})`,
						props: {
							interactive: true,
							variant: 'outline',
							size: 'sm',
							className: activeTab === 'observations' ? 'veil-runtime-tab-btn is-active' : 'veil-runtime-tab-btn'
						}
					},
					{
						id: 'veil-tab-pause',
						component_type: 'Button',
						label: `Pause ${pauseState?.has_pause_state ? '(active)' : '(none)'}`,
						props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						className: activeTab === 'pause' ? 'veil-runtime-tab-btn is-active' : 'veil-runtime-tab-btn'
					}
				},
				{
					id: 'veil-tab-durable',
					component_type: 'Button',
					label: `Durable (${durableArtifacts.length})`,
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						className: activeTab === 'durable' ? 'veil-runtime-tab-btn is-active' : 'veil-runtime-tab-btn'
					}
				}
			]
		});

		if (activeTab === 'events') {
			components.push({
				id: 'veil-events-card',
				component_type: 'Card',
				props: {
					title: 'Event log',
					subtitle: `${events.length} event(s)`,
					body: 'Recent runtime events for the current execution.'
				},
				children: [
					events.length === 0
						? {
								id: 'veil-events-empty',
								component_type: 'EmptyState',
								props: {
									title: 'No events yet',
									description: 'Run an execution to populate the event log.'
								}
							}
						: {
								id: 'veil-events-table',
								component_type: 'Table',
								props: {
									columns: [
										{ key: 'index', label: '#' },
										{ key: 'type', label: 'Type' },
										{ key: 'preview', label: 'Preview' }
									],
									rows: events.map((event, index) => ({
										index: String(index + 1),
										type: event.event_type,
										preview: formatEventPreview(event)
									}))
								}
							}
				]
			});
		} else if (activeTab === 'observations') {
			components.push({
				id: 'veil-observations-card',
				component_type: 'Card',
				props: {
					title: 'Observations',
					subtitle: `${observations.length} observation(s)`,
					body: isLoadingArtifacts ? 'Refreshing artifacts...' : 'Latest captured browser observations.'
				},
				children: observations.length === 0
					? [
							{
								id: 'veil-observations-empty',
								component_type: 'EmptyState',
								props: {
									title: 'No observations yet',
									description: 'Run execution and refresh artifacts to capture observations.'
								}
							}
						]
					: observations.slice(0, 15).map((obs, index) => ({
							id: `veil-observation-card-${index}-${asString(obs.observation_id) || 'obs'}`,
							component_type: 'Card',
							props: {
								title: asString(obs.last_action) || `Observation ${index + 1}`,
								subtitle: asString(obs.url) || asString(obs.page_stage) || 'n/a',
								body: asString(obs.captured_at) ? new Date(asString(obs.captured_at)).toLocaleString() : 'unknown capture time'
							},
							children: [
								...(asString(obs.screenshot_url)
									? [
											{
												id: `veil-observation-image-${index}-${asString(obs.observation_id) || 'obs'}`,
												component_type: 'Image' as const,
												props: {
													src: asString(obs.screenshot_url),
													alt: `Observation ${index + 1}`,
													fit: 'contain'
												}
											}
									]
									: []),
								{
									id: `veil-observation-meta-${index}-${asString(obs.observation_id) || 'obs'}`,
									component_type: 'DataList',
									props: {
										items: [
											{ id: `veil-observation-meta-id-${index}`, key: 'Observation ID', value: asString(obs.observation_id) },
											{ id: `veil-observation-meta-trigger-${index}`, key: 'Trigger', value: asString(obs.trigger) || 'n/a' },
											{ id: `veil-observation-meta-step-${index}`, key: 'Step', value: asString(obs.step_index) || 'n/a' }
										]
									}
								}
							]
						}))
			});
		} else if (activeTab === 'pause') {
			components.push({
				id: 'veil-pause-card',
				component_type: 'Card',
				props: {
					title: 'Pause state',
					subtitle: pauseState?.has_pause_state ? 'active pause state' : 'no active pause',
					body: 'Current pause-state payload for user continuation.'
				},
				children: [
					pauseState
						? {
								id: 'veil-pause-code',
								component_type: 'CodeBlock',
								props: {
									language: 'json',
									code: stringify(pauseState),
									showLineNumbers: true
								}
							}
						: {
								id: 'veil-pause-empty',
								component_type: 'EmptyState',
								props: {
									title: 'No pause state',
									description: 'No pause-state payload found. Refresh artifacts to retry.'
								}
							}
				]
			});
		} else if (activeTab === 'durable') {
			const durableChildren: MuijComponent[] = [
				{
					id: 'veil-durable-refresh',
					component_type: 'Button',
					label: isLoadingDurableArtifacts ? 'Loading...' : 'Refresh',
					props: {
						interactive: true,
						variant: 'secondary',
						size: 'sm',
						disabled: isLoadingDurableArtifacts
					}
				}
			];
			if (durableArtifacts.length === 0) {
				durableChildren.push({
					id: 'veil-durable-empty',
					component_type: 'EmptyState',
					props: {
						title: 'No durable artifacts',
						description: 'Durable artifacts persist across runs. Agents write to the shared workspace to create them.'
					}
				});
			} else {
				for (const a of durableArtifacts) {
					const artId = `${a.namespace}/${a.name}`;
					durableChildren.push({
						id: `veil-durable-item:${a.namespace}:${a.name}`,
						component_type: 'Card',
						props: {
							title: artId,
							subtitle: a.content_type || 'unknown type',
							body: a.last_updated ? `Updated ${new Date(a.last_updated).toLocaleString()} by ${a.last_updated_by || 'unknown'}` : ''
						},
						children: [
							{
								id: `veil-durable-view:${a.namespace}:${a.name}`,
								component_type: 'Button',
								label: 'View',
								props: { interactive: true, variant: 'secondary', size: 'sm' }
							}
						]
					});
				}
			}
			if (durableArtifactContent) {
				durableChildren.push({
					id: 'veil-durable-preview',
					component_type: 'Card',
					props: {
						title: `${durableArtifactContent.namespace}/${durableArtifactContent.name}`,
						subtitle: `Updated by ${durableArtifactContent.last_updated_by} at ${new Date(durableArtifactContent.last_updated).toLocaleString()}`,
						body: durableArtifactContent.content_type || 'text'
					},
					children: [
						{
							id: 'veil-durable-preview-content',
							component_type: 'CodeBlock',
							props: {
								language: durableArtifactContent.content_type === 'application/json' ? 'json' : 'markdown',
								code: durableArtifactContent.content,
								showLineNumbers: true
							}
						}
					]
				});
			} else if (isLoadingDurableContent) {
				durableChildren.push({
					id: 'veil-durable-loading',
					component_type: 'Text',
					props: { children: 'Loading artifact content...', variant: 'body' }
				});
			} else if (durableContentError) {
				durableChildren.push({
					id: 'veil-durable-error',
					component_type: 'Text',
					props: { children: durableContentError, variant: 'caption' }
				});
			}
			components.push({
				id: 'veil-durable-card',
				component_type: 'Card',
				props: {
					title: 'Durable artifacts',
					subtitle: `${durableArtifacts.length} artifact(s)`,
					body: 'Artifacts that persist across execution runs.'
				},
				children: durableChildren
			});
		}

		return components;
	}

	async function handleSurfaceInteraction(event: CustomEvent<MuijInteractionEventDetail>): Promise<void> {
		const detail = event?.detail;
		if (!detail) return;
		const componentId = asString(detail.componentId);

		if (detail.interaction === 'action') {
			if (
				componentId === 'veil-mode-agentic'
				|| componentId.endsWith(':veil-mode-agentic')
				|| componentId.includes('veil-mode-agentic')
			) {
				setExecutionMode('agentic');
				return;
			}
			if (
				componentId === 'veil-mode-direct'
				|| componentId.endsWith(':veil-mode-direct')
				|| componentId.includes('veil-mode-direct')
			) {
				setExecutionMode('direct');
				return;
			}
			if (
				componentId === 'veil-mode-sota'
				|| componentId.endsWith(':veil-mode-sota')
				|| componentId.includes('veil-mode-sota')
			) {
				setExecutionMode('sota-tests');
				return;
			}
			// Dynamic capability environment buttons: veil-env-{packName}
			const envMatch = componentId.match(/veil-env-([a-z0-9_-]+)/);
			if (envMatch) {
				agenticEnvMode = envMatch[1];
				return;
			}
			if (componentId === 'veil-agentic-run' || componentId.endsWith(':veil-agentic-run')) {
				if (!goal.trim()) {
					error = 'Please enter a goal';
					return;
				}
				await runGoal();
				return;
			}
				if (componentId === 'veil-agentic-cancel' || componentId.endsWith(':veil-agentic-cancel')) {
					await cancelExecution();
					return;
				}
				if (componentId === 'veil-sota-chat-cancel' || componentId.endsWith(':veil-sota-chat-cancel')) {
					await cancelSotaTutorChatRun();
					return;
				}
			if (componentId === 'veil-sota-chat-open-session' || componentId.endsWith(':veil-sota-chat-open-session')) {
				if (sotaChatRunResult) {
					await goto(`/t/${encodeURIComponent(sotaChatRunResult.threadId)}/chat?session=${encodeURIComponent(sotaChatRunResult.sessionId)}`);
				}
				return;
			}
			if (componentId === 'veil-open-tutor-cursive-preview' || componentId.endsWith(':veil-open-tutor-cursive-preview')) {
				await goto('/debug/tutor-cursive');
				return;
			}
				if (componentId === 'veil-agentic-resume' || componentId.endsWith(':veil-agentic-resume')) {
					await resumeExecution();
					return;
				}
			if (componentId === 'veil-agentic-cancel-paused' || componentId.endsWith(':veil-agentic-cancel-paused')) {
				await cancelPausedExecution();
				return;
			}
			if (componentId === 'veil-delete-current-execution' || componentId.endsWith(':veil-delete-current-execution')) {
				await deleteCurrentExecution();
				return;
			}
			if (componentId === 'veil-clear-view' || componentId.endsWith(':veil-clear-view')) {
				clearAndReset();
				return;
			}
			if (componentId === 'veil-refresh-artifacts' || componentId.endsWith(':veil-refresh-artifacts')) {
				await refreshArtifacts();
				return;
			}
			if (componentId === 'veil-direct-clear' || componentId.endsWith(':veil-direct-clear')) {
				clearSkillRunState();
				return;
			}
			if (componentId === 'veil-sota-check-server' || componentId.endsWith(':veil-sota-check-server')) {
				await checkSotaServer();
				return;
			}
			if (componentId === 'veil-debug-clear-all' || componentId.endsWith(':veil-debug-clear-all')) {
				await clearDebugData();
				return;
			}
			if (componentId === 'veil-tab-events' || componentId.endsWith(':veil-tab-events')) {
				activeTab = 'events';
				return;
			}
			if (componentId === 'veil-tab-observations' || componentId.endsWith(':veil-tab-observations')) {
				activeTab = 'observations';
				return;
			}
			if (componentId === 'veil-tab-pause' || componentId.endsWith(':veil-tab-pause')) {
				activeTab = 'pause';
				return;
			}
			if (componentId === 'veil-tab-durable' || componentId.endsWith(':veil-tab-durable')) {
				activeTab = 'durable';
				void fetchDurableArtifacts();
				return;
			}
			if (componentId === 'veil-durable-refresh' || componentId.endsWith(':veil-durable-refresh')) {
				void fetchDurableArtifacts();
				return;
			}
			{
				const viewMatch = /(?:^|:)veil-durable-view:([^:]+):(.+)$/.exec(componentId);
				if (viewMatch && viewMatch[1] && viewMatch[2]) {
					void fetchDurableArtifactContent(viewMatch[1], viewMatch[2]);
					return;
				}
			}

			const groupMatch = /(?:^|:)veil-sota-group:(.+)$/.exec(componentId);
			if (groupMatch && groupMatch[1]) {
				try {
					const decoded = decodeURIComponent(groupMatch[1]);
					if ((sotaGroups as readonly string[]).includes(decoded)) {
						activeGroup = decoded;
					}
				} catch {
					// noop
				}
				return;
			}

			const openTestMatch = /(?:^|:)veil-sota-open:(.+)$/.exec(componentId);
			if (openTestMatch && openTestMatch[1] && browser) {
				const test = sotaTests.find((entry) => entry.id === openTestMatch[1]);
				if (test && !test.desktop) {
					window.open(`${SOTA_TEST_BASE_URL}/${test.file}`, '_blank', 'noopener,noreferrer');
				}
				return;
			}

			const runTestMatch = /(?:^|:)veil-sota-run:(.+)$/.exec(componentId);
			if (runTestMatch && runTestMatch[1]) {
				const test = sotaTests.find((entry) => entry.id === runTestMatch[1]);
				if (test) {
					await runSotaTestAgentic(test, { useSuggestedGoal: true });
				}
				return;
			}
			if (componentId === 'veil-sota-run-selected' || componentId.endsWith(':veil-sota-run-selected')) {
				const trimmed = sotaTestBox.trim();
				if (!trimmed) {
					return;
				}
				// Only the per-test Run button (`veil-sota-run:<id>`) prepends
				// `Navigate to ${SOTA_TEST_BASE_URL}/${test.file} and then ` —
				// that's the explicit "run this fixture" path. The global
				// Run Agentic button used to fall back to
				// `filteredSotaTests[0]` when no test was selected, which
				// silently prepended the first test in the currently-filtered
				// group to a freeform prompt. That was surprising and is now
				// removed: with no explicit selection, the global button
				// runs the prompt as written.
				if (selectedSotaTest) {
					await runSotaTestAgentic(selectedSotaTest);
				} else {
					agenticEnvMode = 'browser';
					goal = trimmed;
					await runGoal();
				}
				return;
			}

			const reattachMatch = /(?:^|:)veil-debug-reattach:(.+)$/.exec(componentId);
			if (reattachMatch && reattachMatch[1]) {
				await reattachExecution(reattachMatch[1]);
				return;
			}

			const deleteMatch = /(?:^|:)veil-debug-delete:(.+)$/.exec(componentId);
			if (deleteMatch && deleteMatch[1]) {
				await deleteTrackedExecution(deleteMatch[1]);
				return;
			}
		}

		if (detail.interaction === 'change') {
			if (
				componentId === 'veil-skill-select'
				|| componentId.endsWith(':veil-skill-select')
				|| componentId.includes('veil-skill-select')
			) {
				const payload = asRecord(detail.detail);
				const changed = asString(readChangedValue(payload)).trim();
				if (changed !== selectedSkill) {
					selectedSkill = changed;
					skillParamValues = {};
					skillRunResult = null;
					void loadSkillSchema(changed);
				}
				return;
			}
			if (
				componentId === 'veil-action-select'
				|| componentId.endsWith(':veil-action-select')
				|| componentId.includes('veil-action-select')
			) {
				const payload = asRecord(detail.detail);
				const changed = asString(readChangedValue(payload)).trim();
				if (changed !== selectedAction) {
					selectedAction = changed;
					skillParamValues = {};
					skillRunResult = null;
				}
				return;
			}
			if (
				componentId === 'veil-sota-max-iterations'
				|| componentId === 'veil-agentic-max-iterations'
				|| componentId.endsWith(':veil-sota-max-iterations')
				|| componentId.endsWith(':veil-agentic-max-iterations')
				|| componentId.includes('veil-sota-max-iterations')
				|| componentId.includes('veil-agentic-max-iterations')
			) {
				const payload = asRecord(detail.detail);
				const changed = readChangedValue(payload);
				maxIterations = Math.max(1, Math.min(MAX_AGENTIC_MAX_ITERATIONS, Math.round(asNumber(changed, maxIterations))));
				return;
			}
			if (
				componentId === 'veil-agentic-goal-box'
				|| componentId.endsWith(':veil-agentic-goal-box')
				|| componentId.includes('veil-agentic-goal-box')
			) {
				const payload = asRecord(detail.detail);
				const changed = readChangedValue(payload);
				if (changed !== undefined) {
					goal = asString(changed);
				}
				return;
			}
			if (
				componentId === 'veil-sota-goal-box'
				|| componentId.endsWith(':veil-sota-goal-box')
				|| componentId.includes('veil-sota-goal-box')
			) {
				const payload = asRecord(detail.detail);
				const changed = readChangedValue(payload);
				if (changed !== undefined) {
					sotaTestBox = asString(changed);
				}
				return;
			}
		}

		if (
			detail.interaction === 'submit'
			&& (
				detail.componentId === 'veil-agentic-form'
				|| detail.componentId.endsWith(':veil-agentic-form')
				|| detail.componentId.includes('veil-agentic-form')
			)
		) {
			const payload = asRecord(detail.detail);
			const values = asRecord(payload.values);
			const submittedGoal = asString(values.goal).trim();
			const submittedEnv = asString(values.env_mode).trim();
			const submittedMaxIterations = asNumber(values.max_iterations, maxIterations);
			if (submittedGoal) {
				goal = submittedGoal;
			}
			if (submittedEnv && (DEFAULT_ENV_MODES.includes(submittedEnv) || capabilityPacks.some(p => p.name === submittedEnv))) {
				agenticEnvMode = submittedEnv;
			}
			maxIterations = Math.max(1, Math.min(MAX_AGENTIC_MAX_ITERATIONS, Math.round(submittedMaxIterations)));
			await runGoal();
			return;
		}

		if (
			detail.interaction === 'submit'
			&& (
				detail.componentId === 'veil-skill-form'
				|| detail.componentId.endsWith(':veil-skill-form')
				|| detail.componentId.includes('veil-skill-form')
			)
		) {
			const payload = asRecord(detail.detail);
			const values = asRecord(payload.values);
			applySkillFormValues(values);
			await runSkillAction();
		}

	}

	async function hydrateScopedDebugData(
		token: { generation: number; scopeKey: string } = captureDebugScopeToken()
	): Promise<void> {
		void fetchDurableArtifacts();
		const executions = await refreshDebugExecutions();
		if (isStaleDebugScope(token)) return;
		if (executions.length > 0 && !executionId) {
			await reattachExecution(executions[0]?.id || '');
			if (isStaleDebugScope(token)) return;
		}
		if (executionMode === 'sota-tests') {
			void checkSotaServer();
		}
	}

	onMount(() => {
		if (!browser) return;
		debugMounted = true;
		lastDebugScopeKey = currentDebugScopeKey;
		loadCapabilityPacks();
		const urlMode = new URL(window.location.href).searchParams.get('mode');
		if (urlMode === 'direct' || urlMode === 'sota-tests') {
			executionMode = urlMode;
		}
		void hydrateScopedDebugData();
		if (!v2Events.isConnected) {
			v2Events.connectGlobal();
		}
		eventUnsubscribe = v2Events.subscribe((allEvents) => {
			if (allEvents && allEvents.length > 0) {
				handleEvents(allEvents);
			}
		});
	});

	onDestroy(() => {
		if (eventUnsubscribe) {
			eventUnsubscribe();
		}
		v2Events.disconnect();
		stopAutoRefresh();
	});

	$: currentDebugScopeKey = `${scopedDebugValue('principal')}:${scopedDebugValue('workspace')}`;
	$: if (browser && debugMounted && currentDebugScopeKey !== lastDebugScopeKey) {
		lastDebugScopeKey = currentDebugScopeKey;
		clearScopedDebugState();
		void hydrateScopedDebugData();
	}
</script>

<svelte:head>
	<title>Debug · Magican</title>
</svelte:head>

		<div class="debug-page presto-gaui-page">
			<div class="veil-content-shell">
				<nav class="debug-subnav" aria-label="Debug pages">
					<span class="debug-subnav__label">Debug pages</span>
					<a href="/debug/notify">🔔 Notify overlay</a>
					<a href="/debug/voice">◉ Voice</a>
					<a href="/debug/tasks">🗂 Tasks</a>
					<a href="/debug/update-cards">🃏 Update cards</a>
					<a href="/debug/tutor-cursive">✍️ Tutor cursive</a>
				</nav>
				<MuijRenderer {components} on:interaction={handleSurfaceInteraction} />

		</div>
	</div>

<style>
	.veil-content-shell {
		width: min(100%, 1280px);
		margin: 0 auto;
	}

	/* On-page index of the sibling debug routes (the generative surface below
	   doesn't link them; previously only the command palette did). */
	.debug-subnav {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px;
		margin-bottom: 16px;
		padding: 10px 12px;
		border: 1px solid var(--border-soft, rgba(128, 128, 128, 0.25));
		border-radius: 10px;
		background: var(--bg-soft, rgba(128, 128, 128, 0.06));
	}

	.debug-subnav__label {
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-muted, #8a8a8a);
		margin-right: 4px;
	}

	.debug-subnav a {
		font-size: 0.85rem;
		font-weight: 600;
		color: inherit;
		text-decoration: none;
		padding: 5px 10px;
		border-radius: 7px;
		border: 1px solid var(--border-soft, rgba(128, 128, 128, 0.3));
		background: var(--bg-surface, transparent);
		transition:
			background 120ms ease,
			border-color 120ms ease;
	}

	.debug-subnav a:hover {
		background: var(--bg-soft, rgba(128, 128, 128, 0.12));
		border-color: var(--border-default, rgba(128, 128, 128, 0.5));
	}

</style>
