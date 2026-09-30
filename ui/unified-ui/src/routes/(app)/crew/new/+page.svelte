<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onDestroy, onMount } from 'svelte';
	import { get } from 'svelte/store';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import NativeCrewRenderer from '$lib/magician/crew/NativeCrewRenderer.svelte';
	import type { CrewNativeComponent, CrewNativeInteractionEventDetail } from '$lib/magician/crew/nativeSurface';
	import { createAgent, refreshAgents, updateAgent, agentList } from '$lib/stores/agentStore';
	import type { AgentKind } from '$lib/stores/agentStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		DEFAULT_NEW_AGENT_YAML,
		createAgentFromYaml,
		fetchAgentDefinitionRecord,
		serializeDefinitionYaml,
		updateAgentFromYaml,
		type AgentDefinitionRecord
	} from '../definitionApi';

	type EditorMode = 'form' | 'yaml';

	import { cronToHumanReadable, isValidCronExpression } from '$lib/utils/cron';
	import { timedFetch } from '$lib/shared/fetch';

	interface FocusAreaFormEntry {
		name: string;
		description: string;
		priority: 'high' | 'medium' | 'low';
		schedule: string;
		program: string;
		scope_csv: string;
	}

	interface AgentFormState {
		agent_id: string;
		name: string;
		description: string;
		persona: string;
		trust_level: string;
		kind: AgentKind;
		tools: string[];
		excluded_tools: string[];
		delegation_targets: string[];
		delegate_to_any: boolean;
		max_delegation_depth: number;
		harness_enabled: boolean;
		harness_program_section: string;
		autonomous_enabled: boolean;
		autonomous_schedule: string;
		autonomous_focus_areas: FocusAreaFormEntry[];
		autonomous_max_tasks_per_cycle: number;
		autonomous_max_steps_per_plan: number;
		user_memory_isolation: 'shared' | 'fully_isolated';
		readable_agents_csv: string;
	}

	const DEFAULT_FORM_STATE: AgentFormState = {
		agent_id: '',
		name: 'Demo Agent',
		description: 'Example agent definition',
		persona: 'You are a practical autonomous assistant.',
		trust_level: 'local',
		kind: 'Worker',
		tools: [],
		excluded_tools: [],
		delegation_targets: [],
		delegate_to_any: false,
		max_delegation_depth: 3,
		harness_enabled: false,
		harness_program_section: '',
		autonomous_enabled: false,
		autonomous_schedule: '0 */4 * * *',
		autonomous_focus_areas: [],
		autonomous_max_tasks_per_cycle: 3,
		autonomous_max_steps_per_plan: 10,
		user_memory_isolation: 'shared',
		readable_agents_csv: ''
	};

	const DOUBLES_DEFINITION_SCHEMA_VERSION = 'presto.doubles-definition-v2';
	const TRUST_LEVEL_OPTIONS = [
		{ value: 'local', label: 'Local' },
		{ value: 'reviewed', label: 'Reviewed' },
		{ value: 'builtin', label: 'Built-in' },
		{ value: 'untrusted', label: 'Untrusted' }
	];
	const AGENT_KIND_OPTIONS: Array<{ value: AgentKind; label: string }> = [
		{ value: 'Worker', label: 'Worker' },
		{ value: 'Personal', label: 'Personal' }
	];
	const BOOLEAN_OPTIONS = [
		{ value: 'false', label: 'No' },
		{ value: 'true', label: 'Yes' }
	];

	let editorMode: EditorMode = 'form';
	let formState: AgentFormState = cloneFormState(DEFAULT_FORM_STATE);
	let yamlDraft = DEFAULT_NEW_AGENT_YAML;
	let isHydratingEdit = false;
	let isSubmitting = false;
	let pageError: string | null = null;
	let submitError: string | null = null;
	let editAgentId = '';
	let activeRequestToken = 0;
	let editRecord: AgentDefinitionRecord | null = null;
	let editIfMatchEtag = '';
	let editorMounted = false;
	let currentEditorScopeKey = '';
	let lastEditorScopeKey = '';

	$: editing = editAgentId.length > 0;
	$: submitLabel = isSubmitting ? (editing ? 'Saving...' : 'Creating...') : (editing ? 'Save agent' : 'Create agent');
	$: requestedEditId = ($page.url.searchParams.get('edit') || '').trim();
	$: currentEditorScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (browser && requestedEditId !== editAgentId) {
		void hydrateFromEditQuery(requestedEditId);
	}
	$: existingAgentIds = $agentList.map(a => a.agent_id);
	$: components = buildDefinitionSurface({
		editing,
		editAgentId,
		editorMode,
		formState,
		yamlDraft,
		isHydratingEdit,
		isSubmitting,
		pageError,
		submitError,
		submitLabel,
		existingAgentIds,
		schemaVersion: DOUBLES_DEFINITION_SCHEMA_VERSION
	});

	function cloneFormState(state: AgentFormState): AgentFormState {
		return {
			...state,
			tools: [...state.tools],
			excluded_tools: [...state.excluded_tools],
			delegation_targets: [...state.delegation_targets],
			autonomous_focus_areas: state.autonomous_focus_areas.map((fa) => ({ ...fa })),
			harness_enabled: state.harness_enabled,
			harness_program_section: state.harness_program_section,
			user_memory_isolation: state.user_memory_isolation,
			readable_agents_csv: state.readable_agents_csv
		};
	}

	function resetEditorState(): void {
		activeRequestToken += 1;
		formState = cloneFormState(DEFAULT_FORM_STATE);
		yamlDraft = DEFAULT_NEW_AGENT_YAML;
		isHydratingEdit = false;
		isSubmitting = false;
		pageError = null;
		submitError = null;
		editAgentId = '';
		editRecord = null;
		editIfMatchEtag = '';
		editorMode = 'form';
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		return typeof value === 'object' && value !== null && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') return String(value);
		return '';
	}

	function readString(record: Record<string, unknown>, key: string): string | undefined {
		const value = record[key];
		return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
	}

	function readArray(record: Record<string, unknown>, key: string): unknown[] {
		const value = record[key];
		return Array.isArray(value) ? value : [];
	}

	function readStringArray(record: Record<string, unknown>, key: string): string[] {
		return readArray(record, key).filter((item): item is string => typeof item === 'string' && item.trim().length > 0);
	}

	function normalizeFocusAreaPriority(value: string): 'high' | 'medium' | 'low' {
		const lower = value.trim().toLowerCase();
		if (lower === 'high') return 'high';
		if (lower === 'low') return 'low';
		return 'medium';
	}

	function cloneDefinition(definition: Record<string, unknown>): Record<string, unknown> {
		try {
			return JSON.parse(JSON.stringify(definition)) as Record<string, unknown>;
		} catch {
			return { ...definition };
		}
	}

	function buildDefinitionFromForm(
		state: AgentFormState,
		overrides?: {
			preserveAgentId?: string;
			baseDefinition?: Record<string, unknown>;
		}
	): Record<string, unknown> {
		const baseDefinition = overrides?.baseDefinition ? cloneDefinition(overrides.baseDefinition) : {};
		const agentId = overrides?.preserveAgentId || state.agent_id.trim();

		// Remove legacy fields from base definition
		delete baseDefinition.goals;
		delete baseDefinition.triggers;

		const delegateTo = state.delegate_to_any ? ['*'] : state.delegation_targets;

		const definition: Record<string, unknown> = {
			...baseDefinition,
			agent_id: agentId,
			name: state.name.trim(),
			description: state.description.trim(),
			persona: state.persona.trim(),
			trust_level: state.trust_level.trim(),
			kind: state.kind,
			tools: [...state.tools],
			excluded_tools: [...state.excluded_tools]
		};

		// Delegation targets for Personal and Worker kinds
		if (state.kind === 'Personal' || state.kind === 'Worker') {
			definition.delegation_targets = delegateTo;
			const baseCoordination = asRecord(baseDefinition.coordination);
			definition.coordination = {
				...(baseCoordination || {}),
				max_delegation_depth: state.max_delegation_depth
			};
		}

		// Autonomous config for Personal agents only
		if (state.kind === 'Personal' && state.autonomous_enabled) {
			definition.autonomous_config = {
				schedule: state.autonomous_schedule.trim(),
				focus_areas: state.autonomous_focus_areas.map((fa) => {
					const scope = fa.scope_csv
						.split(',')
						.map((value) => value.trim())
						.filter((value) => value.length > 0);
					return {
						name: fa.name.trim(),
						description: fa.description.trim(),
						priority: fa.priority,
						schedule: fa.schedule.trim() || undefined,
						program: fa.program.trim() || undefined,
						scope: scope.length > 0 ? scope : undefined
					};
				}),
				max_tasks_per_cycle: state.autonomous_max_tasks_per_cycle,
				max_steps_per_plan: state.autonomous_max_steps_per_plan
			};
		} else {
			// Remove autonomous_config when not enabled or not Personal
			delete definition.autonomous_config;
		}

		// Task 2.22: Memory isolation and readable agents for Personal agents
		if (state.kind === 'Personal') {
			if (state.harness_enabled) {
				definition.harness = {};
				if (state.harness_program_section.trim().length > 0) {
					(definition.harness as Record<string, unknown>).program_section =
						state.harness_program_section.trim();
				}
			} else {
				delete definition.harness;
			}
			definition.user_memory_isolation = state.user_memory_isolation;
			const readableAgentsList = state.readable_agents_csv
				.split(',')
				.map((s) => s.trim())
				.filter((s) => s.length > 0);
			if (readableAgentsList.length > 0) {
				definition.readable_agents = readableAgentsList;
			} else {
				delete definition.readable_agents;
			}
		} else {
			delete definition.harness;
			delete definition.user_memory_isolation;
			delete definition.readable_agents;
		}

		return definition;
	}

	function validateForm(state: AgentFormState, isEdit: boolean): string | null {
		if (state.agent_id.trim().length > 0 && !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(state.agent_id.trim())) {
			return 'agent_id must be lowercase with hyphens only (e.g. my-agent-1), or leave blank to auto-generate';
		}
		if (state.name.trim().length === 0) return 'name is required';
		if (state.persona.trim().length === 0) return 'persona is required';
		if (state.trust_level.trim().length === 0) return 'trust_level is required';
		const validKinds: AgentKind[] = ['Personal', 'Worker'];
		if (!validKinds.includes(state.kind)) {
			return `kind must be one of: ${validKinds.join(', ')}`;
		}
		if (state.max_delegation_depth < 1 || state.max_delegation_depth > 10) {
			return 'max_delegation_depth must be between 1 and 10';
		}

		// Validate autonomous config when enabled for Personal agents
		if (state.kind === 'Personal' && state.autonomous_enabled) {
			if (!state.autonomous_schedule.trim()) {
				return 'Autonomous schedule (cron expression) is required when autonomous mode is enabled';
			}
			if (!isValidCronExpression(state.autonomous_schedule)) {
				return 'Invalid cron expression for autonomous schedule. Must be 5 space-separated fields (e.g. "0 */4 * * *")';
			}
			if (state.autonomous_focus_areas.length === 0) {
				return 'At least one focus area is required when autonomous mode is enabled';
			}
			for (let i = 0; i < state.autonomous_focus_areas.length; i++) {
				const fa = state.autonomous_focus_areas[i];
				if (!fa.name.trim()) {
					return `Focus area ${i + 1}: name is required`;
				}
				if (!fa.description.trim()) {
					return `Focus area ${i + 1}: description is required`;
				}
				if (fa.schedule.trim() && !isValidCronExpression(fa.schedule)) {
					return `Focus area ${i + 1}: schedule must be a valid cron expression`;
				}
			}
			if (state.autonomous_max_tasks_per_cycle < 1 || state.autonomous_max_tasks_per_cycle > 10) {
				return 'Max tasks per cycle must be between 1 and 10';
			}
			if (state.autonomous_max_steps_per_plan < 1 || state.autonomous_max_steps_per_plan > 30) {
				return 'Max steps per plan must be between 1 and 30';
			}
		}

		return null;
	}

	function normalizeKind(value: unknown): AgentKind {
		const str = asString(value).trim();
		if (str === 'Personal' || str === 'personal') return 'Personal';
		return 'Worker';
	}

	function agentSelectionOptions(
		existingIds: string[],
		selectedIds: string[],
		excludeId?: string
	): Array<{ value: string; label: string }> {
		const excluded = (excludeId || '').trim();
		return Array.from(new Set([...selectedIds, ...existingIds]))
			.map((id) => id.trim())
			.filter((id) => id.length > 0 && id !== '*' && id !== excluded)
			.sort((left, right) => left.localeCompare(right))
			.map((id) => ({ value: id, label: id }));
	}

	function formStateFromDefinition(definition: Record<string, unknown>): AgentFormState {
		const coordination = asRecord(definition.coordination);
		const delegateTo = readStringArray(definition, 'delegation_targets');
		const maxDelegationDepth = coordination
			? (typeof coordination.max_delegation_depth === 'number' ? coordination.max_delegation_depth : 3)
			: (typeof definition.max_delegation_depth === 'number' ? definition.max_delegation_depth as number : 3);
		const tools = readStringArray(definition, 'tools');
		const excludedTools = readStringArray(definition, 'excluded_tools');
		const harnessRecord = asRecord(definition.harness);

		// Parse autonomous config
		const autoConfigRecord = asRecord(definition.autonomous_config);
		const autonomousEnabled = autoConfigRecord !== null;
		let autonomousSchedule = DEFAULT_FORM_STATE.autonomous_schedule;
		let autonomousFocusAreas: FocusAreaFormEntry[] = [];
		let autonomousMaxTasksPerCycle = DEFAULT_FORM_STATE.autonomous_max_tasks_per_cycle;
		let autonomousMaxStepsPerPlan = DEFAULT_FORM_STATE.autonomous_max_steps_per_plan;

		if (autoConfigRecord) {
			autonomousSchedule = readString(autoConfigRecord, 'schedule') || autonomousSchedule;
			const rawFocusAreas = readArray(autoConfigRecord, 'focus_areas');
			autonomousFocusAreas = rawFocusAreas
				.map((fa) => {
					const faRec = asRecord(fa);
					if (!faRec) return null;
					const name = readString(faRec, 'name');
					const description = readString(faRec, 'description');
					const priority = readString(faRec, 'priority');
					if (!name) return null;
					return {
						name,
						description: description || '',
						priority: normalizeFocusAreaPriority(priority || 'medium'),
						schedule: readString(faRec, 'schedule') || '',
						program: readString(faRec, 'program') || '',
						scope_csv: readStringArray(faRec, 'scope').join(', ')
					};
				})
				.filter((fa): fa is FocusAreaFormEntry => fa !== null);
			if (typeof autoConfigRecord.max_tasks_per_cycle === 'number') {
				autonomousMaxTasksPerCycle = autoConfigRecord.max_tasks_per_cycle;
			}
			if (typeof autoConfigRecord.max_steps_per_plan === 'number') {
				autonomousMaxStepsPerPlan = autoConfigRecord.max_steps_per_plan;
			}
		}

		// Task 2.22: Parse user_memory_isolation and readable_agents
		const rawIsolation = readString(definition, 'user_memory_isolation');
		const userMemoryIsolation: 'shared' | 'fully_isolated' =
			rawIsolation === 'fully_isolated' ? 'fully_isolated' : 'shared';
		const readableAgentsList = readStringArray(definition, 'readable_agents');

		return {
			agent_id: readString(definition, 'agent_id') || '',
			name: readString(definition, 'name') || DEFAULT_FORM_STATE.name,
			description: readString(definition, 'description') || '',
			persona: readString(definition, 'persona') || DEFAULT_FORM_STATE.persona,
			trust_level: readString(definition, 'trust_level') || DEFAULT_FORM_STATE.trust_level,
			kind: normalizeKind(definition.kind),
			tools,
			excluded_tools: excludedTools,
			delegation_targets: delegateTo.filter(d => d !== '*'),
			delegate_to_any: delegateTo.includes('*'),
			max_delegation_depth: maxDelegationDepth,
			harness_enabled: harnessRecord !== null,
			harness_program_section: readString(harnessRecord || {}, 'program_section') || '',
			autonomous_enabled: autonomousEnabled,
			autonomous_schedule: autonomousSchedule,
			autonomous_focus_areas: autonomousFocusAreas,
			autonomous_max_tasks_per_cycle: autonomousMaxTasksPerCycle,
			autonomous_max_steps_per_plan: autonomousMaxStepsPerPlan,
			user_memory_isolation: userMemoryIsolation,
			readable_agents_csv: readableAgentsList.join(', ')
		};
	}

	function applyKindDefaults(state: AgentFormState, kind: AgentKind): AgentFormState {
		const updated = cloneFormState(state);
		updated.kind = kind;
		if (kind === 'Personal') {
			updated.delegate_to_any = true;
			updated.max_delegation_depth = 3;
			// Empty `tools` means "all available" via backend `resolved_tools`,
			// so we leave it as [] rather than copying the live list at form-mount
			// time (which would freeze the personal-agent surface against pack
			// hot-reloads).
			updated.tools = [];
		} else if (kind === 'Worker') {
			updated.delegate_to_any = false;
			updated.max_delegation_depth = 1;
			updated.tools = [];
			updated.harness_enabled = false;
			updated.autonomous_enabled = false;
		}
		return updated;
	}

	function collectFocusAreaIndexes(values: Record<string, unknown>): number[] {
		const indexes = new Set<number>();
		for (const key of Object.keys(values)) {
			const match = /^fa_(?:name|desc|priority|schedule|program|scope)_(\d+)$/.exec(key);
			if (!match) continue;
			const parsed = Number.parseInt(match[1], 10);
			if (Number.isFinite(parsed) && parsed >= 0) {
				indexes.add(parsed);
			}
		}
		return [...indexes].sort((left, right) => left - right);
	}

	function formStateFromFormValues(values: Record<string, unknown>): AgentFormState {
		const delegateToAny = asString(values.delegate_to_any) === 'true' || values.delegate_to_any === true;

		const newKind = normalizeKind(values.kind);
		const previousKind = formState.kind;
		const harnessEnabled = asString(values.harness_enabled) === 'true' || values.harness_enabled === true;
		const harnessProgramSection = asString(values.harness_program_section).trim();

		// Parse autonomous config fields from form values
		const autonomousEnabled = asString(values.autonomous_enabled) === 'true' || values.autonomous_enabled === true;
		const autonomousSchedule = asString(values.autonomous_schedule).trim() || formState.autonomous_schedule;
		const autonomousMaxTasksPerCycle = Math.max(1, Math.min(10, Number(asString(values.autonomous_max_tasks_per_cycle)) || formState.autonomous_max_tasks_per_cycle));
		const autonomousMaxStepsPerPlan = Math.max(1, Math.min(30, Number(asString(values.autonomous_max_steps_per_plan)) || formState.autonomous_max_steps_per_plan));

		// Reconstruct focus areas from indexed form fields
		const focusAreas: FocusAreaFormEntry[] = [];
		for (const i of collectFocusAreaIndexes(values)) {
			const faName = asString(values[`fa_name_${i}`]).trim();
			const faDesc = asString(values[`fa_desc_${i}`]).trim();
			const faPriority = asString(values[`fa_priority_${i}`]).trim();
			const faSchedule = asString(values[`fa_schedule_${i}`]).trim();
			const faProgram = asString(values[`fa_program_${i}`]).trim();
			const faScopeCsv = asString(values[`fa_scope_${i}`]).trim();
			if (faName || faDesc || faSchedule || faProgram || faScopeCsv) {
				focusAreas.push({
					name: faName,
					description: faDesc,
					priority: normalizeFocusAreaPriority(faPriority || 'medium'),
					schedule: faSchedule,
					program: faProgram,
					scope_csv: faScopeCsv
				});
			}
		}

		// Task 2.22: Parse user_memory_isolation and readable_agents from form
		const rawFormIsolation = asString(values.user_memory_isolation).trim().toLowerCase();
		const formUserMemoryIsolation: 'shared' | 'fully_isolated' =
			rawFormIsolation === 'fully_isolated' ? 'fully_isolated' : 'shared';
		const formReadableAgentsCsv = asString(values.readable_agents_csv).trim();

		let nextState: AgentFormState = {
			agent_id: asString(values.agent_id).trim(),
			name: asString(values.name).trim(),
			description: asString(values.description),
			persona: asString(values.persona),
			trust_level: asString(values.trust_level).trim() || DEFAULT_FORM_STATE.trust_level,
			kind: newKind,
			tools: formState.tools,
			excluded_tools: formState.excluded_tools,
			delegation_targets: formState.delegation_targets,
			delegate_to_any: delegateToAny,
			max_delegation_depth: Math.max(1, Math.min(10, Number(asString(values.max_delegation_depth)) || 3)),
			harness_enabled: harnessEnabled,
			harness_program_section: harnessProgramSection,
			autonomous_enabled: autonomousEnabled,
			autonomous_schedule: autonomousSchedule,
			autonomous_focus_areas: focusAreas,
			autonomous_max_tasks_per_cycle: autonomousMaxTasksPerCycle,
			autonomous_max_steps_per_plan: autonomousMaxStepsPerPlan,
			user_memory_isolation: formUserMemoryIsolation,
			readable_agents_csv: formReadableAgentsCsv
		};

		if (newKind !== previousKind) {
			nextState = applyKindDefaults(nextState, newKind);
		}

		return nextState;
	}

	async function hydrateFromEditQuery(nextEditId: string): Promise<void> {
		const requestToken = ++activeRequestToken;
		const requestScopeKey = currentEditorScopeKey;
		editAgentId = nextEditId;
		pageError = null;
		submitError = null;
		editRecord = null;
		editIfMatchEtag = '';

		if (!nextEditId) {
			formState = cloneFormState(DEFAULT_FORM_STATE);
			yamlDraft = DEFAULT_NEW_AGENT_YAML;
			editorMode = 'form';
			return;
		}

		isHydratingEdit = true;
		try {
			const record = await fetchAgentDefinitionRecord(nextEditId);
			if (
				requestToken !== activeRequestToken
				|| editAgentId !== nextEditId
				|| requestScopeKey !== currentEditorScopeKey
			) {
				return;
			}
			if (!record) {
				pageError = `Agent "${nextEditId}" was not found`;
				return;
			}
			editRecord = record;
			editIfMatchEtag = record.etag;
			formState = formStateFromDefinition(record.definition);
			yamlDraft = serializeDefinitionYaml(record.definition);
		} catch (err) {
			if (
				requestToken !== activeRequestToken
				|| editAgentId !== nextEditId
				|| requestScopeKey !== currentEditorScopeKey
			) {
				return;
			}
			pageError = err instanceof Error ? err.message : 'Failed to load agent for editing';
		} finally {
			if (
				requestToken === activeRequestToken
				&& editAgentId === nextEditId
				&& requestScopeKey === currentEditorScopeKey
			) {
				isHydratingEdit = false;
			}
		}
	}

	function applyFormToYamlDraft(): void {
		const validationError = validateForm(formState, editing);
		if (validationError) {
			submitError = validationError;
			showError(validationError);
			return;
		}
		const definition = buildDefinitionFromForm(formState, {
			preserveAgentId: editing ? editAgentId : undefined,
			baseDefinition: editing ? editRecord?.definition : undefined
		});
		yamlDraft = serializeDefinitionYaml(definition);
		submitError = null;
		showSuccess('YAML draft updated from form fields');
	}

	function resetYamlDraft(): void {
		if (editRecord) {
			yamlDraft = serializeDefinitionYaml(editRecord.definition);
		} else {
			yamlDraft = DEFAULT_NEW_AGENT_YAML;
		}
		submitError = null;
	}

	function switchMode(nextMode: EditorMode): void {
		if (editorMode === nextMode) return;
		editorMode = nextMode;
		submitError = null;
	}

	async function submitFormMode(currentFormState: AgentFormState): Promise<void> {
		const requestScopeKey = currentEditorScopeKey;
		const validationError = validateForm(currentFormState, editing);
		if (validationError) {
			throw new Error(validationError);
		}

		const definition = buildDefinitionFromForm(currentFormState, {
			preserveAgentId: editing ? editAgentId : undefined,
			baseDefinition: editing ? editRecord?.definition : undefined
		});

		if (editing) {
			if (!editIfMatchEtag) {
				throw new Error('Missing current definition ETag for update');
			}
			const updated = await updateAgent(editAgentId, definition, { ifMatch: editIfMatchEtag });
			await refreshAgents();
			if (requestScopeKey !== currentEditorScopeKey) return;
			showSuccess(`Updated ${updated.name || updated.agent_id}`);
			await goto(`/crew/${encodeURIComponent(updated.agent_id)}`);
			return;
		}

		const created = await createAgent(definition);
		await refreshAgents();
		if (requestScopeKey !== currentEditorScopeKey) return;
		showSuccess(`Created ${created.name || created.agent_id}`);
		await goto(`/crew/${encodeURIComponent(created.agent_id)}`);
	}

	function validateYamlDraft(text: string): string | null {
		const trimmed = text.trim();
		if (trimmed.length === 0) return 'YAML definition cannot be empty';
		const requiredKeys = ['name:', 'persona:', 'kind:'] as const;
		for (const key of requiredKeys) {
			if (!new RegExp(`^${key.replace(':', ':')}`, 'm').test(trimmed)) {
				return `Missing required field: ${key.replace(':', '')}`;
			}
		}
		const kindMatch = trimmed.match(/^kind:\s*(.+)$/m);
		if (kindMatch) {
			const kindValue = kindMatch[1].trim().replace(/^['"]|['"]$/g, '');
			if (!['Personal', 'Worker'].includes(kindValue)) {
				return `Invalid kind "${kindValue}" — must be Personal or Worker`;
			}
		}
		return null;
	}

	async function submitYamlMode(currentYamlDraft: string): Promise<void> {
		const requestScopeKey = currentEditorScopeKey;
		const yamlError = validateYamlDraft(currentYamlDraft);
		if (yamlError) {
			throw new Error(yamlError);
		}

		if (editing) {
			if (!editIfMatchEtag) {
				throw new Error('Missing current definition ETag for YAML update');
			}
			const updated = await updateAgentFromYaml(editAgentId, currentYamlDraft, editIfMatchEtag);
			const updatedAgentId = readString(updated.definition, 'agent_id');
			if (!updatedAgentId) {
				throw new Error('Updated agent response missing agent_id');
			}
			await refreshAgents();
			if (requestScopeKey !== currentEditorScopeKey) return;
			showSuccess(`Updated ${updatedAgentId}`);
			await goto(`/crew/${encodeURIComponent(updatedAgentId)}`);
			return;
		}

		const created = await createAgentFromYaml(currentYamlDraft);
		const createdAgentId = readString(created.definition, 'agent_id');
		if (!createdAgentId) {
			throw new Error('Created agent response missing agent_id');
		}
		await refreshAgents();
		if (requestScopeKey !== currentEditorScopeKey) return;
		showSuccess(`Created ${createdAgentId}`);
		await goto(`/crew/${encodeURIComponent(createdAgentId)}`);
	}

	async function submitDefinition(values?: Record<string, unknown>): Promise<void> {
		if (isSubmitting || isHydratingEdit) return;
		submitError = null;
		isSubmitting = true;
		try {
			if (editorMode === 'form') {
				const nextFormState = values ? formStateFromFormValues(values) : formState;
				formState = cloneFormState(nextFormState);
				await submitFormMode(nextFormState);
			} else {
				const nextYamlDraft = values ? asString(values.yaml_definition) : yamlDraft;
				yamlDraft = nextYamlDraft;
				await submitYamlMode(nextYamlDraft);
			}
		} catch (err) {
			const message = err instanceof Error ? err.message : 'Failed to save definition';
			submitError = message;
			showError(message);
		} finally {
			isSubmitting = false;
		}
	}

	function buildDefinitionSurface(input: {
		editing: boolean;
		editAgentId: string;
		editorMode: EditorMode;
		formState: AgentFormState;
		yamlDraft: string;
		isHydratingEdit: boolean;
		isSubmitting: boolean;
		pageError: string | null;
		submitError: string | null;
		submitLabel: string;
		existingAgentIds: string[];
		schemaVersion: string;
	}): CrewNativeComponent[] {
		const components: CrewNativeComponent[] = [
			{
				id: 'presto-definition-header',
				component_type: 'Card',
				props: {
					title: input.editing ? 'Edit crew member' : 'New crew member',
					subtitle: `Definition editor · ${input.schemaVersion}`,
					body: input.editing
						? `Updating ${input.editAgentId} with form or YAML mode.`
						: 'Create a new agent definition using guided fields or direct YAML payload.'
				},
				children: [
					{
						id: 'presto-definition-actions',
						component_type: 'Stack',
						props: {
							direction: 'row',
							gap: '0.5rem',
							wrap: true
						},
						children: [
							{
								id: 'presto-definition-action-back',
								component_type: 'Button',
								label: 'Back to agents',
								props: {
									interactive: true,
									variant: 'outline',
									size: 'sm'
								}
							},
							{
								id: 'presto-definition-action-open-detail',
								component_type: 'Button',
								label: 'Open detail',
								props: {
									interactive: true,
									variant: 'outline',
									size: 'sm',
									disabled: !input.editing
								}
							},
							{
								id: 'presto-definition-mode-form',
								component_type: 'Button',
								label: 'Form mode',
								props: {
									interactive: true,
									variant: input.editorMode === 'form' ? 'primary' : 'outline',
									size: 'sm',
									disabled: input.isHydratingEdit || input.isSubmitting
								}
							},
							{
								id: 'presto-definition-mode-yaml',
								component_type: 'Button',
								label: 'YAML mode',
								props: {
									interactive: true,
									variant: input.editorMode === 'yaml' ? 'primary' : 'outline',
									size: 'sm',
									disabled: input.isHydratingEdit || input.isSubmitting
								}
							}
						]
					}
				]
			}
		];

		if (input.pageError) {
			components.push({
				id: 'presto-definition-page-error',
				component_type: 'Alert',
				props: {
					type: 'error',
					message: input.pageError,
					closable: false
				}
			});
		}

		if (input.submitError) {
			components.push({
				id: 'presto-definition-submit-error',
				component_type: 'Alert',
				props: {
					type: 'error',
					message: input.submitError,
					closable: false
				}
			});
		}

		if (input.isHydratingEdit) {
			components.push({
				id: 'presto-definition-loading',
				component_type: 'EmptyState',
				props: {
					title: 'Loading definition',
					description: `Fetching latest definition for ${input.editAgentId}...`
				}
			});
			return components;
		}

		if (input.editorMode === 'form') {
			const kindValue = input.formState.kind;
			const showCoordination = kindValue === 'Personal' || kindValue === 'Worker';
			const kindDescription = kindValue === 'Personal'
				? 'Personal agents are user-facing, appear in the task creation agent picker, and can delegate to any agent by default.'
				: 'Worker agents handle delegated work, use specific capability packs, and can optionally delegate to other workers.';
			const currentAgentId = input.editing ? input.editAgentId : input.formState.agent_id.trim();
			const delegationOptions = agentSelectionOptions(
				input.existingAgentIds,
				input.formState.delegation_targets,
				currentAgentId
			);

			// Build form fields
			const fields: Record<string, unknown>[] = [
				{ id: 'agent_id', label: 'Agent ID (optional, auto-generated from name)', type: 'text', required: false, value: input.formState.agent_id, placeholder: 'Leave blank to auto-generate, or enter e.g. my-research-agent', disabled: input.editing },
				{ id: 'name', label: 'Name', type: 'text', required: true, value: input.formState.name, placeholder: 'Human-readable name, e.g. Research Agent' },
				{ id: 'trust_level', label: 'Trust level', type: 'select', required: true, value: input.formState.trust_level, options: TRUST_LEVEL_OPTIONS, placeholder: 'Select trust level' },
				{ id: 'kind', label: 'Agent kind', type: 'select', required: true, value: kindValue, options: AGENT_KIND_OPTIONS, placeholder: 'Select agent kind', hint: kindDescription }
			];

			if (showCoordination) {
				fields.push(
					{
						id: 'delegate_to_any',
						label: 'Delegate to any agent',
						type: 'select',
						required: false,
						value: input.formState.delegate_to_any ? 'true' : 'false',
						options: BOOLEAN_OPTIONS
					},
					{
						id: 'max_delegation_depth',
						label: 'Max delegation depth (1-10)',
						type: 'text',
						required: false,
						value: String(input.formState.max_delegation_depth),
						placeholder: '3'
					}
				);
			}

			fields.push(
				{ id: 'description', label: 'Description', type: 'textarea', required: false, value: input.formState.description, rows: 3, span: 3, placeholder: 'Brief summary of what this agent does' },
				{ id: 'persona', label: 'Persona', type: 'textarea', required: true, value: input.formState.persona, rows: 4, span: 3, placeholder: 'Describe how this agent should behave, e.g. "You are a web scraping specialist..."' }
			);

			// Task 2.22: Memory Isolation section — only for Personal agents
			if (kindValue === 'Personal') {
				fields.push(
					{
						id: 'harness_enabled',
						label: '--- Harness --- Enable harness mode',
						type: 'text',
						required: false,
						value: input.formState.harness_enabled ? 'true' : 'false',
						placeholder: 'true or false'
					},
					{
						id: 'harness_program_section',
						label: 'Harness program section (optional, blank = full program)',
						type: 'text',
						required: false,
						value: input.formState.harness_program_section,
						placeholder: 'engineering'
					},
					{
						id: 'user_memory_isolation',
						label: 'User memory isolation (shared or fully_isolated)',
						type: 'text',
						required: false,
						value: input.formState.user_memory_isolation,
						placeholder: 'shared'
					},
					{
						id: 'readable_agents_csv',
						label: 'Readable agents (agent IDs this agent can read memory from, comma separated)',
						type: 'text',
						required: false,
						value: input.formState.readable_agents_csv,
						placeholder: 'agent-1, agent-2'
					}
				);
			}

			// Autonomous Behavior section — only for Personal agents
			if (kindValue === 'Personal') {
				fields.push({
					id: 'autonomous_enabled',
					label: '--- Autonomous Behavior --- Enable autonomous mode',
					type: 'text',
					required: false,
					value: input.formState.autonomous_enabled ? 'true' : 'false',
					placeholder: 'true or false'
				});

				if (input.formState.autonomous_enabled) {
					const schedulePreview = cronToHumanReadable(input.formState.autonomous_schedule);
					const scheduleHint = schedulePreview ? ` (${schedulePreview})` : '';
					fields.push(
						{
							id: 'autonomous_schedule',
							label: `Schedule (cron expression)${scheduleHint}`,
							type: 'text',
							required: true,
							value: input.formState.autonomous_schedule,
							placeholder: '0 */4 * * *'
						},
						{
							id: 'autonomous_max_tasks_per_cycle',
							label: 'Max tasks per cycle (1-10)',
							type: 'text',
							required: false,
							value: String(input.formState.autonomous_max_tasks_per_cycle),
							placeholder: '3'
						},
						{
							id: 'autonomous_max_steps_per_plan',
							label: 'Max steps per plan (1-30)',
							type: 'text',
							required: false,
							value: String(input.formState.autonomous_max_steps_per_plan),
							placeholder: '10'
						}
					);

					// Focus areas — repeatable group rendered as indexed fields
					for (let i = 0; i < input.formState.autonomous_focus_areas.length; i++) {
						const fa = input.formState.autonomous_focus_areas[i];
						fields.push(
							{
								id: `fa_name_${i}`,
								label: `Focus area ${i + 1}: Name`,
								type: 'text',
								required: true,
								value: fa.name,
								placeholder: 'e.g. Monitor emails'
							},
							{
								id: `fa_desc_${i}`,
								label: `Focus area ${i + 1}: Description`,
								type: 'textarea',
								required: true,
								value: fa.description,
								rows: 2,
								placeholder: 'e.g. Check for new emails and categorize'
							},
							{
								id: `fa_priority_${i}`,
								label: `Focus area ${i + 1}: Priority`,
								type: 'text',
								required: false,
								value: fa.priority,
								placeholder: 'high, medium, or low'
							},
							{
								id: `fa_schedule_${i}`,
								label: `Focus area ${i + 1}: Schedule override (optional cron)`,
								type: 'text',
								required: false,
								value: fa.schedule,
								placeholder: '0 9 * * 1-5'
							},
							{
								id: `fa_program_${i}`,
								label: `Focus area ${i + 1}: Program override (optional)`,
								type: 'text',
								required: false,
								value: fa.program,
								placeholder: 'daily_ops.md'
							},
							{
								id: `fa_scope_${i}`,
								label: `Focus area ${i + 1}: Scope override (agent IDs, comma separated)`,
								type: 'text',
								required: false,
								value: fa.scope_csv,
								placeholder: 'frontend-engineer, backend-engineer-1'
							}
						);
					}
				}
			}

			components.push({
				id: 'presto-definition-form',
				component_type: 'Form',
				label: 'Definition form',
				props: {
					title: input.editing ? 'Edit definition' : 'Create definition',
					showSubmit: true,
					submitLabel: input.submitLabel,
					disabled: input.isSubmitting || input.isHydratingEdit,
					idBase: 'presto-definition-form',
					fields
				}
			});

			if (showCoordination) {
				components.push({
					id: 'presto-definition-delegation-targets',
					component_type: 'MultiSelect',
					label: 'Delegation allow list',
					props: {
						options: delegationOptions,
						values: input.formState.delegation_targets,
						disabled: input.isSubmitting || input.isHydratingEdit || input.formState.delegate_to_any,
						searchable: true,
						searchPlaceholder: 'Filter agents',
						placeholder: input.formState.delegate_to_any
							? 'Disabled while Delegate to any agent is Yes'
							: 'Select the agents this agent may delegate to'
					}
				});
			}

			// Tool selection tree-selects (outside the Form — arrays don't fit Form's scalar model)
			components.push({
				id: 'presto-definition-tools',
				component_type: 'MultiSelect',
				label: 'Tools',
				props: {
					options: toolOptions,
					values: input.formState.tools,
					disabled: input.isSubmitting || input.isHydratingEdit,
					searchable: true,
					searchPlaceholder: 'Filter tools',
					placeholder: 'Leave empty to allow all available tools'
				}
			});

			components.push({
				id: 'presto-definition-excluded-tools',
				component_type: 'MultiSelect',
				label: 'Excluded tools',
				props: {
					options: toolOptions,
					values: input.formState.excluded_tools,
					disabled: input.isSubmitting || input.isHydratingEdit,
					searchable: true,
					searchPlaceholder: 'Filter excluded tools',
					placeholder: 'Tools to deny even when otherwise available'
				}
			});

			// Autonomous focus area management buttons (outside the form, as action buttons)
			if (kindValue === 'Personal' && input.formState.autonomous_enabled) {
				const focusAreaActionChildren: CrewNativeComponent[] = [
					{
						id: 'presto-definition-autonomous-add-focus',
						component_type: 'Button',
						label: 'Add focus area',
						props: {
							interactive: true,
							variant: 'secondary',
							size: 'sm',
							disabled: input.isSubmitting || input.isHydratingEdit
						}
					}
				];
				if (input.formState.autonomous_focus_areas.length > 0) {
					focusAreaActionChildren.push({
						id: 'presto-definition-autonomous-remove-focus',
						component_type: 'Button',
						label: 'Remove last focus area',
						props: {
							interactive: true,
							variant: 'outline',
							size: 'sm',
							disabled: input.isSubmitting || input.isHydratingEdit
						}
					});
				}
				components.push({
					id: 'presto-definition-autonomous-focus-actions',
					component_type: 'Stack',
					props: { direction: 'row', gap: '0.5rem', wrap: true },
					children: focusAreaActionChildren
				});
			}
		} else {
			components.push({
				id: 'presto-definition-yaml-actions',
				component_type: 'Stack',
				props: {
					direction: 'row',
					gap: '0.5rem',
					wrap: true
				},
				children: [
					{
						id: 'presto-definition-action-populate-yaml',
						component_type: 'Button',
						label: 'Populate from form',
						props: {
							interactive: true,
							variant: 'secondary',
							size: 'sm',
							disabled: input.isSubmitting || input.isHydratingEdit
						}
					},
					{
						id: 'presto-definition-action-reset-yaml',
						component_type: 'Button',
						label: 'Reset draft',
						props: {
							interactive: true,
							variant: 'outline',
							size: 'sm',
							disabled: input.isSubmitting || input.isHydratingEdit
						}
					}
				]
			});
			components.push({
				id: 'presto-definition-yaml-form',
				component_type: 'Form',
				label: 'YAML definition',
				props: {
					title: input.editing ? 'Edit definition (YAML)' : 'Create definition (YAML)',
					showSubmit: true,
					submitLabel: input.submitLabel,
					disabled: input.isSubmitting || input.isHydratingEdit,
					idBase: 'presto-definition-yaml-form',
					fields: [
						{
							id: 'yaml_definition',
							label: 'YAML',
							type: 'textarea',
							required: true,
							value: input.yamlDraft,
							rows: 30,
							placeholder: 'agent_id: ...'
						}
					]
				}
			});
		}

		return components;
	}

	async function handleSurfaceInteraction(event: CustomEvent<CrewNativeInteractionEventDetail>): Promise<void> {
		const detail = event?.detail;
		if (!detail) return;

		if (detail.interaction === 'action') {
			if (detail.componentId === 'presto-definition-action-back') {
				await goto('/crew');
				return;
			}
			if (detail.componentId === 'presto-definition-action-open-detail' && editing) {
				await goto(`/crew/${encodeURIComponent(editAgentId)}`);
				return;
			}
			if (detail.componentId === 'presto-definition-mode-form') {
				switchMode('form');
				return;
			}
			if (detail.componentId === 'presto-definition-mode-yaml') {
				switchMode('yaml');
				return;
			}
			if (detail.componentId === 'presto-definition-action-populate-yaml') {
				applyFormToYamlDraft();
				return;
			}
			if (detail.componentId === 'presto-definition-action-reset-yaml') {
				resetYamlDraft();
				return;
			}
			if (detail.componentId === 'presto-definition-autonomous-add-focus') {
				formState = cloneFormState(formState);
				formState.autonomous_focus_areas = [
					...formState.autonomous_focus_areas,
					{
						name: '',
						description: '',
						priority: 'medium',
						schedule: '',
						program: '',
						scope_csv: ''
					}
				];
				return;
			}
			if (detail.componentId === 'presto-definition-autonomous-remove-focus') {
				if (formState.autonomous_focus_areas.length > 0) {
					formState = cloneFormState(formState);
					formState.autonomous_focus_areas = formState.autonomous_focus_areas.slice(0, -1);
				}
				return;
			}
		}

		if (detail.interaction === 'change' && detail.componentId === 'presto-definition-tools') {
			const payload = asRecord(detail.detail);
			const nextValues = Array.isArray(payload?.values) ? (payload.values as unknown[]).filter(
				(v: unknown): v is string => typeof v === 'string'
			) : [];
			formState = { ...cloneFormState(formState), tools: nextValues };
			return;
		}
		if (detail.interaction === 'change' && detail.componentId === 'presto-definition-form') {
			const payload = asRecord(detail.detail);
			const values = asRecord(payload?.values) || {};
			formState = cloneFormState(formStateFromFormValues(values));
			return;
		}
		if (detail.interaction === 'change' && detail.componentId === 'presto-definition-delegation-targets') {
			const payload = asRecord(detail.detail);
			const nextValues = Array.isArray(payload?.values) ? (payload.values as unknown[]).filter(
				(v: unknown): v is string => typeof v === 'string'
			) : [];
			formState = { ...cloneFormState(formState), delegation_targets: nextValues };
			return;
		}
		if (detail.interaction === 'change' && detail.componentId === 'presto-definition-excluded-tools') {
			const payload = asRecord(detail.detail);
			const nextValues = Array.isArray(payload?.values) ? (payload.values as unknown[]).filter(
				(v: unknown): v is string => typeof v === 'string'
			) : [];
			formState = { ...cloneFormState(formState), excluded_tools: nextValues };
			return;
		}

		if (detail.interaction === 'submit' && detail.componentId === 'presto-definition-form') {
			const payload = asRecord(detail.detail);
			const values = asRecord(payload?.values) || {};
			await submitDefinition(values);
			return;
		}

		if (detail.interaction === 'submit' && detail.componentId === 'presto-definition-yaml-form') {
			const payload = asRecord(detail.detail);
			const values = asRecord(payload?.values) || {};
			await submitDefinition(values);
		}
	}

	let toolOptions: { value: string; label: string }[] = [];

	async function loadCapabilityPacks(): Promise<void> {
		try {
			const scope = get(scopeIdentityStore);
			const headers: Record<string, string> = {};
			const res = await timedFetch('/api/magician/v2/skills', { headers });
			if (!res.ok) return;
			const json = await res.json();
			const skills = (json.skills ?? []) as Array<{ name: string; kind: string }>;
			toolOptions = skills
				.filter(s => s.kind !== 'personality-mode')
				.map(s => ({ value: s.name, label: s.name }))
				.sort((a, b) => a.label.localeCompare(b.label));
		} catch (error) {
			console.warn('[crew] failed to load skills', error);
		}
	}

	onMount(() => {
		if (!browser) return;
		editorMounted = true;
		lastEditorScopeKey = currentEditorScopeKey;
		void refreshAgents().catch((error) => {
			console.warn('[crew] failed to load agents', error);
		});
		void loadCapabilityPacks();
	});

	onDestroy(() => {
		editorMounted = false;
	});

	$: if (browser && editorMounted && currentEditorScopeKey !== lastEditorScopeKey) {
		lastEditorScopeKey = currentEditorScopeKey;
		resetEditorState();
	}
</script>

<svelte:head>
	<title>{editing ? 'Edit crew member' : 'New crew member'} · Magican</title>
</svelte:head>

<div class="new-agent-route presto-gaui-page">
	<NativeCrewRenderer {components} on:interaction={handleSurfaceInteraction} />
</div>
