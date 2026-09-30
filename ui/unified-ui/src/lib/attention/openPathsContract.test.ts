import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { describe, expect, it } from 'vitest';

const SRC_ROOT = join(process.cwd(), 'src');
const STORE_RESOLVED_ALLOWLIST = new Set([
	'lib/attention/AttentionCenter.svelte',
	'lib/attention/centerState.ts',
	'lib/attention/routeLauncher.ts',
	'lib/magician/square/hud/CitizenInspector.svelte'
]);
const LEGACY_ROUTE_ALLOWLIST = new Set([
	'lib/attention/routeLauncher.ts',
	'routes/(app)/square/+page.svelte'
]);
const DIRECT_APPROVAL_MUTATION_ALLOWLIST = new Set(['lib/stores/approvalStore.ts']);
const DIRECT_TARGET_LAUNCHERS = new Map<string, RegExp>([
	['lib/magician/chat/ChatPanel.svelte', /\bopenHitlPrompt\b/],
	['lib/shell/CommandPalette.svelte', /\bopenHitlPrompt\b/],
	['lib/shell/PlanModePane.svelte', /\bopenHitlPrompt\b/],
	// `routes/(app)/ExecutionPanel.svelte` was here until the unified task panel
	// took its last two mounts (chat, `/crew/<id>`) and it was deleted. The
	// surfaces that used to reach a HITL prompt through it now reach one through
	// the attention centre, which is where the other rows already point.
	// Similarly, `routes/(app)/today/+page.svelte` was migrated to the Morning Edition surface.
	['routes/(app)/square/+page.svelte', /\bopenHitlPrompt\b/]
]);

function productionSources(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) return productionSources(path);
		if (!/\.(svelte|ts)$/.test(entry.name)) return [];
		if (/\.(?:component\.)?test\.ts$/.test(entry.name)) return [];
		if (path.includes(`${join('src', 'test')}/`)) return [];
		return [path];
	});
}

describe('HITL Attention open-path architecture', () => {
	it('keeps feed-id resolution confined to store-owned paths', () => {
		const sources = productionSources(SRC_ROOT).map((path) => ({
			path,
			relativePath: relative(SRC_ROOT, path),
			text: readFileSync(path, 'utf8')
		}));
		const directViolations = sources
			.filter(({ text }) => /\bopenAttention(?:Item|ForItems)\b/.test(text))
			.map(({ relativePath }) => relativePath)
			.filter((path) => !STORE_RESOLVED_ALLOWLIST.has(path));
		const routeViolations = sources
			.filter(({ text }) => /\bopenAttentionRoute\b/.test(text))
			.map(({ relativePath }) => relativePath)
			.filter((path) => !LEGACY_ROUTE_ALLOWLIST.has(path));

		expect({ directViolations, routeViolations }).toEqual({
			directViolations: [],
			routeViolations: []
		});
	});

	it('keeps every migrated foreign projection on a typed direct-open boundary', () => {
		const missing = [...DIRECT_TARGET_LAUNCHERS].filter(([relativePath, pattern]) => {
			const text = readFileSync(join(SRC_ROOT, relativePath), 'utf8');
			return !pattern.test(text);
		});

		expect(missing.map(([relativePath]) => relativePath)).toEqual([]);
	});

	it('keeps approval decisions behind the canonical review prompt', () => {
		const violations = productionSources(SRC_ROOT)
			.map((path) => ({
				relativePath: relative(SRC_ROOT, path),
				text: readFileSync(path, 'utf8')
			}))
			.filter(({ text }) => /\bresolveApproval\s*\(/.test(text))
			.map(({ relativePath }) => relativePath)
			.filter((path) => !DIRECT_APPROVAL_MUTATION_ALLOWLIST.has(path));

		expect(violations).toEqual([]);
	});

	it('uses the exact projected Attention id for Square legacy fallback', () => {
		for (const relativePath of [
			'routes/(app)/square/+page.svelte'
		]) {
			const text = readFileSync(join(SRC_ROOT, relativePath), 'utf8');
			expect(text).toMatch(
				/openAttentionRoute\(item\.source_url,\s*todayAttentionItemId\(item\)\)/
			);
			expect(text).not.toMatch(/openAttentionRoute\(sourceUrl,\s*bullet\.(?:source_id|id)/);
		}
	});

	it('binds explicit plan approve and reject calls to the displayed plan version', () => {
		const taskStore = readFileSync(join(SRC_ROOT, 'lib/stores/taskStore.ts'), 'utf8');
		expect(taskStore).toMatch(/plan\/approve\?plan_id=\$\{encodeURIComponent\(normalizedPlanId\)\}/);
		expect(taskStore).toMatch(/plan\/reject\?plan_id=\$\{encodeURIComponent\(normalizedPlanId\)\}/);
	});

	it('renders the canonical Attention page without the general app shell', () => {
		const layout = readFileSync(join(SRC_ROOT, 'routes/(app)/+layout.svelte'), 'utf8');
		const attentionPage = readFileSync(
			join(SRC_ROOT, 'routes/(app)/attention/+page.svelte'),
			'utf8'
		);
		const notificationOverlay = readFileSync(
			join(SRC_ROOT, 'routes/notify-overlay/+page.svelte'),
			'utf8'
		);
		expect(layout).toMatch(/searchParams\.get\('native_attention'\) === '1'/);
		expect(layout).toMatch(/class:native-attention-window=\{nativeAttentionWindow\}/);
		expect(layout).toMatch(/!isAboutRoute && !nativeAttentionWindow/);
		expect(layout).toMatch(/<main class="native-attention-pane">\s*<slot \/>/);
		expect(attentionPage).toMatch(/invoke\('open_app_at', \{ path \}\)/);
		expect(notificationOverlay).toMatch(/attention_item: card\.correlationId/);
		expect(notificationOverlay).toMatch(/invokeOverlayResult\('open_app_at'/);
		expect(notificationOverlay).not.toMatch(/open_attention_target|hitlTarget/);
	});
});
