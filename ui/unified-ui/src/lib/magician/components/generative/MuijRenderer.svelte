<script lang="ts">
	import { browser } from '$app/environment';
	import { page } from '$app/stores';
	import { createEventDispatcher } from 'svelte';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import type { MuijComponent, MuijInteractionKind, MuijInteractionEventDetail } from '$lib/stores/muijStore';
	import { reportPrestoRouteValidation } from '$lib/magician/presto/validation';
	import { BADGE_STATUS_TONES, type BadgeStatusTone } from '$lib/shared/statusTone';
	import { SUPPORTED_MUIJ_COMPONENT_TYPE_SET } from './componentCatalog';
	// GAUI-α components
	import Gauge from './Gauge.svelte';
	import TerminalTransient from './TerminalTransient.svelte';
	import Card from './Card.svelte';
	import Text from './Text.svelte';
	// GAUI-β: EntityGrid (GB-F01)
	import EntityGrid from './EntityGrid.svelte';
	// GAUI-γ: Charts (GC-F02)
	import PieChart from './PieChart.svelte';
	import BarChart from './BarChart.svelte';
	import LineChart from './LineChart.svelte';
	import AreaChart from './AreaChart.svelte';
	import ScatterChart from './ScatterChart.svelte';
	import TrendChart from './TrendChart.svelte';
	import Sparkline from './Sparkline.svelte';
	import Heatmap from './Heatmap.svelte';
	// GAUI-β: Layout (GB-F03)
	import Container from './Container.svelte';
	import Stack from './Stack.svelte';
	import Grid from './Grid.svelte';
	import SplitPanel from './SplitPanel.svelte';
	import Tabs from './Tabs.svelte';
	import Panel from './Panel.svelte';
	import ScrollArea from './ScrollArea.svelte';
	import Divider from './Divider.svelte';
	// GAUI-β: Data display (GB-F04)
	import Table from './Table.svelte';
	import DataList from './DataList.svelte';
	import MetricCard from './MetricCard.svelte';
	import Progress from './Progress.svelte';
	import Badge from './Badge.svelte';
	import Tag from './Tag.svelte';
	import { ICON_PATHS, type IconName } from '$lib/shared/icons/paths';
	// GAUI-β: Input (GB-F05)
	import Button from './Button.svelte';
	import TextField from './TextField.svelte';
	import Select from './Select.svelte';
	// GAUI-γ: Form components (GC-F03)
	import Form from './Form.svelte';
	import TextArea from './TextArea.svelte';
	import NumberField from './NumberField.svelte';
	import Slider from './Slider.svelte';
	import Checkbox from './Checkbox.svelte';
	import RadioGroup from './RadioGroup.svelte';
	import MultiSelect from './MultiSelect.svelte';
	import DatePicker from './DatePicker.svelte';
	import Toggle from './Toggle.svelte';
	import SearchInput from './SearchInput.svelte';
	// GAUI-γ: Feedback components (GC-F04)
	import Alert from './Alert.svelte';
	import Toast from './Toast.svelte';
	import Notification from './Notification.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import Spinner from './Spinner.svelte';
	import Skeleton from './Skeleton.svelte';
	import EmptyState from './EmptyState.svelte';
	import ConfirmDialog from './ConfirmDialog.svelte';
	import Tooltip from './Tooltip.svelte';
	// GAUI-γ: ActionBus (interactive)
	import ActionBus from './ActionBus.svelte';
	// GAUI-δ: LiveSelectors (GD-F01)
	import LiveSelectors from './LiveSelectors.svelte';
	// GAUI-δ: Media components (GD-F02)
	import Image from './Image.svelte';
	import Video from './Video.svelte';
	import Audio from './Audio.svelte';
	import Iframe from './Iframe.svelte';
	import CodeBlock from './CodeBlock.svelte';
	import DiffViewer from './DiffViewer.svelte';
	import Markdown from './Markdown.svelte';
	import QRCode from './QRCode.svelte';
	// GAUI-δ: Collaboration components (GD-F03)
	import CommentThread from './CommentThread.svelte';
	import ReactionBar from './ReactionBar.svelte';
	import PresenceAvatars from './PresenceAvatars.svelte';
	import ActivityFeed from './ActivityFeed.svelte';
	import ApprovalFlow from './ApprovalFlow.svelte';
	import DailyBriefingPreview from './DailyBriefingPreview.svelte';
	// GAUI-ε: Phase 5 components
	import Tree from './Tree.svelte';
	import TreeSelect from './TreeSelect.svelte';
	// Graph family: declarative node/edge surfaces (plan 1.5)
	import Graph from './Graph.svelte';
	// General-purpose modal
	import Modal from './Modal.svelte';

	export let components: MuijComponent[] = [];
	/** Agent routing context for interactive MUIJ components (e.g., ActionBus). */
	export let agentId: string = '';
	/** Current recursion depth — stops rendering beyond MAX_DEPTH (R59). */
	export let depth: number = 0;
	/** Stable namespace to avoid DOM id collisions across renderer instances (R119). */
	export let idNamespace: string = '';
	/**
	 * Whether this depth-0 renderer should run the DEV route-contract check.
	 * Default true (the common case: one renderer owns the whole route surface).
	 * Pages that split a route's surface across several renderers + native chrome
	 * (e.g. /tasks) set this false on the fragment renderers and validate the full
	 * pre-split surface once at the page level. The unsupported-component-type
	 * guard is unaffected and always runs.
	 */
	export let validateRouteContract: boolean = true;

	const MAX_DEPTH = 32;

	interface MuijTabSpec {
		label: string;
		content?: string;
	}

	interface MuijColumn {
		key: string;
		label: string;
		sortable?: boolean;
		width?: string;
	}

	interface MuijDataListItem {
		id: string;
		key: string;
		value: string;
	}

	interface MuijSelectOption {
		value: string;
		label: string;
		group?: string;
	}

	interface MuijSelectableOption extends MuijSelectOption {
		disabled?: boolean;
	}

	interface MuijActionBusAction {
		id: string;
		label: string;
		goal_id: string;
		disabled?: boolean;
		trigger?: string;
	}

	interface MuijChartDatum {
		label: string;
		value: number;
		color?: string;
	}

	interface MuijLinePoint {
		x: number;
		y: number;
	}

	interface MuijLineSeries {
		name?: string;
		color?: string;
		points: MuijLinePoint[];
	}

	interface MuijScatterPoint {
		x: number;
		y: number;
		size?: number;
		color?: string;
		label?: string;
	}

	interface MuijNotificationAction {
		id: string;
		label: string;
		disabled?: boolean;
	}

	const dispatch = createEventDispatcher<{ interaction: MuijInteractionEventDetail }>();
	let lastValidationDigest = '';
	let lastUnknownTypeDigest = '';

	function isScalarValue(value: unknown): value is string | number | boolean | bigint {
		return typeof value === 'string'
			|| typeof value === 'number'
			|| typeof value === 'boolean'
			|| typeof value === 'bigint';
	}

	function isRecord(value: unknown): value is Record<string, unknown> {
		return value != null && typeof value === 'object' && !Array.isArray(value);
	}

	function asString(value: unknown, fallback: string = ''): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		return fallback;
	}

	function asBoolean(value: unknown, fallback: boolean = false): boolean {
		if (typeof value === 'boolean') return value;
		if (typeof value === 'number') return Number.isFinite(value) ? value !== 0 : fallback;
		if (typeof value === 'bigint') return value !== 0n;
		if (typeof value === 'string') {
			const normalized = value.trim().toLowerCase();
			if (normalized === 'true' || normalized === '1' || normalized === 'yes' || normalized === 'on') return true;
			if (normalized === 'false' || normalized === '0' || normalized === 'no' || normalized === 'off' || normalized === '') return false;
		}
		return fallback;
	}

	function asFiniteNumber(value: unknown): number | undefined {
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value === 'string') {
			const trimmed = value.trim();
			if (!trimmed) return undefined;
			const parsed = Number(trimmed);
			return Number.isFinite(parsed) ? parsed : undefined;
		}
		return undefined;
	}

	function recordsFromArray(value: unknown): Record<string, unknown>[] {
		if (!Array.isArray(value)) return [];
		return value.filter(isRecord);
	}

	function collectUnknownComponentTypes(components: MuijComponent[]): string[] {
		const unknown = new Set<string>();
		const stack: MuijComponent[] = [...components];
		while (stack.length > 0) {
			const next = stack.pop();
			if (!next) continue;
			if (!SUPPORTED_MUIJ_COMPONENT_TYPE_SET.has(next.component_type)) {
				unknown.add(next.component_type);
			}
			if (Array.isArray(next.children) && next.children.length > 0) {
				for (const child of next.children) {
					stack.push(child);
				}
			}
		}
		return Array.from(unknown).sort((left, right) => left.localeCompare(right));
	}

	function columnsFromProps(value: unknown): MuijColumn[] {
		// R296: Deduplicate columns by key — first-wins prevents duplicate <th> with simultaneous aria-sort
		const seen = new Set<string>();
		return recordsFromArray(value).reduce<MuijColumn[]>((acc, col) => {
			const key = asString(col.key).trim();
			const label = asString(col.label, key).trim();
			if (!key && !label) return acc;
			const effectiveKey = key || label;
			if (seen.has(effectiveKey)) return acc;
			seen.add(effectiveKey);
			acc.push({
				key: effectiveKey,
				label: label || key,
				sortable: asBoolean(col.sortable, false),
				width: asString(col.width).trim()
			});
			return acc;
		}, []);
	}

	function rowsFromProps(value: unknown): Array<Record<string, unknown>> {
		return recordsFromArray(value);
	}

	function sortValueMapFromProps(value: unknown): Record<string, string> {
		if (!isRecord(value)) return {};
		const map: Record<string, string> = {};
		for (const [key, raw] of Object.entries(value)) {
			const normalizedKey = key.trim();
			const normalizedValue = asString(raw).trim();
			if (!normalizedKey || !normalizedValue) continue;
			map[normalizedKey] = normalizedValue;
		}
		return map;
	}

	function rowsForComponent(component: MuijComponent): Array<Record<string, unknown>> {
		const hasQuery = typeof component.query === 'string' && component.query.trim().length > 0;
		const rawRows = component.props.rows;
		const propRows = rowsFromProps(rawRows);
		const hasMeaningfulPropRow = propRows.some((row) =>
			Object.keys(row).some((key) => key.trim().length > 0)
		);
		// Treat rows as authoritative only when the payload is an explicit empty array
		// or contains at least one meaningful record row. Degenerate rows like [{}]
		// should not block query snapshot fallback.
		const hasUsableRowsProp = Array.isArray(rawRows)
			&& (rawRows.length === 0 || hasMeaningfulPropRow);
		const queryRows = rowsFromProps(component.static_snapshot);

		// Explicit rows payloads are authoritative (can be fresher than reconnect snapshots).
		if (hasUsableRowsProp) {
			return propRows;
		}
		// Query-driven components can fall back to materialized snapshot rows.
		if (hasQuery && Array.isArray(component.static_snapshot)) {
			return queryRows;
		}
		// R280: once query is cleared, do not keep rendering stale snapshot rows.
		return propRows;
	}

	function itemsFromProps(value: unknown): MuijDataListItem[] {
		const keyCounts = new Map<string, number>();
		return recordsFromArray(value)
			.map((item) => {
				const key = asString(item.key).trim();
				const val = asString(item.value).trim();
				// R359: DataList is key/value semantics; drop value-only malformed entries.
				if (!key) return null;
				const base = key;
				const count = keyCounts.get(base) ?? 0;
				keyCounts.set(base, count + 1);
				return { id: `${base}:${count}`, key, value: val };
			})
			.filter((item): item is MuijDataListItem => item != null);
	}

	function selectableOptionsFromProps(value: unknown): MuijSelectableOption[] {
		// Deduplicate options by value — first-wins prevents confusing duplicate entries.
		const seen = new Set<string>();
		return recordsFromArray(value)
			.map<MuijSelectableOption | null>((opt) => {
				const hasValue = opt.value !== undefined && opt.value !== null;
				const hasScalar = isScalarValue(opt.value);
				const val = hasScalar ? asString(opt.value).trim() : '';
				const label = asString(opt.label, hasScalar ? val : '').trim();
				if (!hasValue && !label) return null;
				// Preserve explicit empty-string option values (`value: ''`) and
				// only fall back to label when value is missing/non-scalar.
				const effectiveValue = hasScalar ? val : label;
				const effectiveLabel = label || val;
				const isExplicitEmptyValue = hasScalar && hasValue && val === '';
				const group = asString(opt.group).trim();
				if (!effectiveValue && !effectiveLabel && !isExplicitEmptyValue) return null;
				if (seen.has(effectiveValue)) return null;
				seen.add(effectiveValue);
				return {
					value: effectiveValue,
					label: effectiveLabel,
					disabled: asBoolean(opt.disabled, false),
					...(group ? { group } : {})
				};
			})
			.filter((opt): opt is MuijSelectableOption => opt != null);
	}

	function optionsFromProps(value: unknown): MuijSelectOption[] {
		return selectableOptionsFromProps(value).map((opt) => ({
			value: opt.value,
			label: opt.label,
			...(opt.group ? { group: opt.group } : {})
		}));
	}

	function actionsFromProps(value: unknown): MuijActionBusAction[] {
		const seen = new Set<string>();
		const normalized: MuijActionBusAction[] = [];
		for (const action of recordsFromArray(value)) {
			const id = asString(action.id).trim();
			const goalId = asString(action.goal_id).trim();
			const label = asString(action.label, id).trim();
			const trigger = asString(action.trigger).trim();
			if (!id || !goalId || !label) continue;
			if (seen.has(id)) continue;
			seen.add(id);
			normalized.push({
				id,
				label,
				goal_id: goalId,
				disabled: asBoolean(action.disabled, false),
				...(trigger && { trigger })
			});
		}
		return normalized;
	}

	function notificationActionsFromProps(value: unknown): MuijNotificationAction[] {
		const candidates = recordsFromArray(value);
		const seen = new Set<string>();
		const normalized: MuijNotificationAction[] = [];
		for (let index = 0; index < candidates.length; index++) {
			const action = candidates[index];
			const id = asString(action.id).trim();
			const label = asString(action.label, id).trim();
			if (!label) continue;
			const effectiveId = id || `action-${index + 1}`;
			if (seen.has(effectiveId)) continue;
			seen.add(effectiveId);
			normalized.push({
				id: effectiveId,
				label,
				disabled: asBoolean(action.disabled, false)
			});
		}
		return normalized;
	}

	function chartDataFromProps(value: unknown): MuijChartDatum[] {
		return recordsFromArray(value)
			.map((item, index) => {
				const rawValue = asFiniteNumber(item.value);
				if (rawValue === undefined) return null;
				const label = asString(item.label, `Item ${index + 1}`).trim() || `Item ${index + 1}`;
				const color = asString(item.color).trim();
				return {
					label,
					value: rawValue,
					...(color && { color })
				};
			})
			.filter((item): item is MuijChartDatum => item !== null);
	}

	/**
	 * Extract a `dataSource` spec from MUI-JSON component props. Charts and
	 * Tables with `dataSource: { kind: "llm_calls_sql" | "memory_events_sql", sql: "..." }` self-fetch
	 * from the analytics endpoint via `useLiveDataSource`; passing through
	 * here is what makes published MUI-JSON dashboards actually live.
	 */
	function dataSourceFromProps(
		value: unknown
	): { kind: 'llm_calls_sql' | 'memory_events_sql'; sql: string } | null {
		if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
		const obj = value as Record<string, unknown>;
		if (obj.kind !== 'llm_calls_sql' && obj.kind !== 'memory_events_sql') return null;
		const sql = asString(obj.sql).trim();
		if (!sql) return null;
		return { kind: obj.kind, sql };
	}

	function linePointsFromProps(value: unknown): MuijLinePoint[] {
		if (!Array.isArray(value)) return [];
		const points: MuijLinePoint[] = [];
		for (let i = 0; i < value.length; i++) {
			const item = value[i];
			if (typeof item === 'number' && Number.isFinite(item)) {
				points.push({ x: i, y: item });
				continue;
			}
			const rec = isRecord(item) ? item : null;
			if (!rec) continue;
			const x = asFiniteNumber(rec.x) ?? i;
			const y = asFiniteNumber(rec.y);
			if (y === undefined) continue;
			points.push({ x, y });
		}
		return points;
	}

	function lineSeriesFromProps(value: unknown): MuijLineSeries[] {
		const candidateRecords = recordsFromArray(value);
		const hasNestedSeries = candidateRecords.some((item) => Array.isArray(item.points));
		if (hasNestedSeries) {
			const series: MuijLineSeries[] = [];
			for (let index = 0; index < candidateRecords.length; index++) {
				const item = candidateRecords[index];
				const points = linePointsFromProps(item.points);
				if (points.length === 0) continue;
				const name = asString(item.name, `Series ${index + 1}`).trim() || `Series ${index + 1}`;
				const color = asString(item.color).trim();
				series.push({
					name,
					points,
					...(color && { color })
				});
			}
			if (series.length > 0) return series;
		}
		const fallbackPoints = linePointsFromProps(value);
		return fallbackPoints.length > 0 ? [{ points: fallbackPoints }] : [];
	}

	function scatterPointsFromProps(value: unknown): MuijScatterPoint[] {
		return recordsFromArray(value)
			.map((item, index) => {
				const x = asFiniteNumber(item.x);
				const y = asFiniteNumber(item.y);
				if (x === undefined || y === undefined) return null;
				const size = asFiniteNumber(item.size);
				const color = asString(item.color).trim();
				const label = asString(item.label, `Point ${index + 1}`).trim();
				return {
					x,
					y,
					...(size !== undefined && { size }),
					...(color && { color }),
					...(label && { label })
				};
			})
			.filter((item): item is MuijScatterPoint => item !== null);
	}

	function sparklineDataFromProps(value: unknown): number[] {
		if (!Array.isArray(value)) return [];
		const points: number[] = [];
		for (const item of value) {
			if (typeof item === 'number' && Number.isFinite(item)) {
				points.push(item);
				continue;
			}
			const rec = isRecord(item) ? item : null;
			const maybeValue = rec ? asFiniteNumber(rec.value) : undefined;
			if (maybeValue !== undefined) points.push(maybeValue);
		}
		return points;
	}

	function heatmapMatrixFromProps(value: unknown): Array<Array<number | null>> {
		if (!Array.isArray(value)) return [];
		return value
			.filter((row): row is unknown[] => Array.isArray(row))
			.map((row) => row.map((cell) => asFiniteNumber(cell) ?? null));
	}

	function stringArrayFromProps(value: unknown, preserveEmpty: boolean = false): string[] {
		if (!Array.isArray(value)) return [];
		const mapped = value.map((item) => asString(item).trim());
		return preserveEmpty ? mapped : mapped.filter((item) => item.length > 0);
	}

	function selectedValuesFromProps(value: unknown): string[] {
		if (!Array.isArray(value)) {
			const scalar = isScalarValue(value) ? asString(value).trim() : '';
			return scalar ? [scalar] : [];
		}
		const seen = new Set<string>();
		const selected: string[] = [];
		for (const item of value) {
			if (!isScalarValue(item)) continue;
			const normalized = asString(item).trim();
			if (seen.has(normalized)) continue;
			seen.add(normalized);
			selected.push(normalized);
		}
		return selected;
	}

	function formFieldsFromProps(value: unknown): Array<Record<string, unknown>> {
		return recordsFromArray(value);
	}

	function alertTypeFromProps(value: unknown): 'info' | 'warning' | 'error' | 'success' {
		const normalized = asString(value).trim().toLowerCase();
		if (normalized === 'warning' || normalized === 'error' || normalized === 'success') {
			return normalized;
		}
		return 'info';
	}

	function spinnerSizeFromProps(value: unknown): 'sm' | 'md' | 'lg' {
		const normalized = asString(value).trim().toLowerCase();
		if (normalized === 'sm' || normalized === 'lg') return normalized;
		return 'md';
	}

	/** Badge operational-status mode — only the five semantic tones pass through
	 *  (see $lib/shared/statusTone.statusBadgeTone, which emits these or null). */
	function badgeStatusFromProps(value: unknown): BadgeStatusTone | null {
		const normalized = asString(value).trim().toLowerCase();
		return (BADGE_STATUS_TONES as readonly string[]).includes(normalized)
			? (normalized as BadgeStatusTone)
			: null;
	}

	/** Shared-icon name for Button `props.icon` — unknown names render nothing. */
	function iconNameFromProps(value: unknown): IconName | '' {
		const normalized = asString(value).trim();
		return Object.hasOwn(ICON_PATHS, normalized) ? (normalized as IconName) : '';
	}

	function skeletonVariantFromProps(value: unknown): 'text' | 'circle' | 'rect' {
		const normalized = asString(value).trim().toLowerCase();
		if (normalized === 'circle' || normalized === 'rect') return normalized;
		return 'text';
	}

	function tooltipPositionFromProps(value: unknown): 'top' | 'bottom' | 'left' | 'right' {
		const normalized = asString(value).trim().toLowerCase();
		if (normalized === 'bottom' || normalized === 'left' || normalized === 'right') return normalized;
		return 'top';
	}

	function scopedIdBase(componentId: string): string {
		return idNamespace ? `${idNamespace}:${componentId}` : componentId;
	}

	function tabSpecsFromProps(value: unknown): MuijTabSpec[] {
		if (!Array.isArray(value)) return [];
		return value.map((tab, i) => {
			const rec = (typeof tab === 'object' && tab != null)
				? (tab as Record<string, unknown>)
				: {};
			const rawLabel = asString(rec.label).trim();
			const label = rawLabel || `Tab ${i + 1}`;
			if (rec.content !== undefined && rec.content !== null) {
				return { label, content: asString(rec.content) };
			}
			return { label };
		});
	}

	function tabsForComponent(component: MuijComponent): MuijTabSpec[] {
		const propTabs = tabSpecsFromProps(component.props.tabs);
		const children = component.children ?? [];
		if (children.length === 0) return propTabs;

		// R329: Map over the longer of children vs propTabs to avoid dropping
		// excess tab definitions when children count differs from propTabs count.
		const count = Math.max(children.length, propTabs.length);
		const result: MuijTabSpec[] = [];
		for (let i = 0; i < count; i++) {
			const child = children[i];
			const fromProps = propTabs[i]?.label?.trim() ?? '';
			const fromChild = child && typeof child.label === 'string' ? child.label.trim() : '';
			const content = propTabs[i]?.content;
			result.push({ label: fromProps || fromChild || `Tab ${i + 1}`, ...(content ? { content } : {}) });
		}
		return result;
	}

	function preferComponentLabel(label: unknown, fallback: unknown): string {
		const normalized = asString(label);
		if (normalized.trim() !== '') return normalized;
		const normalizedFallback = asString(fallback);
		return normalizedFallback.trim() !== '' ? normalizedFallback : '';
	}

	function componentAriaLabel(component: MuijComponent, fallback: unknown = ''): string {
		const explicit = asString(component.props.ariaLabel).trim();
		if (explicit) return explicit;
		const fromLabel = preferComponentLabel(component.label, component.props.label).trim();
		if (fromLabel) return fromLabel;
		const fromFallback = asString(fallback).trim();
		if (fromFallback) return fromFallback;
		return asString(component.id, component.component_type).trim() || component.component_type;
	}

	function resolveInteractionGoalId(component: MuijComponent): string | undefined {
		const goalId = asString(component.props.goal_id, asString(component.props.goalId)).trim();
		return goalId.length > 0 ? goalId : undefined;
	}

	function sanitizeTrigger(value: string): string {
		const cleaned = value.replace(/[\u0000-\u001F\u007F]/g, '').trim();
		if (!cleaned) return '';
		return cleaned.length > 255 ? cleaned.slice(0, 255) : cleaned;
	}

	function resolveInteractionTrigger(component: MuijComponent, interaction: MuijInteractionKind): string {
		const interactionTrigger = asString(component.props[`${interaction}Trigger`]).trim();
		const sharedTrigger = asString(component.props.trigger).trim();
		const fallback = `${component.id}:${interaction}`;
		return sanitizeTrigger(interactionTrigger || sharedTrigger || fallback);
	}

	function normalizeInteractionDetail(detail: unknown): Record<string, unknown> {
		return isRecord(detail) ? detail : {};
	}

	function shouldSendUiInteraction(component: MuijComponent, interaction: MuijInteractionKind): boolean {
		const normalizedAgentId = agentId.trim();
		const normalizedComponentId = component.id.trim();
		if (!normalizedAgentId || !normalizedComponentId) return false;
		// Change events can fire per-keystroke; require explicit routing config
		// so these remain intentional in server-driven flows.
		if (interaction !== 'change') return true;
		const hasExplicitTrigger = asString(component.props.changeTrigger, asString(component.props.trigger)).trim().length > 0;
		const hasGoalId = resolveInteractionGoalId(component) !== undefined;
		return hasExplicitTrigger || hasGoalId;
	}

	function emitComponentInteraction(
		component: MuijComponent,
		interaction: MuijInteractionKind,
		detail: unknown = {}
	): void {
		const normalizedDetail = normalizeInteractionDetail(detail);
		const trigger = resolveInteractionTrigger(component, interaction);
		const goalId = resolveInteractionGoalId(component);
		let sent = false;
		if (trigger && shouldSendUiInteraction(component, interaction)) {
			sent = v2Events.send({
				type: 'ui.interaction',
				agent_id: agentId.trim(),
				component_id: component.id.trim(),
				...(goalId ? { goal_id: goalId } : {}),
				trigger
			});
		}
		dispatch('interaction', {
			componentId: component.id,
			interaction,
			detail: normalizedDetail,
			sent
		});
	}

	function forwardInteraction(event: CustomEvent<MuijInteractionEventDetail>): void {
		dispatch('interaction', event.detail);
	}

	// Route-contract check: only when THIS renderer owns the whole route surface.
	// Split pages (e.g. /tasks) disable it here and validate the full surface at
	// the page level — otherwise a fragment is checked against the whole-route
	// contract and falsely reports required types held by a sibling fragment.
	$: if (browser && import.meta.env.DEV && depth === 0 && validateRouteContract) {
		lastValidationDigest = reportPrestoRouteValidation(
			$page.url.pathname,
			components,
			lastValidationDigest
		);
	}

	// Unsupported-component-type guard: always runs per renderer — an unknown type
	// in ANY fragment is genuinely unrenderable, independent of the route contract.
	$: if (browser && import.meta.env.DEV && depth === 0) {
		const unknownTypes = collectUnknownComponentTypes(components);
		const unknownDigest = unknownTypes.join('|');
		if (unknownDigest !== lastUnknownTypeDigest) {
			lastUnknownTypeDigest = unknownDigest;
			if (unknownTypes.length > 0) {
				const message = `[GAUI Renderer] Unsupported component type(s): ${unknownTypes.join(', ')}`;
				console.error(message);
				throw new Error(message);
			}
		}
	}
