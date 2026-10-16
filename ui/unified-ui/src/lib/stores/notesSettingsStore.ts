import { browser } from '$app/environment';
import { writable } from 'svelte/store';

export type NotesProviderId = 'local_markdown' | 'silverbullet';
export type TaskNotePublishMode = 'compact' | 'standard' | 'diagnostic';

export interface NotesProviderSettings {
	enabled: boolean;
	default_provider: NotesProviderId;
	fallback_provider: NotesProviderId;
	local_markdown: {
		root?: string | null;
	};
	silverbullet: {
		space_path?: string | null;
		local_url: string;
		server_url: string;
		public_origin?: string | null;
	};
	task_publishing: {
		auto_publish_completed: boolean;
		default_mode: TaskNotePublishMode;
		include_assets: boolean;
	};
}

export interface ResolvedNotesProviderSettings {
	default_provider: string;
	fallback_provider: string;
	local_markdown_root: string;
	silverbullet_space_path: string;
	silverbullet_write_safe: boolean;
	silverbullet_local_url: string;
	silverbullet_public_origin?: string | null;
	silverbullet_server_url: string;
}

export interface NotesSettingsEnvelope {
	principal: string;
	workspace: string;
	settings_path: string;
	settings: NotesProviderSettings;
	resolved: ResolvedNotesProviderSettings;
	warnings: string[];
}

export interface NotesProviderStatus {
	id: NotesProviderId;
	label: string;
	configured: boolean;
	available: boolean;
	writable: boolean;
	root: string;
	message: string;
}

export interface NotesProviderStatusResponse {
	principal: string;
	workspace: string;
	enabled: boolean;
	active_provider: NotesProviderId;
	fallback_provider: NotesProviderId;
	providers: NotesProviderStatus[];
	warnings: string[];
}

export interface NotesSettingsState {
	envelope: NotesSettingsEnvelope | null;
	status: NotesProviderStatusResponse | null;
	isLoading: boolean;
	isSaving: boolean;
	error: string | null;
	lastLoadedAt: number | null;
}

const DEFAULT_SETTINGS: NotesProviderSettings = {
	enabled: true,
	default_provider: 'local_markdown',
	fallback_provider: 'local_markdown',
	local_markdown: {},
	silverbullet: {
		local_url: 'http://127.0.0.1:3021',
		server_url: 'http://127.0.0.1:3021'
	},
	task_publishing: {
		auto_publish_completed: false,
		default_mode: 'standard',
		include_assets: true
	}
};

const DEFAULT_STATE: NotesSettingsState = {
	envelope: null,
	status: null,
	isLoading: false,
	isSaving: false,
	error: null,
	lastLoadedAt: null
};

