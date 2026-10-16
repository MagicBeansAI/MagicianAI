import { fireEvent, render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import FloatingComposerHarness from '../../test/fixtures/FloatingComposerHarness.svelte';

describe('FloatingComposer send affordance', () => {
	it('hides the send control while the composer is empty', () => {
		render(FloatingComposerHarness);
		expect(screen.queryByRole('button', { name: 'Send message' })).toBeNull();
	});

	it('reveals the send control as soon as a draft is typed, and hides it again when cleared', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness);
		const textbox = screen.getByRole('textbox');

		await user.type(textbox, 'Hi');
		expect(screen.getByRole('button', { name: 'Send message' })).toBeInTheDocument();

		await user.clear(textbox);
		expect(screen.queryByRole('button', { name: 'Send message' })).toBeNull();
	});

	it('treats whitespace as empty so a stray space cannot reveal send', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness);

		await user.type(screen.getByRole('textbox'), '   ');
		expect(screen.queryByRole('button', { name: 'Send message' })).toBeNull();
	});

	it('shows send with no text when the host reports staged content (attachment-only turn)', () => {
		render(FloatingComposerHarness, { props: { hasContent: true } });
		expect(screen.getByRole('button', { name: 'Send message' })).toBeInTheDocument();
	});

	it('keeps the stop control available mid-turn even with an empty composer', () => {
		render(FloatingComposerHarness, { props: { isSending: true } });
		expect(screen.getByRole('button', { name: 'Stop generating' })).toBeInTheDocument();
	});
});

describe('FloatingComposer interactions', () => {
	it('keeps the typed draft bound and sends it from the visible send control', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness);

		await user.type(screen.getByRole('textbox'), 'Summarize the launch notes');
		expect(screen.getByTestId('composer-value')).toHaveTextContent(
			'Summarize the launch notes'
		);
		await user.click(screen.getByRole('button', { name: 'Send message' }));
		expect(screen.getByTestId('composer-action')).toHaveTextContent(
			'send:Summarize the launch notes'
		);
	});

	it('submits on Enter while Shift+Enter remains an editable newline', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness);
		const textbox = screen.getByRole('textbox');

		await user.type(textbox, 'line one{Shift>}{Enter}{/Shift}line two');
		expect(screen.getByTestId('composer-value')).toHaveTextContent('line one line two');
		expect(screen.getByTestId('composer-action')).toHaveTextContent('');
		await user.keyboard('{Enter}');
		expect(screen.getByTestId('composer-action')).toHaveTextContent(
			'send:line one line two'
		);
	});

	it('switches between Do and Plan and explains plan mode', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness);

		await user.click(screen.getByRole('button', { name: 'Plan' }));
		expect(screen.getByTestId('composer-mode')).toHaveTextContent('plan');
		expect(screen.getByText('Plan mode')).toBeInTheDocument();
		// Do carries its permission posture on its face, so the accessible name
		// is "Do · Ask" rather than "Do" — a screen reader hears which posture
		// is in force without opening the menu.
		await user.click(screen.getByRole('button', { name: /^Do/ }));
		expect(screen.getByTestId('composer-mode')).toHaveTextContent('ask');
		expect(screen.queryByText('Plan mode')).not.toBeInTheDocument();
	});

	it('picks the Do permission from the split menu and remembers it across Plan', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness);

		// The menu is closed until the caret is used: Accept relaxes a
		// permission gate, so it should never be one stray click away.
		expect(screen.queryByRole('menuitemradio', { name: /Accept/ })).not.toBeInTheDocument();

		await user.click(
			screen.getByRole('button', { name: /Choose what Do asks before editing files/ })
		);
		const accept = screen.getByRole('menuitemradio', { name: /Accept/ });
		expect(screen.getByRole('menuitemradio', { name: /Ask/ })).toHaveAttribute(
			'aria-checked',
			'true'
		);

		await user.click(accept);
		expect(screen.getByTestId('composer-mode')).toHaveTextContent('accept_in_scope');
		// Choosing closes the menu; the face now advertises the relaxed posture.
		expect(screen.queryByRole('menuitemradio', { name: /Accept/ })).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: /^Do · Accept/ })).toBeInTheDocument();

		// Plan wins while it is on, but the posture is parked rather than lost.
		await user.click(screen.getByRole('button', { name: 'Plan' }));
		expect(screen.getByTestId('composer-mode')).toHaveTextContent('plan');
		await user.click(screen.getByRole('button', { name: /^Do · Accept/ }));
		expect(screen.getByTestId('composer-mode')).toHaveTextContent('accept_in_scope');
	});

	it('turns the send control into a stop command during generation', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness, { isSending: true, canSend: false });

		await user.click(screen.getByRole('button', { name: 'Stop generating' }));
		expect(screen.getByTestId('composer-action')).toHaveTextContent('stop');
		expect(screen.queryByRole('button', { name: 'Send message' })).not.toBeInTheDocument();
	});

	it('blocks send and attachment actions in disabled capability states', () => {
		// `hasContent` so the send control is rendered at all: it is now revealed
		// by content, and this case is about it being DISABLED once revealed.
		render(FloatingComposerHarness, {
			disabled: true,
			supportsAttachments: false,
			hasContent: true
		});

		expect(screen.getByRole('textbox')).toHaveAttribute('aria-disabled', 'true');
		expect(screen.getByRole('textbox')).toHaveAttribute('contenteditable', 'false');
		expect(screen.getByRole('button', { name: 'Send message' })).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Attach files' })).toBeDisabled();
	});
});

