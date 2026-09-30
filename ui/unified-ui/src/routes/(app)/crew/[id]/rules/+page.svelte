<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import NativeCrewRenderer from '$lib/magician/crew/NativeCrewRenderer.svelte';
	import type { CrewNativeComponent, CrewNativeInteractionEventDetail } from '$lib/magician/crew/nativeSurface';
	import {
		fetchAgentDefinitionRecord,
		serializeDefinitionYaml,
		type AgentDefinitionRecord
	} from '../../definitionApi';
	import { showError } from '$lib/shared/stores/notifications';

	interface SurfaceEntry {
		id: string;
		label: string;
		value: unknown;
		itemCount: number;
		valid: boolean;
		notes: string;
		yaml: string;
	}

	let isLoading = false;
	let error: string | null = null;
	let routeParamError: string | null = null;
	let agentId = '';
	let record: AgentDefinitionRecord | null = null;
	let activeRequestToken = 0;

	const COVENANT_SURFACE_SCHEMA = 'presto.double-covenant-v1';
	const ACTION_BACK_ID = 'agent-config-action-back';
	const ACTION_REFRESH_ID = 'agent-config-action-refresh';

	$: decoded = decodeAgentId(($page.params.id || '').trim());
	$: agentId = decoded.agentId;
	$: routeParamError = decoded.error;
	$: definition = record?.definition || null;
	$: surfaces = definition ? buildSurfaces(definition) : [];
	$: validSurfaceCount = surfaces.filter((surface) => surface.valid).length;
	$: invalidSurfaceCount = surfaces.filter((surface) => !surface.valid).length;
	$: stateMachineRows = definition ? extractStateMachineRows(definition['state_machines']) : [];
	$: consolidationRows = definition ? extractConsolidationRows(readArray(definition, 'memory_consolidation')) : [];
	$: configComponents = buildConfigComponents(surfaces, stateMachineRows, consolidationRows);
	$: components = buildRouteComponents({
		agentId,
		routeParamError,
		error,
		isLoading,
		definition,
		validSurfaceCount,
		invalidSurfaceCount,
		surfaces,
		configComponents
	});

	function decodeAgentId(raw: string): { agentId: string; error: string | null } {
		if (!raw) {
			return { agentId: '', error: null };
		}
		// SvelteKit route params are already URL-decoded.
		return {
			agentId: raw,
			error: null
		};
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		return typeof value === 'object' && value !== null && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	}

	function readString(record: Record<string, unknown>, key: string): string | undefined {
		const value = record[key];
		return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
	}

	function readArray(record: Record<string, unknown>, key: string): unknown[] {
		const value = record[key];
		return Array.isArray(value) ? value : [];
	}

	function countItems(value: unknown): number {
		if (Array.isArray(value)) return value.length;
		if (value && typeof value === 'object') return Object.keys(value as Record<string, unknown>).length;
		if (value == null) return 0;
		return 1;
	}

	function statusColor(valid: boolean): 'success' | 'error' {
		return valid ? 'success' : 'error';
	}

	function isNonEmptyString(value: unknown): boolean {
		return typeof value === 'string' && value.trim().length > 0;
	}

	function yamlForSurface(label: string, value: unknown): string {
		if (value && typeof value === 'object' && !Array.isArray(value)) {
			return serializeDefinitionYaml(value as Record<string, unknown>);
		}
		return serializeDefinitionYaml({ [label]: value });
	}

	function validateSurface(label: string, value: unknown): { valid: boolean; notes: string } {
		if (label === 'Persona') {
			return {
				valid: isNonEmptyString(value),
				notes: isNonEmptyString(value) ? 'Persona prompt configured.' : 'Persona is missing or empty.'
			};
		}
		if (label === 'Goals + Evaluation') {
			const goals = Array.isArray(value) ? value : [];
			const valid = goals.length > 0 && goals.every((goal) => {
				const record = asRecord(goal);
				return !!record && isNonEmptyString(record.id) && isNonEmptyString(record.description);
			});
			return {
				valid,
				notes: valid ? `${goals.length} goals with id/description.` : 'Each goal must include id + description.'
			};
		}
		if (label === 'Triggers') {
			const triggers = Array.isArray(value) ? value : [];
			const valid = triggers.length > 0 && triggers.every((trigger) => {
				const record = asRecord(trigger);
				return !!record && isNonEmptyString(record.goal) && asRecord(record.kind) !== null;
			});
			return {
				valid,
				notes: valid ? `${triggers.length} trigger definitions.` : 'Each trigger must include goal + kind.'
			};
		}
		if (label === 'Approval Rules') {
			const rules = Array.isArray(value) ? value : [];
			const valid = rules.every((rule) => {
				const record = asRecord(rule);
				return !!record && isNonEmptyString(record.tool) && record.action != null;
			});
			return {
				valid,
				notes: valid ? `${rules.length} approval rules.` : 'Approval rules require tool + action.'
			};
		}
		if (label === 'Memory Tiers') {
			const tiers = Array.isArray(value) ? value : [];
			const valid = tiers.every((tier) => {
				const record = asRecord(tier);
				return !!record && isNonEmptyString(record.name) && isNonEmptyString(record.scope);
			});
			return {
				valid,
				notes: valid ? `${tiers.length} memory tiers declared.` : 'Each tier should include name + scope.'
			};
		}
		if (label === 'Consolidation') {
			const rules = Array.isArray(value) ? value : [];
			const valid = rules.every((rule) => {
				const record = asRecord(rule);
				return !!record && isNonEmptyString(record.name) && isNonEmptyString(record.source) && isNonEmptyString(record.target);
			});
			return {
				valid,
				notes: valid ? `${rules.length} consolidation rules.` : 'Each rule should include name + source + target.'
			};
		}
		if (label === 'Workflows') {
			const workflows = Array.isArray(value) ? value : [];
			return {
				valid: true,
				notes: workflows.length > 0 ? `${workflows.length} workflow(s) reference this agent.` : 'No workflow step references this agent.'
			};
		}
		if (value == null) {
			return {
				valid: true,
				notes: 'Surface not configured (defaults apply).'
			};
		}
		if (Array.isArray(value)) {
			return {
				valid: true,
				notes: `${value.length} entries configured.`
			};
		}
		if (typeof value === 'object') {
			return {
				valid: true,
				notes: `${Object.keys(value as Record<string, unknown>).length} key(s) configured.`
			};
		}
		return {
			valid: true,
			notes: 'Configured.'
		};
	}

	function buildSurfaces(
		agentDefinition: Record<string, unknown>
	): SurfaceEntry[] {
		const constraints = asRecord(agentDefinition['constraints']);
		const approvalRules = constraints ? readArray(constraints, 'requires_approval') : [];
		const surfaces: Array<{ id: string; label: string; value: unknown }> = [
			{ id: 'persona', label: 'Persona', value: agentDefinition['persona'] },
			{ id: 'goals', label: 'Goals + Evaluation', value: readArray(agentDefinition, 'goals') },
			{ id: 'triggers', label: 'Triggers', value: readArray(agentDefinition, 'triggers') },
			{ id: 'approval_rules', label: 'Approval Rules', value: approvalRules },
			{ id: 'memory_tiers', label: 'Memory Tiers', value: readArray(agentDefinition, 'memory_tiers') },
			{ id: 'consolidation', label: 'Consolidation', value: readArray(agentDefinition, 'memory_consolidation') },
			{ id: 'prompt_pipeline', label: 'Prompt Pipeline', value: agentDefinition['prompt_pipeline'] },
			{ id: 'circuit_breaker', label: 'Circuit Breaker', value: agentDefinition['circuit_breaker'] },
			{ id: 'feedback_loops', label: 'Feedback Loops', value: readArray(agentDefinition, 'feedback_loops') },
			{ id: 'notifications', label: 'Notifications', value: readArray(agentDefinition, 'notification_rules') },
			{ id: 'retention', label: 'Retention', value: agentDefinition['retention'] },
			{ id: 'llm_routing', label: 'LLM Routing', value: agentDefinition['llm_routing'] },
			{ id: 'observation', label: 'Observation', value: agentDefinition['observation'] },
			{ id: 'strategy', label: 'Strategy', value: agentDefinition['strategy'] },
			{ id: 'state_machines', label: 'State Machines', value: agentDefinition['state_machines'] }
		];

		return surfaces.map((surface) => {
			const validation = validateSurface(surface.label, surface.value);
			return {
				id: surface.id,
				label: surface.label,
				value: surface.value,
				itemCount: countItems(surface.value),
				valid: validation.valid,
				notes: validation.notes,
				yaml: yamlForSurface(surface.id, surface.value)
			};
		});
	}

	function extractStateMachineRows(rawStateMachines: unknown): Array<Record<string, string>> {
		const machines = asRecord(rawStateMachines);
		if (!machines) return [];
		const rows: Array<Record<string, string>> = [];
		for (const [machineName, machineValue] of Object.entries(machines)) {
			const machine = asRecord(machineValue);
			if (!machine) {
				rows.push({
					machine: machineName,
					from: '—',
					trigger: '—',
					to: '—',
					guard: 'invalid machine payload',
					actions: '—'
				});
				continue;
			}
			const transitions = Array.isArray(machine.transitions) ? machine.transitions : [];
			if (transitions.length === 0) {
				rows.push({
					machine: machineName,
					from: '—',
					trigger: '—',
					to: machine.initial_state && typeof machine.initial_state === 'string' ? machine.initial_state : '—',
					guard: 'no transitions',
					actions: '—'
				});
				continue;
			}
			for (const transitionEntry of transitions) {
				const transition = asRecord(transitionEntry);
				if (!transition) continue;
				const trigger = Array.isArray(transition.on)
					? transition.on.filter((value): value is string => typeof value === 'string').join(' | ')
					: (typeof transition.on === 'string' ? transition.on : '—');
				const actions = Array.isArray(transition.actions)
					? transition.actions.map((value) => {
						if (typeof value === 'string') return value;
						if (value && typeof value === 'object') return Object.keys(value as Record<string, unknown>).join(',');
						return String(value);
					}).join(', ')
					: '—';
				rows.push({
					machine: machineName,
					from: readString(transition, 'from') || '—',
					trigger: trigger || '—',
					to: readString(transition, 'to') || '—',
					guard: readString(transition, 'when') || '—',
					actions: actions || '—'
				});
			}
		}
		return rows;
	}

	function extractConsolidationRows(rawRules: unknown[]): Array<Record<string, string>> {
		const rows: Array<Record<string, string>> = [];
		for (const rule of rawRules) {
			const record = asRecord(rule);
			if (!record) continue;
			const transform = asRecord(record.transform);
			const transformType = readString(transform || {}, 'type') || 'unknown';
			let transformDetail = transformType;
			if (transformType === 'structured') {
				transformDetail = `structured:${readString(transform || {}, 'builtin') || 'builtin'}`;
			} else if (transformType === 'llm') {
				transformDetail = `llm:${readString(transform || {}, 'prompt') || 'prompt'}`;
			} else if (transformType === 'render') {
				transformDetail = `render:${readString(transform || {}, 'template') || 'template'}`;
			}
			rows.push({
				rule: readString(record, 'name') || 'unnamed',
				trigger: typeof record.trigger === 'string' ? record.trigger : 'custom',
				source: readString(record, 'source') || '—',
				transform: transformDetail,
				target: readString(record, 'target') || '—'
			});
		}
		return rows;
	}

	function buildConfigComponents(
		surfaces: SurfaceEntry[],
		stateMachineRows: Array<Record<string, string>>,
		consolidationRows: Array<Record<string, string>>
	): CrewNativeComponent[] {
		const surfaceRows = surfaces.map((surface) => ({
			surface: surface.label,
			status: surface.valid ? 'valid' : 'invalid',
			items: String(surface.itemCount),
			notes: surface.notes
		}));

		const tabChildren: CrewNativeComponent[] = surfaces.map((surface) => {
			const items = [
				{ id: `${surface.id}-items`, key: 'items', value: String(surface.itemCount) },
				{ id: `${surface.id}-status`, key: 'status', value: surface.valid ? 'valid' : 'invalid' },
				{ id: `${surface.id}-notes`, key: 'notes', value: surface.notes }
			];
			return {
				id: `surface-tab-${surface.id}`,
				component_type: 'Stack',
				label: surface.label,
				props: {
					direction: 'column',
					gap: 'var(--space-sm)'
				},
				children: [
					{
						id: `surface-tab-${surface.id}-badge`,
						component_type: 'Badge',
						label: `${surface.label} Status`,
						props: {
							text: surface.valid ? 'valid' : 'invalid',
							color: statusColor(surface.valid)
						}
					},
					{
						id: `surface-tab-${surface.id}-summary`,
						component_type: 'DataList',
						label: `${surface.label} Summary`,
						props: {
							items
						}
					},
					{
						id: `surface-tab-${surface.id}-yaml-scroll`,
						component_type: 'ScrollArea',
						label: `${surface.label} YAML`,
						props: {
							maxHeight: '320px'
						},
						children: [
							{
								id: `surface-tab-${surface.id}-yaml`,
								component_type: 'TextArea',
								label: `${surface.label} YAML`,
								props: {
									value: surface.yaml,
									rows: Math.min(22, Math.max(8, surface.yaml.split('\n').length + 1)),
									disabled: true
								}
							}
						]
					}
				]
			};
		});

		return [
			{
				id: 'agent-config-metrics',
				component_type: 'Grid',
				label: 'Config Metrics',
				props: {
					autoFit: true,
					minColumnWidth: '180px',
					gap: 'var(--space-sm)'
				},
				children: [
					{
						id: 'config-metric-surfaces',
						component_type: 'MetricCard',
						label: 'Surface Count',
						props: {
							value: String(surfaces.length),
							label: 'tracked surfaces'
						}
					},
					{
						id: 'config-metric-valid',
						component_type: 'MetricCard',
						label: 'Valid',
						props: {
							value: String(surfaces.filter((surface) => surface.valid).length),
							label: 'parse checks passing',
							trend: 'up',
							trendLabel: 'healthy'
						}
					},
					{
						id: 'config-metric-invalid',
						component_type: 'MetricCard',
						label: 'Invalid',
						props: {
							value: String(surfaces.filter((surface) => !surface.valid).length),
							label: 'needs correction',
							trend: surfaces.some((surface) => !surface.valid) ? 'down' : 'flat',
							trendLabel: surfaces.some((surface) => !surface.valid) ? 'attention required' : 'none'
						}
					},
					{
						id: 'config-metric-workflow-links',
						component_type: 'MetricCard',
						label: 'Workflow Links',
						props: {
							value: String(surfaces.find((surface) => surface.id === 'workflows')?.itemCount || 0),
							label: 'workflow references'
						}
					}
				]
			},
			{
				id: 'agent-config-surface-table',
				component_type: 'Table',
				label: 'Surface Status',
				props: {
					columns: [
						{ key: 'surface', label: 'Surface', sortable: true },
						{ key: 'status', label: 'Status', sortable: true },
						{ key: 'items', label: 'Items', sortable: true },
						{ key: 'notes', label: 'Notes' }
					],
					rows: surfaceRows,
					sortKey: 'surface',
					sortDir: 'asc'
				}
			},
			{
				id: 'agent-config-surface-tabs',
				component_type: 'Tabs',
				label: 'Declarative Surfaces',
				props: {
					tabs: surfaces.map((surface) => ({ label: surface.label })),
					activeIndex: 0
				},
				children: tabChildren
			},
			{
				id: 'agent-config-state-machine-table',
				component_type: 'Table',
				label: 'State Machine Diagram',
				props: {
					columns: [
						{ key: 'machine', label: 'Machine' },
						{ key: 'from', label: 'From' },
						{ key: 'trigger', label: 'Trigger' },
						{ key: 'to', label: 'To' },
						{ key: 'guard', label: 'Guard' },
						{ key: 'actions', label: 'Actions' }
					],
					rows: stateMachineRows
				}
			},
			{
				id: 'agent-config-consolidation-table',
				component_type: 'Table',
				label: 'Consolidation Pipeline',
				props: {
					columns: [
						{ key: 'rule', label: 'Rule' },
						{ key: 'trigger', label: 'Trigger' },
						{ key: 'source', label: 'Source' },
						{ key: 'transform', label: 'Transform' },
						{ key: 'target', label: 'Target' }
					],
					rows: consolidationRows
				}
			}
		];
	}

	function buildRouteComponents(input: {
		agentId: string;
		routeParamError: string | null;
		error: string | null;
		isLoading: boolean;
		definition: Record<string, unknown> | null;
		validSurfaceCount: number;
		invalidSurfaceCount: number;
		surfaces: SurfaceEntry[];
		configComponents: CrewNativeComponent[];
	}): CrewNativeComponent[] {
		const canRefresh = !!input.agentId && !input.routeParamError && !input.isLoading;
		const definitionAgentId = input.definition ? (readString(input.definition, 'agent_id') || input.agentId || '—') : '—';
		const definitionVersion = input.definition ? String(input.definition['version'] || 'n/a') : 'n/a';

		const components: CrewNativeComponent[] = [
			{
				id: 'agent-config-header',
				component_type: 'Card',
				label: 'Agent rules header',
				props: {
					title: 'Declarative Surface Browser',
					subtitle: `Agent configuration surfaces · ${COVENANT_SURFACE_SCHEMA}`,
					body: 'Parse-state indicators with GAUI-native state-machine and consolidation tables.'
				},
				children: [
					{
						id: 'agent-config-header-meta',
						component_type: 'DataList',
						props: {
							items: [
								{ id: 'agent-config-meta-agent', key: 'Agent', value: input.agentId || '—' },
								{ id: 'agent-config-meta-state', key: 'State', value: input.isLoading ? 'loading' : 'ready' },
								{ id: 'agent-config-meta-valid', key: 'Valid surfaces', value: `${input.validSurfaceCount}/${input.surfaces.length}` },
								{ id: 'agent-config-meta-invalid', key: 'Invalid surfaces', value: String(input.invalidSurfaceCount) }
							]
						}
					},
					{
						id: 'agent-config-header-actions',
						component_type: 'Stack',
						props: {
							direction: 'row',
							gap: '0.5rem',
							wrap: true
						},
						children: [
							{
								id: ACTION_BACK_ID,
								component_type: 'Button',
								label: 'Back to Agent',
								props: {
									interactive: true,
									variant: 'outline',
									size: 'sm'
								}
							},
							{
								id: ACTION_REFRESH_ID,
								component_type: 'Button',
								label: input.isLoading ? 'Refreshing...' : 'Refresh',
								props: {
									interactive: true,
									variant: 'primary',
									size: 'sm',
									disabled: !canRefresh
								}
							}
						]
					}
				]
			}
		];

		if (input.routeParamError) {
			components.push({
				id: 'agent-config-route-error',
				component_type: 'Alert',
				props: {
					type: 'error',
					message: input.routeParamError,
					closable: false
				}
			});
			return components;
		}

		if (input.error) {
			components.push({
				id: 'agent-config-error',
				component_type: 'Alert',
				props: {
					type: 'error',
					message: input.error,
					closable: false
				}
			});
		}

		if (input.isLoading && !input.definition) {
			components.push({
				id: 'agent-config-loading',
				component_type: 'EmptyState',
				props: {
					title: 'Loading declarative surfaces',
					description: 'Fetching agent definition, workflows, and parse diagnostics.'
				}
			});
			return components;
		}

		if (!input.definition) {
			components.push({
				id: 'agent-config-empty',
				component_type: 'EmptyState',
				props: {
					title: 'No definition loaded',
					description: 'Agent config is unavailable for this route.'
				}
			});
			return components;
		}

		components.push({
			id: 'agent-config-snapshot',
			component_type: 'Card',
			label: 'Agent snapshot',
			props: {
				title: 'Configuration snapshot',
				subtitle: definitionAgentId,
				body: 'Diagnostic summary for loaded agent declarative surfaces.'
			},
			children: [
				{
					id: 'agent-config-snapshot-meta',
					component_type: 'DataList',
					props: {
						items: [
							{ id: 'agent-config-snapshot-agent', key: 'Agent', value: definitionAgentId },
							{ id: 'agent-config-snapshot-version', key: 'Version', value: definitionVersion },
							{ id: 'agent-config-snapshot-surfaces', key: 'Surface count', value: String(input.surfaces.length) },
							{ id: 'agent-config-snapshot-invalid', key: 'Invalid surfaces', value: String(input.invalidSurfaceCount) }
						]
					}
				}
			]
		});

		components.push(...input.configComponents);
		return components;
	}

	async function hydrateConfig(nextAgentId: string): Promise<void> {
		if (!nextAgentId) return;
		const requestToken = ++activeRequestToken;
		isLoading = true;
		error = null;
		record = null;

		try {
			const loadedRecord = await fetchAgentDefinitionRecord(nextAgentId);
			if (requestToken !== activeRequestToken) return;
			if (!loadedRecord) {
				error = `Agent \"${nextAgentId}\" was not found`;
				isLoading = false;
				return;
			}
			record = loadedRecord;
		} catch (loadError) {
			if (requestToken !== activeRequestToken) return;
			error = loadError instanceof Error ? loadError.message : 'Failed to load agent config';
			showError(error);
		} finally {
			if (requestToken === activeRequestToken) {
				isLoading = false;
			}
		}
	}

	function refreshCurrentAgent(): void {
		if (!browser || !agentId || routeParamError || isLoading) {
			return;
		}
		void hydrateConfig(agentId);
	}

	async function handleSurfaceInteraction(event: CustomEvent<CrewNativeInteractionEventDetail>): Promise<void> {
		const detail = event?.detail;
		if (!detail || detail.interaction !== 'action') return;

		if (detail.componentId === ACTION_BACK_ID) {
			const target = agentId ? `/crew/${encodeURIComponent(agentId)}` : '/crew';
			await goto(target);
			return;
		}

		if (detail.componentId === ACTION_REFRESH_ID) {
			refreshCurrentAgent();
		}
	}

	$: if (browser && agentId && !routeParamError) {
		void hydrateConfig(agentId);
	}

</script>

<svelte:head>
	<title>{agentId ? `${agentId} · Rules` : 'Agent Rules'} · Magican</title>
</svelte:head>

<div class="agent-config-route presto-gaui-page">
	<NativeCrewRenderer
		{components}
		idNamespace={`agent-config-${agentId}`}
		on:interaction={handleSurfaceInteraction}
	/>
</div>
