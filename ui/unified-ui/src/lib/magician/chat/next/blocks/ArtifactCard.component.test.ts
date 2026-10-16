import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { StructuredArtifactsBlockV1 } from '$lib/magician/structuredResponse/types';
import ArtifactCard from './ArtifactCard.svelte';

afterEach(() => cleanup());

const block = (items: StructuredArtifactsBlockV1['items']): StructuredArtifactsBlockV1 => ({
	kind: 'artifacts',
	title: 'Artifacts',
	items
});

describe('ArtifactCard', () => {
	it('renders an image artifact as a lazy thumbnail with alt text', () => {
		render(ArtifactCard, {
			block: block([{ label: 'Chart', href: 'https://example.com/chart.png', mime_type: 'image/png' }])
		});

		const img = screen.getByRole('img', { name: 'Chart' });
		expect(img).toHaveAttribute('src', 'https://example.com/chart.png');
		expect(img).toHaveAttribute('loading', 'lazy');
	});

	it('renders a pdf artifact with a type glyph and human size', () => {
		render(ArtifactCard, {
			block: block([
				{ label: 'report.pdf', artifact_id: 'art-1', mime_type: 'application/pdf', size: 1_258_291 }
			])
		});

		expect(document.querySelector('[data-glyph="pdf"]')).not.toBeNull();
		expect(screen.getByText('report.pdf')).toBeInTheDocument();
		expect(screen.getByText(/1\.2 MB/)).toBeInTheDocument();
	});

	it('picks glyphs from the extension when the mime type is missing', () => {
		render(ArtifactCard, {
			block: block([
				{ label: 'data.xlsx', href: 'https://example.com/data.xlsx' },
				{ label: 'bundle.zip', href: 'https://example.com/bundle.zip' },
				{ label: 'main.rs', href: 'https://example.com/main.rs' },
				{ label: 'notes', href: 'https://example.com/notes' }
			])
		});

		expect(document.querySelector('[data-glyph="table"]')).not.toBeNull();
		expect(document.querySelector('[data-glyph="zip"]')).not.toBeNull();
		expect(document.querySelector('[data-glyph="code"]')).not.toBeNull();
		expect(document.querySelector('[data-glyph="file"]')).not.toBeNull();
	});

	it('renders video and audio players', () => {
		render(ArtifactCard, {
			block: block([
				{ label: 'Clip', href: 'https://example.com/clip.mp4', mime_type: 'video/mp4' },
				{ label: 'Memo', href: 'https://example.com/memo.mp3', mime_type: 'audio/mpeg' }
			])
		});

		const video = document.querySelector('video');
		expect(video).not.toBeNull();
		expect(video).toHaveAttribute('controls');
		expect(video).toHaveAttribute('preload', 'metadata');
		expect(document.querySelector('audio')).toHaveAttribute('controls');
	});

	it('opens a lightbox dialog from the thumbnail and closes it on Escape', async () => {
		render(ArtifactCard, {
			block: block([{ label: 'Chart', href: 'https://example.com/chart.png', mime_type: 'image/png' }])
		});

		await fireEvent.click(screen.getByRole('button', { name: 'View Chart' }));
		const dialog = await screen.findByRole('dialog');
		expect(dialog).toHaveAttribute('aria-modal', 'true');

		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
	});

	it('routes open through onAction like the classic block', async () => {
		const onAction = vi.fn();
		render(ArtifactCard, {
			block: block([{ label: 'report.pdf', artifact_id: 'art-1', mime_type: 'application/pdf' }]),
			onAction
		});

		await fireEvent.click(screen.getByRole('button', { name: /Open report\.pdf/ }));

		expect(onAction).toHaveBeenCalledWith({
			kind: 'open_artifact',
			label: 'report.pdf',
			artifact_id: 'art-1'
		});
	});

	it('does not inline media from an unsafe href', () => {
		render(ArtifactCard, {
			block: block([{ label: 'Bad', href: 'javascript:alert(1)', mime_type: 'image/png' }])
		});

		expect(screen.queryByRole('img', { name: 'Bad' })).not.toBeInTheDocument();
		expect(screen.getByText('Bad')).toBeInTheDocument();
	});
});
