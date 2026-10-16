/**
 * `/hud` sits outside the `(app)` layout, so it inherits neither the prompt
 * modal nor the pending-ask bridge. Without the modal an escalation card's
 * button awaits a host that never mounts; without the bridge the composer's
 * pending pill and the orb badge have no data. The page mounts both itself.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const hud = readFileSync(join(__dirname, '+page.svelte'), 'utf8');

describe('/hud answers HITL prompts', () => {
	it('mounts the prompt modal host', () => {
		expect(hud).toContain(
			"import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';"
		);
		expect(hud).toContain('<AttentionPromptModal />');
	});

	it('opens the pending-ask bridge', () => {
		expect(hud).toContain("import { ensurePendingHitlBridge } from '$lib/stores/pendingHitlStore';");
		expect(hud).toMatch(/if \(browser\) \{[\s\S]*?ensurePendingHitlBridge\(\);/);
	});

	it('does not dismiss the HUD when the modal is clicked', () => {
		const guards = hud.match(/target\?\.closest\('[^']*'\)/g) ?? [];
		expect(guards.length).toBeGreaterThan(0);
		for (const guard of guards) expect(guard).toContain('.muij-modal-backdrop');
	});
});
