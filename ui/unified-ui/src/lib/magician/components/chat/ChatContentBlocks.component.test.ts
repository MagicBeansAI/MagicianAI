import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ContentBlockRecord } from '$lib/stores/chatStore';
import ChatContentBlocks from './ChatContentBlocks.svelte';

const openAuthenticatedTaskOutput = vi.hoisted(() => vi.fn(async () => undefined));
const downloadAuthenticatedTaskOutput = vi.hoisted(() => vi.fn(async () => undefined));
const authenticatedTaskOutputImage = vi.hoisted(() =>
	vi.fn(() => ({ update: vi.fn(), destroy: vi.fn() }))
);
const authenticatedTaskOutputPdf = vi.hoisted(() =>
	vi.fn(() => ({ update: vi.fn(), destroy: vi.fn() }))
);

vi.mock('$lib/magician/tasks/taskOutputs', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskOutputs')>()),
	openAuthenticatedTaskOutput,
	downloadAuthenticatedTaskOutput,
	authenticatedTaskOutputImage,
	authenticatedTaskOutputPdf
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
});

describe('ChatContentBlocks protected outputs', () => {
	it('formats an inline text report and discloses its verification warning through an info button', async () => {
		const text = '> ⚠️ **Not fully verified.**\n\n```html\n<h1>Runtime checks</h1><table><tr><th>Result</th></tr><tr><td>UNAVAILABLE</td></tr></table>\n```';
		const { container } = render(ChatContentBlocks, { props: { blocks: [{ type: 'text', text }] } });
		expect(screen.getByRole('heading', { name: 'Runtime checks' })).toBeVisible();
		expect(screen.getByRole('table')).toBeVisible();
		expect(container.querySelector('pre,blockquote')).toBeNull();
		expect(container.textContent).not.toContain('Not fully verified');
		await fireEvent.click(screen.getByRole('button', { name: 'Not fully verified — view details' }));
		expect(screen.getByRole('dialog', { name: 'Answer verification' })).toBeVisible();
	});

	it('keeps separate MP3 and WAV download actions for long task artifact names', async () => {
		const names = ['mp3', 'wav'].map((extension) =>
			`shell_0_creative-mind_task_${'19a5f59f04684e7d8f62d9c0cb6c28ba'.repeat(3)}_exec_8b170eceb144455ba27ea0aad3c75909.${extension}`
		);
		const blocks: ContentBlockRecord[] = names.map((name, index) => ({
			type: 'file',
			source: { type: 'task_output', task_id: 'task-medley' },
			relative_path: `outputs/${name}`,
			display_name: name,
			mime_type: index === 0 ? 'audio/mpeg' : 'audio/wav'
		}));
		render(ChatContentBlocks, { props: { blocks } });
		const buttons = screen.getAllByRole('button', { name: 'Download' });
		expect(buttons).toHaveLength(2);
		for (let index = 0; index < buttons.length; index++) {
			// Long names are middle-ellipsized (head + tail spans); the full name
			// lives on the element's title.
			expect(screen.getByTitle(names[index])).toBeVisible();
			await fireEvent.click(buttons[index]);
			expect(downloadAuthenticatedTaskOutput).toHaveBeenLastCalledWith(
				`/api/magician/v3/tasks/task-medley/outputs/${names[index]}`, names[index]
			);
		}
	});
	it('opens and downloads task output bytes through authenticated helpers', async () => {
		const blocks: ContentBlockRecord[] = [
			{
				type: 'file',
				source: { type: 'task_output', task_id: 'task/alpha' },
				relative_path: 'reports/final report.pdf',
				display_name: 'final-report.pdf',
				mime_type: 'application/pdf'
			},
			{
				type: 'file',
				source: { type: 'task_output', task_id: 'task/alpha' },
				relative_path: 'images/chart.png',
				display_name: 'chart.png',
				mime_type: 'image/png'
			}
		];

		const { container } = render(ChatContentBlocks, { props: { blocks } });
		const outputUrl = '/api/magician/v3/tasks/task%2Falpha/outputs/reports/final%20report.pdf';
		const imageUrl = '/api/magician/v3/tasks/task%2Falpha/outputs/images/chart.png';

		await fireEvent.click(screen.getByRole('button', { name: 'Open ↗' }));
		expect(openAuthenticatedTaskOutput).toHaveBeenCalledWith(outputUrl);

		await fireEvent.click(screen.getByRole('button', { name: 'Download' }));
		expect(downloadAuthenticatedTaskOutput).toHaveBeenCalledWith(outputUrl, 'final-report.pdf');

		expect(authenticatedTaskOutputImage).toHaveBeenCalledWith(
			expect.any(HTMLImageElement),
			imageUrl
		);
		expect(container.querySelector('a[href^="/api/magician/"]')).toBeNull();
	});

	it('opens session output cards through the same authenticated path', async () => {
		const blocks: ContentBlockRecord[] = [
			{
				type: 'file',
				source: { type: 'session_output' },
				relative_path: 'outputs/archive/result.bin',
				display_name: 'result.bin',
				mime_type: 'application/octet-stream'
			}
		];

		const { container } = render(ChatContentBlocks, {
			props: { sessionId: 'session/one', blocks }
		});

		await fireEvent.click(screen.getByRole('button', { name: /open file/i }));
		expect(openAuthenticatedTaskOutput).toHaveBeenCalledWith(
			'/api/magician/v2/chat/sessions/session%2Fone/outputs/archive/result.bin'
		);
		expect(container.querySelector('a[href^="/api/magician/"]')).toBeNull();
	});
});

