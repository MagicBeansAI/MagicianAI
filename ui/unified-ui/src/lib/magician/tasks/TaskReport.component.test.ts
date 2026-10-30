import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('./completedTaskReport', () => ({ readCompletedTaskReport: vi.fn(async () => null) }));
import { readCompletedTaskReport } from './completedTaskReport';
import { scopeIdentityStore, setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import TaskReport from './TaskReport.svelte';

beforeEach(() => {
	vi.mocked(readCompletedTaskReport).mockReset().mockResolvedValue(null);
	vi.stubGlobal('IntersectionObserver', undefined);
	vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ agents: [] }), { status: 200 })));
});
afterEach(() => { cleanup(); setCurrentScopeBearerToken(null); scopeIdentityStore.reset(); vi.unstubAllGlobals(); });

const warning = "> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: No observed dispatch.";
const report = '```html\n<h1>Runtime checks</h1><table><thead><tr><th>Check</th><th>Result</th></tr></thead><tbody><tr><td>Loaded definition</td><td>Confirmed</td></tr></tbody></table>\n```';

describe('TaskReport', () => {
	it('replaces a flattened completed-card summary with its formatted source report', async () => {
		scopeIdentityStore.observe('owner', 'space');
		vi.mocked(readCompletedTaskReport).mockResolvedValue('<h1>Completed findings</h1><table><tr><th>Result</th></tr><tr><td>Confirmed</td></tr></table>');
		render(TaskReport, { content: 'Completed findings Result Confirmed', taskId: 'task', executionId: 'run' });
		expect(await screen.findByRole('heading', { name: 'Completed findings' })).toBeVisible();
		expect(screen.getByRole('table')).toBeVisible();
		expect(readCompletedTaskReport).toHaveBeenCalledWith('task', 'run', 'owner', 'space');
	});

	it('clears a mounted source report when credentials rotate within the same scope', async () => {
		scopeIdentityStore.observe('owner', 'space');
		vi.mocked(readCompletedTaskReport).mockResolvedValueOnce('<h1>Earlier login</h1>');
		render(TaskReport, { content: 'Summary', taskId: 'task', executionId: 'run' });
		expect(await screen.findByRole('heading', { name: 'Earlier login' })).toBeVisible();
		let finish!: (value: string) => void;
		vi.mocked(readCompletedTaskReport).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
		setCurrentScopeBearerToken('rotated-fixture-session');
		await waitFor(() => expect(screen.queryByRole('heading', { name: 'Earlier login' })).toBeNull());
		finish('<h1>Current login</h1>');
		expect(await screen.findByRole('heading', { name: 'Current login' })).toBeVisible();
	});

	it('retries a temporarily missing completed report while keeping the summary visible', async () => {
		scopeIdentityStore.observe('owner', 'space');
		vi.mocked(readCompletedTaskReport).mockResolvedValueOnce(null).mockResolvedValueOnce('<h1>Landed report</h1>');
		render(TaskReport, { content: 'Available summary', taskId: 'task', executionId: 'run' });
		expect(screen.getByText('Available summary')).toBeVisible();
		expect(await screen.findByRole('heading', { name: 'Landed report' }, { timeout: 3000 })).toBeVisible();
		expect(readCompletedTaskReport).toHaveBeenCalledTimes(2);
	});
	it('does not apply a late document after the card switches runs', async () => {
		scopeIdentityStore.observe('owner', 'space');
		let finish!: (value: string | null) => void;
		vi.mocked(readCompletedTaskReport).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
		const { rerender } = render(TaskReport, { content: 'Old fallback', taskId: 'task', executionId: 'old-run' });
		await waitFor(() => expect(readCompletedTaskReport).toHaveBeenCalledTimes(1));
		vi.mocked(readCompletedTaskReport).mockResolvedValue('<h1>New run report</h1>');
		await rerender({ content: 'New fallback', taskId: 'task', executionId: 'new-run' });
		expect(await screen.findByRole('heading', { name: 'New run report' })).toBeVisible();
		finish('<h1>Old run report</h1>'); await Promise.resolve();
		expect(screen.queryByRole('heading', { name: 'Old run report' })).toBeNull();
	});
	it('defers source loading until a card becomes visible', async () => {
		scopeIdentityStore.observe('owner', 'space');
		let enter!: IntersectionObserverCallback;
		vi.stubGlobal('IntersectionObserver', class {
			constructor(callback: IntersectionObserverCallback) { enter = callback; }
			observe() {} disconnect() {}
		});
		vi.mocked(readCompletedTaskReport).mockResolvedValue('<h1>Visible report</h1>');
		render(TaskReport, { content: 'Fallback', taskId: 'task', executionId: 'run' });
		expect(readCompletedTaskReport).not.toHaveBeenCalled();
		enter([{ isIntersecting: true }] as IntersectionObserverEntry[], {} as IntersectionObserver);
		expect(await screen.findByRole('heading', { name: 'Visible report' })).toBeVisible();
	});
	it('renders a fenced HTML report and keeps the verification reason behind an info button', async () => {
		const { container } = render(TaskReport, { content: `${warning}\n\n${report}` });
		expect(screen.getByRole('heading', { name: 'Runtime checks' })).toBeVisible();
		expect(screen.getByRole('table')).toBeVisible();
		expect(container.querySelector('pre')).toBeNull();
		expect(container.textContent).not.toContain('Not fully verified');
		expect(screen.queryByText('No observed dispatch.')).toBeNull();
		await fireEvent.click(screen.getByRole('button', { name: 'Not fully verified — view details' }));
		expect(screen.getByRole('dialog', { name: 'Answer verification' })).toHaveTextContent('No observed dispatch.');
	});

	it('retains summary verification when rendering the separate source document', async () => {
		const { container } = render(TaskReport, { content: '<h1>Complete document</h1><p>Useful result.</p>', verificationContent: warning });
		expect(screen.getByRole('heading', {name:'Complete document'})).toBeVisible();
		expect(container.textContent).not.toContain('Not fully verified');
		await fireEvent.click(screen.getByRole('button', {name:'Not fully verified — view details'}));
		expect(screen.getByRole('dialog', {name:'Answer verification'})).toHaveTextContent('No observed dispatch.');
	});

	it.each([false,true])('moves a marked HTML notice behind the info icon (fenced=%s)', async fenced => {
		const html = `<aside data-magician-caveat="unverified" role="note"><p>⚠️ <strong>Not fully verified.</strong> Magician's evidence check could not confirm part of this answer: &lt;img src=x&gt; synthetic reason</p></aside><h1>Actual result</h1><table><tr><th>Result</th></tr><tr><td>Complete</td></tr></table>`;
		const {container} = render(TaskReport,{content:fenced ? '```html\n'+html+'\n```' : html});
		expect(screen.getByRole('heading',{name:'Actual result'})).toBeVisible();
		expect(screen.getByRole('table')).toBeVisible();
		expect(container.textContent).not.toContain('Not fully verified');
		expect(container.querySelector('aside,img')).toBeNull();
		await fireEvent.click(screen.getByRole('button',{name:'Not fully verified — view details'}));
		expect(screen.getByRole('dialog',{name:'Answer verification'})).toHaveTextContent('<img src=x> synthetic reason');
		expect(container.querySelector('img')).toBeNull();
	});

	it('renders a report whose closing fence was cut off by truncation', () => {
		const cut = '```html\n<article><h1>From Leaf to Pearl</h1><p>Tea began in China and spre';
		const { container } = render(TaskReport, { content: `${warning}\n\n${cut}` });
		expect(screen.getByRole('heading', { name: 'From Leaf to Pearl' })).toBeVisible();
		expect(container.querySelector('pre')).toBeNull();
	});

	it('does not strip a second warning label from the actual narrative', () => {
		const literal = '⚠️ Not fully verified. This is the status reported by the source.';
		const {container} = render(TaskReport,{content:warning+'\n\n'+literal});
		expect(container.textContent).toContain('Not fully verified. This is the status reported by the source.');
		expect(screen.getAllByRole('button',{name:'Not fully verified — view details'})).toHaveLength(1);
	});

	it('retains code examples in mixed prose', () => {
		const { container } = render(TaskReport, { content: `Example source:\n\n${report}` });
		expect(container.querySelector('pre code')).toHaveTextContent('<h1>Runtime checks</h1>');
		expect(screen.queryByRole('table')).toBeNull();
	});

	it('uses the existing HTML sanitizer for report documents', () => {
		const { container } = render(TaskReport, { content: '```html\n<h1 onclick="alert(1)">Safe report</h1><script>alert(1)</script><iframe src="https://example.com"></iframe>\n```' });
		expect(screen.getByRole('heading', { name: 'Safe report' })).not.toHaveAttribute('onclick');
		expect(container.querySelector('script,iframe')).toBeNull();
	});

	it('keeps multiple source blocks as code even when the first block contains HTML', () => {
		const { container } = render(TaskReport, { content: `${report}\n\n\`\`\`js\nconsole.log('example');\n\`\`\`` });
		expect(container.querySelectorAll('pre code')).toHaveLength(2);
		expect(screen.queryByRole('table')).toBeNull();
	});
});
