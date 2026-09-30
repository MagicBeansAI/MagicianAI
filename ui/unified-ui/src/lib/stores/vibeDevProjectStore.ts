import { get, writable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';
import {
	scopedRequestHeaders,
	scopeIdentityStore
} from '$lib/stores/scopeIdentityStore';
import type { ChatSession } from '$lib/stores/chatStore';

export interface VibeDevDeployment {
	deployment_id: string;
	target_id: string;
	target_label: string;
	provider: string;
	status: 'running' | 'succeeded' | 'failed' | string;
	public_url?: string | null;
	provider_deployment_id?: string | null;
	build_command?: string | null;
	output_dir: string;
	artifact_files: number;
	artifact_bytes: number;
	created_at_ms: number;
	completed_at_ms?: number | null;
	logs_tail?: string | null;
	error?: string | null;
}

export interface VibeDevDeployTarget {
	id: string;
	label: string;
	provider: string;
	kind: string;
	is_default: boolean;
}

export interface VibeDevProject {
	project_id: string;
	name: string;
	chat_thread_id: string;
	chat_session_id: string;
	repo_path?: string | null;
	repo_display_path?: string | null;
	repo_absolute_path?: string | null;
	active_root_task_id?: string | null;
	run_task_ids?: string[];
	preview_url?: string | null;
	published_url?: string | null;
	deployments: VibeDevDeployment[];
	deploy_targets: VibeDevDeployTarget[];
	created_at_ms: number;
	updated_at_ms: number;
	archived: boolean;
	chat_session_status: 'active' | 'archived' | 'missing' | string;
	/** Provenance (§13.3 #20): the meeting thread / chat session this project was seeded from. */
	source_meeting_thread_id?: string | null;
	source_chat_session_id?: string | null;
}

interface VibeDevProjectState {
	projects: VibeDevProject[];
	activeProjectId: string | null;
	workspaceDisplayPath: string | null;
	workspaceAbsolutePath: string | null;
	isLoading: boolean;
	error: string | null;
	scopeKey: string;
}

interface VibeDevProjectMutationResponse {
	project: VibeDevProject;
	session?: ChatSession;
}

interface VibeDevDeployResponse {
	project: VibeDevProject;
	deployment: VibeDevDeployment;
}

export interface VibeDevDeploySettingsCheck {
	ok: boolean;
	output_tail: string;
	error?: string | null;
}

export interface VibeDevDeploySettings {
	enabled: boolean;
	target: VibeDevDeployTarget;
	config_path: string;
	env_target_path: string;
	env_target_mode: string;
	env_development_path: string;
	env_path: string;
	account_id?: string | null;
	account_id_source?: string | null;
	pages_token_present: boolean;
	pages_token_source?: string | null;
	generic_token_present: boolean;
	generic_token_source?: string | null;
	process_env_updated: boolean;
	check?: VibeDevDeploySettingsCheck | null;
}

export interface UpdateVibeDevDeploySettingsRequest {
	enabled: boolean;
	account_id?: string | null;
	pages_api_token?: string | null;
	clear_token?: boolean;
	clear_credentials?: boolean;
}

interface VibeDevProjectDeleteResponse {
	deleted: boolean;
	project_id: string;
	chat_session_id: string;
	chat_session_deleted: boolean;
}

const initialState: VibeDevProjectState = {
	projects: [],
	activeProjectId: null,
	workspaceDisplayPath: null,
	workspaceAbsolutePath: null,
	isLoading: false,
	error: null,
	scopeKey: ''
};

function currentScopeKey(): string {
	const scope = get(scopeIdentityStore);
	return `${scope.principal}::${scope.workspace}`;
}

function buildUrl(path: string): string {
	return path;
}

async function readApiError(response: Response, fallback: string): Promise<Error> {
	try {
		const payload = await response.json();
		const message =
			(typeof payload?.message === 'string' && payload.message) ||
			(typeof payload?.error === 'string' && payload.error) ||
			fallback;
		return new Error(message);
	} catch {
		return new Error(fallback);
	}
}

function mergeProject(projects: VibeDevProject[], project: VibeDevProject): VibeDevProject[] {
	const next = projects.filter((candidate) => candidate.project_id !== project.project_id);
	next.push(project);
	return next.sort((a, b) => b.updated_at_ms - a.updated_at_ms);
}

function nextActiveProjectId(projects: VibeDevProject[], preferredId?: string | null): string | null {
	if (preferredId && projects.some((project) => project.project_id === preferredId && !project.archived)) {
		return preferredId;
	}
	return (
		projects.find((project) => project.chat_session_status === 'active' && !project.archived)?.project_id ||
		projects.find((project) => !project.archived)?.project_id ||
		null
	);
}

function createVibeDevProjectStore() {
	const { subscribe, set, update } = writable<VibeDevProjectState>(initialState);

	async function load(): Promise<VibeDevProject[]> {
		const scopeKey = currentScopeKey();
		update((state) =>
			state.scopeKey && state.scopeKey !== scopeKey
				? {
						...state,
						projects: [],
						activeProjectId: null,
						workspaceDisplayPath: null,
						workspaceAbsolutePath: null,
						isLoading: true,
						error: null,
						scopeKey
					}
				: { ...state, isLoading: true, error: null, scopeKey }
		);
		try {
			const response = await timedFetch(buildUrl('/api/magician/v2/vibedev/projects'), {
				headers: scopedRequestHeaders()
			});
			if (!response.ok) {
				throw await readApiError(response, `Failed to load VibeDev projects (${response.status})`);
			}
			const payload = await response.json();
			const projects = Array.isArray(payload.projects) ? payload.projects as VibeDevProject[] : [];
			const workspaceDisplayPath =
				typeof payload.workspace_display_path === 'string' ? payload.workspace_display_path : null;
			const workspaceAbsolutePath =
				typeof payload.workspace_absolute_path === 'string' ? payload.workspace_absolute_path : null;
			const activeProjectId =
				nextActiveProjectId(
					projects,
					typeof payload.active_project_id === 'string' ? payload.active_project_id : null
				);
			set({
				projects,
				activeProjectId,
				workspaceDisplayPath,
				workspaceAbsolutePath,
				isLoading: false,
				error: null,
				scopeKey
			});
			return projects;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => ({
				...state,
				projects: state.scopeKey === scopeKey ? state.projects : [],
				activeProjectId: state.scopeKey === scopeKey ? state.activeProjectId : null,
				workspaceDisplayPath: state.scopeKey === scopeKey ? state.workspaceDisplayPath : null,
				workspaceAbsolutePath: state.scopeKey === scopeKey ? state.workspaceAbsolutePath : null,
				isLoading: false,
				error: message,
				scopeKey
			}));
			return [];
		}
	}

	async function createProject(
		request?: string | { name?: string; repo_path?: string }
	): Promise<VibeDevProject | null> {
		const body =
			typeof request === 'string'
				? { name: request }
				: { name: request?.name, repo_path: request?.repo_path };
		const scopeKey = currentScopeKey();
		update((state) => ({ ...state, isLoading: true, error: null, scopeKey }));
		try {
			const response = await timedFetch(buildUrl('/api/magician/v2/vibedev/projects'), {
				method: 'POST',
				headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: JSON.stringify(body)
			});
			if (!response.ok) {
				throw await readApiError(response, `Failed to create VibeDev project (${response.status})`);
			}
			const payload = await response.json() as VibeDevProjectMutationResponse;
			update((state) => ({
				...state,
				projects: mergeProject(state.projects, payload.project),
				activeProjectId: payload.project.project_id,
				isLoading: false,
				error: null,
				scopeKey
			}));
			return payload.project;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => ({ ...state, isLoading: false, error: message, scopeKey }));
			return null;
		}
	}

	async function activateProject(projectId: string): Promise<VibeDevProject | null> {
		const scopeKey = currentScopeKey();
		update((state) => ({ ...state, isLoading: true, error: null, scopeKey }));
		try {
			const response = await timedFetch(
				buildUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/activate`),
				{
					method: 'POST',
					headers: scopedRequestHeaders()
				}
			);
			if (!response.ok) {
				throw await readApiError(response, `Failed to activate VibeDev project (${response.status})`);
			}
			const payload = await response.json() as VibeDevProjectMutationResponse;
			update((state) => ({
				...state,
				projects: state.projects
					.map((project) => ({
						...project,
						chat_session_status:
							project.project_id === payload.project.project_id
								? payload.project.chat_session_status
								: project.chat_session_status === 'active'
									? 'archived'
									: project.chat_session_status
					}))
					.filter((project) => project.project_id !== payload.project.project_id)
					.concat(payload.project)
					.sort((a, b) => b.updated_at_ms - a.updated_at_ms),
				activeProjectId: payload.project.project_id,
				isLoading: false,
				error: null,
				scopeKey
			}));
			return payload.project;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => ({ ...state, isLoading: false, error: message, scopeKey }));
			return null;
		}
	}

	async function updateProject(
		projectId: string,
		patch: Partial<
			Pick<
				VibeDevProject,
				| 'name'
				| 'active_root_task_id'
				| 'preview_url'
				| 'archived'
				| 'source_meeting_thread_id'
				| 'source_chat_session_id'
			>
		>
	): Promise<VibeDevProject | null> {
		const scopeKey = currentScopeKey();
		try {
			const response = await timedFetch(
				buildUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}`),
				{
					method: 'PATCH',
					headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
					body: JSON.stringify(patch)
				}
			);
			if (!response.ok) {
				throw await readApiError(response, `Failed to update VibeDev project (${response.status})`);
			}
			const payload = await response.json() as VibeDevProjectMutationResponse;
			update((state) => {
				const projects = mergeProject(state.projects, payload.project);
				const activeProjectId = payload.project.archived
					? nextActiveProjectId(
						projects,
						state.activeProjectId === projectId ? null : state.activeProjectId
					)
					: state.activeProjectId === projectId || payload.project.chat_session_status === 'active'
						? payload.project.project_id
						: state.activeProjectId;
				return {
					...state,
					projects,
					activeProjectId,
					error: null,
					scopeKey
				};
			});
			return payload.project;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => ({ ...state, error: message, scopeKey }));
			return null;
		}
	}

	async function renameProject(projectId: string, name: string): Promise<VibeDevProject | null> {
		return updateProject(projectId, { name });
	}

	async function archiveProject(projectId: string): Promise<VibeDevProject | null> {
		return updateProject(projectId, { archived: true });
	}

	async function deleteProject(projectId: string): Promise<VibeDevProjectDeleteResponse | null> {
		const scopeKey = currentScopeKey();
		update((state) => ({ ...state, isLoading: true, error: null, scopeKey }));
		try {
			const response = await timedFetch(
				buildUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}`),
				{
					method: 'DELETE',
					headers: scopedRequestHeaders()
				}
			);
			if (!response.ok) {
				throw await readApiError(response, `Failed to delete VibeDev project (${response.status})`);
			}
			const payload = await response.json() as VibeDevProjectDeleteResponse;
			update((state) => {
				const projects = state.projects.filter((project) => project.project_id !== payload.project_id);
				return {
					...state,
					projects,
					activeProjectId: nextActiveProjectId(
						projects,
						state.activeProjectId === payload.project_id ? null : state.activeProjectId
					),
					isLoading: false,
					error: null,
					scopeKey
				};
			});
			return payload;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => ({ ...state, isLoading: false, error: message, scopeKey }));
			return null;
		}
	}

	async function deployProject(
		projectId: string,
		targetId?: string | null
	): Promise<VibeDevDeployResponse | null> {
		const scopeKey = currentScopeKey();
		update((state) => ({ ...state, error: null, scopeKey }));
		try {
			const body: { target_id?: string } = {};
			if (targetId) body.target_id = targetId;
			const response = await timedFetch(
				buildUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/deploy`),
				{
					method: 'POST',
					headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
					body: JSON.stringify(body)
				}
			);
			if (!response.ok) {
				throw await readApiError(response, `Failed to deploy VibeDev project (${response.status})`);
			}
			const payload = await response.json() as VibeDevDeployResponse;
			update((state) => {
				if (state.scopeKey && state.scopeKey !== scopeKey) {
					return state;
				}
				const projects = mergeProject(state.projects, payload.project);
				const activeProjectId = state.activeProjectId === projectId
					? (payload.project.archived ? nextActiveProjectId(projects, null) : payload.project.project_id)
					: state.activeProjectId;
				return {
					...state,
					projects,
					activeProjectId,
					error: null,
					scopeKey
				};
			});
			return payload;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => (state.scopeKey && state.scopeKey !== scopeKey
				? state
				: { ...state, error: message, scopeKey }));
			return null;
		}
	}

	async function loadDeploySettings(): Promise<VibeDevDeploySettings | null> {
		const scopeKey = currentScopeKey();
		try {
			const response = await timedFetch(buildUrl('/api/magician/v2/vibedev/deploy/settings'), {
				headers: scopedRequestHeaders()
			});
			if (!response.ok) {
				throw await readApiError(response, `Failed to load VibeDev deploy settings (${response.status})`);
			}
			update((state) => ({ ...state, error: null, scopeKey }));
			return await response.json() as VibeDevDeploySettings;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => (state.scopeKey && state.scopeKey !== scopeKey
				? state
				: { ...state, error: message, scopeKey }));
			return null;
		}
	}

	async function saveDeploySettings(
		request: UpdateVibeDevDeploySettingsRequest
	): Promise<VibeDevDeploySettings | null> {
		const scopeKey = currentScopeKey();
		try {
			const response = await timedFetch(buildUrl('/api/magician/v2/vibedev/deploy/settings'), {
				method: 'PUT',
				headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: JSON.stringify(request)
			});
			if (!response.ok) {
				throw await readApiError(response, `Failed to save VibeDev deploy settings (${response.status})`);
			}
			const payload = await response.json() as VibeDevDeploySettings;
			update((state) => ({ ...state, error: null, scopeKey }));
			await load();
			return payload;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => (state.scopeKey && state.scopeKey !== scopeKey
				? state
				: { ...state, error: message, scopeKey }));
			return null;
		}
	}

	async function checkDeploySettings(): Promise<VibeDevDeploySettings | null> {
		const scopeKey = currentScopeKey();
		try {
			const response = await timedFetch(buildUrl('/api/magician/v2/vibedev/deploy/settings/check'), {
				method: 'POST',
				headers: scopedRequestHeaders()
			});
			if (!response.ok) {
				throw await readApiError(response, `Failed to check VibeDev deploy settings (${response.status})`);
			}
			update((state) => ({ ...state, error: null, scopeKey }));
			return await response.json() as VibeDevDeploySettings;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			update((state) => (state.scopeKey && state.scopeKey !== scopeKey
				? state
				: { ...state, error: message, scopeKey }));
			return null;
		}
	}

	/** Reveal the project's repo directory in the OS file manager (server-side;
	 *  for local dev that's the operator's own machine). Returns true on success. */
	async function openRepo(projectId: string): Promise<boolean> {
		try {
			const response = await timedFetch(
				buildUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/open-repo`),
				{ method: 'POST', headers: scopedRequestHeaders() }
			);
			return response.ok;
		} catch {
			return false;
		}
	}

	return {
		subscribe,
		load,
		createProject,
		activateProject,
		updateProject,
		renameProject,
		archiveProject,
		deleteProject,
		deployProject,
		loadDeploySettings,
		saveDeploySettings,
		checkDeploySettings,
		openRepo,
		clear: () => set(initialState)
	};
}

export const vibeDevProjectStore = createVibeDevProjectStore();