describe('ChatContentBlocks previews by default', () => {
	// The shared test polyfill's IntersectionObserver never fires; this one
	// reports every observed node as on screen, like a block in view.
	beforeEach(() => {
		vi.stubGlobal(
			'IntersectionObserver',
			class {
				constructor(private readonly callback: IntersectionObserverCallback) {}
				observe(target: Element): void {
					this.callback(
						[{ isIntersecting: true, target } as IntersectionObserverEntry],
						this as unknown as IntersectionObserver
					);
				}
				unobserve(): void {}
				disconnect(): void {}
				takeRecords(): IntersectionObserverEntry[] {
					return [];
				}
			}
		);
	});

	function taskFile(name: string, mime: string, extra: Partial<ContentBlockRecord> = {}): ContentBlockRecord {
		return {
			type: 'file',
			source: { type: 'task_output', task_id: 'task-1' },
			relative_path: name,
			display_name: name,
			mime_type: mime,
			...extra
		} as ContentBlockRecord;
	}

	it('renders an image preview without a click', () => {
		const { container } = render(ChatContentBlocks, {
			props: { blocks: [taskFile('chart.png', 'image/png')] }
		});
		expect(container.querySelector('img.chat-rich-image')).not.toBeNull();
	});

	it('renders a PDF preview frame without a click, through the authenticated action', () => {
		render(ChatContentBlocks, { props: { blocks: [taskFile('report.pdf', 'application/pdf')] } });
		const frame = screen.getByTitle('Preview of report.pdf');
		expect(frame.tagName).toBe('IFRAME');
		expect(authenticatedTaskOutputPdf).toHaveBeenCalledWith(
			frame,
			'/api/magician/v3/tasks/task-1/outputs/report.pdf'
		);
		// The Open card and its actions stay.
		expect(screen.getByRole('button', { name: 'Open ↗' })).toBeInTheDocument();
	});

	it('keeps the Open card only for a PDF over the inline cap', () => {
		render(ChatContentBlocks, {
			props: { blocks: [taskFile('huge.pdf', 'application/pdf', { size: 50 * 1024 * 1024 })] }
		});
		expect(screen.queryByTitle('Preview of huge.pdf')).toBeNull();
		expect(screen.getByRole('button', { name: 'Open ↗' })).toBeInTheDocument();
	});

	it('loads a text preview without a click and can hide it', async () => {
		const fetchMock = vi.fn(async () => new Response('hello from the notes', { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		render(ChatContentBlocks, { props: { blocks: [taskFile('notes.txt', 'text/plain')] } });
		expect(await screen.findByText('hello from the notes')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Show preview' })).toBeNull();

		await fireEvent.click(screen.getByRole('button', { name: 'Hide preview' }));
		expect(screen.queryByText('hello from the notes')).toBeNull();
		expect(screen.getByRole('button', { name: 'Show preview' })).toBeInTheDocument();
	});

	it('formats a task report preview and places its verification disclosure beside the filename', async () => {
		const warning = "> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: No observed dispatch.";
		const report = '```html\n<h1>Runtime checks</h1><table><tr><th>Check</th><th>Result</th></tr><tr><td>Definition</td><td>UNAVAILABLE</td></tr></table>\n```';
		vi.stubGlobal('fetch', vi.fn(async () => new Response(`${warning}\n\n${report}`, { status: 200 })));
		const { container } = render(ChatContentBlocks, { props: { blocks: [taskFile('report.md', 'text/markdown')] } });
		expect(await screen.findByRole('heading', { name: 'Runtime checks' })).toBeVisible();
		expect(screen.getByRole('table')).toBeVisible();
		expect(container.querySelector('pre,blockquote')).toBeNull();
		expect(container.textContent).not.toContain('Not fully verified');
		const info = screen.getByRole('button', { name: 'Not fully verified — view details' });
		expect(info.closest('.chat-rich-text-preview-title')).toHaveTextContent('report.md');
		await fireEvent.click(info);
		expect(screen.getByRole('dialog', { name: 'Answer verification' })).toHaveTextContent('No observed dispatch.');
	});

	it('waits for the block to scroll into view before fetching', () => {
		vi.stubGlobal(
			'IntersectionObserver',
			class {
				observe(): void {}
				unobserve(): void {}
				disconnect(): void {}
				takeRecords(): IntersectionObserverEntry[] {
					return [];
				}
			}
		);
		const fetchMock = vi.fn(async () => new Response('x', { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		render(ChatContentBlocks, {
			props: { blocks: [taskFile('notes.txt', 'text/plain'), taskFile('report.pdf', 'application/pdf')] }
		});
		expect(fetchMock).not.toHaveBeenCalled();
		expect(screen.queryByTitle('Preview of report.pdf')).toBeNull();
		expect(authenticatedTaskOutputPdf).not.toHaveBeenCalled();
	});

	it('does not fetch a text file over the inline cap', async () => {
		const fetchMock = vi.fn(async () => new Response('x', { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		render(ChatContentBlocks, {
			props: { blocks: [taskFile('big.log', 'text/plain', { size: 10 * 1024 * 1024 })] }
		});
		expect(await screen.findByText(/File too large for inline preview/)).toBeInTheDocument();
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('gives Open, Download, Open in app and Reveal in Finder the shared action chip', () => {
		render(ChatContentBlocks, { props: { blocks: [taskFile('report.pdf', 'application/pdf')] } });
		for (const name of ['Open ↗', 'Download', 'Open in app', 'Reveal in Finder']) {
			expect(screen.getByRole('button', { name })).toHaveClass('mg-chip-action');
		}
	});
});
