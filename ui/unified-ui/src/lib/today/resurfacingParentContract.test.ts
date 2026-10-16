import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const parentPages = [
	['Square', new URL('../../routes/(app)/square/+page.svelte', import.meta.url)]
] as const;

function between(source: string, start: string, end: string): string {
	const startIndex = source.indexOf(start);
	const endIndex = source.indexOf(end, startIndex + start.length);
	expect(startIndex).toBeGreaterThanOrEqual(0);
	expect(endIndex).toBeGreaterThan(startIndex);
	return source.slice(startIndex, endIndex);
}

describe.each(parentPages)('%s Worth a look parent contract', (_name, pageUrl) => {
	it('passes an existing scoped owner chat thread without a general fallback', () => {
		const source = readFileSync(pageUrl, 'utf8');
		expect(source).toContain('canonicalResurfacingChatThread($threadStore.threads)');
		expect(source).toContain('chatThreadId={resurfacingChatThreadId}');
		expect(source).toContain('threadStore.start()');
		expect(source).toContain('threadStore.stop()');
		expect(source).not.toContain('chatThreadId="general"');
	});

	it('keeps routine Follow-up and canonical feedback quiet while preserving errors', () => {
		const source = readFileSync(pageUrl, 'utf8');
		const followUpSuccess = between(
			source,
			'function onTodayChannelFollowUpResolved',
			'function onTodayChannelFollowUpFailed'
		);
		const canonicalSuccess = between(
			source,
			'function onCanonicalAttentionResolved',
			'function onCanonicalAttentionFailed'
		);
		const followUpFailure = between(
			source,
			'function onTodayChannelFollowUpFailed',
			'function onTodayChannelFollowUpGroupChanged'
		);
		const groupSuccess = between(
			source,
			'function onTodayChannelFollowUpGroupChanged',
			'async function loadResurfacingCount'
		);
		const canonicalFailure = between(
			source,
			'function onCanonicalAttentionFailed',
			'$: todaySections'
		);

		expect(followUpSuccess).not.toContain('showSuccess(');
		expect(groupSuccess).not.toContain('showSuccess(');
		expect(canonicalSuccess).not.toContain('showSuccess(');
		expect(followUpFailure).toContain('showError(');
		expect(canonicalFailure).toContain('showError(');
	});
});
