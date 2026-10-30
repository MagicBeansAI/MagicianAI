<script lang="ts">
	import { HOST_APP_NAME } from '$lib/presentationIdentity';
	import { onMount } from 'svelte';
	import { getCurrentScopeIdentity, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';

	type TargetStateId =
		| 'selection'
		| 'selection-field'
		| 'empty-context'
		| 'empty-no-context'
		| 'page-context'
		| 'files'
		| 'draft'
		| 'secure'
		| 'excluded'
		| 'unsupported';
	type AssistDisplay = 'hidden' | 'chip' | 'menu';

	interface ActionOption {
		id: string;
		label: string;
		description: string;
		kind: 'primary' | 'secondary';
		intent?: string;
		requiresContext?: boolean;
		requiresObservation?: boolean;
		requiresScreenshot?: boolean;
		requiresWritableTarget?: boolean;
		mutatesText?: boolean;
		createsTask?: boolean;
		opensHud?: boolean;
	}

	interface ActionState {
		id: TargetStateId;
		label: string;
		actions: ActionOption[];
	}

	interface TargetState {
		id: TargetStateId;
		label: string;
		target: string;
		app: string;
		windowTitle: string;
		status: string;
		body: string;
		chipVisible: boolean;
		suppression?: string;
		actions: ActionOption[];
	}

	interface AssistSettings {
		enabled: boolean;
		show_on_selected_text: boolean;
		show_in_writable_fields: boolean;
		explicit_hotkey_only: boolean;
		default_personality: string;
	}

	interface NativeAssistTarget {
		state: TargetStateId;
		app?: string;
		window_title?: string;
		url?: string;
		context_text?: string;
		source?: string;
		frame_url?: string;
		browser_tab_id?: number;
		browser_window_id?: number;
		window_rect?: {
			x: number;
			y: number;
			width: number;
			height: number;
		};
	}

	interface NativeAssistDisplay {
		display: AssistDisplay;
		target?: NativeAssistTarget | null;
	}

	interface NativeAssistDismissRequest {
		reason?: string;
	}

	interface ContextualAssistActionResponse {
		status: string;
		action: ActionOption;
		context: {
			state: TargetStateId;
			personality: string;
			app?: string;
			windowTitle?: string;
			url?: string;
			contextText?: string;
			hasContextText: boolean;
		};
		routing: {
			agentId: string;
			sourceKind: string;
			sourceKey: string;
			sessionKey: string;
			rootUrl?: string;
			targetTextKind: string;
			actionIntent: string;
			personality: string;
		};
		visualContext: {
			screenshot: {
				policy: string;
				required: boolean;
				source: string;
				reason: string;
				degraded: boolean;
				windowRect?: {
					x: number;
					y: number;
					width: number;
					height: number;
				};
				browserTabId?: number;
				browserWindowId?: number;
			};
		};
		message: string;
		nextBinding: string;
		draftText?: string;
		sessionId?: string;
		threadId?: string;
		attachmentIds?: string[];
		screenshotAttachmentId?: string;
		provenance?: ContextualAssistProvenance;
	}

	interface ContextualAssistProvenance {
		generatedAt?: string;
		action?: {
			id?: string;
			label?: string;
			intent?: string;
			userGuidanceSupplied?: boolean;
			createsTask?: boolean;
		};
		source?: {
			kind?: string;
			key?: string;
			app?: string;
			windowTitle?: string;
			url?: string;
			rootUrl?: string;
		};
		target?: {
			state?: string;
			textKind?: string;
			hasContextText?: boolean;
			contextTextChars?: number;
			contextTextPreview?: string;
		};
		persona?: {
			agentId?: string;
			personality?: string;
		};
		visual?: {
			screenshotPolicy?: string;
			screenshotRequired?: boolean;
			screenshotSupplied?: boolean;
			screenshotReused?: boolean;
			screenshotSource?: string;
			screenshotDegraded?: boolean;
			screenshotReason?: string;
			screenshotAttachmentId?: string;
		};
		memory?: {
			agentScopedMemory?: boolean;
			sharedUserMemoryAllowed?: boolean;
			durableMemoryContentsExposed?: boolean;
			note?: string;
		};
		usage?: {
			calls?: number;
			provider?: string;
			model?: string;
			profile?: string;
			inputTokens?: number;
			outputTokens?: number;
			reasoningTokens?: number;
			cacheReadTokens?: number;
			cacheCreationTokens?: number;
			totalTokens?: number;
			costUsd?: number | null;
		};
		chat?: {
			sessionId?: string;
			threadId?: string;
			sessionTitle?: string;
			userMessageId?: string;
			assistantMessageId?: string;
			attachmentIds?: string[];
			screenshotAttachmentId?: string;
		};
	}

	interface SkillListEntry {
		name: string;
		description?: string;
		kind: string;
		layer?: string;
	}

	interface SkillListResponse {
		skills?: SkillListEntry[];
	}

	interface PersonalityOption {
		id: string;
		label: string;
	}

	const DEFAULT_PERSONALITY_OPTIONS: PersonalityOption[] = [
		{ id: 'active', label: 'Active personality' },
	];

	const targetStates: TargetState[] = [
		{
			id: 'selection',
			label: 'Selected text',
			target: 'Selected paragraph',
			app: 'Current app',
			windowTitle: 'Visible document',
			status: 'Selection',
			body: 'Text selection is active.',
			chipVisible: true,
			actions: [
				action('rewrite', 'Rewrite', 'Rewrite the selected text.', 'primary'),
				action('summarize', 'Summarize', 'Summarize the selected text.', 'primary'),
				action('draft_reply', 'Reply', 'Draft a reply using visible context.', 'primary'),
				action('create_task', 'Task', 'Create an unscheduled task from this text.', 'secondary'),
				action('schedule_followup', 'Follow-up', 'Create a dated reminder from this text.', 'secondary'),
				action('open_hud', 'HUD', 'Open the full HUD with this context.', 'secondary')
			]
		},
		{
			id: 'selection-field',
			label: 'Selection in field',
			target: 'Selected draft text',
			app: 'Current app',
			windowTitle: 'Writable field',
			status: 'Writable selection',
			body: 'Selected text is inside a writable field.',
			chipVisible: true,
			actions: [
				action('rewrite', 'Rewrite', 'Replace only the selected text.', 'primary'),
				action('shorten', 'Shorten', 'Make the selected text tighter.', 'primary'),
				action('clarify', 'Clarify', 'Clarify the selected sentence.', 'primary'),
				action('continue_draft', 'Continue', 'Continue after the selected text.', 'primary'),
				action('create_task', 'Task', 'Turn the selection into a task proposal.', 'secondary'),
				action('open_hud', 'HUD', 'Open the full HUD with this selection.', 'secondary')
			]
		},
		{
			id: 'empty-context',
			label: 'Empty field + context',
			target: 'Empty reply box',
			app: 'Current app',
			windowTitle: 'Writable field',
			status: 'Empty field',
			body: 'The field is empty and surrounding context can be observed before drafting.',
			chipVisible: true,
			actions: [
				action('draft_reply', 'Draft reply', 'Use current screen context to draft a reply.', 'primary'),
				action('write_from_context', 'Write', 'Start writing from the current context.', 'primary'),
				action('create_task', 'Task', 'Create a task from the current screen context.', 'secondary'),
				action('schedule_followup', 'Follow-up', 'Create a scheduled follow-up.', 'secondary'),
				action('open_hud', 'HUD', 'Open the full HUD with this context.', 'secondary')
			]
		},
		{
			id: 'empty-no-context',
			label: 'Empty field',
			target: 'Empty text box',
			app: 'Current app',
			windowTitle: 'Writable field',
			status: 'Context unread',
			body: 'The field is empty and context has not been read yet.',
			chipVisible: true,
			actions: [
				action('observe_then_draft', 'Observe + draft', 'Read the current screen, then draft.', 'primary'),
				action('write_from_context', 'Start writing', 'Start a blank draft with the selected personality.', 'primary'),
				action('create_task', 'Task', 'Read the screen and create a task proposal.', 'secondary'),
				action('schedule_followup', 'Follow-up', 'Read the screen and create a scheduled follow-up.', 'secondary'),
				action('open_hud', 'HUD', 'Use the larger HUD for a broad request.', 'secondary')
			]
		},
		{
			id: 'page-context',
			label: 'Web page',
			target: 'Ask Magican about this page',
			app: 'Browser',
			windowTitle: 'Active tab',
			status: 'Page context',
			body: 'The page itself is the target — grounding comes from its URL and a tab capture.',
			chipVisible: false,
			actions: [
				action('summarize_page', 'Summarize page', 'Summarize the current page from its URL and visible content.', 'primary'),
				action('create_task', 'Task', 'Create a task proposal from this page.', 'secondary'),
				action('open_hud', 'HUD', 'Ask about this page in the full HUD.', 'secondary')
			]
		},
		{
			id: 'files',
			label: 'Finder selection',
			target: 'Selected files',
			app: 'Finder',
			windowTitle: 'Folder',
			status: 'Files',
			body: 'The selected file paths are the target — grounding is the paths plus a Finder window capture.',
			chipVisible: false,
			actions: [
				action('summarize', 'Summarize', 'Summarize the selected files from their names, paths, and the visible window.', 'primary'),
				action('create_task', 'Task', 'Create a task proposal from the selected files.', 'secondary'),
				action('open_hud', 'HUD', 'Ask about these files in the full HUD.', 'secondary')
			]
		},
		{
			id: 'draft',
			label: 'Non-empty field',
			target: 'Existing draft',
			app: 'Current app',
			windowTitle: 'Writable field',
			status: 'Draft text',
			body: 'The focused field already has text.',
			chipVisible: true,
			actions: [
				action('continue_draft', 'Continue', 'Continue the existing draft.', 'primary'),
				action('improve_draft', 'Improve', 'Improve the draft without changing intent.', 'primary'),
				action('shorten', 'Shorten', 'Make the draft more concise.', 'primary'),
				action('clarify', 'Clarify', 'Clarify the draft.', 'primary'),
				action('create_task', 'Task', 'Create a task proposal from the draft.', 'secondary'),
				action('schedule_followup', 'Follow-up', 'Create a dated reminder from the draft.', 'secondary'),
				action('open_hud', 'HUD', 'Open the full HUD with this draft.', 'secondary')
			]
		},
		{
			id: 'secure',
			label: 'Secure field',
			target: 'Secure field',
			app: 'Current app',
			windowTitle: 'Credential surface',
			status: 'Suppressed',
			body: 'Secure fields are suppressed.',
			chipVisible: false,
			suppression: 'Hidden for secure fields.',
			actions: []
		},
		{
			id: 'excluded',
			label: 'Excluded app',
			target: 'Focused field',
			app: 'Excluded app',
			windowTitle: 'Private surface',
			status: 'Suppressed',
			body: 'Per-app exclusions suppress contextual invoke.',
			chipVisible: false,
			suppression: 'Hidden because this app is excluded.',
			actions: [action('open_hud', 'HUD', 'Explicit invocation can still open the HUD.', 'secondary')]
		},
		{
			id: 'unsupported',
			label: 'Unsupported field',
			target: 'Unknown editor',
			app: 'Current app',
			windowTitle: 'Unknown editor',
			status: 'Explicit only',
			body: 'Unsupported insertion targets can fall back to copyable drafts.',
			chipVisible: false,
			suppression: 'No reliable writable target was found.',
			actions: [action('open_hud', 'HUD', 'Use the full HUD and copy text manually.', 'secondary')]
		}
	];

	const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
	let runtimeChecked = $state(typeof window !== 'undefined');
	let selectedState: TargetStateId = $state('empty-context');
	let display: AssistDisplay = $state('hidden');
	let personality = $state('active');
	let lastAction: string | null = $state(null);
	let lastDraft: string | null = $state(null);
	let lastDraftExpanded = $state(false);
	let lastProvenance: ContextualAssistProvenance | null = $state(null);
	let lastScreenshotAttachmentId: string | null = $state(null);
	let provenanceExpanded = $state(false);
	let selectedAction: ActionOption | null = $state(null);
	let userPrompt = $state('');
	let actionPending = $state(false);
	let dismissConfirmVisible = $state(false);
	// Preview-card insertion state (Tab to insert / R to refine / Esc).
	let insertPending = $state(false);
	let insertError = $state<string | null>(null);
	let refineVisible = $state(false);
	let refineText = $state('');
	let actionRunGeneration = 0;
	let contextExpanded = $state(false);
	let nativeTarget = $state<NativeAssistTarget | null>(null);
	let actionCatalog = $state<ActionState[]>([]);
	// First-run honesty: without Accessibility trust nothing can be detected;
	// the menu explains that instead of silently doing nothing.
	let axTrusted = $state(true);
	let personalityOptions = $state<PersonalityOption[]>(DEFAULT_PERSONALITY_OPTIONS);
	let personalityLoadError = $state('');
	let settings = $state<AssistSettings>({
		enabled: true,
		show_on_selected_text: true,
		show_in_writable_fields: true,
		explicit_hotkey_only: false,
		default_personality: 'active'
	});

	const currentTargetState = $derived(
		targetStates.find((entry) => entry.id === selectedState) ?? targetStates[0]
	);
	const currentActionState = $derived(actionCatalog.find((entry) => entry.id === selectedState));
	const currentActions = $derived(
		currentActionState?.actions?.length ? currentActionState.actions : currentTargetState.actions
	);
	const primaryActions = $derived(currentActions.filter((entry) => entry.kind === 'primary'));
	const secondaryActions = $derived(
		currentActions.filter((entry) => entry.kind === 'secondary')
	);
	const personalityLabel = $derived(
		personalityOptions.find((entry) => entry.id === personality)?.label ?? 'Active personality'
	);
	const targetApp = $derived(nativeTarget?.app || currentTargetState.app);
	const targetWindowTitle = $derived(nativeTarget?.window_title || currentTargetState.windowTitle);
	const targetUrl = $derived(nativeTarget?.url?.trim() || '');
	const targetUrlLabel = $derived(formatUrlLabel(targetUrl));
	const contextText = $derived(nativeTarget?.context_text?.trim() || '');
	const contextPreviewLabel = $derived(contextPreviewLabelFor(selectedState as TargetStateId));

	function action(
		id: string,
		label: string,
		description: string,
		kind: ActionOption['kind']
	): ActionOption {
		return { id, label, description, kind };
	}

	function formatUrlLabel(url: string): string {
		if (!url) return '';
		try {
			const parsed = new URL(url);
			return `${parsed.hostname}${parsed.pathname === '/' ? '' : parsed.pathname}`;
		} catch {
			return url;
		}
	}

	function contextPreviewLabelFor(state: TargetStateId): string {
		if (state === 'draft') return 'Field text';
		if (state === 'selection' || state === 'selection-field') return 'Selected text';
		return 'Context text';
	}

	function yesNo(value?: boolean): string {
		return value ? 'Yes' : 'No';
	}

	function compactValue(value?: string | null): string {
		const trimmed = value?.trim();
		return trimmed || '-';
	}

	function formatTimestamp(value?: string): string {
		if (!value) return '-';
		const date = new Date(value);
		if (Number.isNaN(date.getTime())) return value;
		return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	function formatTokenCount(value?: number): string {
		if (typeof value !== 'number' || !Number.isFinite(value)) return '-';
		return new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(value);
	}

	function formatCost(value?: number | null): string {
		if (typeof value !== 'number' || !Number.isFinite(value)) return '-';
		if (value === 0) return '$0.00';
		if (value < 0.01) return `$${value.toFixed(4)}`;
		return `$${value.toFixed(2)}`;
	}

	function provenanceSourceLabel(provenance: ContextualAssistProvenance): string {
		const source = provenance.source;
		if (!source) return '-';
		if (source.rootUrl) return formatUrlLabel(source.rootUrl);
		if (source.url) return formatUrlLabel(source.url);
		if (source.app) return source.app;
		return source.key || source.kind || '-';
	}

	function provenanceTargetLabel(provenance: ContextualAssistProvenance): string {
		const target = provenance.target;
		if (!target) return '-';
		const kind = target.textKind ? target.textKind.replace(/_/g, ' ') : 'context';
		const chars = typeof target.contextTextChars === 'number'
			? `, ${target.contextTextChars} chars`
			: '';
		return `${kind}${chars}`;
	}

	function provenanceVisualLabel(provenance: ContextualAssistProvenance): string {
		const visual = provenance.visual;
		if (!visual) return '-';
		if (!visual.screenshotRequired) return 'No screenshot';
		return visual.screenshotSupplied
			? `${compactValue(visual.screenshotSource)} screenshot${visual.screenshotReused ? ' reused' : ''}`
			: 'Screenshot requested, not supplied';
	}

	function provenanceUsageLabel(provenance: ContextualAssistProvenance): string {
		const usage = provenance.usage;
		if (!usage) return 'Not reported';
		return `${formatTokenCount(usage.totalTokens)} tokens, ${formatCost(usage.costUsd)}`;
	}

	function formatPersonalityLabel(name: string): string {
		return name
			.replace(/[_-]+/g, ' ')
			.split(' ')
			.filter(Boolean)
			.map((part) => part.slice(0, 1).toUpperCase() + part.slice(1))
			.join(' ');
	}

	function addPersonalityOption(
		options: PersonalityOption[],
		seen: Set<string>,
		option: PersonalityOption
	) {
		const id = option.id.trim();
		if (!id) return;
		const key = id.toLowerCase();
		if (seen.has(key)) return;
		seen.add(key);
		options.push({ id, label: option.label });
	}

	function normalizePersonalityOptions(
		skills: SkillListEntry[],
		selectedPersonality: string
	): PersonalityOption[] {
		const options: PersonalityOption[] = [];
		const seen = new Set<string>();
		for (const option of DEFAULT_PERSONALITY_OPTIONS) {
			addPersonalityOption(options, seen, option);
		}
		for (const skill of skills) {
			if (skill.kind !== 'personality-mode') continue;
			addPersonalityOption(options, seen, {
				id: skill.name,
				label: formatPersonalityLabel(skill.name)
			});
		}
		if (selectedPersonality && !seen.has(selectedPersonality.toLowerCase())) {
			addPersonalityOption(options, seen, {
				id: selectedPersonality,
				label: `${formatPersonalityLabel(selectedPersonality)} (unavailable)`
			});
		}
		return options;
	}

	/**
	 * The native config's magician port, retained after settings load so paths
	 * other than personality loading can reach the backend. Null in the browser,
	 * where the relative URL is proxied.
	 */
	let resolvedMagicianPort: number | null = null;

	function skillsUrl(magicianPort?: number | null): string {
		if (magicianPort && Number.isFinite(magicianPort)) {
			return `http://127.0.0.1:${magicianPort}/api/magician/v2/skills`;
		}
		return '/api/magician/v2/skills';
	}

	async function loadPersonalities(magicianPort?: number | null) {
		personalityLoadError = '';
		try {
			const scope = getCurrentScopeIdentity();
			const response = await timedFetch(skillsUrl(magicianPort), {
				headers: scopedRequestHeaders({
				})
			});
			if (!response.ok) throw new Error(`server returned ${response.status}`);
			const payload = (await response.json()) as SkillListResponse;
			personalityOptions = normalizePersonalityOptions(payload.skills ?? [], personality);
		} catch (error) {
			personalityLoadError = `Workspace personalities unavailable: ${error}`;
			personalityOptions = normalizePersonalityOptions([], personality);
		}
	}

	onMount(() => {
		runtimeChecked = true;
		if (!isTauri) {
			display = 'hidden';
			return;
		}
		let unlistenTarget: (() => void) | null = null;
		let unlistenDisplay: (() => void) | null = null;
		let unlistenDismissRequest: (() => void) | null = null;
		window.addEventListener('keydown', handleKeydown);
		document.addEventListener('pointerdown', handleDocumentPointerDown);
		window.addEventListener('resize', syncFromDom);
		void loadSettings();
		void loadActionCatalog();
		void loadPermissionStatus();
		void setupNativeTargetListener((unlisten) => {
			unlistenTarget = unlisten;
		});
		void setupNativeDisplayListener((unlisten) => {
			unlistenDisplay = unlisten;
		});
		void setupNativeDismissRequestListener((unlisten) => {
			unlistenDismissRequest = unlisten;
		});
		window.setTimeout(syncFromDom, 0);
		return () => {
			unlistenTarget?.();
			unlistenDisplay?.();
			unlistenDismissRequest?.();
			window.removeEventListener('keydown', handleKeydown);
			document.removeEventListener('pointerdown', handleDocumentPointerDown);
			window.removeEventListener('resize', syncFromDom);
		};
	});

	async function loadSettings() {
		if (!isTauri) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const config = await invoke<{
				contextual_assist?: Partial<AssistSettings>;
				network?: { magician_port?: number };
			}>('get_config');
			const assist = config.contextual_assist ?? {};
			settings = {
				enabled: assist.enabled ?? true,
				show_on_selected_text: assist.show_on_selected_text ?? true,
				show_in_writable_fields: assist.show_in_writable_fields ?? true,
				explicit_hotkey_only: assist.explicit_hotkey_only ?? false,
				default_personality: assist.default_personality || 'active'
			};
			personality = settings.default_personality;
			resolvedMagicianPort = config.network?.magician_port ?? null;
			await loadPersonalities(config.network?.magician_port);
			syncFromDom();
		} catch (error) {
			console.warn('[contextual-assist] config load failed:', error);
			await loadPersonalities();
		}
	}

	async function loadActionCatalog() {
		if (!isTauri) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			actionCatalog = await invoke<ActionState[]>('get_contextual_assist_action_catalog');
		} catch (error) {
			console.warn('[contextual-assist] action catalog load failed:', error);
		}
	}

	async function loadPermissionStatus() {
		if (!isTauri) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const status = await invoke<{ axTrusted: boolean }>(
				'get_contextual_assist_permission_status'
			);
			axTrusted = status.axTrusted;
		} catch {
			// Unavailable (non-macOS) — assume fine rather than nag.
		}
	}

	async function openAccessibilitySettings() {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('open_desktop_permission_settings', { permissionKey: 'accessibility' });
		} catch (error) {
			console.warn('[contextual-assist] failed to open accessibility settings:', error);
		}
	}

	async function setupNativeTargetListener(setUnlisten: (unlisten: () => void) => void) {
		if (!isTauri) return;
		try {
			const [{ invoke }, { listen }] = await Promise.all([
				import('@tauri-apps/api/core'),
				import('@tauri-apps/api/event')
			]);
			const target = await invoke<NativeAssistTarget | null>('get_contextual_assist_target');
			applyNativeTarget(target);
			const unlisten = await listen<NativeAssistTarget>('contextual-assist-target', (event) => {
				applyNativeTarget(event.payload);
			});
			setUnlisten(unlisten);
		} catch (error) {
			console.warn('[contextual-assist] target listener failed:', error);
		}
	}

	async function setupNativeDisplayListener(setUnlisten: (unlisten: () => void) => void) {
		if (!isTauri) return;
		try {
			const [{ invoke }, { listen }] = await Promise.all([
				import('@tauri-apps/api/core'),
				import('@tauri-apps/api/event')
			]);
			const unlisten = await listen<NativeAssistDisplay>('contextual-assist-display', (event) => {
				applyNativeDisplay(event.payload);
			});
			setUnlisten(unlisten);
			const currentDisplay = await invoke<NativeAssistDisplay>('get_contextual_assist_display');
			applyNativeDisplay(currentDisplay);
		} catch (error) {
			console.warn('[contextual-assist] display listener failed:', error);
		}
	}

	async function setupNativeDismissRequestListener(setUnlisten: (unlisten: () => void) => void) {
		if (!isTauri) return;
		try {
			const { listen } = await import('@tauri-apps/api/event');
			const unlisten = await listen<NativeAssistDismissRequest>(
				'contextual-assist-dismiss-request',
				() => {
					void dismiss();
				}
			);
			setUnlisten(unlisten);
		} catch (error) {
			console.warn('[contextual-assist] dismiss-request listener failed:', error);
		}
	}

	function applyNativeTarget(target: NativeAssistTarget | null) {
		nativeTarget = target;
		contextExpanded = false;
		if (!actionPending) {
			clearResultState();
		}
		if (target) {
			selectedState = target.state;
		}
	}

	function applyNativeDisplay(payload: NativeAssistDisplay | null) {
		if (!payload) return;
		if (payload.target !== undefined && payload.target !== null) {
			applyNativeTarget(payload.target);
		}
		display = payload.display;
		if (payload.display !== 'menu') {
			if (actionPending) {
				void cancelCurrentAction(false);
			} else {
				clearResultState();
			}
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') {
			event.preventDefault();
			// Refine field is open: Esc closes it first — the draft must
			// survive a stray Esc mid-sentence.
			if (refineVisible) {
				refineVisible = false;
				return;
			}
			void dismiss();
			return;
		}
		if (refineVisible && event.key === 'Enter') {
			event.preventDefault();
			void submitRefine();
			return;
		}
		if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
			event.preventDefault();
			openMenuFromCurrentTarget();
			return;
		}
		// Preview-card keys below. Never hijack keys while the user is
		// typing in a field (guidance or refine).
		const target = event.target as HTMLElement | null;
		const typing =
			target?.tagName === 'INPUT'
			|| target?.tagName === 'TEXTAREA'
			|| target?.isContentEditable === true;
		if (typing || !lastDraft || actionPending || insertPending) return;
		if (event.key === 'Tab' && draftAllowsInsert) {
			event.preventDefault();
			void insertDraft();
		} else if (!refineVisible && (event.key === 'r' || event.key === 'R')) {
			event.preventDefault();
			openRefine();
		}
	}

	function handleDocumentPointerDown(event: PointerEvent) {
		const target = event.target;
		if (!(target instanceof Element)) return;
		if (target.closest('.assist-popover') || target.closest('.assist-chip')) {
			return;
		}
		if (display === 'menu') {
			void dismiss();
			return;
		}
	}

	function syncFromDom() {
		if (!settings.enabled) {
			display = 'hidden';
			return;
		}
		if (display === 'hidden') display = 'chip';
	}

	function openMenuFromCurrentTarget() {
		if (!settings.enabled) return;
		void setNativeExpanded(true);
		display = 'menu';
		clearResultState();
	}

	/**
	 * Well-known action that files the selection instead of generating text.
	 *
	 * Must equal `ACTION_SAVE_TO_NOTES` in the desktop crate, which publishes it
	 * in the action catalog. A rename on either side would not error — the button
	 * would simply stop matching and fall through to the model path — so
	 * `make check-notes-capture-surface` fails the build if they diverge.
	 */
	const SAVE_TO_NOTES_ACTION_ID = 'save_to_notes';

	function notesCaptureUrl(port?: number | null): string {
		if (port && Number.isFinite(port)) {
			return `http://127.0.0.1:${port}/api/magician/v2/notes/capture-selection`;
		}
		return '/api/magician/v2/notes/capture-selection';
	}

	/**
	 * File the selection and stop.
	 *
	 * Deliberately not routed through the action/generation path: every other
	 * action there sends an intent into a prompt and returns text to review.
	 * Keeping something is not a draft to approve, so it neither waits for the
	 * model nor leaves a pending draft on screen.
	 */
	async function captureSelectionToNotes(option: ActionOption) {
		const text = contextText.trim();
		if (!text) {
			lastAction = 'Nothing selected to save.';
			return;
		}
		actionPending = true;
		lastAction = `${option.label} is saving...`;
		try {
			const scope = getCurrentScopeIdentity();
			const response = await timedFetch(notesCaptureUrl(resolvedMagicianPort), {
				method: 'POST',
				headers: scopedRequestHeaders({
					'Content-Type': 'application/json',
				}),
				body: JSON.stringify({
					text,
					source_app: targetApp || null,
					source_title: targetWindowTitle || null,
					source_url: targetUrl || null,
					// One id per invocation, so a retry after a lost response
					// files this selection once rather than twice.
					capture_id: crypto.randomUUID()
				})
			});
			if (!response.ok) throw new Error(`server returned ${response.status}`);
			const note = (await response.json()) as { path?: string; provider?: string };
			// Name where it actually landed: a fallback may have put it somewhere
			// other than the configured provider, and "Saved" alone would hide that.
			lastAction = note?.path ? `Saved to ${note.path}` : 'Saved to notes.';
		} catch (error) {
			lastAction = `Could not save to notes: ${
				error instanceof Error ? error.message : 'unknown error'
			}`;
		} finally {
			actionPending = false;
		}
	}

	async function chooseAction(option: ActionOption) {
		if (actionPending) {
			dismissConfirmVisible = true;
			lastAction = 'Writing is still running.';
			return;
		}
		if (!isTauri) {
			lastAction = `${option.label} selected. Native action binding is unavailable here.`;
			lastDraft = null;
			lastDraftExpanded = false;
			lastProvenance = null;
			lastScreenshotAttachmentId = null;
			provenanceExpanded = false;
			selectedAction = null;
			userPrompt = '';
			return;
		}
		if (option.id === SAVE_TO_NOTES_ACTION_ID) {
			selectedAction = null;
			await captureSelectionToNotes(option);
			return;
		}
		if (selectedAction?.id !== option.id) {
			userPrompt = '';
		}
		selectedAction = option;
		dismissConfirmVisible = false;
		lastAction = `${option.label} selected.`;
		lastDraft = null;
		lastDraftExpanded = false;
		lastProvenance = null;
		lastScreenshotAttachmentId = null;
		provenanceExpanded = false;
	}

	async function generateSelectedAction(reuseVisualContext = false) {
		const option = selectedAction;
		if (!option || actionPending) return;
		const reuseScreenshotAttachmentId =
			reuseVisualContext && lastScreenshotAttachmentId ? lastScreenshotAttachmentId : null;
		const runGeneration = ++actionRunGeneration;
		actionPending = true;
		dismissConfirmVisible = false;
		insertError = null;
		refineVisible = false;
			lastAction = `${option.label} is working...`;
		lastDraft = null;
		lastDraftExpanded = false;
		lastProvenance = null;
			provenanceExpanded = false;
			try {
			const { invoke } = await import('@tauri-apps/api/core');
			const response = await invoke<ContextualAssistActionResponse>(
				'invoke_contextual_assist_action',
				{
					request: {
						actionId: option.id,
						userPrompt: userPrompt.trim() || null,
						reuseScreenshotAttachmentId,
						personality,
						state: selectedState,
						target: nativeTarget
					}
				}
			);
			if (runGeneration !== actionRunGeneration) return;
			lastAction = response.message || `${response.action.label} accepted.`;
			lastDraft = response.draftText?.trim() || null;
			lastDraftExpanded = false;
			lastProvenance = response.provenance ?? null;
				lastScreenshotAttachmentId = response.screenshotAttachmentId ?? null;
				provenanceExpanded = false;
			} catch (error) {
			if (runGeneration !== actionRunGeneration) return;
			lastAction = error instanceof Error ? error.message : String(error);
			lastDraft = null;
			lastDraftExpanded = false;
			lastProvenance = null;
				lastScreenshotAttachmentId = null;
				provenanceExpanded = false;
			} finally {
			if (runGeneration === actionRunGeneration) {
					actionPending = false;
					dismissConfirmVisible = false;
				}
			}
	}

	function toggleContextPreview() {
		contextExpanded = !contextExpanded;
	}

	function toggleDraftResult() {
		lastDraftExpanded = !lastDraftExpanded;
	}

	function actionAllowsInsert(option: ActionOption | null): boolean {
		return option?.requiresWritableTarget === true;
	}

	/** Insert is offered only for actions with a writable target; everything
	 *  else (screen fallback, page, files) gets Copy as the landing action. */
	const draftAllowsInsert = $derived(
		Boolean(lastDraft)
			&& !actionPending
			&& actionAllowsInsert(selectedAction)
	);

	async function insertDraft(): Promise<void> {
		if (!lastDraft || insertPending || !draftAllowsInsert) return;
		insertPending = true;
		insertError = null;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const result = await invoke<{ ok: boolean; reason?: string | null }>(
				'insert_contextual_text',
				{ text: lastDraft }
			);
			if (result.ok) {
				lastAction = 'Inserted.';
				await dismiss();
			} else {
				insertError = result.reason || 'Insertion was aborted.';
				lastAction = 'Not inserted — use Copy.';
			}
		} catch (error) {
			insertError = error instanceof Error ? error.message : String(error);
			lastAction = 'Insertion failed.';
		} finally {
			insertPending = false;
		}
	}

	async function copyDraft(): Promise<void> {
		if (!lastDraft) return;
		try {
			await navigator.clipboard.writeText(lastDraft);
			lastAction = 'Copied.';
		} catch {
			lastAction = 'Copy failed.';
		}
	}

	function openRefine(): void {
		if (!lastDraft || actionPending) return;
		refineVisible = true;
		refineText = userPrompt;
	}

	async function submitRefine(): Promise<void> {
		if (!selectedAction || !refineVisible || actionPending) return;
		refineVisible = false;
		userPrompt = refineText;
		// Rerun the same action with the added sentence and the prior visual
		// context (screenshot attachment) intact. Preview-card keys (Tab to
		// insert, R to refine, Esc closes refine before dismissing) live in
		// the page-level handleKeydown registered once at setup.
		await generateSelectedAction(true);
	}

	function toggleProvenance() {
		provenanceExpanded = !provenanceExpanded;
	}

	function continueCurrentAction() {
		dismissConfirmVisible = false;
		if (actionPending) {
			lastAction = 'Still working...';
		}
	}

	function clearResultState() {
		lastAction = null;
		lastDraft = null;
		lastDraftExpanded = false;
		lastProvenance = null;
		lastScreenshotAttachmentId = null;
		provenanceExpanded = false;
		selectedAction = null;
		userPrompt = '';
		dismissConfirmVisible = false;
		insertError = null;
		refineVisible = false;
		refineText = '';
	}

	function hideSurface() {
		display = 'hidden';
		clearResultState();
		actionPending = false;
	}

	async function dismiss() {
		if (actionPending) {
			dismissConfirmVisible = true;
			lastAction = 'Writing is still running.';
			return;
		}
		hideSurface();
		await hideNativeSurface();
	}

	async function cancelCurrentAction(hideAfterCancel: boolean) {
		actionRunGeneration += 1;
		actionPending = false;
		dismissConfirmVisible = false;
		lastAction = 'Cancelled.';
		lastDraft = null;
		lastDraftExpanded = false;
		lastProvenance = null;
		lastScreenshotAttachmentId = null;
		provenanceExpanded = false;
		selectedAction = null;
		userPrompt = '';
		if (isTauri) {
			try {
				const { invoke } = await import('@tauri-apps/api/core');
				await invoke('cancel_contextual_assist_action');
			} catch (error) {
				console.warn('[contextual-assist] cancel failed:', error);
			}
		}
		if (hideAfterCancel) {
			hideSurface();
			await hideNativeSurface();
		}
	}

	async function hideNativeSurface() {
		if (!isTauri) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('hide_contextual_assist');
		} catch (error) {
			console.warn('[contextual-assist] hide failed:', error);
		}
	}

	async function setNativeExpanded(expanded: boolean) {
		if (!isTauri) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('set_contextual_assist_expanded', { expanded });
		} catch (error) {
			console.warn('[contextual-assist] expand failed:', error);
		}
	}
