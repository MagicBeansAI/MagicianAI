import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

import {
	OVERLAY_IDS,
	OVERLAY_PRIORITIES,
	closeAll,
	closeFocused,
	openOverlays,
	requestFocus
} from './overlayCoordinator';

describe('OverlayCoordinator retained parents', () => {
	beforeEach(() => closeAll());

	it('closes the prompt first and leaves the Attention center registered', () => {
		const closeCenter = vi.fn();
		const closePrompt = vi.fn();
		requestFocus({
			id: 'attention-center',
			priority: OVERLAY_PRIORITIES.attentionCenter,
			allowedChildOverlayIds: ['attention-prompt'],
			onClose: closeCenter
		});
		requestFocus({
			id: 'attention-prompt',
			priority: OVERLAY_PRIORITIES.attentionInput,
			onClose: closePrompt
		});

		expect(get(openOverlays).map((entry) => entry.id)).toEqual([
			'attention-center',
			'attention-prompt'
		]);
		closeFocused();
		expect(closePrompt).toHaveBeenCalledTimes(1);
		expect(closeCenter).not.toHaveBeenCalled();
		expect(get(openOverlays).map((entry) => entry.id)).toEqual(['attention-center']);
	});

	it('does not retain a parent under an unrelated higher-priority overlay', () => {
		const closeCenter = vi.fn();
		requestFocus({
			id: 'attention-center',
			priority: OVERLAY_PRIORITIES.attentionCenter,
			allowedChildOverlayIds: ['attention-prompt'],
			onClose: closeCenter
		});
		requestFocus({
			id: 'approval-modal',
			priority: OVERLAY_PRIORITIES.approvalDecision
		});

		expect(closeCenter).toHaveBeenCalledTimes(1);
		expect(get(openOverlays).map((entry) => entry.id)).toEqual(['approval-modal']);
	});

	it('rejects and closes a lower-priority launch beneath the focused modal', () => {
		const closePalette = vi.fn();
		requestFocus({
			id: 'attention-prompt',
			priority: OVERLAY_PRIORITIES.attentionInput
		});

		const granted = requestFocus({
			id: 'command-palette',
			priority: OVERLAY_PRIORITIES.commandPalette,
			onClose: closePalette
		});

		expect(granted).toBe(false);
		expect(closePalette).toHaveBeenCalledTimes(1);
		expect(get(openOverlays).map((entry) => entry.id)).toEqual(['attention-prompt']);
	});

	it('replaces a channel child with a prompt while retaining their explicit parent', () => {
		const closeCenter = vi.fn();
		const closeChannelChild = vi.fn();
		requestFocus({
			id: OVERLAY_IDS.attentionCenter,
			priority: OVERLAY_PRIORITIES.attentionCenter,
			allowedChildOverlayIds: [
				OVERLAY_IDS.attentionChannelChild,
				OVERLAY_IDS.attentionPrompt
			],
			onClose: closeCenter
		});
		requestFocus({
			id: OVERLAY_IDS.attentionChannelChild,
			priority: OVERLAY_PRIORITIES.attentionInput,
			onClose: closeChannelChild
		});

		requestFocus({
			id: OVERLAY_IDS.attentionPrompt,
			priority: OVERLAY_PRIORITIES.attentionInput
		});

		expect(closeChannelChild).toHaveBeenCalledTimes(1);
		expect(closeCenter).not.toHaveBeenCalled();
		expect(get(openOverlays).map((entry) => entry.id)).toEqual([
			OVERLAY_IDS.attentionCenter,
			OVERLAY_IDS.attentionPrompt
		]);
	});

	it('keeps re-registering the same overlay id idempotent', () => {
		const closeOriginal = vi.fn();
		const closeReplacement = vi.fn();
		requestFocus({
			id: 'page-modal',
			priority: OVERLAY_PRIORITIES.pageModal,
			onClose: closeOriginal
		});

		const granted = requestFocus({
			id: 'page-modal',
			priority: OVERLAY_PRIORITIES.pageModal,
			onClose: closeReplacement
		});

		expect(granted).toBe(true);
		expect(get(openOverlays)).toHaveLength(1);
		closeFocused();

		expect(closeOriginal).toHaveBeenCalledTimes(1);
		expect(closeReplacement).not.toHaveBeenCalled();
	});
});
