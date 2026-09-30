import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

function source(path: string): string {
	return readFileSync(resolve(process.cwd(), path), 'utf8');
}

describe('canonical crew overview reuse', () => {
	it('adopts the shared overview without moving route-owned detail behavior', () => {
		const route = source('src/routes/(app)/crew/[id]/+page.svelte');

		expect(route).toContain("import CrewMemberOverview from '$lib/magician/crew/CrewMemberOverview.svelte'");
		expect(route).toContain('components.push(buildCrewMemberOverviewComponent({');
		expect(route).toContain('<CrewMemberOverview overview={crewOverview}');
		expect(route).toContain("} else if (input.selectedTab === 'memory') {");
		expect(route).toContain('async function hydrateAgent(nextAgentId: string)');
		expect(route).toContain('await goto(`/crew/${encodeURIComponent(agentId)}?tab=');
	});

	it('mounts the canonical overview in Town Square only after the detail command', () => {
		const dock = source('src/lib/magician/square/hud/CommandDock.svelte');
		const overlay = source('src/lib/magician/crew/CrewMemberOverviewOverlay.svelte');

		expect(dock).toContain("crewDetailModule = import('$lib/magician/crew/CrewMemberOverviewOverlay.svelte')");
		expect(dock).toContain('on:detail={() => openCrewDetail(selectedCitizen.id)}');
		expect(dock).not.toMatch(/^\s*import\s+CrewMemberOverviewOverlay\b/m);
		expect(overlay).toContain("import CrewMemberOverview from './CrewMemberOverview.svelte'");
		expect(overlay).toContain('await loadAgent(requestedAgentId)');
		expect(overlay).toContain('Open full crew record');
		expect(overlay).toContain('href={`/crew/${encodeURIComponent(agentId)}`}');
	});

	it('keeps the citizen inspector limited to immediate command context', () => {
		const inspector = source('src/lib/magician/square/hud/CitizenDetail.svelte');

		expect(inspector).toContain('$: highestPriorityNeed = needsItems[0] ?? null;');
		expect(inspector).toContain("dispatch('detail')");
		for (const fullRecordField of [
			'citizen.role',
			'citizen.quests',
			'citizen.tools',
			'citizen.delegationTargets',
			'citizen.health',
			'citizen.successRate7d',
			'citizen.spendUsd7d',
			'citizen.trustLevel',
			'citizen.bornAt'
		]) {
			expect(inspector).not.toContain(fullRecordField);
		}
	});

	it('reuses the canonical leaderboard in Crew and Town Square without a scroll-body copy', () => {
		const crewRoute = source('src/routes/(app)/crew/+page.svelte');
		const squareRoute = source('src/routes/(app)/square/+page.svelte');
		const gameSheet = source('src/lib/magician/square/hud/CrewBoardSheet.svelte');

		expect(crewRoute).toContain("import CrewLeaderboard from '$lib/magician/crew/CrewLeaderboard.svelte'");
		expect(crewRoute).toContain('<CrewLeaderboard');
		expect(gameSheet).toContain("import CrewLeaderboard from '$lib/magician/crew/CrewLeaderboard.svelte'");
		expect(gameSheet).toContain('crewLeaderboardRowsFromCitizens(citizens)');
		expect(squareRoute).not.toContain("import CrewBoard from '$lib/magician/square/CrewBoard.svelte'");
		expect(squareRoute).not.toContain('<CrewBoard');
	});
});
