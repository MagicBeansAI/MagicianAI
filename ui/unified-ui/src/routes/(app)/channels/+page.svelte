<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy, onMount } from 'svelte';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import {
		botQrUrl,
		botStore,
		clearBotError,
		deleteBotConfig,
		loadBotAuth,
		loadBotAuthState,
		loadBotConfig,
		loadBotLogs,
		loadBots,
		restartBot,
		saveBotConfig,
		startBotAuth,
		startBot,
		stopBot,
		type BotAuthSnapshot,
		type BotLogLine,
		type BotProcessConfig,
		type BotRuntimeState,
		type BotStatusSnapshot
	} from '$lib/stores/botStore';
	import {
		getCurrentScopeIdentity,
		scopeIdentityStore
	} from '$lib/stores/scopeIdentityStore';
	import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';
	import { requestConfirmation } from '$lib/stores/confirmationStore';

	const REFRESH_INTERVAL_MS = 10_000;
	// LOG_LIMIT was 120 — too tight when wu-cli re-prints the pairing
	// QR every ~30s (17 half-block rows + status lines per refresh).
	// After a few cycles the FIFO rotated past the latest QR's top
	// rows, so the rendered code came back truncated. 400 holds ~8
	// QR cycles plus surrounding chatter without dropping any.
	const LOG_LIMIT = 400;
	const LOG_PREVIEW_LIMIT = 1;
	const DEFAULT_RESTART_MAX_BACKOFF_SECS = 30;

	type EditorMode = 'create' | 'edit';

	interface EnvRow {
		id: number;
		key: string;
		value: string;
	}

	interface BotEditorForm {
		name: string;
		enabled: boolean;
		command: string;
		argsText: string;
		cwd: string;
		auto_restart: boolean;
		restart_max_backoff_secs: number;
		envRows: EnvRow[];
	}

	let autoRefreshHandle: ReturnType<typeof setInterval> | null = null;
	let expandedLogs: Record<string, boolean> = {};
	// Click-to-zoom modal: any log entry (line or QR block) can be
	// opened full-size to make long errors / QR codes legible without
	// being constrained by the inline log container.
	interface LogModalState {
		open: boolean;
		title: string;
		body: string;
		variant: 'line' | 'qr';
		qrMode?: 'half' | 'solid';
	}
	let logModal: LogModalState = {
		open: false,
		title: '',
		body: '',
		variant: 'line',
		qrMode: 'half',
	};
	function openLogModal(state: Omit<LogModalState, 'open'>): void {
		logModal = { ...state, open: true };
	}
	function closeLogModal(): void {
		logModal = { ...logModal, open: false };
	}
	let qrStateByName: Record<string, 'unknown' | 'available' | 'missing'> = {};
	/** Bots for which we already auto-opened logs (don't re-open after user closes). */
	let autoExpandedOnce: Record<string, boolean> = {};
	let envRowId = 0;
	let editorOpen = false;
	let editorMode: EditorMode = 'create';
	let editorOriginalName: string | null = null;
	let editorLoading = false;
	let editorSaving = false;
	let editorDeleting = false;
	let editorError: string | null = null;
	let editorForm: BotEditorForm = createEmptyEditorForm();
	let stopScopeSubscription: (() => void) | null = null;

	type ChannelsScopeToken = {
		generation: number;
		scopeKey: string;
	};

	let channelsScopeGeneration = 0;
	let lastChannelsScopeKey = browser ? currentChannelsScopeKey() : 'anonymous:default';

	$: state = $botStore;
	$: bots = state.bots;
	$: authState = state.authState;
	$: editorTitle =
		editorMode === 'create'
			? 'Add Bot Configuration'
			: `Edit ${formatBotName(editorOriginalName ?? (editorForm.name || 'bot'))}`;

	function currentChannelsScopeKey(): string {
		const scope = getCurrentScopeIdentity();
		return `${scope.principal}:${scope.workspace}`;
	}

	function nextChannelsScopeToken(): ChannelsScopeToken {
		return {
			generation: channelsScopeGeneration,
			scopeKey: currentChannelsScopeKey()
		};
	}

	function isStaleChannelsScopeToken(token: ChannelsScopeToken): boolean {
		return (
			token.generation !== channelsScopeGeneration
			|| currentChannelsScopeKey() !== token.scopeKey
		);
	}

	function formatState(state: BotRuntimeState): string {
		return state.replace(/_/g, ' ');
	}

	function formatBotName(name: string): string {
		return name
			.split(/[-_]/g)
			.filter(Boolean)
			.map((segment) => segment.charAt(0).toUpperCase() + segment.slice(1))
			.join(' ');
	}

	function formatTimestamp(value: string | undefined): string {
		if (!value) return 'n/a';
		const parsed = new Date(value);
		if (Number.isNaN(parsed.getTime())) return value;
		return parsed.toLocaleString();
	}

	function formatUptime(seconds: number | undefined): string {
		if (seconds === undefined) return 'n/a';
		if (seconds < 60) return `${seconds}s`;
		if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
		if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
		return `${Math.floor(seconds / 86400)}d ${Math.floor((seconds % 86400) / 3600)}h`;
	}

	function stateTone(state: BotRuntimeState): 'ok' | 'warn' | 'muted' | 'danger' {
		if (state === 'running') return 'ok';
		if (state === 'restarting' || state === 'stop_requested') return 'warn';
		if (state === 'failed') return 'danger';
		return 'muted';
	}

	function toneBadgeColor(
		tone: 'ok' | 'warn' | 'muted' | 'danger'
	): 'default' | 'success' | 'warning' | 'error' | 'info' {
		if (tone === 'ok') return 'success';
		if (tone === 'warn') return 'warning';
		if (tone === 'danger') return 'error';
		return 'default';
	}

	function authBadgeColor(
		auth: BotAuthSnapshot | undefined
	): 'default' | 'success' | 'warning' | 'error' | 'info' {
		if (auth?.flow_state === 'active' || auth?.flow_state === 'queued') return 'info';
		return toneBadgeColor(authTone(auth));
	}

	function lineTone(stream: string): 'stderr' | 'stdout' | 'supervisor' {
		if (stream === 'stderr') return 'stderr';
		if (stream === 'supervisor') return 'supervisor';
		return 'stdout';
	}

	// Many Node CLI tools (wu-cli pulls pino + baileys + qrcode-terminal,
	// gws pulls its own logger, etc.) emit INFO/DEBUG to stderr by
	// convention — stdout is reserved for primary output. Coloring purely
	// by stream then makes every routine info line look like an error.
	// `messageTone` peeks at the line's content to reclassify pino JSON
	// records and conventional `[LEVEL]` / `LEVEL:` prefixes; on no
	// match, falls back to the underlying stream tone. Tones map to:
	//   'stdout'     → routine info (was rendered red on stderr)
	//   'supervisor' → warning (yellow — reused; no dedicated warn tone)
	//   'stderr'     → real error / fatal
	function messageTone(line: BotLogLine): 'stderr' | 'stdout' | 'supervisor' {
		const stream = lineTone(line.stream);
		// Only attempt content overrides when the stream is stderr — we
		// trust stdout/supervisor classifications as-is.
		if (stream !== 'stderr') return stream;

		const text = line.line.trim();
		if (!text) return stream;

		// pino emits one-JSON-per-line: {"level":30,"time":...,"msg":"..."}
		// Level codes: 10=trace, 20=debug, 30=info, 40=warn, 50=error, 60=fatal.
		if (text.startsWith('{') && text.endsWith('}')) {
			const levelMatch = text.match(/"level"\s*:\s*(\d+)/);
			if (levelMatch) {
				const level = Number(levelMatch[1]);
				if (level >= 50) return 'stderr';
				if (level >= 40) return 'supervisor';
				return 'stdout';
			}
		}

		// Common conventional prefixes from various loggers (winston,
		// log4js, debug, plus plain `[INFO]` / `INFO:` styles). Match
		// case-insensitive at the head of the trimmed line, after an
		// optional ANSI color code or timestamp.
		const stripped = text
			.replace(/^\u001b\[[0-9;]*m/, '') // leading ANSI color
			.replace(/^\d{4}-\d{2}-\d{2}T?[\d:.\sZ+-]*\s*/, '') // ISO timestamp
			.replace(/^\u001b\[[0-9;]*m/, ''); // ANSI again post-timestamp
		const prefix = stripped.slice(0, 16).toLowerCase();
		if (/^\[?(error|fatal|fail)/.test(prefix) || /^err\b/.test(prefix)) return 'stderr';
		if (/^\[?(warn|warning)/.test(prefix)) return 'supervisor';
		if (/^\[?(info|debug|trace|notice|verbose|log)\b/.test(prefix)) return 'stdout';

		return stream;
	}

	// Visible label printed alongside the timestamp — keep in sync with
	// `messageTone` so a line colored as info doesn't read "stderr".
	// pino JSON exposes a precise level; prefix matches surface the
	// detected level too. Otherwise we fall back to the raw stream
	// (`stdout` / `stderr` / `supervisor`).
	function streamLabel(line: BotLogLine): string {
		const text = line.line.trim();

		if (text.startsWith('{') && text.endsWith('}')) {
			const levelMatch = text.match(/"level"\s*:\s*(\d+)/);
			if (levelMatch) {
				const level = Number(levelMatch[1]);
				if (level >= 60) return 'fatal';
				if (level >= 50) return 'error';
				if (level >= 40) return 'warn';
				if (level >= 30) return 'info';
				if (level >= 20) return 'debug';
				return 'trace';
			}
		}

		const stripped = text
			.replace(/^\u001b\[[0-9;]*m/, '')
			.replace(/^\d{4}-\d{2}-\d{2}T?[\d:.\sZ+-]*\s*/, '')
			.replace(/^\u001b\[[0-9;]*m/, '');
		const head = stripped.slice(0, 16).toLowerCase();
		const m = head.match(/^\[?(error|fatal|fail|warn|warning|info|debug|trace|notice|verbose|log)\b/);
		if (m) {
			const word = m[1];
			if (word === 'fail') return 'error';
			if (word === 'warning') return 'warn';
			return word;
		}

		return line.stream;
	}

	function isMutating(name: string): boolean {
		return Boolean(state.mutatingByName[name]);
	}

	function isLogsLoading(name: string): boolean {
		return state.logsLoadingByName[name] === true;
	}

	function isAuthLoading(name: string): boolean {
		return state.authLoadingByName[name] === true;
	}

	function isAuthMutating(name: string): boolean {
		return state.authMutatingByName[name] === true;
	}

	function logsFor(name: string): BotLogLine[] {
		return state.logsByName[name] ?? [];
	}

	// QR-detection: WhatsApp's wu-cli prints pairing QR codes as ASCII
	// art using unicode block-drawing chars (██▀▀ etc). Each row of
	// the QR is one stdout line, so the default "one box per line" log
	// renderer breaks visual continuity and hides most of the code.
	// We detect a stretch of QR-shaped lines and render them as a
	// single `<pre>` block so the QR is scannable.
	const QR_CHAR_RE = /[\u2580-\u259F\u2800-\u28FF\u2588 ]/g;
	function isQrLine(text: string): boolean {
		const trimmed = text.replace(/\s+$/, '');
		if (trimmed.length < 16) return false;
		const matches = trimmed.match(QR_CHAR_RE);
		if (!matches) return false;
		return matches.length / trimmed.length >= 0.7;
	}

	type LogRenderEntry =
		| { kind: 'line'; line: BotLogLine; key: string }
		| { kind: 'qr'; lines: BotLogLine[]; key: string; mode: 'half' | 'solid' };

	/**
	 * Detect whether a QR run uses half-block chars (▀▄) — meaning each
	 * text row encodes 2 QR pixels stacked vertically — or solid blocks
	 * (█ + space) — one text row = one QR pixel. The aspect-ratio CSS
	 * differs because monospace chars are roughly 0.6em wide × 1em tall:
	 *   - half-block: QR_w_pixels = chars_per_row, QR_h_pixels = 2 × rows
	 *     → line-height ≈ 1.2em renders square
	 *   - solid:      QR_w_pixels = chars_per_row, QR_h_pixels = rows
	 *     → line-height ≈ 0.6em renders square
	 */
	function detectQrMode(lines: BotLogLine[]): 'half' | 'solid' {
		let half = 0;
		let solid = 0;
		for (const line of lines) {
			for (const ch of line.line) {
				if (ch === '\u2580' || ch === '\u2584') half++;
				else if (ch === '\u2588') solid++;
			}
		}
		return half > solid ? 'half' : 'solid';
	}

	function groupLogsForRender(lines: BotLogLine[]): LogRenderEntry[] {
		const out: LogRenderEntry[] = [];
		let qrBuffer: BotLogLine[] = [];
		const flushQr = () => {
			if (qrBuffer.length === 0) return;
			if (qrBuffer.length >= 4) {
				const head = qrBuffer[0];
				out.push({
					kind: 'qr',
					lines: qrBuffer,
					key: `qr-${head.timestamp}-${out.length}`,
					mode: detectQrMode(qrBuffer),
				});
			} else {
				for (const line of qrBuffer) {
					out.push({
						kind: 'line',
						line,
						key: `${line.timestamp}-${line.stream}-${out.length}`,
					});
				}
			}
			qrBuffer = [];
		};
		for (const line of lines) {
			if (isQrLine(line.line)) {
				qrBuffer.push(line);
			} else {
				flushQr();
				out.push({
					kind: 'line',
					line,
					key: `${line.timestamp}-${line.stream}-${out.length}`,
				});
			}
		}
		flushQr();
		return out;
	}

	function latestLogFor(name: string): BotLogLine | null {
		const lines = logsFor(name);
		return lines.length > 0 ? lines[lines.length - 1] : null;
	}

	function hasLogSnapshotFor(name: string): boolean {
		return Object.prototype.hasOwnProperty.call(state.logsByName, name);
	}

	function authFor(name: string): BotAuthSnapshot | undefined {
		return state.authByName[name];
	}

	function authTone(
		auth: BotAuthSnapshot | undefined
	): 'ok' | 'warn' | 'muted' | 'danger' {
		if (!auth || !auth.supported) return 'muted';
		if (auth.status === 'ok') return 'ok';
		if (auth.status === 'needs_auth') return 'warn';
		if (auth.status === 'account_mismatch' || auth.status === 'error') return 'danger';
		return 'muted';
	}

	function authStatusLabel(auth: BotAuthSnapshot | undefined): string {
		if (!auth || !auth.supported) return 'Not managed';
		if (auth.flow_state === 'active') return 'Authenticating';
		if (auth.flow_state === 'queued') return 'Queued';
		if (auth.status === 'ok') return 'Ready';
		if (auth.status === 'needs_auth') return 'Needs auth';
		if (auth.status === 'account_mismatch') return 'Wrong account';
		if (auth.status === 'error') return 'Auth error';
		return 'Unknown';
	}

	function canManageAuth(auth: BotAuthSnapshot | undefined): boolean {
		return Boolean(auth?.supported && auth?.provider === 'google_workspace');
	}

	function authBlockedByOtherBot(botName: string): boolean {
		return Boolean(authState.active && authState.active.name !== botName);
	}

	function authButtonLabel(auth: BotAuthSnapshot | undefined, name: string): string {
		if (isAuthMutating(name)) return 'Requesting...';
		if (auth?.flow_state === 'active') return 'Authenticating...';
		if (auth?.flow_state === 'queued') return 'Queued...';
		return 'Re-authenticate';
	}

	function authActiveMessage(name: string): string | null {
		if (authState.active?.name !== name) return null;
		if (authState.active.expected_account) {
			return `Google sign-in is active for this bot. Sign in as ${authState.active.expected_account}.`;
		}
		return 'Google sign-in is active for this bot in the browser.';
	}

	function authQueuedMessage(name: string): string | null {
		const queueIndex = authState.queue.findIndex((entry) => entry.name === name);
		if (queueIndex === -1) return null;
		const activeName = authState.active ? formatBotName(authState.active.name) : null;
		if (activeName) {
			return queueIndex === 0
				? `Queued behind ${activeName}.`
				: `Queued behind ${activeName} and ${queueIndex} other bot${queueIndex === 1 ? '' : 's'}.`;
		}
		return 'Queued for the next auth slot.';
	}

	function wantsQr(bot: BotStatusSnapshot): boolean {
		return bot.qr_supported;
	}

	function qrMessage(bot: BotStatusSnapshot): string {
		const qrState = qrStateByName[bot.name] ?? 'unknown';
		if (qrState === 'available') {
			return 'Scan this QR with the account or device that should own this channel.';
		}
		if (bot.state === 'running' || bot.state === 'restarting' || bot.desired_running) {
			return 'Waiting for QR generation or an already-linked session.';
		}
		return 'Start the bot to generate a QR code.';
	}

	function createEnvRow(key = '', value = ''): EnvRow {
		envRowId += 1;
		return { id: envRowId, key, value };
	}

	function createEmptyEditorForm(): BotEditorForm {
		return {
			name: '',
			enabled: false,
			command: '',
			argsText: '',
			cwd: '',
			auto_restart: true,
			restart_max_backoff_secs: DEFAULT_RESTART_MAX_BACKOFF_SECS,
			envRows: [createEnvRow()]
		};
	}

	function formFromConfig(name: string, config: BotProcessConfig): BotEditorForm {
		const envEntries = Object.entries(config.env).sort(([left], [right]) => left.localeCompare(right));
		return {
			name,
			enabled: config.enabled,
			command: config.command,
			argsText: config.args.join('\n'),
			cwd: config.cwd ?? '',
			auto_restart: config.auto_restart,
			restart_max_backoff_secs: config.restart_max_backoff_secs,
			envRows: envEntries.length > 0
				? envEntries.map(([key, value]) => createEnvRow(key, value))
				: [createEnvRow()]
		};
	}

	function resetEditor(): void {
		editorMode = 'create';
		editorOriginalName = null;
		editorLoading = false;
		editorSaving = false;
		editorDeleting = false;
		editorError = null;
		editorForm = createEmptyEditorForm();
	}

	function resetScopedBotViewState(): void {
		expandedLogs = {};
		qrStateByName = {};
		autoExpandedOnce = {};
		resetEditor();
		editorOpen = false;
	}

	function openCreateEditor(): void {
		resetEditor();
		editorOpen = true;
	}

	async function openEditEditor(bot: BotStatusSnapshot): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		resetEditor();
		editorMode = 'edit';
		editorOriginalName = bot.name;
		editorOpen = true;
		editorLoading = true;

		try {
			const config = await loadBotConfig(bot.name);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			editorForm = formFromConfig(bot.name, config);
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message = error instanceof Error ? error.message : `Failed to load config for ${bot.name}`;
			editorError = message;
			showError(message);
			editorOpen = false;
		} finally {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			editorLoading = false;
		}
	}

	function closeEditor(): void {
		if (editorSaving || editorDeleting) return;
		editorOpen = false;
		editorError = null;
	}

	function addEnvRow(): void {
		editorForm = {
			...editorForm,
			envRows: [...editorForm.envRows, createEnvRow()]
		};
	}

	function removeEnvRow(id: number): void {
		const remaining = editorForm.envRows.filter((row) => row.id !== id);
		editorForm = {
			...editorForm,
			envRows: remaining.length > 0 ? remaining : [createEnvRow()]
		};
	}

	function updateEnvRow(id: number, field: 'key' | 'value', value: string): void {
		editorForm = {
			...editorForm,
			envRows: editorForm.envRows.map((row) => (row.id === id ? { ...row, [field]: value } : row))
		};
	}

	function buildEditorPayload(): BotProcessConfig {
		const name = editorForm.name.trim();
		if (!name) {
			throw new Error('Bot name is required');
		}
		if (name.includes('/')) {
			throw new Error('Bot name cannot contain `/`');
		}

		const command = editorForm.command.trim();
		if (!command) {
			throw new Error('Command is required');
		}

		if (
			!Number.isFinite(editorForm.restart_max_backoff_secs) ||
			editorForm.restart_max_backoff_secs < 1
		) {
			throw new Error('Restart max backoff must be at least 1 second');
		}

		const env: Record<string, string> = {};
		for (const row of editorForm.envRows) {
			const key = row.key.trim();
			const hasValue = row.value.length > 0;
			if (!key && !hasValue) continue;
			if (!key) {
				throw new Error('Environment variable names cannot be empty');
			}
			if (key in env) {
				throw new Error(`Duplicate environment variable "${key}"`);
			}
			env[key] = row.value;
		}

		return {
			enabled: editorForm.enabled,
			command,
			args: editorForm.argsText
				.split('\n')
				.map((entry) => entry.trim())
				.filter(Boolean),
			env,
			auto_restart: editorForm.auto_restart,
			restart_max_backoff_secs: Math.floor(editorForm.restart_max_backoff_secs),
			...(editorForm.cwd.trim() ? { cwd: editorForm.cwd.trim() } : {})
		};
	}

	async function saveEditor(): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		editorError = null;
		const targetName = editorForm.name.trim();
		const savedLabel = formatBotName(targetName);

		let payload: BotProcessConfig;
		try {
			payload = buildEditorPayload();
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Bot configuration is invalid';
			editorError = message;
			showError(message);
			return;
		}

		editorSaving = true;
		try {
			await saveBotConfig(targetName, payload);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			if (expandedLogs[targetName]) {
				await loadBotLogs(targetName, LOG_LIMIT);
				if (isStaleChannelsScopeToken(scopeToken)) {
					return;
				}
			}
			qrStateByName = {
				...qrStateByName,
				[targetName]: 'unknown'
			};
			await refreshBots(false, scopeToken);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			editorOpen = false;
			showSuccess(
				editorMode === 'create' ? `${savedLabel} config created` : `${savedLabel} config updated`
			);
			resetEditor();
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message =
				error instanceof Error ? error.message : `Failed to save config for ${targetName}`;
			editorError = message;
			showError(message);
		} finally {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			editorSaving = false;
		}
	}

	async function deleteCurrentConfig(): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		if (!editorOriginalName) return;
		if (browser) {
			const confirmed = await requestConfirmation({
				title: `Delete bot "${editorOriginalName}"?`,
				message: 'This stops it and removes its config.',
				confirmLabel: 'Delete bot',
				destructive: true
			});
			if (!confirmed) return;
		}

		editorDeleting = true;
		try {
			await deleteBotConfig(editorOriginalName);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			expandedLogs = Object.fromEntries(
				Object.entries(expandedLogs).filter(([name]) => name !== editorOriginalName)
			);
			qrStateByName = Object.fromEntries(
				Object.entries(qrStateByName).filter(([name]) => name !== editorOriginalName)
			);
			await refreshBots(false, scopeToken);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			editorOpen = false;
			showSuccess(`${formatBotName(editorOriginalName)} config deleted`);
			resetEditor();
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message =
				error instanceof Error ? error.message : `Failed to delete ${editorOriginalName}`;
			editorError = message;
			showError(message);
		} finally {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			editorDeleting = false;
		}
	}

	async function refreshBots(
		showToast = false,
		scopeToken: ChannelsScopeToken = nextChannelsScopeToken()
	): Promise<void> {
		clearBotError();
		try {
			const bots = await loadBots();
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			await refreshAuth(bots, scopeToken);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			if (showToast) {
				showSuccess('Bot status refreshed');
			}
			autoExpandAuthLogs(bots);
			void refreshLogPreviews(bots, scopeToken);
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message = error instanceof Error ? error.message : 'Failed to load bot status';
			showError(message);
		}
	}

	async function refreshLogPreviews(
		botList: BotStatusSnapshot[] = bots,
		scopeToken: ChannelsScopeToken = nextChannelsScopeToken()
	): Promise<void> {
		if (isStaleChannelsScopeToken(scopeToken)) {
			return;
		}
		await Promise.allSettled(
			botList.map((bot) => {
				if (expandedLogs[bot.name]) {
					return Promise.resolve();
				}
				return loadBotLogs(bot.name, LOG_PREVIEW_LIMIT, {
					captureError: false,
					markLoading: false
				});
			})
		);
	}

	async function refreshAuth(
		botList: BotStatusSnapshot[] = bots,
		scopeToken: ChannelsScopeToken = nextChannelsScopeToken()
	): Promise<void> {
		try {
			await loadBotAuthState();
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message =
				error instanceof Error ? error.message : 'Failed to load bot auth state';
			showError(message);
		}

		await Promise.all(
			botList.map(async (bot) => {
				try {
					await loadBotAuth(bot.name);
					if (isStaleChannelsScopeToken(scopeToken)) {
						return;
					}
				} catch (error) {
					if (isStaleChannelsScopeToken(scopeToken)) {
						return;
					}
					const message =
						error instanceof Error ? error.message : `Failed to load auth for ${bot.name}`;
					showError(message);
				}
			})
		);
	}

	/**
	 * Auto-expand logs for bots that appear to need auth (restarting/failed
	 * shortly after boot). Only triggers once per bot — if the user closes
	 * the panel we don't re-open it.
	 */
	function autoExpandAuthLogs(botList: BotStatusSnapshot[]): void {
		for (const bot of botList) {
			if (autoExpandedOnce[bot.name]) continue;
			if (expandedLogs[bot.name]) continue;

			const needsAttention =
				(bot.state === 'restarting' || bot.state === 'failed') &&
				bot.enabled &&
				bot.desired_running;

			if (!needsAttention) continue;

			autoExpandedOnce = { ...autoExpandedOnce, [bot.name]: true };
			expandedLogs = { ...expandedLogs, [bot.name]: true };
			void loadBotLogs(bot.name, LOG_LIMIT).catch(() => {});
		}
	}

	async function runAction(
		bot: BotStatusSnapshot,
		action: 'start' | 'stop' | 'restart'
	): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		try {
			if (action === 'start') {
				await startBot(bot.name);
				if (isStaleChannelsScopeToken(scopeToken)) {
					return;
				}
				showSuccess(`${formatBotName(bot.name)} started`);
			} else if (action === 'stop') {
				await stopBot(bot.name);
				if (isStaleChannelsScopeToken(scopeToken)) {
					return;
				}
				showSuccess(`${formatBotName(bot.name)} stopped`);
			} else {
				await restartBot(bot.name);
				if (isStaleChannelsScopeToken(scopeToken)) {
					return;
				}
				showSuccess(`${formatBotName(bot.name)} restarting`);
			}

			if (expandedLogs[bot.name]) {
				await loadBotLogs(bot.name, LOG_LIMIT);
			} else {
				await loadBotLogs(bot.name, LOG_PREVIEW_LIMIT, {
					captureError: false,
					markLoading: false
				});
			}
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			qrStateByName = {
				...qrStateByName,
				[bot.name]: 'unknown'
			};
			await refreshBots(false, scopeToken);
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message = error instanceof Error ? error.message : `Failed to ${action} ${bot.name}`;
			showError(message);
		}
	}

	async function toggleLogs(bot: BotStatusSnapshot): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		const nextExpanded = !expandedLogs[bot.name];
		expandedLogs = {
			...expandedLogs,
			[bot.name]: nextExpanded
		};

		if (!nextExpanded) {
			return;
		}

		try {
			await loadBotLogs(bot.name, LOG_LIMIT);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message = error instanceof Error ? error.message : `Failed to load logs for ${bot.name}`;
			showError(message);
		}
	}

	async function refreshLogs(bot: BotStatusSnapshot): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		try {
			await loadBotLogs(bot.name, LOG_LIMIT);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message = error instanceof Error ? error.message : `Failed to load logs for ${bot.name}`;
			showError(message);
		}
	}

	async function refreshBotAuth(bot: BotStatusSnapshot): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		try {
			await loadBotAuthState();
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			await loadBotAuth(bot.name);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message =
				error instanceof Error ? error.message : `Failed to load auth for ${bot.name}`;
			showError(message);
		}
	}

	async function startAuth(bot: BotStatusSnapshot): Promise<void> {
		const scopeToken = nextChannelsScopeToken();
		try {
			const response = await startBotAuth(bot.name);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			await loadBotAuth(bot.name);
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			if (response.flow_state === 'queued') {
				showInfo(
					`${formatBotName(bot.name)} auth queued`,
					'Another Google auth flow is already active.'
				);
			} else {
				showInfo(
					`${formatBotName(bot.name)} auth started`,
					authFor(bot.name)?.expected_account
						? `Sign in as ${authFor(bot.name)?.expected_account}.`
						: 'Complete the Google sign-in in the browser.'
				);
			}
		} catch (error) {
			if (isStaleChannelsScopeToken(scopeToken)) {
				return;
			}
			const message =
				error instanceof Error ? error.message : `Failed to start auth for ${bot.name}`;
			showError(message);
		}
	}

	function handleQrLoad(name: string): void {
		qrStateByName = {
			...qrStateByName,
			[name]: 'available'
		};
	}

	function handleQrError(name: string): void {
		qrStateByName = {
			...qrStateByName,
			[name]: 'missing'
		};
	}

	function maybeStartAutoRefresh(): void {
		if (!browser || autoRefreshHandle) return;
		autoRefreshHandle = setInterval(() => {
			void loadBots().catch(() => {
				// Passive refresh only; keep the visible error as-is.
			});
			void loadBotAuthState().catch(() => {});
			for (const bot of bots) {
				void loadBotAuth(bot.name).catch(() => {});
			}
			// Auto-refresh logs for any expanded log panels.
			for (const botName of Object.keys(expandedLogs)) {
				if (expandedLogs[botName]) {
					void loadBotLogs(botName, LOG_LIMIT).catch(() => {});
				}
			}
			void refreshLogPreviews(bots);
		}, REFRESH_INTERVAL_MS);
	}

	onMount(async () => {
		if (!browser) return;
		stopScopeSubscription = scopeIdentityStore.subscribe((scope) => {
			const scopeKey = `${scope.principal}:${scope.workspace}`;
			if (scopeKey === lastChannelsScopeKey) {
				return;
			}
			lastChannelsScopeKey = scopeKey;
			channelsScopeGeneration += 1;
			resetScopedBotViewState();
			clearBotError();
			void refreshBots(false, nextChannelsScopeToken());
		});
		await refreshBots(false, nextChannelsScopeToken());
		maybeStartAutoRefresh();
	});

	onDestroy(() => {
		stopScopeSubscription?.();
		stopScopeSubscription = null;
		if (autoRefreshHandle) {
			clearInterval(autoRefreshHandle);
			autoRefreshHandle = null;
		}
	});
</script>

<svelte:head>
	<title>Bots · Magican</title>
</svelte:head>

<div class="presto-gaui-page bots-page">
	<Card className="bots-hero" elevation={1}>
		<div class="bots-hero-copy">
			<p class="bots-kicker">Agent Ops</p>
			<h1>Bot Control</h1>
			<p>Manage Bots and Channels.</p>
		</div>
		<div class="bots-hero-actions">
			<Button label="New bot" on:click={openCreateEditor} />
			<Button
				variant="secondary"
				label={state.isLoading ? 'Refreshing...' : 'Refresh'}
				disabled={state.isLoading}
				on:click={() => refreshBots(true)}
			/>
		</div>
	</Card>

	{#if state.error}
		<Card className="bots-banner bots-banner-error" elevation={0}>
			<strong>Bot API error</strong>
			<span>{state.error}</span>
		</Card>
	{/if}

	<Modal bind:open={editorOpen} title={editorTitle} size="lg" on:close={closeEditor}>
		{#if editorLoading}
			<div class="bot-editor-loading">Loading bot configuration…</div>
		{:else}
			<form class="bot-editor" on:submit|preventDefault={saveEditor}>
				<p class="bot-editor-intro">
					This edits the scoped live bot config under the active capability root and syncs the
					live bot manager immediately.
				</p>

				{#if editorError}
					<div class="bot-inline-alert bot-inline-alert-error">
						<strong>Config error</strong>
						<span>{editorError}</span>
					</div>
				{/if}

				<div class="bot-editor-grid">
					<label class="bot-editor-field">
						<span>Name</span>
						<input
							type="text"
							bind:value={editorForm.name}
							disabled={editorMode === 'edit'}
							placeholder="telegram"
						/>
					</label>
					<label class="bot-editor-field">
						<span>Command</span>
						<input type="text" bind:value={editorForm.command} placeholder="node" />
					</label>
					<label class="bot-editor-field">
						<span>Working directory</span>
						<input
							type="text"
							bind:value={editorForm.cwd}
							placeholder="magician_data_v3/scopes/<principal>/<workspace>/capabilities/bots/telegram"
						/>
					</label>
					<label class="bot-editor-field">
						<span>Restart max backoff (secs)</span>
						<input type="number" min="1" bind:value={editorForm.restart_max_backoff_secs} />
					</label>
				</div>

				<div class="bot-editor-toggles">
					<label class="bot-editor-checkbox">
						<input type="checkbox" bind:checked={editorForm.enabled} />
						<span>Start automatically on boot</span>
					</label>
					<label class="bot-editor-checkbox">
						<input type="checkbox" bind:checked={editorForm.auto_restart} />
						<span>Auto-restart after unexpected exit</span>
					</label>
				</div>

				<label class="bot-editor-field">
					<span>Arguments</span>
					<textarea
						bind:value={editorForm.argsText}
						rows="5"
						placeholder="One argument per line"
					></textarea>
				</label>

				<section class="bot-editor-env">
					<div class="bot-editor-env-header">
						<div>
							<h3>Environment</h3>
							<p>Use literal values or {'${ENV_VAR}'} references.</p>
						</div>
						<Button
							size="sm"
							variant="outline"
							label="Add row"
							type="button"
							on:click={addEnvRow}
						/>
					</div>

					<div class="bot-editor-env-rows">
						{#each editorForm.envRows as row (row.id)}
							<div class="bot-editor-env-row">
								<input
									type="text"
									value={row.key}
									placeholder="TELEGRAM_TOKEN"
									on:input={(event) =>
										updateEnvRow(
											row.id,
											'key',
											(event.currentTarget as HTMLInputElement).value
										)}
								/>
								<input
									type="text"
									value={row.value}
									placeholder={'${TELEGRAM_BOT_TOKEN}'}
									on:input={(event) =>
										updateEnvRow(
											row.id,
											'value',
											(event.currentTarget as HTMLInputElement).value
										)}
								/>
								<Button
									size="sm"
									variant="outline"
									label="Remove"
									type="button"
									on:click={() => removeEnvRow(row.id)}
								/>
							</div>
						{/each}
					</div>
				</section>

				<div class="bot-editor-actions">
					<div>
						{#if editorMode === 'edit'}
							<Button
								className="bot-delete-btn"
								variant="outline"
								type="button"
								label={editorDeleting ? 'Deleting…' : 'Delete bot'}
								disabled={editorSaving || editorDeleting}
								on:click={deleteCurrentConfig}
							/>
						{/if}
					</div>
					<div class="bot-editor-actions-right">
						<Button
							variant="outline"
							type="button"
							label="Cancel"
							disabled={editorSaving || editorDeleting}
							on:click={closeEditor}
						/>
						<Button
							type="submit"
							label={editorSaving ? 'Saving…' : editorMode === 'create' ? 'Create bot' : 'Save changes'}
							disabled={editorSaving || editorDeleting}
						/>
					</div>
				</div>
			</form>
		{/if}
	</Modal>

	<Modal bind:open={logModal.open} title={logModal.title} size="lg" on:close={closeLogModal}>
		{#if logModal.variant === 'qr'}
			<div class="bot-log-modal-qr-wrap">
				<pre class={`bot-log-modal-qr-art bot-log-modal-qr-art--${logModal.qrMode}`}>{logModal.body}</pre>
			</div>
		{:else}
			<pre class="bot-log-modal-body">{logModal.body}</pre>
		{/if}
	</Modal>

	{#if bots.length === 0 && !state.isLoading}
		<Card className="bots-empty" elevation={1}>
			<h2>No bots configured</h2>
			<p>
				Add entries under the `bots:` map in Magician config to expose them here. The backend
				control plane is live, but nothing is registered yet.
			</p>
			<Button label="Add first bot" on:click={openCreateEditor} />
		</Card>
	{:else}
		<div class="bots-grid">
			{#each bots as bot (bot.name)}
				{@const auth = authFor(bot.name)}
				{@const managedAuth = canManageAuth(auth)}
				{@const activeAuthMessage = authActiveMessage(bot.name)}
				{@const queuedAuthMessage = authQueuedMessage(bot.name)}
				{@const showQr = wantsQr(bot)}
				{@const showNotices = Boolean(activeAuthMessage || queuedAuthMessage || auth?.detail || bot.last_error || bot.last_exit)}
				{@const latestLog = latestLogFor(bot.name)}
				{@const hasLogSnapshot = hasLogSnapshotFor(bot.name)}
				<Card className={`bot-card ${expandedLogs[bot.name] ? 'bot-card-log-open' : ''}`} elevation={1}>
						<div class="bot-primary-row">
							<div class="bot-identity">
								<div class="bot-heading-row">
									<div>
										<h2>{formatBotName(bot.name)}</h2>
								</div>
								<div class="bot-badge-row">
									<Badge text={bot.command} color="default" />
									<Badge
										text={formatState(bot.state)}
										color={toneBadgeColor(stateTone(bot.state))}
									/>
									{#if managedAuth}
										<Badge
											text={authStatusLabel(auth)}
											color={authBadgeColor(auth)}
										/>
										{/if}
									</div>
								</div>
							</div>
							<div class="bot-actions bot-actions-primary">
							<Button
								size="sm"
								label={isMutating(bot.name) && state.mutatingByName[bot.name] === 'start' ? 'Starting...' : 'Start'}
								disabled={isMutating(bot.name) || bot.state === 'running'}
								on:click={() => runAction(bot, 'start')}
							/>
							<Button
								size="sm"
								variant="secondary"
								label={isMutating(bot.name) && state.mutatingByName[bot.name] === 'stop' ? 'Stopping...' : 'Stop'}
								disabled={isMutating(bot.name) || bot.state === 'stopped'}
								on:click={() => runAction(bot, 'stop')}
							/>
							<Button
								size="sm"
								variant="secondary"
								label={isMutating(bot.name) && state.mutatingByName[bot.name] === 'restart'
									? 'Restarting...'
									: 'Restart'}
								disabled={isMutating(bot.name)}
								on:click={() => runAction(bot, 'restart')}
							/>
							<Button
								size="sm"
								variant="outline"
								label="Edit config"
								on:click={() => openEditEditor(bot)}
							/>
							{#if managedAuth}
								<Button
									size="sm"
									variant="secondary"
									label={authButtonLabel(auth, bot.name)}
									disabled={isAuthMutating(bot.name) || authBlockedByOtherBot(bot.name) || auth?.flow_state === 'active' || auth?.flow_state === 'queued'}
									on:click={() => startAuth(bot)}
								/>
								<Button
									size="sm"
									className="bot-auth-refresh-btn"
									variant="outline"
									label="Refresh auth"
									disabled={isAuthLoading(bot.name)}
									title={isAuthLoading(bot.name) ? 'Refreshing auth…' : 'Refresh auth'}
									on:click={() => refreshBotAuth(bot)}
								/>
							{/if}
							<Button
								size="sm"
								variant="outline"
								label={expandedLogs[bot.name] ? 'Hide logs' : 'Show logs'}
								disabled={isLogsLoading(bot.name)}
								on:click={() => toggleLogs(bot)}
							/>
						</div>
					</div>

					<div class="bot-content-shell" class:is-log-open={expandedLogs[bot.name]}>
						<div class="bot-content-base">
							<div class="bot-command-stack">
								<p class="bot-command">{[bot.command, ...bot.args].join(' ')}</p>
								{#if bot.cwd}
									<p class="bot-command bot-command-secondary">
										<span>cwd</span>
										<code>{bot.cwd}</code>
									</p>
								{/if}
							</div>

							<div class="bot-summary-row">
								<div class="bot-fact-chips">
									<div class="bot-chip">
										<span>Enabled</span>
										<strong>{bot.enabled ? 'Yes' : 'No'}</strong>
									</div>
									<div class="bot-chip">
										<span>Uptime</span>
										<strong>{formatUptime(bot.uptime_secs)}</strong>
									</div>
									<div class="bot-chip">
										<span>PID</span>
										<strong>{bot.pid ?? 'n/a'}</strong>
									</div>
									<div class="bot-chip">
										<span>Restarts</span>
										<strong>{bot.restart_count}</strong>
									</div>
									<div class="bot-chip">
										<span>Auto restart</span>
										<strong>{bot.auto_restart ? 'On' : 'Off'}</strong>
									</div>
									{#if managedAuth}
										<div class="bot-chip bot-chip-accent">
											<span>Expected</span>
											<code>{auth?.expected_account ?? 'n/a'}</code>
										</div>
										<div class="bot-chip bot-chip-accent">
											<span>Authenticated</span>
											<code>{auth?.current_account ?? 'n/a'}</code>
										</div>
										<div class="bot-chip">
											<span>Auth flow</span>
											<strong>{auth?.flow_state ?? 'idle'}</strong>
										</div>
									{/if}
								</div>
							</div>

							<div class="bot-log-preview-row">
								<div class={`bot-log-preview ${latestLog ? `bot-log-preview-${messageTone(latestLog)}` : ''}`}>
									{#if latestLog}
										<span class="bot-log-preview-meta">
											{streamLabel(latestLog)} · {formatTimestamp(latestLog.timestamp)}
										</span>
										<code title={latestLog.line}>{latestLog.line}</code>
									{:else if hasLogSnapshot}
										<span class="bot-log-preview-empty">No captured logs yet.</span>
									{:else}
										<span class="bot-log-preview-empty">Loading latest log…</span>
									{/if}
								</div>
							</div>

							{#if showNotices || showQr}
								<div class="bot-tertiary-row">
									{#if showNotices}
										<div class="bot-notice-stack">
											{#if activeAuthMessage}
												<div class="bot-inline-alert bot-inline-alert-info">
													<strong>Auth in progress</strong>
													<span>{activeAuthMessage}</span>
												</div>
											{:else if queuedAuthMessage}
												<div class="bot-inline-alert bot-inline-alert-info">
													<strong>Auth queued</strong>
													<span>{queuedAuthMessage}</span>
												</div>
											{/if}

											{#if auth?.detail}
												<div class={`bot-inline-alert bot-inline-alert-${authTone(auth) === 'danger' ? 'error' : 'warning'}`}>
													<strong>Auth detail</strong>
													<span>{auth.detail}</span>
												</div>
											{/if}

											{#if bot.last_error}
												<div class="bot-inline-alert bot-inline-alert-error">
													<strong>Last error</strong>
													<span>{bot.last_error}</span>
												</div>
											{/if}

											{#if bot.last_exit}
												<div class="bot-inline-alert">
													<strong>Last exit</strong>
													<span>
														{bot.last_exit.success ? 'Clean' : 'Failed'}
														{#if bot.last_exit.code !== undefined}
															· code {bot.last_exit.code}
														{/if}
														· {formatTimestamp(bot.last_exit.finished_at)}
													</span>
												</div>
											{/if}
										</div>
									{/if}

									{#if showQr}
										<Card className="bot-qr-card" elevation={0}>
											<div class="bot-qr-copy">
												<h3>Pairing</h3>
												<p>{qrMessage(bot)}</p>
											</div>
											<div class="bot-qr-frame">
												<img
													src={botQrUrl(bot.name, state.qrRefreshToken)}
													alt={`${formatBotName(bot.name)} pairing QR code`}
													class:bot-qr-hidden={(qrStateByName[bot.name] ?? 'unknown') !== 'available'}
													on:load={() => handleQrLoad(bot.name)}
													on:error={() => handleQrError(bot.name)}
												/>
											</div>
										</Card>
									{/if}
								</div>
							{/if}
						</div>

						{#if expandedLogs[bot.name]}
							<section class="bot-logs bot-logs-overlay">
								<div class="bot-logs-header">
									<h3>Recent logs</h3>
									<Button
										size="sm"
										variant="outline"
										label={isLogsLoading(bot.name) ? 'Refreshing...' : 'Refresh logs'}
										disabled={isLogsLoading(bot.name)}
										on:click={() => refreshLogs(bot)}
									/>
								</div>

								{#if logsFor(bot.name).length === 0 && !isLogsLoading(bot.name)}
									<p class="bot-logs-empty">No captured logs yet.</p>
								{:else}
									<div class="bot-log-lines">
										{#each [...groupLogsForRender(logsFor(bot.name))].reverse() as entry (entry.key)}
											{#if entry.kind === 'qr'}
												<div
													class={`bot-log-qr-block bot-log-qr-block--${entry.mode}`}
													role="button"
													tabindex="0"
													title="Click to open larger"
													on:click={() =>
														openLogModal({
															title: `${formatBotName(bot.name)} pairing QR — ${formatTimestamp(entry.lines[0].timestamp)}`,
															body: entry.lines.map((l) => l.line).join('\n'),
															variant: 'qr',
															qrMode: entry.mode,
														})}
													on:keydown={(ev) => {
														if (ev.key === 'Enter' || ev.key === ' ') {
															ev.preventDefault();
															openLogModal({
																title: `${formatBotName(bot.name)} pairing QR — ${formatTimestamp(entry.lines[0].timestamp)}`,
																body: entry.lines.map((l) => l.line).join('\n'),
																variant: 'qr',
																qrMode: entry.mode,
															});
														}
													}}
												>
													<span class="bot-log-meta">
														{streamLabel(entry.lines[0])} · {formatTimestamp(entry.lines[0].timestamp)} · QR ({entry.lines.length} rows) · click to zoom
													</span>
													<pre class={`bot-log-qr-art bot-log-qr-art--${entry.mode}`}>{entry.lines.map((l) => l.line).join('\n')}</pre>
												</div>
											{:else}
												<div
													class={`bot-log-line bot-log-line-${messageTone(entry.line)}`}
													role="button"
													tabindex="0"
													title="Click to open larger"
													on:click={() =>
														openLogModal({
															title: `${formatBotName(bot.name)} log — ${streamLabel(entry.line)} · ${formatTimestamp(entry.line.timestamp)}`,
															body: entry.line.line,
															variant: 'line',
														})}
													on:keydown={(ev) => {
														if (ev.key === 'Enter' || ev.key === ' ') {
															ev.preventDefault();
															openLogModal({
																title: `${formatBotName(bot.name)} log — ${streamLabel(entry.line)} · ${formatTimestamp(entry.line.timestamp)}`,
																body: entry.line.line,
																variant: 'line',
															});
														}
													}}
												>
													<span class="bot-log-meta">
														{streamLabel(entry.line)} · {formatTimestamp(entry.line.timestamp)}
													</span>
													<code>{entry.line.line}</code>
												</div>
											{/if}
										{/each}
									</div>
								{/if}
							</section>
						{/if}
					</div>
				</Card>
			{/each}
		</div>
	{/if}
</div>

<style>
	.bots-page {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 1rem;
		min-width: 0;
		max-width: var(--app-content-max, 1320px);
		width: 100%;
		margin-inline: auto;
	}

	.bots-page :global(.muij-card) {
		margin-bottom: 0;
	}

	.bots-page :global(.muij-button) {
		white-space: nowrap;
		overflow-wrap: normal;
	}

	:global(.bots-hero.muij-card) {
		display: flex;
		justify-content: space-between;
		align-items: flex-end;
		gap: 1.5rem;
		padding: 1.4rem 1.5rem;
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 72%, transparent);
		border-radius: var(--radius-lg, 18px);
		background:
			radial-gradient(
				circle at top right,
				color-mix(in srgb, var(--accent-primary, #bf6f45) 14%, transparent),
				transparent 40%
			),
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 94%, var(--bg-soft, #f6f1e8) 6%),
				var(--bg-card, #fff)
			);
		box-shadow: 0 18px 40px rgba(22, 24, 35, 0.06);
	}

	.bots-kicker {
		margin: 0 0 0.35rem;
		font-size: 0.78rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--text-muted, #8a847a);
	}

	.bots-hero-copy h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: clamp(2rem, 4vw, 2.9rem);
		line-height: 0.98;
		letter-spacing: -0.04em;
		color: var(--text-primary, #2d2a26);
	}

	.bots-hero-copy > p:last-child {
		margin: 0.5rem 0 0;
		max-width: 60ch;
		color: var(--text-secondary, #5f5b55);
	}

	.bots-hero-actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.75rem;
	}

	:global(.bots-banner.muij-card),
	:global(.bots-empty.muij-card) {
		padding: 1rem 1.1rem;
		border-radius: var(--radius-md, 14px);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 70%, transparent);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 92%, transparent),
				color-mix(in srgb, var(--bg-soft, #f6f1e8) 44%, transparent)
			),
			var(--bg-card, #fff);
		box-shadow: 0 14px 32px rgba(22, 24, 35, 0.05);
	}

	:global(.bots-banner.muij-card) {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	:global(.bots-banner.muij-card) span {
		overflow-wrap: anywhere;
	}

	:global(.bots-empty.muij-card) {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		align-items: flex-start;
	}

	:global(.bots-banner-error.muij-card) {
		border-color: color-mix(in srgb, #b91c1c 24%, var(--border-soft, #d8d0c5));
		background: linear-gradient(
			180deg,
			color-mix(in srgb, #fef2f2 76%, var(--bg-card, #fff)),
			color-mix(in srgb, #fef2f2 52%, var(--bg-soft, #f6f1e8))
		);
		color: #991b1b;
	}

	:global(.bots-banner-info.muij-card) {
		border-color: color-mix(in srgb, var(--accent-primary, #bf6f45) 34%, var(--border-soft, #d8d0c5));
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--accent-primary, #bf6f45) 10%, var(--bg-card, #fff)),
			color-mix(in srgb, var(--accent-primary, #bf6f45) 5%, var(--bg-soft, #f6f1e8))
		);
		color: #0f4c81;
	}

	.bot-editor {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.bot-editor-intro {
		margin: 0;
		color: var(--text-secondary, #5f5b55);
	}

	.bot-editor-loading {
		padding: 1rem 0;
		color: var(--text-secondary, #5f5b55);
	}

	.bot-editor-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.85rem;
	}

	.bot-editor-field {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.bot-editor-field span {
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-secondary, #6b665e);
	}

	.bot-editor-field input,
	.bot-editor-field textarea,
	.bot-editor-env-row input {
		width: 100%;
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 78%, transparent);
		border-radius: var(--radius-sm, 10px);
		background: color-mix(in srgb, var(--bg-base, #fffdf8) 94%, transparent);
		padding: 0.7rem 0.8rem;
		font: inherit;
		color: var(--text-primary, #2d2a26);
	}

	.bot-editor-field textarea {
		min-height: 7rem;
		resize: vertical;
	}

	.bot-editor-field input:disabled {
		background: color-mix(in srgb, var(--bg-soft, #f6f1e8) 72%, var(--bg-card, #fff));
		color: var(--text-secondary, #6b665e);
	}

	.bot-editor-toggles {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.75rem;
	}

	.bot-editor-checkbox {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		padding: 0.8rem 0.9rem;
		border-radius: var(--radius-md, 12px);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 72%, transparent);
		background: color-mix(in srgb, var(--bg-card, #fff) 86%, var(--bg-soft, #f6f1e8) 14%);
	}

	.bot-editor-checkbox input {
		margin: 0;
	}

	.bot-editor-env {
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
		padding: 0.95rem;
		border-radius: var(--radius-md, 14px);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 72%, transparent);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 88%, transparent),
				color-mix(in srgb, var(--bg-soft, #f6f1e8) 42%, transparent)
			),
			var(--bg-card, #fff);
	}

	.bot-editor-env-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 1rem;
	}

	.bot-editor-env-header h3 {
		margin: 0;
		font-size: 0.98rem;
	}

	.bot-editor-env-header p {
		margin: 0.25rem 0 0;
		color: var(--text-secondary, #5f5b55);
		font-size: 0.84rem;
	}

	.bot-editor-env-rows {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	.bot-editor-env-row {
		display: grid;
		grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.2fr) auto;
		gap: 0.65rem;
		align-items: center;
	}

	.bot-editor-actions {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 1rem;
		padding-top: 0.25rem;
	}

	.bot-editor-actions-right {
		display: flex;
		justify-content: flex-end;
		gap: 0.75rem;
	}

	:global(.bot-delete-btn.muij-button) {
		border-color: color-mix(in srgb, #dc2626 22%, var(--border-soft, #d8d0c5));
		color: #991b1b;
	}

	.bots-grid {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		min-width: 0;
		max-width: 100%;
	}

	:global(.bot-card.muij-card) {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		padding: 1rem 1.1rem;
		border-radius: var(--radius-lg, 18px);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 70%, transparent);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 94%, transparent),
				color-mix(in srgb, var(--bg-soft, #f6f1e8) 34%, transparent)
			),
			var(--bg-card, #fff);
		box-shadow: 0 18px 40px rgba(22, 24, 35, 0.05);
		min-width: 0;
		max-width: 100%;
		overflow: hidden;
	}

	.bot-primary-row {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: 0.9rem 1rem;
		align-items: flex-start;
	}

	.bot-identity {
		display: grid;
		gap: 0.35rem;
		min-width: 0;
		flex: 1 1 20rem;
	}

	.bot-heading-row {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 0.9rem;
	}

	.bot-badge-row {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		justify-content: flex-end;
	}

	.bot-label {
		margin: 0 0 0.2rem;
		font-size: 0.82rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-secondary, #6b665e);
	}

	.bot-heading-row h2 {
		margin: 0;
		font-size: 1.05rem;
		line-height: 1.2;
		word-break: break-word;
	}

	.bot-command {
		margin: 0;
		font-family: var(--font-mono, 'IBM Plex Mono', monospace);
		font-size: 0.82rem;
		color: var(--text-secondary, #5f5b55);
		word-break: break-word;
	}

	.bot-command-stack {
		display: grid;
		gap: 0.2rem;
	}

	.bot-command-secondary {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		align-items: baseline;
		font-size: 0.78rem;
		color: var(--text-muted, #8a847a);
	}

	.bot-command-secondary span {
		font-size: 0.68rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.bot-command-secondary code {
		font-family: var(--font-mono, 'IBM Plex Mono', monospace);
		font-size: 0.78rem;
		line-height: 1.45;
		color: var(--text-secondary, #5f5b55);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.bot-actions {
		display: flex;
		flex-wrap: nowrap;
		gap: 0.55rem;
		overflow-x: auto;
		padding-bottom: 0.15rem;
		scrollbar-width: thin;
	}

	.bot-actions :global(.muij-button) {
		flex: 0 0 auto;
	}

	.bot-actions-primary {
		justify-content: flex-end;
		align-content: flex-start;
		flex: 0 1 auto;
		max-width: 100%;
		margin-left: auto;
	}

	.bot-content-shell {
		position: relative;
		min-width: 0;
	}

	.bot-content-shell.is-log-open {
		min-height: calc(clamp(13rem, 34vh, 18rem) + 3.25rem);
		overflow: hidden;
	}

	.bot-content-base {
		display: grid;
		gap: 0.85rem;
		min-width: 0;
		transition: opacity 140ms ease;
	}

	.bot-content-shell.is-log-open .bot-content-base {
		visibility: hidden;
		opacity: 0;
		pointer-events: none;
		user-select: none;
	}

	.bot-summary-row {
		display: block;
	}

	.bot-fact-chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.55rem;
		min-width: 0;
	}

	.bot-chip {
		display: grid;
		gap: 0.18rem;
		min-width: 8rem;
		max-width: 100%;
		padding: 0.55rem 0.7rem;
		border-radius: var(--radius-sm, 10px);
		background: color-mix(in srgb, var(--bg-base, #fffdf8) 94%, transparent);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 68%, transparent);
	}

	.bot-chip span {
		font-size: 0.68rem;
		font-weight: 700;
		color: var(--text-secondary, #6b665e);
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.bot-chip strong,
	.bot-chip code {
		font-size: 0.82rem;
		line-height: 1.4;
		color: var(--text-primary, #2d2a26);
	}

	.bot-chip code {
		font-family: var(--font-mono, 'IBM Plex Mono', monospace);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.bot-chip-accent {
		border-color: color-mix(in srgb, var(--accent-primary, #bf6f45) 22%, var(--border-soft, #d8d0c5));
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--accent-primary, #bf6f45) 8%, var(--bg-card, #fff)),
				color-mix(in srgb, var(--bg-card, #fff) 88%, var(--bg-soft, #f6f1e8) 12%)
			);
	}

	.bot-log-preview-row {
		display: block;
	}

	.bot-log-preview {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		min-width: 0;
		padding: 0.65rem 0.8rem;
		border-radius: var(--radius-md, 12px);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 70%, transparent);
		background: color-mix(in srgb, var(--bg-base, #fffdf8) 88%, transparent);
		box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.5);
	}

	.bot-log-preview-meta {
		flex: 0 0 auto;
		font-size: 0.68rem;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted, #8a847a);
		white-space: nowrap;
	}

	.bot-log-preview code,
	.bot-log-preview-empty {
		flex: 1 1 auto;
		min-width: 0;
		font-family: var(--font-mono, 'IBM Plex Mono', monospace);
		font-size: 0.78rem;
		line-height: 1.35;
		color: var(--text-secondary, #5f5b55);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.bot-log-preview-empty {
		font-family: var(--font-primary);
	}

	.bot-log-preview-stdout {
		border-left: 4px solid #38bdf8;
	}

	.bot-log-preview-stderr {
		border-left: 4px solid #fb7185;
	}

	.bot-log-preview-supervisor {
		border-left: 4px solid #f59e0b;
	}

	.bot-inline-alert {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		padding: 0.8rem 0.9rem;
		border-radius: var(--radius-md, 12px);
		background: color-mix(in srgb, var(--bg-card, #fff) 84%, var(--bg-soft, #f6f1e8) 16%);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 68%, transparent);
		min-width: 0;
		max-width: 100%;
		overflow: hidden;
	}

	.bot-inline-alert > span,
	.bot-inline-alert > strong {
		min-width: 0;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.bot-notice-stack {
		min-width: 0;
	}

	.bot-inline-alert-error {
		background: linear-gradient(
			180deg,
			color-mix(in srgb, #fef2f2 76%, var(--bg-card, #fff)),
			color-mix(in srgb, #fef2f2 48%, var(--bg-soft, #f6f1e8))
		);
		border-color: color-mix(in srgb, #dc2626 18%, var(--border-soft, #d8d0c5));
		color: #991b1b;
	}

	.bot-inline-alert-warning {
		background: linear-gradient(
			180deg,
			color-mix(in srgb, #fef3c7 58%, var(--bg-card, #fff)),
			color-mix(in srgb, #fef3c7 34%, var(--bg-soft, #f6f1e8))
		);
		border-color: color-mix(in srgb, #f59e0b 24%, var(--border-soft, #d8d0c5));
		color: #92400e;
	}

	.bot-inline-alert-info {
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--accent-primary, #bf6f45) 10%, var(--bg-card, #fff)),
			color-mix(in srgb, var(--accent-primary, #bf6f45) 5%, var(--bg-soft, #f6f1e8))
		);
		border-color: color-mix(in srgb, var(--accent-primary, #bf6f45) 30%, var(--border-soft, #d8d0c5));
		color: var(--text-primary, #2d2a26);
	}

	.bot-tertiary-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 0.75rem 1rem;
		align-items: start;
	}

	.bot-notice-stack {
		display: grid;
		gap: 0.55rem;
		min-width: 0;
	}

	:global(.bot-auth-refresh-btn.muij-button) {
		min-inline-size: 7.4rem;
	}

	:global(.bot-qr-card.muij-card) {
		width: min(100%, 25rem);
		display: grid;
		grid-template-columns: minmax(0, 1fr) 116px;
		gap: 0.75rem;
		align-items: center;
		padding: 0.75rem 0.85rem;
		border-radius: var(--radius-md, 12px);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--accent-primary, #bf6f45) 6%, var(--bg-card, #fff)),
				color-mix(in srgb, var(--bg-card, #fff) 82%, var(--bg-soft, #f6f1e8) 18%)
			);
		border: 1px solid color-mix(in srgb, var(--accent-primary, #bf6f45) 18%, var(--border-soft, #d8d0c5));
	}

	.bot-qr-copy h3 {
		margin: 0 0 0.25rem;
		font-size: 0.9rem;
	}

	.bot-qr-copy p {
		margin: 0;
		font-size: 0.82rem;
		line-height: 1.45;
		color: var(--text-secondary, #5f5b55);
	}

	.bot-qr-frame {
		display: flex;
		align-items: center;
		justify-content: center;
		min-height: 116px;
		border-radius: var(--radius-sm, 10px);
		background:
			repeating-linear-gradient(
				45deg,
				color-mix(in srgb, var(--accent-primary, #bf6f45) 5%, transparent),
				color-mix(in srgb, var(--accent-primary, #bf6f45) 5%, transparent) 10px,
				color-mix(in srgb, var(--bg-card, #fff) 94%, transparent) 10px,
				color-mix(in srgb, var(--bg-card, #fff) 94%, transparent) 20px
			);
		border: 1px dashed color-mix(in srgb, var(--accent-primary, #bf6f45) 24%, var(--border-soft, #d8d0c5));
		overflow: hidden;
	}

	.bot-qr-frame img {
		width: 100%;
		height: auto;
		display: block;
	}

	.bot-qr-hidden {
		display: none;
	}

	.bot-logs {
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		min-width: 0;
		padding-top: 0.1rem;
	}

	.bot-logs-overlay {
		position: absolute;
		inset: 0;
		z-index: 2;
		padding: 0.15rem 0 0;
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 94%, transparent),
				color-mix(in srgb, var(--bg-soft, #f6f1e8) 28%, transparent)
			),
			var(--bg-card, #fff);
		border-radius: var(--radius-md, 12px);
	}

	.bot-logs-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.75rem;
	}

	.bot-logs-header h3 {
		margin: 0;
		font-size: 0.9rem;
	}

	.bot-logs-empty {
		margin: 0;
		display: flex;
		flex: 1 1 auto;
		align-items: center;
		justify-content: center;
		min-height: 0;
		padding: 0.9rem;
		border-radius: var(--radius-md, 12px);
		background: color-mix(in srgb, var(--bg-base, #fffdf8) 72%, transparent);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 65%, transparent);
		color: var(--text-secondary, #6b665e);
	}

	.bot-log-lines {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		flex: 1 1 auto;
		max-height: 22rem;
		overflow: auto;
		min-height: 0;
		padding: 0.35rem;
		border-radius: var(--radius-md, 12px);
		background: color-mix(in srgb, var(--bg-base, #fffdf8) 70%, transparent);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 65%, transparent);
	}

	.bot-logs-overlay .bot-log-lines {
		max-height: none;
	}

	.bot-log-line {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		padding: 0.55rem 0.65rem;
		border-radius: var(--radius-sm, 10px);
		background: #0f172a;
		color: #e2e8f0;
		min-width: 0;
		max-width: 100%;
		cursor: pointer;
	}

	.bot-log-line:hover {
		outline: 1px solid color-mix(in srgb, #38bdf8 50%, transparent);
	}

	.bot-log-line code {
		font-family: var(--font-mono, 'IBM Plex Mono', monospace);
		font-size: 0.76rem;
		line-height: 1.45;
		white-space: pre-wrap;
		word-break: break-word;
		overflow-wrap: anywhere;
		max-width: 100%;
	}

	/* QR codes printed by wu-cli (whatsapp pairing) come through stdout
	   one row per line. Render the consecutive QR-shaped lines as a
	   single tight `<pre>` so the resulting block is actually
	   scannable. Tight line-height + zero letter-spacing matter — any
	   gap between rows breaks the QR's visual continuity. */
	.bot-log-qr-block {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.45rem;
		padding: 0.85rem 0.65rem;
		border-radius: var(--radius-sm, 10px);
		background: #ffffff;
		color: #000000;
		border: 1px solid color-mix(in srgb, #0f172a 20%, transparent);
		overflow-x: auto;
		cursor: pointer;
		width: 100%;
		box-sizing: border-box;
		min-height: 8rem;
	}

	.bot-log-qr-block:hover {
		outline: 1px solid color-mix(in srgb, #0f172a 40%, transparent);
	}

	.bot-log-qr-block .bot-log-meta {
		color: rgba(15, 23, 42, 0.65);
		align-self: flex-start;
	}

	/* Aspect-ratio matters here. Monospace chars are roughly 0.6em wide
	   × 1em tall, so to render the QR as a true square we set
	   line-height proportional to font-size based on which mode the
	   bot emits:
	   - half-block (▀▄): each text row encodes 2 QR pixels stacked, so
	     we need line-height ≈ 1.2em (chars wider:taller balances).
	   - solid (█ + space): each row = 1 QR pixel, so line-height
	     ≈ 0.6em squashes the row height to match char width. */
	.bot-log-qr-art {
		font-family: var(--font-mono, 'IBM Plex Mono', 'SFMono-Regular', monospace);
		font-size: 0.7rem;
		letter-spacing: 0;
		white-space: pre;
		margin: 0;
		color: #000;
		background: #fff;
		padding: 0;
	}

	.bot-log-qr-art--half {
		line-height: 1.2em;
	}

	.bot-log-qr-art--solid {
		line-height: 0.6em;
	}

	/* Click-to-zoom modal contents — sized to fill the modal viewport
	   without busting it. The QR art is centred and rendered at a
	   font-size that fits the modal width so the whole code is
	   scannable in one go. */
	.bot-log-modal-body {
		font-family: var(--font-mono, 'IBM Plex Mono', monospace);
		font-size: 0.85rem;
		line-height: 1.55;
		white-space: pre-wrap;
		word-break: break-word;
		overflow-wrap: anywhere;
		max-height: 70vh;
		overflow: auto;
		margin: 0;
		padding: 1rem;
		border-radius: var(--radius-md, 12px);
		background: #0f172a;
		color: #e2e8f0;
	}

	.bot-log-modal-qr-wrap {
		display: flex;
		justify-content: center;
		align-items: center;
		padding: 1.5rem;
		background: #ffffff;
		border-radius: var(--radius-md, 12px);
	}

	/* Modal QR sized to be comfortably scannable but compact — slightly
	   bigger than the inline 0.7rem render but capped low enough that
	   it stays a small square inside the modal rather than filling it.
	   line-height multiplier matches the chosen mode (half-block
	   1.2em / solid 0.6em) so the rendered block is square. */
	.bot-log-modal-qr-art {
		font-family: var(--font-mono, 'IBM Plex Mono', 'SFMono-Regular', monospace);
		font-size: clamp(0.3rem, 0.45vw, 0.45rem);
		letter-spacing: 0;
		white-space: pre;
		margin: 0;
		color: #000;
		background: #fff;
		padding: 0;
	}

	.bot-log-modal-qr-art--half {
		line-height: 1.2em;
	}

	.bot-log-modal-qr-art--solid {
		line-height: 0.6em;
	}

	.bot-log-line-stdout {
		border-left: 4px solid #38bdf8;
	}

	.bot-log-line-stderr {
		border-left: 4px solid #fb7185;
	}

	.bot-log-line-supervisor {
		border-left: 4px solid #f59e0b;
	}

	.bot-log-meta {
		font-size: 0.68rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: rgba(226, 232, 240, 0.78);
	}

	@media (max-width: 900px) {
		:global(.bots-hero.muij-card) {
			flex-direction: column;
			align-items: stretch;
		}

		.bot-editor-grid,
		.bot-editor-toggles,
		.bot-editor-env-row {
			grid-template-columns: 1fr;
		}

		.bot-primary-row {
			flex-direction: column;
		}

		.bot-tertiary-row {
			grid-template-columns: 1fr;
		}

		.bot-actions-primary,
		.bot-badge-row {
			justify-content: flex-start;
			margin-left: 0;
		}

		:global(.bot-qr-card.muij-card) {
			width: 100%;
		}
	}

	@media (max-width: 640px) {
		.bot-heading-row {
			flex-direction: column;
		}

		.bot-chip {
			flex: 1 1 9rem;
			min-width: 0;
		}

		.bot-chip-wide {
			flex-basis: 100%;
		}

		:global(.bot-qr-card.muij-card) {
			grid-template-columns: 1fr;
		}

		.bot-qr-frame {
			max-width: 180px;
		}

		.bot-editor-actions {
			flex-direction: column-reverse;
			align-items: stretch;
		}

		.bot-editor-actions-right {
			flex-direction: column;
		}

		.bot-logs-header {
			flex-direction: column;
			align-items: stretch;
		}
	}
</style>