</script>

{#each components as component (component.id)}
	{#if component.component_type === 'Gauge'}
		<div class="muij-renderer-item">
			<Gauge
				fill={component.props.fill as number | undefined}
				label={component.label}
				sublabel={component.props.sublabel as string | undefined}
				iteration={component.props.iteration as number | undefined}
				max_iterations={component.props.max_iterations as number | undefined}
			/>
		</div>
	{:else if component.component_type === 'TerminalTransient'}
		<div class="muij-renderer-item">
				<TerminalTransient
					line={component.props.line as string | undefined}
					seq={component.props.seq as number | undefined}
					label={component.label}
					maxLines={component.props.maxLines as number | undefined}
					autoscroll={asBoolean(component.props.autoscroll, true)}
				/>
		</div>
		{:else if component.component_type === 'LiveSelectors'}
			<div class="muij-renderer-item">
				<LiveSelectors
					selectors={component.props.selectors}
					maxEntries={asFiniteNumber(component.props.maxEntries) ?? 10}
				/>
			</div>
		{:else if component.component_type === 'Image'}
			<div class="muij-renderer-item">
				<Image
					src={component.props.src}
					alt={asString(component.props.alt)}
					fit={asString(component.props.fit) ?? 'contain'}
					width={component.props.width}
					height={component.props.height}
				/>
			</div>
		{:else if component.component_type === 'Video'}
			<div class="muij-renderer-item">
				<Video
					src={component.props.src}
					poster={component.props.poster}
					controls={component.props.controls ?? true}
					autoplay={component.props.autoplay ?? false}
					loop={component.props.loop ?? false}
					muted={component.props.muted ?? false}
				/>
			</div>
		{:else if component.component_type === 'Audio'}
			<div class="muij-renderer-item">
				<Audio
					src={component.props.src}
					controls={component.props.controls ?? true}
					autoplay={component.props.autoplay ?? false}
					loop={component.props.loop ?? false}
				/>
			</div>
		{:else if component.component_type === 'Iframe'}
			<div class="muij-renderer-item">
				<Iframe
					url={asString(component.props.url)}
					height={asFiniteNumber(component.props.height) ?? 600}
					aspectRatio={asString(component.props.aspectRatio)}
					sandbox={asString(component.props.sandbox) ?? 'allow-scripts allow-same-origin'}
					title={asString(component.props.title) ?? 'Embedded notebook'}
				/>
			</div>
		{:else if component.component_type === 'CodeBlock'}
			<div class="muij-renderer-item">
				<CodeBlock
					code={asString(component.props.code)}
					language={asString(component.props.language) ?? 'plaintext'}
					showLineNumbers={component.props.showLineNumbers ?? false}
					filename={asString(component.props.filename)}
					maxHeight={asFiniteNumber(component.props.maxHeight)}
				/>
			</div>
		{:else if component.component_type === 'DiffViewer'}
			<div class="muij-renderer-item">
				<DiffViewer
					old={component.props.old}
					newContent={component.props.new}
					splitView={component.props.splitView ?? false}
					showLineNumbers={component.props.showLineNumbers ?? true}
				/>
			</div>
		{:else if component.component_type === 'Markdown'}
			<div class="muij-renderer-item">
				<Markdown content={component.props.content} />
			</div>
		{:else if component.component_type === 'QRCode'}
			<div class="muij-renderer-item">
				<QRCode
					value={component.props.value}
					size={asFiniteNumber(component.props.size) ?? 128}
					bgColor={asString(component.props.bgColor) ?? '#ffffff'}
					fgColor={asString(component.props.fgColor) ?? '#000000'}
				/>
			</div>
		{:else if component.component_type === 'ReactionBar'}
			<div class="muij-renderer-item">
				<ReactionBar
					reactions={component.props.reactions}
					showCount={component.props.showCount ?? true}
				/>
			</div>
		{:else if component.component_type === 'PresenceAvatars'}
			<div class="muij-renderer-item">
				<PresenceAvatars
					users={component.props.users}
					maxVisible={asFiniteNumber(component.props.maxVisible) ?? 4}
					size={asString(component.props.size) ?? 'md'}
				/>
			</div>
		{:else if component.component_type === 'ActivityFeed'}
			<div class="muij-renderer-item">
				<ActivityFeed
					items={component.props.items}
					maxItems={asFiniteNumber(component.props.maxItems)}
				/>
			</div>
		{:else if component.component_type === 'CommentThread'}
			<div class="muij-renderer-item">
				<CommentThread
					comments={component.props.comments}
					maxDepth={asFiniteNumber(component.props.maxDepth) ?? 3}
				/>
			</div>
		{:else if component.component_type === 'ApprovalFlow'}
			<div class="muij-renderer-item">
				<ApprovalFlow
					approvers={component.props.approvers}
					requireAll={component.props.requireAll ?? true}
				/>
			</div>
		{:else if component.component_type === 'Card'}
			<div class="muij-renderer-item">
				<Card
					title={preferComponentLabel(component.label, component.props.title)}
					subtitle={asString(component.props.subtitle)}
					body={asString(component.props.body)}
					elevation={component.props.elevation as number | undefined}
					className={asString(component.props.className)}
					interactive={asBoolean(component.props.interactive, false)}
					disabled={asBoolean(component.props.disabled, false)}
					tooltip={asString(component.props.tooltip)}
					on:click={() => emitComponentInteraction(component, 'action', { action: 'open' })}
				>
					{#if component.children?.length && depth < MAX_DEPTH}
						<svelte:self
							components={component.children}
							depth={depth + 1}
							{idNamespace}
							{agentId}
							on:interaction={forwardInteraction}
						/>
					{:else if component.children?.length && depth >= MAX_DEPTH}
						<div class="muij-renderer-depth-limit">max depth reached</div>
					{/if}
				</Card>
			</div>
		{:else if component.component_type === 'Text'}
			<div class="muij-renderer-item">
				<Text
					children={asString(component.props.children)}
					variant={(component.props.variant as string | undefined) ?? 'body'}
					className={asString(component.props.className)}
					title={asString(component.props.title)}
				/>
			</div>
		{:else if component.component_type === 'EntityGrid'}
			<div class="muij-renderer-item">
				<EntityGrid
					columns={columnsFromProps(component.props.columns)}
					rows={rowsForComponent(component)}
					pageSize={(component.props.pageSize as number | undefined) ?? 25}
					paginationMode={asString(component.props.paginationMode) === 'server' ? 'server' : 'client'}
					currentPage={asFiniteNumber(component.props.currentPage) ?? 1}
					pageCount={asFiniteNumber(component.props.pageCount) ?? 1}
					totalItems={asFiniteNumber(component.props.totalItems) ?? 0}
					pageCountExact={asBoolean(component.props.pageCountExact, true)}
					totalItemsExact={asBoolean(component.props.totalItemsExact, true)}
					startItem={asFiniteNumber(component.props.startItem) ?? 0}
					endItem={asFiniteNumber(component.props.endItem) ?? 0}
					sortKey={asString(component.props.sortKey)}
					sortDir={(component.props.sortDir as 'asc' | 'desc' | undefined) ?? 'asc'}
					expandable={asBoolean(component.props.expandable, false)}
					rowIdKey={asString(component.props.rowIdKey) || 'id'}
					actions={Array.isArray(component.props.actions) ? component.props.actions : []}
					filterKeys={stringArrayFromProps(component.props.filterKeys)}
					filterPlaceholder={asString(component.props.filterPlaceholder) || 'Filter rows'}
					filterLabel={asString(component.props.filterLabel) || 'Filter rows'}
					stackedFields={recordsFromArray(component.props.stackedFields).map((field) => ({
						key: asString(field.key),
						label: asString(field.label)
					})).filter((field) => field.key)}
					stackedValueMaxLines={asFiniteNumber(component.props.stackedValueMaxLines) ?? 0}
					stackedRowsExpandable={asBoolean(component.props.stackedRowsExpandable, false)}
					wrapTable={asBoolean(component.props.wrapTable, false)}
					on:action={(e) => emitComponentInteraction(component, 'action', e.detail)}
					on:expand={(e) => emitComponentInteraction(component, 'action', { ...e.detail, action: 'expand' })}
					on:pagechange={(e) => emitComponentInteraction(component, 'change', { ...e.detail, action: 'page' })}
					on:sortchange={(e) => emitComponentInteraction(component, 'change', { ...e.detail, action: 'sort' })}
					on:filterchange={(e) => emitComponentInteraction(component, 'change', { ...e.detail, action: 'filter' })}
				/>
			</div>
		{:else if component.component_type === 'PieChart'}
			<div class="muij-renderer-item">
				<PieChart
					segments={chartDataFromProps(component.props.segments ?? component.props.data)}
					size={asFiniteNumber(component.props.size) ?? 180}
					innerRatio={asFiniteNumber(component.props.innerRatio) ?? 0.58}
					showLegend={asBoolean(component.props.showLegend, true)}
					dataSource={dataSourceFromProps(component.props.dataSource)}
					labelField={asString(component.props.labelField) || 'label'}
					valueField={asString(component.props.valueField) || 'value'}
				/>
			</div>
		{:else if component.component_type === 'BarChart'}
			<div class="muij-renderer-item">
				<BarChart
					data={chartDataFromProps(component.props.data)}
					horizontal={asBoolean(component.props.horizontal, false)}
					maxValue={asFiniteNumber(component.props.maxValue)}
					dataSource={dataSourceFromProps(component.props.dataSource)}
					xField={asString(component.props.xField) || 'label'}
					yField={asString(component.props.yField) || 'value'}
				/>
			</div>
		{:else if component.component_type === 'LineChart'}
			<div class="muij-renderer-item">
				<LineChart
					series={lineSeriesFromProps(component.props.series ?? component.props.data)}
					points={linePointsFromProps(component.props.points)}
					width={asFiniteNumber(component.props.width) ?? 360}
					height={asFiniteNumber(component.props.height) ?? 220}
					showLegend={asBoolean(component.props.showLegend, true)}
					dataSource={dataSourceFromProps(component.props.dataSource)}
					xField={asString(component.props.xField) || 'x'}
					yField={asString(component.props.yField) || 'y'}
					seriesField={asString(component.props.seriesField) || null}
				/>
			</div>
		{:else if component.component_type === 'AreaChart'}
			<div class="muij-renderer-item">
				<AreaChart
					series={lineSeriesFromProps(component.props.series ?? component.props.data)}
					points={linePointsFromProps(component.props.points)}
					width={asFiniteNumber(component.props.width) ?? 360}
					height={asFiniteNumber(component.props.height) ?? 220}
				/>
			</div>
		{:else if component.component_type === 'ScatterChart'}
			<div class="muij-renderer-item">
				<ScatterChart
					points={scatterPointsFromProps(component.props.points ?? component.props.data)}
					width={asFiniteNumber(component.props.width) ?? 360}
					height={asFiniteNumber(component.props.height) ?? 220}
				/>
			</div>
		{:else if component.component_type === 'TrendChart'}
			<div class="muij-renderer-item">
				<TrendChart
					data={chartDataFromProps(component.props.data ?? component.props.series)}
					width={asFiniteNumber(component.props.width) ?? 420}
					height={asFiniteNumber(component.props.height) ?? 220}
					color={asString(component.props.color, 'var(--accent-primary)')}
				/>
			</div>
		{:else if component.component_type === 'Sparkline'}
			<div class="muij-renderer-item">
				<Sparkline
					data={sparklineDataFromProps(component.props.data ?? component.props.points)}
					width={asFiniteNumber(component.props.width) ?? 120}
					height={asFiniteNumber(component.props.height) ?? 36}
					color={asString(component.props.color, 'var(--accent-primary)')}
					strokeWidth={asFiniteNumber(component.props.strokeWidth) ?? 2}
				/>
			</div>
		{:else if component.component_type === 'Heatmap'}
			<div class="muij-renderer-item">
				<Heatmap
					matrix={heatmapMatrixFromProps(component.props.matrix ?? component.props.data)}
					rowLabels={stringArrayFromProps(component.props.rowLabels, true)}
					colLabels={stringArrayFromProps(component.props.colLabels, true)}
					showValues={asBoolean(component.props.showValues, false)}
				/>
			</div>
	{:else if component.component_type === 'Container'}
		<div class="muij-renderer-item">
			<Container
				direction={(component.props.direction as 'row' | 'column' | undefined) ?? 'column'}
					gap={(component.props.gap as string | undefined) ?? 'var(--space-md)'}
					padding={(component.props.padding as string | undefined) ?? '0'}
				>
					{#if component.children?.length && depth < MAX_DEPTH}
						<svelte:self
							components={component.children}
							depth={depth + 1}
							{idNamespace}
							{agentId}
							on:interaction={forwardInteraction}
						/>
					{:else if component.children?.length && depth >= MAX_DEPTH}
						<!-- R145: Only show depth limit when there are actual children to render -->
						<div class="muij-renderer-depth-limit">max depth reached</div>
					{/if}
			</Container>
		</div>
	{:else if component.component_type === 'Stack'}
		<div class="muij-renderer-item">
				<Stack
					className={asString(component.props.className)}
					direction={(component.props.direction as 'row' | 'column' | undefined) ?? 'column'}
					align={(component.props.align as string | undefined) ?? 'stretch'}
					justify={(component.props.justify as string | undefined) ?? 'flex-start'}
						gap={(component.props.gap as string | undefined) ?? 'var(--space-sm)'}
						wrap={asBoolean(component.props.wrap, false)}
					>
					{#if component.children?.length && depth < MAX_DEPTH}
						<svelte:self
							components={component.children}
							depth={depth + 1}
							{idNamespace}
							{agentId}
							on:interaction={forwardInteraction}
						/>
					{:else if component.children?.length && depth >= MAX_DEPTH}
						<!-- R145: Only show depth limit when there are actual children to render -->
						<div class="muij-renderer-depth-limit">max depth reached</div>
					{/if}
			</Stack>
		</div>
	{:else if component.component_type === 'Grid'}
		<div class="muij-renderer-item">
			<Grid
				columns={(component.props.columns as number | undefined) ?? 2}
				gap={(component.props.gap as string | undefined) ?? 'var(--space-md)'}
				autoFit={asBoolean(component.props.autoFit, false)}
				minColumnWidth={(component.props.minColumnWidth as string | undefined) ?? '200px'}
				className={asString(component.props.className)}
			>
				{#if component.children?.length && depth < MAX_DEPTH}
					<svelte:self
						components={component.children}
						depth={depth + 1}
						{idNamespace}
						{agentId}
						on:interaction={forwardInteraction}
					/>
				{:else if component.children?.length && depth >= MAX_DEPTH}
					<!-- R145: Only show depth limit when there are actual children to render -->
					<div class="muij-renderer-depth-limit">max depth reached</div>
				{/if}
			</Grid>
		</div>
	{:else if component.component_type === 'SplitPanel'}
		<div class="muij-renderer-item">
			<SplitPanel
				ratio={(component.props.ratio as number | undefined) ?? 0.5}
				direction={(component.props.direction as 'horizontal' | 'vertical' | undefined) ?? 'horizontal'}
				minFirst={(component.props.minFirst as string | undefined) ?? '100px'}
				minSecond={(component.props.minSecond as string | undefined) ?? '100px'}
			>
					<!-- R57/R202: Route children[0] to first pane and children[1..n] to second pane -->
					<!-- R135: .slice() creates new array wrappers but keyed children (component.id) remain stable; accepted limitation -->
					{#if component.children?.length && depth < MAX_DEPTH}
						<svelte:self
							components={component.children.slice(0, 1)}
							depth={depth + 1}
							{idNamespace}
							{agentId}
							on:interaction={forwardInteraction}
						/>
					{:else if component.children?.length && depth >= MAX_DEPTH}
						<!-- R145: Only show depth limit when there are actual children to render -->
						<div class="muij-renderer-depth-limit">max depth reached</div>
					{/if}
					<svelte:fragment slot="second">
						{#if component.children && component.children.length > 1 && depth < MAX_DEPTH}
							<svelte:self
								components={component.children.slice(1)}
								depth={depth + 1}
								{idNamespace}
								{agentId}
								on:interaction={forwardInteraction}
							/>
						{:else if component.children && component.children.length > 1 && depth >= MAX_DEPTH}
							<div class="muij-renderer-depth-limit">max depth reached</div>
						{/if}
					</svelte:fragment>
			</SplitPanel>
		</div>
			{:else if component.component_type === 'Tabs'}
				<div class="muij-renderer-item">
					{#if component.children?.length}
							<!-- R368: Pin tabsForComponent once per render to prevent divergence
								between prop binding and slot fallback during live delta updates. -->
							{@const resolvedTabs = tabsForComponent(component)}
							<Tabs
								tabs={resolvedTabs}
							activeIndex={(component.props.activeIndex as number | undefined) ?? 0}
							idBase={scopedIdBase(component.id)}
							let:activeIndex
							>
								{#if component.children[activeIndex]}
									{#if depth < MAX_DEPTH}
										<svelte:self
											components={[component.children[activeIndex]]}
											depth={depth + 1}
											{idNamespace}
											{agentId}
											on:interaction={forwardInteraction}
										/>
									{:else}
										<div class="muij-renderer-depth-limit">max depth reached</div>
									{/if}
							{:else}
								<!-- R352/R357: When active tab has no child, render prop-defined
									content directly (no recursion), regardless of parent depth. -->
								{resolvedTabs[activeIndex]?.content ?? ''}
							{/if}
						</Tabs>
				{:else}
					<Tabs
						tabs={tabsForComponent(component)}
						activeIndex={(component.props.activeIndex as number | undefined) ?? 0}
						idBase={scopedIdBase(component.id)}
					/>
				{/if}
		</div>
	{:else if component.component_type === 'Panel'}
		<div class="muij-renderer-item">
					<Panel
						header={preferComponentLabel(component.label, component.props.header)}
						footer={asString(component.props.footer)}
						collapsible={asBoolean(component.props.collapsible, false)}
						collapsed={asBoolean(component.props.collapsed, false)}
						interactive={asBoolean(component.props.interactive, true)}
							idBase={scopedIdBase(component.id)}
						>
					{#if component.children?.length && depth < MAX_DEPTH}
						<svelte:self
							components={component.children}
							depth={depth + 1}
							{idNamespace}
							{agentId}
							on:interaction={forwardInteraction}
						/>
					{:else if component.children?.length && depth >= MAX_DEPTH}
						<!-- R145: Only show depth limit when there are actual children to render -->
						<div class="muij-renderer-depth-limit">max depth reached</div>
					{/if}
			</Panel>
		</div>
	{:else if component.component_type === 'ScrollArea'}
		<div class="muij-renderer-item">
			<ScrollArea
				maxHeight={(component.props.maxHeight as string | undefined) ?? '300px'}
					scrollbar={(component.props.scrollbar as 'auto' | 'thin' | 'hidden' | undefined) ?? 'auto'}
					ariaLabel={asString(component.props.ariaLabel)}
				>
					{#if component.children?.length && depth < MAX_DEPTH}
						<svelte:self
							components={component.children}
							depth={depth + 1}
							{idNamespace}
							{agentId}
							on:interaction={forwardInteraction}
						/>
					{:else if component.children?.length && depth >= MAX_DEPTH}
						<!-- R145: Only show depth limit when there are actual children to render -->
						<div class="muij-renderer-depth-limit">max depth reached</div>
					{/if}
			</ScrollArea>
		</div>
	{:else if component.component_type === 'Divider'}
		<div class="muij-renderer-item">
			<Divider
				orientation={(component.props.orientation as 'horizontal' | 'vertical' | undefined) ?? 'horizontal'}
				variant={(component.props.variant as 'solid' | 'dashed' | 'dotted' | undefined) ?? 'solid'}
				spacing={(component.props.spacing as string | undefined) ?? 'var(--space-md)'}
			/>
		</div>
		{:else if component.component_type === 'Table'}
			<div class="muij-renderer-item">
				<Table
					columns={columnsFromProps(component.props.columns)}
					rows={rowsForComponent(component)}
					sortKey={asString(component.props.sortKey)}
					sortDir={(component.props.sortDir as 'asc' | 'desc' | undefined) ?? 'asc'}
					sortValueByKey={sortValueMapFromProps(component.props.sortValueByKey)}
					presorted={asBoolean(component.props.presorted, false)}
					dataSource={dataSourceFromProps(component.props.dataSource)}
				/>
			</div>
	{:else if component.component_type === 'DataList'}
		<div class="muij-renderer-item">
			<DataList items={itemsFromProps(component.props.items)} />
		</div>
		{:else if component.component_type === 'MetricCard'}
			<div class="muij-renderer-item">
				<MetricCard
					value={asString(component.props.value)}
					label={preferComponentLabel(component.label, component.props.label)}
					trend={component.props.trend as 'up' | 'down' | 'flat' | undefined}
					trendLabel={asString(component.props.trendLabel)}
					dataSource={dataSourceFromProps(component.props.dataSource)}
					valueField={asString(component.props.valueField) || null}
					formatAs={(component.props.formatAs as 'number' | 'currency' | 'percent' | 'string' | undefined) ?? 'number'}
				/>
			</div>
	{:else if component.component_type === 'Progress'}
		<div class="muij-renderer-item">
				<Progress
					percent={(component.props.percent as number | undefined) ?? 0}
					status={(component.props.status as 'active' | 'success' | 'error' | undefined) ?? 'active'}
					label={component.label}
					showPercent={asBoolean(component.props.showPercent, true)}
				/>
		</div>
		{:else if component.component_type === 'Badge'}
			<div class="muij-renderer-item">
				<Badge
					text={asString(component.props.text)}
					color={(component.props.color as 'default' | 'success' | 'warning' | 'error' | 'info' | undefined) ?? 'default'}
					status={badgeStatusFromProps(component.props.status)}
					className={asString(component.props.className)}
				/>
			</div>
		{:else if component.component_type === 'Tag'}
				<div class="muij-renderer-item">
					<Tag
						text={asString(component.props.text)}
						color={(component.props.color as 'default' | 'success' | 'warning' | 'error' | 'info' | undefined) ?? 'default'}
						className={asString(component.props.className)}
						dismissible={component.props.dismissible === true}
						on:dismiss={() => emitComponentInteraction(component, 'dismiss')}
					/>
				</div>
		{:else if component.component_type === 'Button'}
			<div class="muij-renderer-item">
					<Button
						label={preferComponentLabel(component.label, component.props.label)}
						icon={iconNameFromProps(component.props.icon)}
						variant={(component.props.variant as 'primary' | 'secondary' | 'outline' | undefined) ?? 'primary'}
						disabled={asBoolean(component.props.disabled, false)}
						size={(component.props.size as 'sm' | 'md' | 'lg' | undefined) ?? 'md'}
						className={asString(component.props.className)}
						title={asString(component.props.title)}
						interactive={asBoolean(component.props.interactive, true)}
						type={(component.props.type as 'button' | 'submit' | 'reset' | undefined) ?? 'button'}
						on:click={() => emitComponentInteraction(component, 'action')}
				/>
			</div>
			{:else if component.component_type === 'TextField'}
				<div class="muij-renderer-item">
					<TextField
						value={asString(component.props.value)}
						placeholder={asString(component.props.placeholder)}
						disabled={asBoolean(component.props.disabled, false)}
						label={preferComponentLabel(component.label, component.props.label)}
						idBase={scopedIdBase(component.id)}
					/>
				</div>
			{:else if component.component_type === 'Select'}
				<div class="muij-renderer-item">
					<Select
						options={optionsFromProps(component.props.options)}
						value={asString(component.props.value)}
						disabled={asBoolean(component.props.disabled, false)}
						label={preferComponentLabel(component.label, component.props.label)}
						placeholder={asString(component.props.placeholder, 'Select...')}
						idBase={scopedIdBase(component.id)}
						interactive={asBoolean(component.props.interactive, false)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'Form'}
				<div class="muij-renderer-item">
					<Form
						title={preferComponentLabel(component.label, component.props.title)}
						showSubmit={asBoolean(component.props.showSubmit, false)}
						submitLabel={asString(component.props.submitLabel, 'Submit')}
						disabled={asBoolean(component.props.disabled, false)}
						fields={formFieldsFromProps(component.props.fields)}
						idBase={scopedIdBase(component.id)}
							on:submit={(event) => emitComponentInteraction(component, 'submit', event.detail)}
							on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
						>
							{#if component.children?.length && depth < MAX_DEPTH}
								<svelte:self
									components={component.children}
									depth={depth + 1}
									{idNamespace}
									{agentId}
									on:interaction={forwardInteraction}
								/>
							{:else if component.children?.length && depth >= MAX_DEPTH}
								<div class="muij-renderer-depth-limit">max depth reached</div>
							{/if}
						</Form>
				</div>
			{:else if component.component_type === 'TextArea'}
				<div class="muij-renderer-item">
					<TextArea
						value={asString(component.props.value)}
						placeholder={asString(component.props.placeholder)}
						rows={asFiniteNumber(component.props.rows) ?? 4}
						maxLength={asFiniteNumber(component.props.maxLength)}
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, component.props.placeholder)}
						disabled={asBoolean(component.props.disabled, false)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'NumberField'}
				<div class="muij-renderer-item">
					<NumberField
						value={asFiniteNumber(component.props.value) ?? 0}
						min={asFiniteNumber(component.props.min)}
						max={asFiniteNumber(component.props.max)}
						step={asFiniteNumber(component.props.step) ?? 1}
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, 'Number input')}
						disabled={asBoolean(component.props.disabled, false)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'Slider'}
				<div class="muij-renderer-item">
					<Slider
						value={asFiniteNumber(component.props.value) ?? 0}
						min={asFiniteNumber(component.props.min) ?? 0}
						max={asFiniteNumber(component.props.max) ?? 100}
						step={asFiniteNumber(component.props.step) ?? 1}
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, 'Slider')}
						disabled={asBoolean(component.props.disabled, false)}
						showValue={asBoolean(component.props.showValue, true)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'Checkbox'}
				<div class="muij-renderer-item">
					<Checkbox
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, 'Checkbox')}
						checked={asBoolean(component.props.checked, false)}
						indeterminate={asBoolean(component.props.indeterminate, false)}
						disabled={asBoolean(component.props.disabled, false)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'RadioGroup'}
				<div class="muij-renderer-item">
					<RadioGroup
						label={preferComponentLabel(component.label, component.props.label)}
						options={selectableOptionsFromProps(component.props.options)}
						value={asString(component.props.value)}
						disabled={asBoolean(component.props.disabled, false)}
						name={asString(component.props.name, `muij-rg-${component.id}`)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'MultiSelect'}
				<div class="muij-renderer-item">
					<MultiSelect
						label={preferComponentLabel(component.label, component.props.label)}
						options={selectableOptionsFromProps(component.props.options)}
						values={selectedValuesFromProps(component.props.values ?? component.props.value)}
						disabled={asBoolean(component.props.disabled, false)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'TreeSelect'}
				<div class="muij-renderer-item">
					<TreeSelect
						label={preferComponentLabel(component.label, component.props.label)}
						groups={Array.isArray(component.props.groups) ? component.props.groups : []}
						values={selectedValuesFromProps(component.props.values ?? component.props.value)}
						disabled={asBoolean(component.props.disabled, false)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'DatePicker'}
				<div class="muij-renderer-item">
					<DatePicker
						value={asString(component.props.value)}
						min={asString(component.props.min)}
						max={asString(component.props.max)}
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, 'Date')}
						disabled={asBoolean(component.props.disabled, false)}
						idBase={scopedIdBase(component.id)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'Toggle'}
				<div class="muij-renderer-item">
					<Toggle
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, 'Toggle')}
						checked={asBoolean(component.props.checked, false)}
						disabled={asBoolean(component.props.disabled, false)}
						on:change={(event) => emitComponentInteraction(component, 'change', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'SearchInput'}
				<div class="muij-renderer-item">
					<SearchInput
						value={asString(component.props.value)}
						placeholder={asString(component.props.placeholder, 'Search...')}
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, component.props.placeholder)}
						disabled={asBoolean(component.props.disabled, false)}
						idBase={scopedIdBase(component.id)}
						on:search={(event) => emitComponentInteraction(component, 'search', event.detail)}
					/>
				</div>
				{:else if component.component_type === 'Alert'}
					<div class="muij-renderer-item">
						<Alert
							type={alertTypeFromProps(component.props.type)}
							message={asString(component.props.message, preferComponentLabel(component.label, component.props.title))}
							closable={asBoolean(component.props.closable, false)}
							updateToken={component}
						/>
					</div>
				{:else if component.component_type === 'Toast'}
					<div class="muij-renderer-item">
						<Toast
							message={asString(component.props.message, preferComponentLabel(component.label, component.props.title))}
							duration={asFiniteNumber(component.props.duration) ?? 4000}
							actionLabel={asString(component.props.actionLabel)}
							updateToken={component}
							on:action={(event) => emitComponentInteraction(component, 'action', event.detail)}
						/>
					</div>
			{:else if component.component_type === 'Notification'}
				<div class="muij-renderer-item">
					<Notification
						title={preferComponentLabel(component.label, component.props.title)}
						body={asString(component.props.body, asString(component.props.message))}
						actions={notificationActionsFromProps(component.props.actions)}
						on:action={(event) => emitComponentInteraction(component, 'action', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'ProgressBar'}
				<div class="muij-renderer-item">
					<ProgressBar
						percent={asFiniteNumber(component.props.percent) ?? 0}
						label={preferComponentLabel(component.label, component.props.label)}
						ariaLabel={componentAriaLabel(component, 'Progress')}
						color={asString(component.props.color, 'var(--accent-primary)')}
					/>
				</div>
			{:else if component.component_type === 'Spinner'}
				<div class="muij-renderer-item">
					<Spinner
						size={spinnerSizeFromProps(component.props.size)}
						label={asString(component.props.label, preferComponentLabel(component.label, 'Loading'))}
						centered={asBoolean(component.props.centered, false)}
					/>
				</div>
			{:else if component.component_type === 'Skeleton'}
				<div class="muij-renderer-item">
					<Skeleton
						variant={skeletonVariantFromProps(component.props.variant)}
						animate={asBoolean(component.props.animate, true)}
						width={asString(component.props.width, '100%')}
						height={asString(component.props.height)}
					/>
				</div>
			{:else if component.component_type === 'EmptyState'}
				<div class="muij-renderer-item">
					<EmptyState
						icon={asString(component.props.icon, '[ ]')}
						title={asString(component.props.title, preferComponentLabel(component.label, 'No results')) || 'No results'}
						description={asString(component.props.description)}
						actionLabel={asString(component.props.actionLabel)}
						className={asString(component.props.className)}
						on:action={(event) => emitComponentInteraction(component, 'action', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'ConfirmDialog'}
				<div class="muij-renderer-item">
					<ConfirmDialog
						open={asBoolean(component.props.open, false)}
						title={preferComponentLabel(component.label, component.props.title) || 'Confirm'}
						message={asString(component.props.message, 'Are you sure?')}
						confirmLabel={asString(component.props.confirmLabel, 'Confirm')}
						cancelLabel={asString(component.props.cancelLabel, 'Cancel')}
						on:confirm={(event) => emitComponentInteraction(component, 'confirm', event.detail)}
						on:cancel={(event) => emitComponentInteraction(component, 'cancel', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'Modal'}
				<Modal
					open={asBoolean(component.props.open, false)}
					title={asString(component.props.title)}
					size={(component.props.size as 'sm' | 'md' | 'lg' | 'xl' | 'full' | undefined) ?? 'lg'}
					closable={asBoolean(component.props.closable, true)}
					on:close={() => emitComponentInteraction(component, 'close', {})}
				>
					{#if component.children?.length && depth < MAX_DEPTH}
						<svelte:self
							components={component.children}
							depth={depth + 1}
							{idNamespace}
							{agentId}
							on:interaction={forwardInteraction}
						/>
					{:else if component.children?.length && depth >= MAX_DEPTH}
						<div class="muij-renderer-depth-limit">max depth reached</div>
					{/if}
				</Modal>
			{:else if component.component_type === 'Tooltip'}
				<div class="muij-renderer-item">
					<Tooltip
							content={asString(component.props.content, asString(component.props.message))}
							position={tooltipPositionFromProps(component.props.position)}
						>
							{#if component.children?.length && depth < MAX_DEPTH}
								<svelte:self
									components={component.children}
									depth={depth + 1}
									{idNamespace}
									{agentId}
									on:interaction={forwardInteraction}
								/>
							{:else if component.children?.length && depth >= MAX_DEPTH}
								<span class="muij-renderer-depth-limit">max depth reached</span>
							{:else}
							<span>{preferComponentLabel(component.label, component.props.label) || 'i'}</span>
						{/if}
					</Tooltip>
				</div>
			{:else if component.component_type === 'ActionBus'}
				<div class="muij-renderer-item">
					<ActionBus
						actions={actionsFromProps(component.props.actions)}
						agentId={agentId}
						componentId={component.id}
						title={preferComponentLabel(component.label, component.props.title)}
						disabled={asBoolean(component.props.disabled, false)}
					/>
				</div>
			{:else if component.component_type === 'DailyBriefingPreview'}
				<div class="muij-renderer-item">
					<DailyBriefingPreview
						showHeader={asBoolean(component.props.showHeader, false)}
						on:addWidget={(event) => emitComponentInteraction(component, 'action', event.detail)}
					/>
				</div>
			{:else if component.component_type === 'Tree'}
				<div class="muij-renderer-item">
					<Tree
						nodes={component.props.nodes}
						expandAll={component.props.expandAll ?? true}
						maxDepth={component.props.maxDepth ?? 3}
						selectable={component.props.selectable ?? false}
						on:select={(event) => emitComponentInteraction(component, 'action', { action: 'select', rowId: event.detail.nodeId })}
					/>
				</div>
			{:else if component.component_type === 'TreeNode'}
				<div class="muij-renderer-item">
					<Tree
						nodes={[{ id: component.id, label: component.props.label ?? component.id, status: component.props.status, icon: component.props.icon, children: component.props.children }]}
						expandAll={component.props.expandAll ?? true}
						maxDepth={component.props.maxDepth ?? 3}
					/>
				</div>
			{:else if component.component_type === 'Graph'}
				<div class="muij-renderer-item">
					<Graph
						nodes={component.props.nodes}
						edges={component.props.edges}
						layout={component.props.layout ?? 'layered'}
						focusNodeId={component.props.focusNodeId ?? component.props.focus_node_id ?? ''}
						revealOrder={component.props.revealOrder ?? component.props.reveal_order ?? []}
						on:select={(event) => emitComponentInteraction(component, 'action', { action: 'select', rowId: event.detail.nodeId })}
					/>
				</div>
			{:else}
				<div class="muij-renderer-item muij-renderer-unknown">
					<span class="muij-renderer-unknown-type">{component.component_type}</span>
					{#if component.label}
						<span class="muij-renderer-unknown-label">{component.label}</span>
					{/if}
				</div>
			{/if}
{/each}

<style>
	/* R501: minimal style to avoid empty-ruleset lint warning */
	.muij-renderer-item {
		display: contents;
	}

	.muij-renderer-unknown {
		padding: 8px 12px;
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--text-muted);
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.muij-renderer-unknown-type {
		background: var(--bg-soft);
		padding: 2px 6px;
		border-radius: var(--radius-sm);
	}

	.muij-renderer-depth-limit {
		padding: 4px 8px;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		color: var(--text-muted);
		font-style: italic;
	}
</style>
