import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

// The feed now lives as the Square → Social tab; the contract follows the component.
const source = readFileSync(join(process.cwd(), 'src/lib/townSquare/TownSquareSocial.svelte'), 'utf8');

describe('Town Square social transport contract', () => {
	it('uses the scoped v2 API and never the stale v3 route', () => {
		expect(source).toContain("const SOCIAL_API = '/api/magician/v2/social'");
		expect(source).not.toContain('/api/magician/v3/social');
	});

	it('surfaces backend health and errors instead of rendering failures as an empty feed', () => {
		expect(source).toContain("fetchJson<SocialHealth>('/health'");
		expect(source).toContain('autonomous_scope_enabled');
		expect(source).toContain('scope_policy');
		expect(source).toContain('worker_global');
		expect(source).toContain('not configured for this workspace');
		expect(source).toContain('Town Square could not load.');
		expect(source).toContain('role="alert"');
		expect(source).toContain('!loadError && posts.length === 0');
	});

	it('owns a persistent operator chatter switch that defaults off', () => {
		expect(source).toContain("fetchJson<OperatorPolicy>('/policy'");
		expect(source).toContain("JSON.stringify({ autonomous_enabled: enabled })");
		expect(source).toContain('let autonomousEnabled = false');
		expect(source).toContain('shouldApplyOperatorPolicy');
		expect(source).toContain('label="Agent chatter"');
		expect(source).toContain('Turn on Agent chatter to invite idle agents.');
	});

	it('owns one non-overlapping cancellable refresh loop', () => {
		expect(source).toContain('while (projectionInFlight)');
		expect(source).toContain('if (!waitForCurrent) return;');
		expect(source).toContain('projectionController?.abort();');
		expect(source).toContain('}, 30000);');
	});

	it('supports durable agent mentions and parent-bound replies', () => {
		expect(source).toContain('function tagAgent(memberId: string)');
		expect(source).toContain('function beginReply(post: PostWithReactions)');
		expect(source).toContain("post_type: replyingTo ? 'reply' : 'thought'");
		expect(source).toContain('parent_id: replyingTo?.post_id ?? null');
		expect(source).toContain('mentioned_member_ids: requestedMemberIds');
		expect(source).toContain('function exactMentionIndex(body: string, memberId: string)');
		expect(source).toContain('if (exactMentionIndex(composingBody, memberId) < 0)');
		expect(source).toContain('.sort((left, right) => left.index - right.index)');
		expect(source).toContain('known.index === typed.index');
		expect(source).toContain('Load older posts');
		expect(source).toContain('@{m.member.member_id}');
		expect(source).toContain('watchAppLiveCollection(session.collection, installationId,');
		expect(source).toContain('post-shell--reply');
	});
});
