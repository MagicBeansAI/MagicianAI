import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ContentBlockRecord } from '$lib/stores/chatStore';
import ChatContentBlocks from './ChatContentBlocks.svelte';

const openAuthenticatedTaskOutput = vi.hoisted(() => vi.fn(async () => undefined));
const downloadAuthenticatedTaskOutput = vi.hoisted(() => vi.fn(async () => undefined));
const authenticatedTaskOutputImage = vi.hoisted(() =>
	vi.fn(() => ({ update: vi.fn(), destroy: vi.fn() }))
);

vi.mock('$lib/magician/tasks/taskOutputs', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskOutputs')>()),
	openAuthenticatedTaskOutput,
	downloadAuthenticatedTaskOutput,
	authenticatedTaskOutputImage
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('ChatContentBlocks protected outputs', () => {
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

		await fireEvent.click(screen.getByRole('button', { name: /result\.bin/i }));
		expect(openAuthenticatedTaskOutput).toHaveBeenCalledWith(
			'/api/magician/v2/chat/sessions/session%2Fone/outputs/archive/result.bin'
		);
		expect(container.querySelector('a[href^="/api/magician/"]')).toBeNull();
	});
});