function asRecord(value: unknown): Record<string, unknown> | null {
	return value && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(record: Record<string, unknown>, key: string): string | undefined {
	const value = record[key];
	return typeof value === 'string' ? value : undefined;
}

function readBoolean(record: Record<string, unknown>, key: string): boolean | undefined {
	const value = record[key];
	return typeof value === 'boolean' ? value : undefined;
}

function normalizeProvider(value: unknown): NotesProviderId {
	if (value === 'silverbullet' || value === 'silverbullet_space' || value === 'sb') {
		return 'silverbullet';
	}
	return 'local_markdown';
}

function normalizeTaskPublishMode(value: unknown): TaskNotePublishMode {
	return value === 'compact' || value === 'diagnostic' ? value : 'standard';
}

function normalizeSettings(raw: unknown): NotesProviderSettings {
	const root = asRecord(raw) ?? {};
	const local = asRecord(root.local_markdown) ?? {};
	const silverbullet = asRecord(root.silverbullet) ?? {};
	const taskPublishing = asRecord(root.task_publishing) ?? {};
	return {
		enabled: readBoolean(root, 'enabled') ?? DEFAULT_SETTINGS.enabled,
		default_provider: normalizeProvider(root.default_provider),
		fallback_provider: normalizeProvider(root.fallback_provider),
		local_markdown: {
			root: readString(local, 'root') ?? null
		},
		silverbullet: {
			space_path: readString(silverbullet, 'space_path') ?? null,
			local_url:
				readString(silverbullet, 'local_url')
				?? DEFAULT_SETTINGS.silverbullet.local_url,
			server_url:
				readString(silverbullet, 'server_url')
				?? DEFAULT_SETTINGS.silverbullet.server_url,
			public_origin: readString(silverbullet, 'public_origin') ?? null
		},
		task_publishing: {
			auto_publish_completed:
				readBoolean(taskPublishing, 'auto_publish_completed')
				?? DEFAULT_SETTINGS.task_publishing.auto_publish_completed,
			default_mode: normalizeTaskPublishMode(taskPublishing.default_mode),
			include_assets:
				readBoolean(taskPublishing, 'include_assets')
				?? DEFAULT_SETTINGS.task_publishing.include_assets
		}
	};
}

function normalizeEnvelope(raw: unknown): NotesSettingsEnvelope | null {
	const root = asRecord(raw);
	if (!root) return null;
	const resolved = asRecord(root.resolved);
	if (!resolved) return null;
	const settings = normalizeSettings(root.settings);
	return {
		principal: readString(root, 'principal') ?? '',
		workspace: readString(root, 'workspace') ?? '',
		settings_path: readString(root, 'settings_path') ?? '',
		settings,
		resolved: {
			default_provider: readString(resolved, 'default_provider') ?? settings.default_provider,
			fallback_provider: readString(resolved, 'fallback_provider') ?? settings.fallback_provider,
			local_markdown_root: readString(resolved, 'local_markdown_root') ?? '',
			silverbullet_space_path: readString(resolved, 'silverbullet_space_path') ?? '',
			silverbullet_write_safe:
				readBoolean(resolved, 'silverbullet_write_safe') ?? true,
			silverbullet_local_url:
				readString(resolved, 'silverbullet_local_url')
				?? settings.silverbullet.local_url,
			silverbullet_public_origin:
				readString(resolved, 'silverbullet_public_origin')
				?? settings.silverbullet.public_origin
				?? null,
			silverbullet_server_url:
				readString(resolved, 'silverbullet_server_url')
				?? DEFAULT_SETTINGS.silverbullet.server_url
		},
		warnings: Array.isArray(root.warnings)
			? root.warnings.filter((warning): warning is string => typeof warning === 'string')
			: []
	};
}

function normalizeStatus(raw: unknown): NotesProviderStatusResponse | null {
	const root = asRecord(raw);
	if (!root) return null;
	const providers = Array.isArray(root.providers)
		? root.providers
			.map((entry) => {
				const provider = asRecord(entry);
				if (!provider) return null;
				return {
					id: normalizeProvider(provider.id),
					label: readString(provider, 'label') ?? normalizeProvider(provider.id),
					configured: readBoolean(provider, 'configured') ?? false,
					available: readBoolean(provider, 'available') ?? false,
					writable: readBoolean(provider, 'writable') ?? false,
					root: readString(provider, 'root') ?? '',
					message: readString(provider, 'message') ?? ''
				};
			})
			.filter((entry): entry is NotesProviderStatus => Boolean(entry))
		: [];
	return {
		principal: readString(root, 'principal') ?? '',
		workspace: readString(root, 'workspace') ?? '',
		enabled: readBoolean(root, 'enabled') ?? true,
		active_provider: normalizeProvider(root.active_provider),
		fallback_provider: normalizeProvider(root.fallback_provider),
		providers,
		warnings: Array.isArray(root.warnings)
			? root.warnings.filter((warning): warning is string => typeof warning === 'string')
			: []
	};
}

async function readApiError(response: Response): Promise<string> {
	try {
		const payload = asRecord(await response.json());
		return (
			readString(payload ?? {}, 'message')
			?? readString(payload ?? {}, 'error')
			?? `Request failed (${response.status})`
		);
	} catch {
		return `Request failed (${response.status})`;
	}
}

function createNotesSettingsStore() {
	const { subscribe, update } = writable<NotesSettingsState>(DEFAULT_STATE);
	let current = DEFAULT_STATE;

	function setState(next: NotesSettingsState): void {
		current = next;
		update(() => next);
	}

	async function refresh(): Promise<NotesSettingsEnvelope | null> {
		if (!browser) return null;
		setState({ ...current, isLoading: true, error: null });
		try {
			const [settingsResponse, statusResponse] = await Promise.all([
				fetch('/api/magician/v2/notes/settings'),
				fetch('/api/magician/v2/notes/providers/status')
			]);
			if (!settingsResponse.ok) throw new Error(await readApiError(settingsResponse));
			if (!statusResponse.ok) throw new Error(await readApiError(statusResponse));
			const envelope = normalizeEnvelope(await settingsResponse.json());
			const status = normalizeStatus(await statusResponse.json());
			if (!envelope) throw new Error('Malformed notes settings response');
			setState({
				envelope,
				status,
				isLoading: false,
				isSaving: false,
				error: null,
				lastLoadedAt: Date.now()
			});
			return envelope;
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to load notes settings';
			setState({ ...current, isLoading: false, error: message });
			throw error;
		}
	}

	async function save(settings: NotesProviderSettings): Promise<NotesSettingsEnvelope> {
		if (!browser) throw new Error('Notes settings are unavailable outside the browser');
		const previous = current;
		setState({ ...current, isSaving: true, error: null });
		try {
			const response = await fetch('/api/magician/v2/notes/settings', {
				method: 'PUT',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({
					...settings
				})
			});
			if (!response.ok) throw new Error(await readApiError(response));
			const envelope = normalizeEnvelope(await response.json());
			if (!envelope) throw new Error('Malformed notes settings response');
			setState({
				...current,
				envelope,
				isSaving: false,
				error: null,
				lastLoadedAt: Date.now()
			});
			await refresh();
			return envelope;
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to save notes settings';
			setState({ ...previous, isSaving: false, error: message });
			throw error;
		}
	}

	return {
		subscribe,
		refresh,
		save
	};
}

export const notesSettingsStore = createNotesSettingsStore();
export const refreshNotesSettings = notesSettingsStore.refresh;
export const saveNotesSettings = notesSettingsStore.save;
