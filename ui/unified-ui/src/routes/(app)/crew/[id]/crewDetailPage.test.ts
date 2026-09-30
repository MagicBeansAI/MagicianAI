import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

function getRouteSource(): string {
	return readFileSync(resolve(process.cwd(), 'src/routes/(app)/crew/[id]/+page.svelte'), 'utf8');
}

describe('Single Crew Page Modernization Contract (crewDetailPage.test.ts)', () => {
	it('conforms to the canonical canvas layout with breadcrumb and status indicator', () => {
		const source = getRouteSource();

		expect(source).toContain('class="agent-detail-route presto-gaui-page"');
		expect(source).toContain('class="crew-detail-breadcrumb-bar"');
		expect(source).toContain('class="crew-detail-back-link"');
		expect(source).toContain('href="/crew"');
		expect(source).toContain('Back to Crew Fleet');
		expect(source).toContain('class="crew-detail-status-pill"');
		expect(source).toContain('class="crew-pulse-dot"');
		expect(source).toContain('max-width: var(--app-content-max, 1360px)');
	});

	it('preserves all five operational detail tabs without removal', () => {
		const source = getRouteSource();

		expect(source).toContain("type DetailTabId = 'overview' | 'memory' | 'episodes' | 'corrections' | 'config'");
		expect(source).toContain("{ id: 'overview', label: 'Overview' }");
		expect(source).toContain("{ id: 'memory', label: 'Memory' }");
		expect(source).toContain("{ id: 'episodes', label: 'History' }");
		expect(source).toContain("{ id: 'corrections', label: 'Corrections' }");
		expect(source).toContain("{ id: 'config', label: 'Settings (YAML)' }");
	});

	it('elevates overview panels into a high-density 2-column responsive dashboard grid', () => {
		const source = getRouteSource();

		expect(source).toContain('class="crew-overview-dashboard-grid"');
		expect(source).toContain('class="crew-overview-column"');
		expect(source).toContain('<CrewMemberOverview overview={crewOverview} idNamespace="crew-route-overview" />');
		expect(source).toContain('<CrewHealthOverview health={crewHealth} loading={crewHealthLoading} />');
		expect(source).toContain('<AgentModelPinsPanel {agentId}');
		expect(source).toContain('<EffectiveToolPolicyPanel {agentId} />');
	});

	it('preserves all Presto GAUI interaction handlers and execution drawer contracts', () => {
		const source = getRouteSource();

		expect(source).toContain("detail.componentId === 'presto-double-detail-action-back'");
		expect(source).toContain("detail.componentId === 'presto-double-detail-action-edit'");
		expect(source).toContain("detail.componentId === 'presto-double-detail-action-execution'");
		expect(source).toContain("detail.componentId === 'presto-double-detail-action-make-primary'");
		expect(source).toContain('toExecutionPanelModel(executionPanelState, null)');
		expect(source).toContain('<TaskPanelDrawer');
	});

	it('enforces horizontal scroll containment and column overflow protections', () => {
		const source = getRouteSource();

		expect(source).toContain('overflow-x: hidden;');
		expect(source).toContain('scrollbar-width: none !important;');
		expect(source).toContain(':global(.agent-detail-route .crew-health)');
		expect(source).toContain(':global(.agent-detail-route .crew-health__body)');
		expect(source).toContain('word-break: break-all !important;');
		expect(source).toContain('overflow-wrap: anywhere !important;');
	});
});