describe('FloatingComposer dock row', () => {
    it('keeps queued and background updates above the session selector in chat and HUD', () => {
        const { container } = render(FloatingComposerHarness, { props: { showTop: true, showDock: true, showDockActions: true } });
        const top = screen.getByTestId('composer-top');
        const dock = container.querySelector('.composer-dock');
        expect(top.compareDocumentPosition(dock!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    });
	it('renders no dock row at all when the host does not opt in', () => {
		const { container } = render(FloatingComposerHarness);
		expect(screen.queryByTestId('dock-content')).toBeNull();
		expect(container.querySelector('.composer-dock')).toBeNull();
	});

	it('renders docked content above the input', () => {
		const { container } = render(FloatingComposerHarness, { props: { showDock: true } });
		const dock = container.querySelector('.composer-dock');
		const row = container.querySelector('.composer-row');

		expect(screen.getByTestId('dock-content')).toBeInTheDocument();
		// compareDocumentPosition: 4 = FOLLOWING, so the dock precedes the input row.
		expect(dock?.compareDocumentPosition(row!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
	});

	it('lets a host pin its own actions to the right of the docked content', () => {
		render(FloatingComposerHarness, {
			props: { showDock: true, showDockActions: true }
		});
		const dock = screen.getByTestId('dock-content');
		const action = screen.getByTestId('dock-action');

		expect(dock.compareDocumentPosition(action)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
	});

	it('supports dock actions with no docked pill', () => {
		render(FloatingComposerHarness, { props: { showDockActions: true } });
		expect(screen.getByTestId('dock-action')).toBeInTheDocument();
		expect(screen.queryByTestId('dock-content')).toBeNull();
	});
});

describe('FloatingComposer file drag-and-drop', () => {
	function composerWrap(container: HTMLElement): HTMLElement {
		const wrap = container.querySelector('.composer-wrap');
		expect(wrap).not.toBeNull();
		return wrap as HTMLElement;
	}

	it('stages dropped files through attachFiles', () => {
		const { container } = render(FloatingComposerHarness);
		const wrap = composerWrap(container);
		expect(screen.getByRole('group', { name: 'Message composer' })).toBe(wrap);
		const file = new File(['hello'], 'notes.txt', { type: 'text/plain' });

		fireEvent.drop(wrap, { dataTransfer: { files: [file], types: ['Files'] } });

		expect(screen.getByTestId('composer-action')).toHaveTextContent('attachFiles:1');
		expect(screen.getByTestId('composer-dropped-files')).toHaveTextContent('1');
	});

	it('highlights the composer while files hover and clears on leave', () => {
		const { container } = render(FloatingComposerHarness);
		const wrap = composerWrap(container);

		fireEvent.dragEnter(wrap, { dataTransfer: { types: ['Files'] } });
		expect(wrap).toHaveClass('composer-wrap--dragging');

		fireEvent.dragLeave(wrap, { relatedTarget: null });
		expect(wrap).not.toHaveClass('composer-wrap--dragging');
	});

	it('ignores file drops when the host disables attachments', () => {
		const { container } = render(FloatingComposerHarness, {
			props: { supportsAttachments: false }
		});
		const wrap = composerWrap(container);
		const file = new File(['hello'], 'notes.txt', { type: 'text/plain' });

		fireEvent.drop(wrap, { dataTransfer: { files: [file], types: ['Files'] } });

		expect(screen.getByTestId('composer-dropped-files')).toHaveTextContent('0');
		expect(container.querySelector('.composer-wrap')).not.toHaveClass(
			'composer-wrap--dragging'
		);
	});

	it('ignores drops that carry no files', () => {
		const { container } = render(FloatingComposerHarness);
		const wrap = composerWrap(container);

		fireEvent.drop(wrap, { dataTransfer: { files: [], types: ['text/plain'] } });

		expect(screen.getByTestId('composer-dropped-files')).toHaveTextContent('0');
	});
});


describe('text queue and parallel controls shared with the HUD', () => {
    it('sends a typed message while busy without invoking Stop', async () => {
        const user = userEvent.setup();
        render(FloatingComposerHarness, { props: { isSending: true, allowParallel: true } });
        await user.type(screen.getByRole('textbox'), 'Next question');
        expect(screen.queryByRole('button', { name: 'Stop generating' })).toBeNull();
        await user.click(screen.getByRole('button', { name: 'Queue message' }));
        expect(screen.getByTestId('composer-action')).toHaveTextContent('send:Next question');
    });
    it('keeps stop-and-send and parallel execution explicit', async () => {
        const user = userEvent.setup();
        render(FloatingComposerHarness, { props: { isSending: true, allowParallel: true, showDockActions: true } });
        await user.type(screen.getByRole('textbox'), 'Independent question');
        await user.click(screen.getByRole('button', { name: 'Send options' }));
        await user.click(screen.getByRole('button', { name: 'Run in parallel' }));
        expect(screen.getByTestId('composer-action')).toHaveTextContent('parallel:Independent question');
        await user.click(screen.getByRole('button', { name: 'Send options' }));
        await user.click(screen.getByRole('button', { name: 'Stop & send' }));
        expect(screen.getByTestId('composer-action')).toHaveTextContent('stopAndSend:Independent question');
    });
});
