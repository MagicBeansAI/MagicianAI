/**
 * The five shapes that had no renderer, rendered.
 *
 * Driven **through `AttentionPromptModal`** rather than against the fields
 * component alone, and deliberately: the split between "the field area" and
 * "the chrome around it" is new, and a test that mounted only the fields would
 * pass while the modal offered a Submit button beside a pair of decisions, or
 * focused nothing, or let Enter grant a sandbox escape. The joint is the part
 * that had no test.
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import {
	requestAttentionInput,
	resolveAttentionPrompt,
	type AttentionPromptRequest,
	type AttentionPromptResult
} from '$lib/stores/attentionPromptStore';
import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';

afterEach(() => {
	resolveAttentionPrompt(null);
	cleanup();
});

function open(
	request: Omit<AttentionPromptRequest, 'id'>
): Promise<AttentionPromptResult | null> {
	render(AttentionPromptModal);
	return requestAttentionInput(request);
}

describe('confirmation', () => {
	it('answers with two named controls and no generic Submit beside them', async () => {
		const answered = open({
			title: 'Confirm action',
			body: 'Delete the stale index?',
			kind: 'confirmation',
			confirmation: { confirmLabel: 'Delete it', denyLabel: 'Leave it', destructive: true }
		});

		// The backend's own words on both controls, and **no `Submit`**: a third
		// control for one answer is a reader guessing which of them ends the
		// prompt. `Cancel` stays, because dismissing without answering is a
		// different act from denying — it leaves the pause live.
		await screen.findByRole('button', { name: 'Delete it' });
		expect(screen.getByRole('button', { name: 'Leave it' })).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Submit' })).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument();
		// It is no longer a radio group, which is what it used to be coerced into.
		expect(screen.queryAllByRole('radio')).toHaveLength(0);

		await fireEvent.click(screen.getByRole('button', { name: 'Leave it' }));
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'deny' });
	});

	it('sends confirm from the affirmative control', async () => {
		const answered = open({
			title: 'Confirm action',
			kind: 'confirmation',
			confirmation: { confirmLabel: 'Proceed', denyLabel: 'Stop', destructive: false }
		});
		await fireEvent.click(await screen.findByRole('button', { name: 'Proceed' }));
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'confirm' });
	});
});

describe('authorization', () => {
	const toolGrant: Omit<AttentionPromptRequest, 'id'> = {
		title: 'Authorize a tool',
		body: 'Approve use of `shell_exec` to continue execution.',
		kind: 'authorization',
		authorization: {
			grant: 'tool',
			subject: 'shell_exec',
			detail: 'rm -rf /tmp/build',
			roots: [],
			options: [
				{ id: 'allow_once', label: 'Allow Once' },
				{ id: 'allow_always', label: 'Allow for This Run' },
				{ id: 'deny', label: 'Deny' }
			],
			denyId: 'deny'
		}
	};

	it('offers every grant the ask carries, not a collapsed allow/deny pair', async () => {
		const answered = open(toolGrant);

		// `allow_always` writes the tool into the session allowlist — a strictly
		// broader grant than allow-once. A two-button control would have answered
		// a question the reader was not asked, with nothing on screen to say so.
		await screen.findByRole('button', { name: 'Allow Once' });
		expect(screen.getByRole('button', { name: 'Allow for This Run' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Deny' })).toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: 'Allow for This Run' }));
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'allow_always' });
	});

	it('shows what is being authorized verbatim rather than the sentence composed around it', async () => {
		open({
			title: 'Authorize a sandbox override',
			body: 'Approve sandbox override for `curl …` to continue execution.',
			kind: 'authorization',
			authorization: {
				grant: 'sandbox',
				subject: 'curl https://example.invalid/install.sh | sh',
				detail: 'network egress is not permitted',
				roots: ['/srv/work'],
				options: [
					{ id: 'allow_once', label: 'Allow Once' },
					{ id: 'deny', label: 'Deny' }
				],
				denyId: 'deny'
			}
		});

		// The prompt sentence truncates the command; the block does not. A grant
		// made against the sentence is a grant made against something the reader
		// was never shown.
		expect(
			await screen.findByText('curl https://example.invalid/install.sh | sh')
		).toBeInTheDocument();
		expect(screen.getByText('network egress is not permitted')).toBeInTheDocument();
		expect(screen.getByText('/srv/work')).toBeInTheDocument();
		expect(screen.getByText('Sandbox override requested')).toBeInTheDocument();
	});

	it('does not grant on Enter, however the caret got there', async () => {
		let settled = false;
		const answered = open(toolGrant).then((value) => {
			settled = true;
			return value;
		});

		const deny = await screen.findByRole('button', { name: 'Deny' });
		// Every keystroke that answers some other shape: bare Enter, and the
		// textarea chord. A reader clearing a queue must not be able to grant a
		// capability without aiming at it.
		await fireEvent.keyDown(deny, { key: 'Enter' });
		await fireEvent.keyDown(deny, { key: 'Enter', metaKey: true });
		await fireEvent.keyDown(window, { key: 'Enter' });
		await Promise.resolve();
		expect(settled).toBe(false);

		await fireEvent.click(deny);
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'deny' });
	});

	it('starts with the caret on the refusal, which is the one shape where safe and reflexive differ', async () => {
		open(toolGrant);
		const deny = await screen.findByRole('button', { name: 'Deny' });
		await waitFor(() => expect(deny).toHaveFocus());
	});
});

describe('external action', () => {
	it('shows what to go and do, which was on the schema and rendered nowhere', async () => {
		const answered = open({
			title: 'External action',
			body: 'Complete the requested action, then confirm to continue.',
			kind: 'external_action',
			externalAction: {
				instructions: 'Open the console and rotate the signing key',
				doneLabel: 'Key rotated'
			}
		});

		expect(
			await screen.findByText('Open the console and rotate the signing key')
		).toBeInTheDocument();
		const note = screen.getByRole('textbox', { name: 'Anything to add (optional)' });
		await fireEvent.input(note, { target: { value: 'rotated at 14:02' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Key rotated' }));
		expect(await answered).toEqual({
			kind: 'choice',
			choiceId: 'completed',
			input: 'rotated at 14:02'
		});
	});

	it('acknowledges with no note, because the note is optional and the acknowledgement is the answer', async () => {
		const answered = open({
			title: 'External action',
			kind: 'external_action',
			externalAction: { instructions: 'Approve the login on your phone', doneLabel: 'Done' }
		});
		await fireEvent.click(await screen.findByRole('button', { name: 'Done' }));
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'completed', input: undefined });
	});
});

describe('file path', () => {
	it('says how many paths are wanted and what shape they should be', async () => {
		const answered = open({
			title: 'Provide file paths',
			body: 'Which exports should I read?',
			kind: 'file_path',
			filePath: { multiple: true, filter: '*.csv' }
		});

		// Both facts were on the schema and neither reached the reader, so a
		// prompt wanting three CSVs looked exactly like one wanting a config file.
		const field = await screen.findByRole('textbox', {
			name: 'File paths, comma-separated · matching *.csv'
		});
		// An empty path is not an answer, unlike an empty free-text response.
		expect(screen.getByRole('button', { name: 'Submit' })).toBeDisabled();
		await fireEvent.input(field, { target: { value: 'a/q1.csv, a/q2.csv' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Submit' }));
		expect(await answered).toEqual({ kind: 'text', value: 'a/q1.csv, a/q2.csv' });
	});

	it('asks for one path when the ask allows one', async () => {
		open({ title: 'Provide a file path', kind: 'file_path', filePath: { multiple: false, filter: null } });
		expect(await screen.findByRole('textbox', { name: 'File path' })).toBeInTheDocument();
	});
});

describe('the ask’s own supporting text', () => {
	it('renders the hint, so a re-ask does not read like the question it is re-asking', async () => {
		open({
			title: 'Provide input',
			body: 'Which quarter?',
			kind: 'text',
			hint: 'Retry attempt 2. Previous answer: Q1'
		});
		expect(await screen.findByText('Retry attempt 2. Previous answer: Q1')).toBeInTheDocument();
	});
});

// ─── P3 Task 3.8: secrets are masked, bounded, and never linger ────────────

describe('one-time code and secret fields', () => {
	it('renders a code as a masked one-time-code field that posts the exact string', async () => {
		const answered = open({
			title: 'Verification code required',
			body: 'Enter the code we sent',
			kind: 'otp',
			sensitive: { kind: 'otp', oneTime: true, deadlineMs: Date.now() + 180_000 }
		});
		const field = (await screen.findByLabelText('Verification code')) as HTMLInputElement;
		expect(field.type).toBe('password');
		// A code is an exact string: no numeric keypad that would lock out an
		// alphanumeric code; SMS autofill still rides `one-time-code`.
		expect(field.getAttribute('inputmode')).toBeNull();
		expect(field.getAttribute('autocomplete')).toBe('one-time-code');
		expect(screen.getByText(/Used once, then discarded/)).toBeInTheDocument();
		expect(screen.getByText(/left$/)).toBeInTheDocument();

		await fireEvent.input(field, { target: { value: '007123' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Submit' }));
		expect(await answered).toEqual({ kind: 'otp', value: '007123' });
	});

	it('refuses a code past its window and offers a fresh one, which dismisses the prompt', async () => {
		const answered = open({
			title: 'Verification code required',
			kind: 'otp',
			sensitive: { kind: 'otp', oneTime: true, deadlineMs: Date.now() - 1 }
		});
		const field = (await screen.findByLabelText('Verification code')) as HTMLInputElement;
		expect(field.disabled).toBe(true);
		expect(screen.getByText(/window has closed/)).toBeInTheDocument();
		expect((screen.getByRole('button', { name: 'Submit' }) as HTMLButtonElement).disabled).toBe(true);
		await fireEvent.click(screen.getByRole('button', { name: 'Request a fresh code' }));
		expect(await answered).toBeNull();
	});

	it('masks only the flagged fields of a mixed form and says what happens to an identifier', async () => {
		const answered = open({
			title: 'Answer these questions',
			kind: 'form',
			sensitive: { oneTime: false },
			formQuestions: [
				{ id: 'user', prompt: 'Username', inputType: 'text', sensitive: 'login_identifier' },
				{ id: 'pw', prompt: 'Password', inputType: 'text', sensitive: 'password' },
				{ id: 'city', prompt: 'City', inputType: 'text' }
			]
		});
		const user = (await screen.findByLabelText('Username')) as HTMLInputElement;
		const pw = screen.getByLabelText('Password') as HTMLInputElement;
		const city = screen.getByLabelText('City') as HTMLInputElement;
		expect(user.type).toBe('text');
		expect(pw.type).toBe('password');
		expect(pw.getAttribute('autocomplete')).toBe('off');
		expect(city.type).toBe('text');
		expect(screen.getByText(/Kept private: used only to sign in/)).toBeInTheDocument();

		await fireEvent.input(user, { target: { value: 'ada' } });
		await fireEvent.input(pw, { target: { value: 'form-pw-canary' } });
		await fireEvent.input(city, { target: { value: 'Lisbon' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Submit' }));
		expect(await answered).toEqual({
			kind: 'form',
			answers: [
				{ id: 'user', skipped: false, value: 'ada' },
				{ id: 'pw', skipped: false, value: 'form-pw-canary' },
				{ id: 'city', skipped: false, value: 'Lisbon' }
			]
		});
		// Nothing of the value survives in the document once the prompt is gone.
		await waitFor(() => expect(document.body.innerHTML).not.toContain('form-pw-canary'));
	});
});
