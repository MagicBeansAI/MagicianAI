import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

describe('Storage governance visual contract', () => {
	it('uses shared theme tokens and a mobile reflow instead of fixed palette/layout values', () => {
		const source = readFileSync(
			join(process.cwd(), 'src/lib/storage/StorageGovernancePanel.svelte'),
			'utf8'
		);
		expect(source).toContain('var(--bg-card)');
		expect(source).toContain('var(--text-primary)');
		expect(source).toContain('var(--accent-primary)');
		expect(source).toContain('@media(max-width:800px)');
		expect(source).toContain('grid-template-columns:1fr');
	});

	it('is reachable from both Settings and the global command palette', () => {
		const settings = readFileSync(
			join(process.cwd(), 'src/routes/(app)/settings/+page.svelte'),
			'utf8'
		);
		const palette = readFileSync(
			join(process.cwd(), 'src/lib/shell/CommandPalette.svelte'),
			'utf8'
		);
		expect(settings).toContain('href="/storage"');
		expect(palette).toContain("goto('/storage')");
		expect(palette).toContain("goto('/storage#activation')");
	});
});
