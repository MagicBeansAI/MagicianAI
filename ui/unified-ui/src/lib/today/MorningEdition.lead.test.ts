/**
 * The Morning Edition's lead story is one urgent decision, so its action must
 * open that decision — not the Attention list the reader would then have to
 * search for it in.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = readFileSync(join(__dirname, 'MorningEdition.svelte'), 'utf8');
const launcher = readFileSync(join(__dirname, '../attention/routeLauncher.ts'), 'utf8');

describe('MorningEdition lead story', () => {
	it('opens the lead item, not the Attention list', () => {
		expect(source).toContain('on:click={() => void openLeadItem(leadItem)}');
		const lead = source.match(/async function openLeadItem[\s\S]*?\n\t}\n/)?.[0] ?? '';
		expect(lead).toContain('todayHitlOpenTarget(item)');
		expect(lead).toContain('openHitlPrompt(target)');
		expect(lead).toContain('openResolvedAttention(item.source_url, todayAttentionItemId(item))');

		const resolved = launcher.match(/function openResolvedAttention[\s\S]*?\n}\n/)?.[0] ?? '';
		expect(resolved).toContain('if (openAttentionRoute(route, itemId)) return;');
		expect(resolved).toContain('if (itemId) openAttentionItem(itemId);');
		expect(resolved).toContain('else openAttentionCenter();');
	});
});