</script>

<svelte:head>
	<title>Contextual Assist</title>
</svelte:head>

<main class="assist-shell" class:browser-preview={!isTauri}>
	{#if !isTauri && !runtimeChecked}
		<section class="native-only-panel" aria-label="Contextual Assist loading">
			<p class="eyebrow">Magican Contextual Assist</p>
			<h1>Loading native overlay...</h1>
		</section>
	{:else if isTauri}
		<section class="overlay-stage" aria-label="Contextual Assist">
			{#if !axTrusted && display === 'menu'}
				<div class="permission-banner" role="alert">
					<span>
						{HOST_APP_NAME} needs <strong>Accessibility</strong> access to read what you're
						working on. Grant it, then press the left Option key again.
					</span>
					<button type="button" onclick={() => void openAccessibilitySettings()}>
						Open Settings
					</button>
				</div>
			{/if}
			{#if settings.enabled && display === 'chip' && currentTargetState.chipVisible}
				<button
					class="assist-chip assist-chip--static"
					type="button"
					aria-label="Open Contextual Assist"
					onclick={openMenuFromCurrentTarget}
				>
					<span class="chip-mark">M</span>
				</button>
			{:else if settings.enabled && display === 'menu'}
				<div
					class="assist-popover assist-popover--static"
					role="dialog"
					aria-label="Contextual Assist actions"
				>
					<div class="palette-header">
						<div>
							<p class="eyebrow">{currentTargetState.status}</p>
							<h1>{currentTargetState.target}</h1>
						</div>
						<button
							class="icon-button"
							type="button"
							aria-label={actionPending ? 'Cancel or continue writing action' : 'Dismiss'}
							onclick={dismiss}
						>
							<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
								<path d="M4 4l8 8M12 4l-8 8" />
							</svg>
						</button>
					</div>
					<div class="context-row">
						<div class="context-info">
							<span class="context-label">{targetApp}</span>
							<strong>{targetWindowTitle}</strong>
							{#if targetUrlLabel}
								<span class="context-url">{targetUrlLabel}</span>
							{/if}
						</div>
						<div class="personality-control">
							<label for="personality">Personality</label>
							<select id="personality" bind:value={personality}>
								{#each personalityOptions as option}
									<option value={option.id}>{option.label}</option>
								{/each}
							</select>
							{#if personalityLoadError}
								<span class="personality-warning">{personalityLoadError}</span>
							{/if}
						</div>
					</div>
					{#if contextText}
						<button
							class="context-preview"
							class:context-preview--expanded={contextExpanded}
							type="button"
							aria-expanded={contextExpanded}
							onclick={toggleContextPreview}
						>
							<span class="context-preview-label">{contextPreviewLabel}</span>
							<span class="context-preview-body">{contextText}</span>
						</button>
					{/if}
					<div class="action-grid">
						{#each primaryActions as option}
							<button
								type="button"
								class="action-button"
								class:action-button--selected={selectedAction?.id === option.id}
								disabled={actionPending}
								onclick={() => void chooseAction(option)}
							>
								<span>{option.label}</span>
								<small>{option.description}</small>
							</button>
						{/each}
					</div>
					<div class="secondary-row">
						{#each secondaryActions as option}
							<button
								type="button"
								class="secondary-action"
								class:secondary-action--selected={selectedAction?.id === option.id}
								disabled={actionPending}
								onclick={() => void chooseAction(option)}
							>
								{option.label}
							</button>
						{/each}
					</div>
					<div class="result-panel" class:result-panel--pending={actionPending}>
						<div class="result-summary">
							<p>{lastAction ?? `Ready with ${personalityLabel}.`}</p>
							<div class="result-status">
								{#if lastProvenance}
									<button
										class="provenance-toggle"
										class:provenance-toggle--active={provenanceExpanded}
										type="button"
										aria-label="Show context provenance"
										aria-expanded={provenanceExpanded}
										onclick={toggleProvenance}
									>
										i
									</button>
								{/if}
								{#if actionPending}
									<span class="pending-spinner" role="status" aria-label="Working"></span>
								{/if}
							</div>
						</div>
						{#if selectedAction && !actionPending}
							<div class="prompt-composer">
								<textarea
									class="prompt-input"
									rows="2"
									bind:value={userPrompt}
									placeholder="Optional guidance"
									aria-label="Optional guidance for this writing action"
								></textarea>
							</div>
						{/if}
						{#if lastDraft}
							<div class="draft-result-block">
								<div class="draft-result-toolbar">
									<span>Response</span>
									<button
										class="draft-regenerate"
										type="button"
										title="Regenerate again"
										aria-label="Regenerate again"
										disabled={!selectedAction || actionPending}
										onclick={() => void generateSelectedAction(true)}
									>
										<svg viewBox="0 0 24 24" aria-hidden="true">
											<path
												d="M20 11a8 8 0 0 0-14.6-4.5L4 8m0-4v4h4m-4 5a8 8 0 0 0 14.6 4.5L20 16m0 4v-4h-4"
											/>
										</svg>
									</button>
								</div>
								<button
									class="draft-result"
									class:draft-result--expanded={lastDraftExpanded}
									type="button"
									aria-label="Generated draft"
									aria-expanded={lastDraftExpanded}
									onclick={toggleDraftResult}
								>
									{lastDraft}
								</button>
								<div class="draft-action-row" role="group" aria-label="Draft actions">
									{#if draftAllowsInsert}
										<button
											class="draft-action draft-action--primary"
											type="button"
											disabled={insertPending || actionPending}
											onclick={() => void insertDraft()}
										>
											{insertPending ? 'Inserting…' : 'Insert'}
											<kbd>Tab</kbd>
										</button>
									{/if}
									<button
										class="draft-action"
										type="button"
										disabled={actionPending || refineVisible}
										onclick={openRefine}
									>
										Refine
										<kbd>R</kbd>
									</button>
									<button class="draft-action" type="button" onclick={() => void copyDraft()}>
										Copy
									</button>
								</div>
								{#if insertError}
									<div class="draft-insert-error" role="status">{insertError}</div>
								{/if}
								{#if refineVisible}
									<div class="draft-refine">
										<input
											type="text"
											bind:value={refineText}
											placeholder="Add one sentence and rerun…"
											aria-label="Refinement guidance"
										/>
										<button
											type="button"
											disabled={actionPending}
											onclick={() => void submitRefine()}
										>
											Rerun
										</button>
									</div>
								{/if}
							</div>
						{/if}
						{#if lastProvenance && provenanceExpanded}
							<div class="provenance-panel" aria-label="Context provenance">
								<div class="provenance-grid">
									<div>
										<span>Source</span>
										<strong>{provenanceSourceLabel(lastProvenance)}</strong>
									</div>
									<div>
										<span>Target</span>
										<strong>{provenanceTargetLabel(lastProvenance)}</strong>
									</div>
									<div>
										<span>Visual</span>
										<strong>{provenanceVisualLabel(lastProvenance)}</strong>
									</div>
									<div>
										<span>Persona</span>
										<strong>{compactValue(lastProvenance.persona?.personality)}</strong>
									</div>
									<div>
										<span>Agent</span>
										<strong>{compactValue(lastProvenance.persona?.agentId)}</strong>
									</div>
									<div>
										<span>Memory</span>
										<strong>
											Agent {yesNo(lastProvenance.memory?.agentScopedMemory)}, shared user {yesNo(
												lastProvenance.memory?.sharedUserMemoryAllowed
											)}
										</strong>
									</div>
									<div>
										<span>Session</span>
										<strong>{compactValue(lastProvenance.chat?.threadId)}</strong>
									</div>
									<div>
										<span>Time</span>
										<strong>{formatTimestamp(lastProvenance.generatedAt)}</strong>
									</div>
									<div>
										<span>Usage</span>
										<strong>{provenanceUsageLabel(lastProvenance)}</strong>
									</div>
									<div>
										<span>Model</span>
										<strong>{compactValue(lastProvenance.usage?.model)}</strong>
									</div>
								</div>
								{#if lastProvenance.usage}
									<p class="provenance-meta">
										Input {formatTokenCount(lastProvenance.usage.inputTokens)} | Output {formatTokenCount(
											lastProvenance.usage.outputTokens
										)} | Reasoning {formatTokenCount(lastProvenance.usage.reasoningTokens)} | Cache {formatTokenCount(
											lastProvenance.usage.cacheReadTokens
										)}
									</p>
								{/if}
								{#if lastProvenance.target?.contextTextPreview}
									<p class="provenance-preview">{lastProvenance.target.contextTextPreview}</p>
								{/if}
								{#if lastProvenance.visual?.screenshotAttachmentId || lastProvenance.chat?.assistantMessageId}
									<p class="provenance-meta">
										{#if lastProvenance.visual?.screenshotAttachmentId}
											Screenshot {lastProvenance.visual.screenshotAttachmentId}
										{/if}
										{#if lastProvenance.chat?.assistantMessageId}
											{lastProvenance.visual?.screenshotAttachmentId ? ' | ' : ''}Reply {lastProvenance.chat.assistantMessageId}
										{/if}
									</p>
								{/if}
							</div>
						{/if}
						{#if actionPending || dismissConfirmVisible}
							<div class="result-actions">
								<button
									class="result-control result-control--cancel"
									type="button"
									onclick={() => void cancelCurrentAction(true)}
								>
									Cancel
								</button>
								<button class="result-control" type="button" onclick={continueCurrentAction}>
									Continue
								</button>
							</div>
						{:else if selectedAction}
							<div class="result-actions">
								<button class="result-control" type="button" onclick={clearResultState}>
									Cancel
								</button>
								<button
									class="result-control result-control--primary"
									type="button"
									disabled={actionPending}
									onclick={() => void generateSelectedAction()}
								>
									Generate
								</button>
							</div>
						{/if}
					</div>
				</div>
			{:else}
				<div class="disabled-panel">
					<h1>Contextual Assist disabled</h1>
					<p>Enable it in Desktop Settings to show this surface.</p>
				</div>
			{/if}
		</section>
	{:else}
		<section class="native-only-panel" aria-label="Contextual Assist native-only">
			<p class="eyebrow">Magican Contextual Assist</p>
			<h1>Native overlay only</h1>
			<p>
				This surface runs from {HOST_APP_NAME} so it can appear near the real
				cursor across apps, including browser windows. Open it from Desktop Settings or
				the tray while the Tauri app is running.
			</p>
		</section>
	{/if}
</main>

<style>
	:global(html),
	:global(body) {
		margin: 0;
		width: 100%;
		height: 100%;
		background: transparent !important;
		overflow: hidden;
	}

	:global(body) {
		font-family: var(--font-primary, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif);
		color: var(--text-primary, #17202a);
	}

	:global(body::before),
	:global(body::after) {
		display: none !important;
		content: none !important;
		background: none !important;
	}

	:global(body > div) {
		background: transparent !important;
	}

	.assist-shell {
		box-sizing: border-box;
		position: relative;
		display: grid;
		place-items: stretch;
		width: 100vw;
		height: 100vh;
		min-height: 0;
		padding: 0;
		background: transparent !important;
		overflow: hidden;
	}

	.assist-shell.browser-preview {
		padding: 14px;
		background: var(--bg-base, #eef2f6) !important;
	}

	.overlay-stage {
		position: relative;
		box-sizing: border-box;
		display: grid;
		place-items: center;
		width: 100%;
		height: 100%;
		min-height: 0;
		padding: 0;
		background: transparent !important;
		overflow: hidden;
	}

	.palette-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}

	.eyebrow {
		margin: 0 0 2px;
		font-family: var(--font-primary, inherit);
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--accent-primary, #22748f);
	}

	h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary, inherit));
		font-size: 16px;
		font-weight: 700;
		line-height: 1.25;
		color: var(--text-primary, #111827);
	}

	.assist-popover,
	.disabled-panel,
	.native-only-panel {
		border: 1px solid var(--border-base, rgba(63, 79, 98, 0.16));
		background: var(--bg-surface, #ffffff);
		box-shadow: 0 10px 26px rgba(0, 0, 0, 0.12);
	}

	.context-label {
		display: block;
		font-family: var(--font-primary, inherit);
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted, #526171);
	}

	.assist-chip {
		position: absolute;
		z-index: 20;
		box-sizing: border-box;
		display: grid;
		place-items: center;
		width: 38px;
		height: 38px;
		padding: 0;
		border: 2px solid var(--border-base, #ffffff);
		border-radius: var(--radius-lg, 10px);
		background: var(--bg-elevated, rgba(0, 0, 0, 0.9));
		color: var(--text-primary, #ffffff);
		font: inherit;
		line-height: 1;
		outline: none !important;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.2) !important;
		cursor: pointer;
		opacity: 0.96;
		appearance: none;
		-webkit-appearance: none;
		-webkit-tap-highlight-color: transparent;
		transition: transform 0.12s ease, opacity 0.12s ease;
	}

	.assist-chip:hover {
		opacity: 1;
		transform: scale(1.04);
	}

	.assist-chip:focus,
	.assist-chip:focus-visible,
	.assist-chip:active {
		outline: none !important;
	}

	.assist-chip--static {
		position: static;
		margin: 0;
	}

	.chip-mark {
		display: grid;
		place-items: center;
		width: 100%;
		height: 100%;
		font-family: var(--font-display, inherit);
		font-size: 13px;
		font-weight: 800;
	}

	.assist-popover {
		position: absolute;
		z-index: 30;
		box-sizing: border-box;
		width: min(440px, calc(100vw - 28px));
		padding: 12px;
		border-radius: var(--radius-xl, 14px);
		overflow: hidden;
	}

	.assist-popover--static {
		position: static;
		width: 100%;
		max-width: 100%;
		max-height: 100%;
		border-radius: var(--radius-xl, 14px);
		border: 1px solid var(--border-base, rgba(63, 79, 98, 0.18));
		background: color-mix(in srgb, var(--bg-surface, #ffffff) 94%, transparent);
		backdrop-filter: blur(20px);
		-webkit-backdrop-filter: blur(20px);
		box-shadow: 0 20px 60px rgba(0, 0, 0, 0.24);
		overflow-x: hidden;
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	.icon-button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-subtle, rgba(31, 41, 55, 0.14));
		background: var(--bg-soft, #f3f4f6);
		color: var(--text-secondary, #374151);
		cursor: pointer;
		transition: background 0.15s ease, border-color 0.15s ease, color 0.15s ease;
	}

	.icon-button:hover {
		border-color: var(--border-base, #111827);
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, #111827);
	}

	.context-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		margin-top: 10px;
		padding: 8px 12px;
		border-radius: var(--radius-lg, 10px);
		background: var(--bg-soft, #f4f6f8);
		border: 1px solid var(--border-subtle, rgba(128, 128, 128, 0.12));
	}

	.context-info {
		min-width: 0;
		flex: 1 1 auto;
	}

	.context-info strong {
		display: block;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-primary, inherit);
		font-size: 13px;
		font-weight: 700;
		color: var(--text-primary, #17202a);
	}

	.context-info .context-url {
		display: block;
		max-width: 100%;
		margin-top: 2px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-secondary, #4f6274);
		font-family: var(--font-primary, inherit);
		font-size: 11px;
		line-height: 1.25;
	}

	.personality-control {
		flex: 0 0 148px;
		width: 148px;
		max-width: 155px;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}

	.personality-control label {
		display: block;
		font-family: var(--font-primary, inherit);
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted, #526171);
		margin-bottom: 3px;
		line-height: 1;
	}

	select#personality,
	.personality-control select {
		box-sizing: border-box;
		width: 100%;
		height: 28px;
		padding: 0 20px 0 8px;
		font-family: var(--font-primary, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif);
		font-size: 11px;
		font-weight: 600;
		line-height: 26px;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-base, rgba(31, 41, 55, 0.2));
		background-color: var(--bg-surface, #ffffff);
		color: var(--text-primary, #111827);
		text-overflow: ellipsis;
		white-space: nowrap;
		overflow: hidden;
		outline: none;
		appearance: none;
		-webkit-appearance: none;
		background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6'%3E%3Cpath d='M1 1l4 4 4-4' fill='none' stroke='%238a8178' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
		background-repeat: no-repeat;
		background-position: right 7px center;
		background-size: 8px 5px;
		cursor: pointer;
		transition: border-color 0.15s ease, box-shadow 0.15s ease;
	}

	select#personality:focus,
	.personality-control select:focus {
		border-color: var(--accent-primary, #2563eb);
		box-shadow: 0 0 0 2px var(--accent-primary-soft, rgba(37, 99, 235, 0.2));
		outline: none;
	}

	select#personality option,
	.personality-control select option {
		font-family: var(--font-primary, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif);
		font-size: 11px;
		font-weight: 500;
		line-height: 1.3;
		background-color: var(--bg-surface, #ffffff);
		color: var(--text-primary, #111827);
		padding: 4px 8px;
	}

	.personality-warning {
		display: block;
		max-width: 100%;
		margin-top: 3px;
		font-size: 9.5px;
		line-height: 1.2;
		color: var(--color-error, #b42318);
		word-break: break-word;
	}

	.context-preview {
		box-sizing: border-box;
		display: block;
		width: 100%;
		margin-top: 8px;
		padding: 10px 12px;
		border: 1px solid var(--border-subtle, rgba(31, 41, 55, 0.12));
		border-radius: var(--radius-lg, 10px);
		background: var(--bg-soft, #fbfcfd);
		color: var(--text-primary, #17202a);
		font-family: var(--font-primary, inherit);
		text-align: left;
		cursor: pointer;
		transition: border-color 0.15s ease, background 0.15s ease;
	}

	.context-preview:hover {
		border-color: var(--border-base, rgba(31, 41, 55, 0.25));
		background: var(--bg-surface, #ffffff);
	}

	.context-preview-label {
		display: block;
		margin-bottom: 4px;
		color: var(--text-muted, #526171);
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}

	.context-preview-body {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		white-space: pre-wrap;
		color: var(--text-primary, #1f2a37);
		font-size: 12px;
		line-height: 1.4;
	}

	.context-preview--expanded .context-preview-body {
		display: block;
		max-height: 96px;
		overflow-y: auto;
		-webkit-line-clamp: unset;
		line-clamp: unset;
	}

	.action-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 8px;
		margin-top: 10px;
	}

	.action-button,
	.secondary-action {
		border: 1px solid var(--border-subtle, rgba(31, 41, 55, 0.14));
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, #16202a);
		cursor: pointer;
		transition: border-color 0.15s ease, background 0.15s ease, color 0.15s ease, transform 0.1s ease;
	}

	.action-button {
		min-height: 54px;
		padding: 10px 12px;
		border-radius: var(--radius-lg, 10px);
		text-align: left;
		font-family: var(--font-primary, inherit);
	}

	.action-button span {
		display: block;
		font-family: var(--font-display, var(--font-primary, inherit));
		font-size: 13px;
		font-weight: 700;
		color: var(--text-primary, inherit);
	}

	.action-button small {
		display: block;
		margin-top: 4px;
		color: var(--text-secondary, #5b6774);
		font-size: 11px;
		line-height: 1.35;
	}

	.secondary-row {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin-top: 8px;
	}

	.secondary-action {
		min-height: 28px;
		padding: 0 12px;
		border-radius: var(--radius-md, 8px);
		font-family: var(--font-primary, inherit);
		font-size: 11px;
		font-weight: 700;
		color: var(--text-secondary, #374151);
	}

	.action-button:hover:not(:disabled),
	.secondary-action:hover:not(:disabled) {
		border-color: var(--accent-primary, #2563eb);
		background: var(--bg-elevated, #ffffff);
		color: var(--text-primary);
	}

	.action-button--selected,
	.secondary-action--selected {
		border-color: var(--accent-primary, #2563eb);
		background: var(--accent-primary-soft, rgba(37, 99, 235, 0.12));
		color: var(--accent-primary, #2563eb);
	}

	.action-button:disabled,
	.secondary-action:disabled {
		cursor: default;
		opacity: 0.52;
	}

	.result-panel {
		margin-top: 10px;
		min-height: 32px;
		padding: 10px 12px;
		border-radius: var(--radius-lg, 10px);
		background: var(--bg-soft, #f4f7fb);
		border: 1px solid var(--border-subtle, rgba(75, 92, 112, 0.12));
		transition: border-color 0.15s ease, background 0.15s ease;
	}

	.result-panel--pending {
		border-color: var(--accent-primary, #2563eb);
		background: var(--accent-primary-soft, rgba(37, 99, 235, 0.1));
	}

	.result-summary {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
	}

	.result-status {
		display: flex;
		flex: 0 0 auto;
		align-items: center;
		gap: 7px;
	}

	.result-panel p,
	.disabled-panel p,
	.native-only-panel p {
		margin: 0;
		color: var(--text-secondary, #3e4c59);
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		line-height: 1.45;
	}

	.pending-spinner {
		box-sizing: border-box;
		width: 16px;
		height: 16px;
		flex: 0 0 auto;
		border: 2px solid var(--accent-primary-soft, rgba(37, 99, 235, 0.2));
		border-top-color: var(--accent-primary, #2563eb);
		border-radius: var(--radius-full, 999px);
		animation: contextual-assist-spin 0.75s linear infinite;
	}

	@keyframes contextual-assist-spin {
		to {
			transform: rotate(360deg);
		}
	}

	.provenance-toggle {
		display: grid;
		place-items: center;
		width: 22px;
		height: 22px;
		padding: 0;
		border: 1px solid var(--border-subtle, rgba(31, 41, 55, 0.18));
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-surface, #ffffff);
		color: var(--text-secondary, #273444);
		font-family: var(--font-mono, monospace);
		font-size: 11px;
		font-weight: 800;
		line-height: 1;
		cursor: pointer;
		transition: border-color 0.15s ease, background 0.15s ease;
	}

	.provenance-toggle--active {
		border-color: var(--accent-primary, #2563eb);
		background: var(--accent-primary, #2563eb);
		color: var(--text-on-accent, #ffffff);
	}

	.prompt-composer {
		margin-top: 10px;
	}

	.prompt-input {
		box-sizing: border-box;
		display: block;
		width: 100%;
		min-height: 54px;
		max-height: 110px;
		padding: 10px 12px;
		resize: vertical;
		border: 1px solid var(--border-base, rgba(31, 41, 55, 0.2));
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, #16202a);
		font-family: var(--font-primary, inherit);
		font-size: 13px;
		line-height: 1.45;
		outline: none;
		transition: border-color 0.15s ease, box-shadow 0.15s ease;
	}

	.prompt-input::placeholder {
		color: var(--text-muted, #8f9799);
	}

	.prompt-input:focus {
		border-color: var(--accent-primary, #2563eb);
		box-shadow: 0 0 0 2px var(--accent-primary-soft, rgba(37, 99, 235, 0.2));
	}

	.draft-result-block {
		margin-top: 10px;
	}

	.draft-result-toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		margin-bottom: 6px;
	}

	.draft-result-toolbar span {
		color: var(--text-muted, #526171);
		font-family: var(--font-primary, inherit);
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}

	.draft-regenerate {
		display: grid;
		place-items: center;
		width: 24px;
		height: 24px;
		padding: 0;
		border: 1px solid var(--border-subtle, rgba(37, 99, 235, 0.24));
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-surface, #ffffff);
		color: var(--accent-primary, #1d4ed8);
		cursor: pointer;
		transition: background 0.15s ease, border-color 0.15s ease;
	}

	.draft-regenerate:hover:not(:disabled) {
		background: var(--accent-primary-soft, rgba(37, 99, 235, 0.1));
		border-color: var(--accent-primary, #1d4ed8);
	}

	.draft-regenerate svg {
		width: 14px;
		height: 14px;
		fill: none;
		stroke: currentColor;
		stroke-width: 2;
		stroke-linecap: round;
		stroke-linejoin: round;
	}

	.draft-regenerate:disabled {
		cursor: default;
		opacity: 0.5;
	}

	.permission-banner {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 8px;
		padding: 8px 10px;
		border: 1px solid var(--color-warning, rgba(217, 119, 6, 0.45));
		border-radius: var(--radius-md, 8px);
		background: var(--color-warning-soft, rgba(254, 243, 199, 0.85));
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		color: var(--text-primary);
	}

	.permission-banner span {
		flex: 1;
	}

	.permission-banner button {
		flex-shrink: 0;
		padding: 5px 10px;
		border: 1px solid var(--border-base, rgba(31, 41, 55, 0.2));
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, inherit);
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		font-weight: 600;
		cursor: pointer;
		transition: background 0.15s ease, border-color 0.15s ease;
	}

	.permission-banner button:hover {
		background: var(--bg-elevated, #ffffff);
		border-color: var(--accent-primary, #2563eb);
	}

	.draft-action-row {
		display: flex;
		gap: 6px;
		margin-top: 8px;
	}

	.draft-action {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 6px 12px;
		border: 1px solid var(--border-base, rgba(31, 41, 55, 0.2));
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, inherit);
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		font-weight: 600;
		cursor: pointer;
		transition: background 0.15s ease, border-color 0.15s ease, color 0.15s ease;
	}

	.draft-action:hover:not(:disabled) {
		border-color: var(--accent-primary, #2563eb);
		background: var(--bg-elevated, #ffffff);
	}

	.draft-action:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.draft-action--primary {
		border-color: var(--accent-primary, rgba(59, 130, 246, 0.55));
		background: var(--accent-primary-soft, rgba(59, 130, 246, 0.12));
		color: var(--accent-primary, #2563eb);
		font-weight: 700;
	}

	.draft-action kbd {
		padding: 1px 5px;
		border: 1px solid var(--border-subtle, rgba(31, 41, 55, 0.22));
		border-bottom-width: 2px;
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-soft, #f8fafc);
		font-family: var(--font-mono, monospace);
		font-size: 10px;
		line-height: 1.4;
	}

	.draft-insert-error {
		margin-top: 6px;
		padding: 6px 10px;
		border: 1px solid var(--color-error, rgba(220, 38, 38, 0.35));
		border-radius: var(--radius-md, 8px);
		background: var(--color-error-soft, rgba(254, 226, 226, 0.65));
		color: var(--color-error, #b91c1c);
		font-size: 12px;
	}

	.draft-refine {
		display: flex;
		gap: 6px;
		margin-top: 8px;
	}

	.draft-refine input {
		flex: 1;
		min-width: 0;
		padding: 7px 10px;
		border: 1px solid var(--border-base, rgba(59, 130, 246, 0.45));
		border-radius: var(--radius-md, 8px);
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, inherit);
		outline: none;
		transition: border-color 0.15s ease, box-shadow 0.15s ease;
	}

	.draft-refine input::placeholder {
		color: var(--text-muted, #8f9799);
	}

	.draft-refine input:focus {
		border-color: var(--accent-primary, #2563eb);
		box-shadow: 0 0 0 2px var(--accent-primary-soft, rgba(37, 99, 235, 0.2));
	}

	.draft-refine button {
		padding: 7px 12px;
		border: 1px solid var(--border-base, rgba(31, 41, 55, 0.2));
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, inherit);
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		font-weight: 600;
		cursor: pointer;
		transition: border-color 0.15s ease, background 0.15s ease;
	}

	.draft-refine button:hover:not(:disabled) {
		border-color: var(--accent-primary, #2563eb);
		background: var(--bg-elevated, #ffffff);
	}

	.draft-result {
		box-sizing: border-box;
		display: -webkit-box;
		width: 100%;
		max-height: none;
		margin-top: 0;
		padding: 10px 12px;
		overflow: hidden;
		border: 1px solid var(--border-subtle, rgba(31, 41, 55, 0.14));
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, #16202a);
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		line-height: 1.5;
		text-align: left;
		white-space: pre-wrap;
		cursor: pointer;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		transition: border-color 0.15s ease;
	}

	.draft-result:hover {
		border-color: var(--border-base, rgba(31, 41, 55, 0.25));
	}

	.draft-result--expanded {
		display: block;
		max-height: 120px;
		overflow-y: auto;
		-webkit-line-clamp: unset;
		line-clamp: unset;
	}

	.provenance-panel {
		box-sizing: border-box;
		max-height: 140px;
		margin-top: 8px;
		padding: 10px;
		overflow-y: auto;
		border: 1px solid var(--border-subtle, rgba(31, 41, 55, 0.12));
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, #ffffff);
	}

	.provenance-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 7px 10px;
	}

	.provenance-grid div {
		min-width: 0;
	}

	.provenance-grid span {
		display: block;
		color: var(--text-muted, #647385);
		font-family: var(--font-primary, inherit);
		font-size: 9px;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}

	.provenance-grid strong {
		display: block;
		margin-top: 2px;
		overflow: hidden;
		color: var(--text-primary, #17202a);
		font-family: var(--font-primary, inherit);
		font-size: 10px;
		line-height: 1.25;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.provenance-preview,
	.provenance-meta {
		margin-top: 8px !important;
		font-size: 10px !important;
		line-height: 1.35 !important;
	}

	.provenance-preview {
		display: -webkit-box;
		overflow: hidden;
		color: var(--text-secondary, #273444) !important;
		font-family: var(--font-primary, inherit);
		white-space: pre-wrap;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
	}

	.provenance-meta {
		overflow: hidden;
		color: var(--text-muted, #5f7083) !important;
		font-family: var(--font-mono, monospace);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.result-actions {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 10px;
		padding-top: 2px;
	}

	.result-control {
		min-height: 28px;
		padding: 0 12px;
		border: 1px solid var(--border-base, rgba(31, 41, 55, 0.2));
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, #ffffff);
		color: var(--text-primary, #17202a);
		font-family: var(--font-primary, inherit);
		font-size: 12px;
		font-weight: 700;
		cursor: pointer;
		transition: background 0.15s ease, border-color 0.15s ease, color 0.15s ease;
	}

	.result-control:hover:not(:disabled) {
		border-color: var(--border-base, #111827);
		background: var(--bg-elevated, #ffffff);
	}

	.result-control--cancel {
		border-color: var(--color-error-soft, rgba(185, 28, 28, 0.22));
		color: var(--color-error, #8f1d1d);
	}

	.result-control--cancel:hover:not(:disabled) {
		border-color: var(--color-error, #8f1d1d);
		background: var(--color-error-soft, rgba(185, 28, 28, 0.08));
	}

	.result-control--primary {
		border-color: var(--accent-primary, #2563eb);
		background: var(--accent-primary, #2563eb);
		color: var(--text-on-accent, #ffffff);
	}

	.result-control--primary:hover:not(:disabled) {
		background: var(--accent-primary-hover, #1d4ed8);
		border-color: var(--accent-primary-hover, #1d4ed8);
	}

	.disabled-panel,
	.native-only-panel {
		padding: 16px;
		border-radius: var(--radius-xl, 14px);
	}

	.disabled-panel p,
	.native-only-panel p {
		margin-top: 6px;
	}
</style>
