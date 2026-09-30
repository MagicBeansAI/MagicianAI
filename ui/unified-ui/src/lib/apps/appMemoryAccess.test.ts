import { describe, expect, it } from 'vitest';
import { parseAppMemoryAccess } from './appMemoryAccess';
import {
	defaultGrantedNames,
	toggleMemoryGrant,
	type AppInstallationReview,
	type AppReviewedMemoryRead
} from './installationReview';

const memory: AppReviewedMemoryRead = {
	request: { user_tiers: ['preferences', 'identity'], agents: ['scribe'], purpose: 'Personalise' },
	request_digest: 'blake3:req',
	sensitive_tiers: ['identity'],
	default_grant: {
		schema: 'magician.app-memory-read-grant.v1',
		request_digest: 'blake3:req',
		engagement: 'owner_only',
		interactive: { user_tiers: ['preferences'], agents: ['scribe'] },
		background: { user_tiers: [], agents: [] }
	}
};

function reviewWith(memoryRead?: AppReviewedMemoryRead): AppInstallationReview {
	return {
		workflow_material_digest: 'blake3:wf',
		requested_tools: [],
		requested_agents: [],
		requested_personalities: [],
		...(memoryRead ? { requested_memory_read: memoryRead } : {})
	} as unknown as AppInstallationReview;
}

describe('memory grant at install review', () => {
	it('starts from the reviewed default: sensitive unticked, background empty', () => {
		const grant = defaultGrantedNames(reviewWith(memory)).granted_memory_read;
		expect(grant?.reviewed_request_digest).toBe('blake3:req');
		expect(grant?.interactive).toEqual({ user_tiers: ['preferences'], agents: ['scribe'] });
		expect(grant?.background).toEqual({ user_tiers: [], agents: [] });
	});

	it('omits the memory choice for apps that request no memory', () => {
		expect(defaultGrantedNames(reviewWith()).granted_memory_read).toBeUndefined();
	});

	it('toggles one run mode without touching the other', () => {
		const start = defaultGrantedNames(reviewWith(memory));
		const ticked = toggleMemoryGrant(start, 'background', 'user_tiers', 'preferences');
		expect(ticked.granted_memory_read?.background.user_tiers).toEqual(['preferences']);
		expect(ticked.granted_memory_read?.interactive.user_tiers).toEqual(['preferences']);
		const unticked = toggleMemoryGrant(ticked, 'interactive', 'agents', 'scribe');
		expect(unticked.granted_memory_read?.interactive.agents).toEqual([]);
		expect(unticked.granted_memory_read?.background.agents).toEqual([]);
	});
});

describe('parseAppMemoryAccess', () => {
	const body = {
		installation_id: 'install_1',
		request: memory.request,
		reviewed: memory.default_grant,
		effective: memory.default_grant,
		edit_revision: 2,
		installation_enabled: true,
		user_tier_catalog: [
			{ name: 'preferences', readability: 'ordinary' },
			{ name: 'identity', readability: 'sensitive' }
		]
	};

	it('parses a well-formed response', () => {
		const access = parseAppMemoryAccess(body);
		expect(access.effective?.interactive.agents).toEqual(['scribe']);
		expect(access.edit_revision).toBe(2);
		expect(access.user_tier_catalog).toHaveLength(2);
	});

	it('treats a null effective grant as no access', () => {
		expect(parseAppMemoryAccess({ ...body, effective: null }).effective).toBeNull();
	});

	it('rejects a grant it does not fully understand', () => {
		expect(() =>
			parseAppMemoryAccess({ ...body, effective: { ...memory.default_grant, engagement: 'any' } })
		).toThrow();
		expect(() => parseAppMemoryAccess({ ...body, edit_revision: '2' })).toThrow();
	});
});
