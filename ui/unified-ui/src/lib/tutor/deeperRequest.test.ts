import { readFileSync } from 'node:fs';
import { describe, expect, it, vi } from 'vitest';

import {
	buildDeeperPrompt,
	buildDeeperRequest,
	canRequestDeeper,
	postDeeperRequest,
	type DeeperStep
} from './deeperRequest';

const STEP: DeeperStep = {
	revealId: 'why-it-unrolls',
	label: 'Why it unrolls',
	narration: 'The curved side flattens into a sector.'
};

describe('canRequestDeeper', () => {
	it('offers the control for a step that can be named', () => {
		expect(canRequestDeeper(STEP, 'session-1')).toBe(true);
	});

	it('accepts a step with narration but no label, and vice versa', () => {
		expect(canRequestDeeper({ narration: 'Only narration.' }, 'session-1')).toBe(true);
		expect(canRequestDeeper({ label: 'Only a label' }, 'session-1')).toBe(true);
	});

	it('still offers the control when there is no step to name', () => {
		// A learner can want more detail at any moment. The worst case of showing
		// this button is that someone clicks it and gets more explanation, so
		// naming the step optimises the ask rather than gating it.
		expect(canRequestDeeper({ revealId: 'step-1' }, 'session-1')).toBe(true);
		expect(canRequestDeeper(null, 'session-1')).toBe(true);
		expect(canRequestDeeper(undefined, 'session-1')).toBe(true);
	});

	it('declines without a session to send into', () => {
		expect(canRequestDeeper(STEP, null)).toBe(false);
		expect(canRequestDeeper(STEP, '   ')).toBe(false);
	});
});

describe('buildDeeperPrompt', () => {
	it('invokes the tutor rail', () => {
		// The rail is selected by invoke word. Without `@tutor` this is an
		// ordinary chat turn and draws nothing at all.
		expect(buildDeeperPrompt(STEP).startsWith('@tutor ')).toBe(true);
	});

	it('names the one step rather than the lesson', () => {
		const prompt = buildDeeperPrompt(STEP);
		expect(prompt).toContain('"Why it unrolls"');
		expect(prompt).toContain('keep the rest of the lesson as it was');
	});

	it('carries what was already said, so the retry can differ from it', () => {
		// The storyboard contract requires a second explanation to change
		// representation rather than volume; the model can only avoid repeating
		// narration it can actually see.
		expect(buildDeeperPrompt(STEP)).toContain('The curved side flattens into a sector.');
	});

	it('asks for decomposition and drawing, not more words', () => {
		const prompt = buildDeeperPrompt(STEP);
		expect(prompt).toContain('sub-steps');
		expect(prompt).toContain('intermediate stages');
		expect(prompt).toMatch(/rather than restating/);
	});

	it('flattens multi-line narration into one quoted sentence', () => {
		const prompt = buildDeeperPrompt({
			label: 'A\nstep',
			narration: 'Line one.\n\n   Line two.'
		});
		expect(prompt).toContain('"A step"');
		expect(prompt).toContain('"Line one. Line two."');
		expect(prompt).not.toContain('\n\n');
	});

	it('deepens the most recent explanation when no step is named', () => {
		expect(buildDeeperPrompt({ narration: 'Only narration.' })).toContain(
			'go deeper on the part you just explained'
		);
		expect(buildDeeperPrompt({})).toContain('go deeper on the part you just explained');
	});

	it('omits the quoted narration when there is none', () => {
		expect(buildDeeperPrompt({ label: 'Just a label' })).not.toContain('So far you explained');
	});
});

describe('buildDeeperRequest', () => {
	it('produces the session and prompt to POST', () => {
		const request = buildDeeperRequest(STEP, ' session-1 ');
		expect(request?.sessionId).toBe('session-1');
		expect(request?.prompt).toBe(buildDeeperPrompt(STEP));
	});

	it('builds a request even with no step, given a session', () => {
		const request = buildDeeperRequest(null, 'session-1');
		expect(request?.sessionId).toBe('session-1');
		expect(request?.prompt).toContain('the part you just explained');
	});

	it('returns null exactly when the control would be hidden', () => {
		// The guard and the builder must agree; a disagreement would either
		// show a button that does nothing or throw inside a click handler.
		const cases: Array<[DeeperStep | null, string | null]> = [
			[STEP, null],
			[STEP, '  '],
			[null, null]
		];
		for (const [step, session] of cases) {
			expect(canRequestDeeper(step, session)).toBe(false);
			expect(buildDeeperRequest(step, session)).toBeNull();
		}
	});
});

describe('postDeeperRequest', () => {
	it('posts the canonical text field with one tutor invocation', async () => {
		const request = buildDeeperRequest(STEP, 'session-1');
		expect(request).not.toBeNull();
		const fetcher = vi.fn(
			async (_input: RequestInfo | URL, _init?: RequestInit) =>
				new Response('{}', { status: 200 })
		);

		await postDeeperRequest(
			'/api/magician/v2/chat/sessions/session-1/messages',
			request!,
			{ 'Content-Type': 'application/json' },
			fetcher
		);

		expect(fetcher).toHaveBeenCalledOnce();
		const [endpoint, init] = fetcher.mock.calls[0];
		expect(endpoint).toContain('/chat/sessions/session-1/messages');
		expect(endpoint).not.toContain('principal=');
		const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
		expect(body.text).toBe(request!.prompt);
		expect(body).not.toHaveProperty('content');
		expect(body.source_surface).toBe('tutor_draw_overlay');
		expect((String(body.text).match(/@tutor/g) ?? []).length).toBe(1);
	});

	it('turns a non-2xx response into an actionable failure', async () => {
		const request = buildDeeperRequest(STEP, 'session-1');
		const fetcher = vi.fn(
			async (_input: RequestInfo | URL, _init?: RequestInit) =>
				new Response('{"error":"Message text is required"}', { status: 400 })
		);

		await expect(
			postDeeperRequest('/messages', request!, {}, fetcher)
		).rejects.toThrow(/HTTP 400.*Message text is required/);
	});

	it('keeps browser clicks and the desktop event on the same sender', () => {
		const source = readFileSync(
			new URL('../../routes/draw-overlay/+page.svelte', import.meta.url),
			'utf8'
		);
		expect(source).toContain("listen('overlay-explain-deeper-request'");
		expect(source).toMatch(/onclick=\{\(\) => void requestExplainDeeper\(\)\}/);
		expect(source.match(/postDeeperRequest\(/g)).toHaveLength(1);
	});
});
