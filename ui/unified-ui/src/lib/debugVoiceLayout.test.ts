import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const source = readFileSync(
	new URL('../routes/(app)/debug/voice/+page.svelte', import.meta.url),
	'utf8'
);

describe('voice debug active-session layout', () => {
	it('keeps the status pill inside narrow session cards', () => {
		expect(source).toMatch(
			/\.voice-debug\s*\{[^}]*width:\s*100%;[^}]*max-width:\s*1320px;[^}]*min-width:\s*0;/s
		);
		expect(source).not.toContain('width: min(1320px, calc(100vw - 32px));');
		expect(source).toContain(
			'grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));'
		);
		expect(source).toMatch(/\.session-card\s*\{[^}]*box-sizing:\s*border-box;[^}]*min-width:\s*0;/s);
		expect(source).not.toMatch(/\.session-card\s*\{[^}]*overflow:\s*hidden;/s);
		expect(source).toMatch(
			/\.session-top\s*\{[^}]*display:\s*grid;[^}]*grid-template-columns:\s*minmax\(0, 1fr\) max-content;/s
		);
		expect(source).toMatch(/\.session-top > div\s*\{[^}]*min-width:\s*0;/s);
		expect(source).toMatch(
			/\.status\s*\{[^}]*width:\s*max-content;[^}]*justify-self:\s*end;[^}]*white-space:\s*nowrap;/s
		);
	});
});
