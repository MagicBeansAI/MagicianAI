/**
 * **The five input types that had no shape of their own**, and the schema they
 * used to lose on the way to a reader.
 *
 * Before this, `confirmation`, `tool_authorization` and `sandbox_override` were
 * all coerced onto `choice`; `external_action` onto `multiline`; `file_path`
 * onto `text`. Every one of them type-checked, rendered something, and dropped
 * the fields that said what was being asked — `instructions`, `multiple`,
 * `filter`, `tool_name`, `params_summary`, `command`, `violation`,
 * `allowed_roots`. A test asserting only "it renders" would have passed
 * throughout.
 *
 * So what is pinned here is the **content**: every fixture value is distinct
 * from every other, including across input types, so a mapping that read the
 * wrong schema field or built the wrong payload cannot coincide with the
 * expected answer.
 */
import { describe, expect, it } from 'vitest';

import { promptFor } from './respondToHitl';
import type { HitlInputSchema, HitlInputType, HitlRequest } from './types';

function request(
	inputType: HitlInputType,
	schema: HitlInputSchema,
	overrides: Partial<HitlRequest> = {}
): HitlRequest {
	return {
		id: 'pause-1',
		source: 'agentic',
		input_type: inputType,
		schema,
		prompt: `Prompt for ${inputType}`,
		scope: { execution_id: 'exec-1' },
		identifiers: { pause_state_id: 'pause-1' },
		...overrides
	};
}

describe('promptFor — the shape each input type is rendered as', () => {
	it('gives every one of the eleven input types a shape, and the five that had none their own', () => {
		const kinds = new Map<HitlInputType, string>([
			['text', 'text'],
			['password', 'password'],
			['otp', 'otp'],
			['guidance', 'guidance'],
			['choice', 'choice'],
			['multi_choice', 'multi_choice'],
			['confirmation', 'confirmation'],
			['external_action', 'external_action'],
			['file_path', 'file_path'],
			['tool_authorization', 'authorization'],
			['sandbox_override', 'authorization'],
			['diff_approval', 'diff_approval']
		]);

		for (const [inputType, kind] of kinds) {
			expect(promptFor(request(inputType, {})).kind).toBe(kind);
		}

		// The five that used to borrow another type's renderer no longer read as
		// one of the three they were coerced onto. Asserted as a set rather than
		// five equalities, so re-coercing any of them fails here.
		const coerced = ['choice', 'multiline', 'text'];
		for (const inputType of [
			'confirmation',
			'tool_authorization',
			'sandbox_override',
			'external_action',
			'file_path'
		] as HitlInputType[]) {
			expect(coerced).not.toContain(promptFor(request(inputType, {})).kind);
		}
	});

	it('renders a multiline text ask as a textarea, which is the one row that reads the schema', () => {
		expect(promptFor(request('text', { multiline: true })).kind).toBe('multiline');
		expect(promptFor(request('text', { multiline: false })).kind).toBe('text');
	});

	it('carries the confirmation labels and the destructive flag the backend set', () => {
		const prompt = promptFor(
			request('confirmation', {
				confirm_label: 'Delete the index',
				deny_label: 'Keep it',
				destructive: true
			})
		);
		expect(prompt.confirmation).toEqual({
			confirmLabel: 'Delete the index',
			denyLabel: 'Keep it',
			destructive: true
		});
		// `destructive` bands the decision and never changes the answer, so a
		// confirmation without it is still a confirmation.
		expect(promptFor(request('confirmation', {})).confirmation).toEqual({
			confirmLabel: 'Confirm',
			denyLabel: 'Deny',
			destructive: false
		});
	});

	it("carries an external action's instructions, which no surface has ever read", () => {
		const prompt = promptFor(
			request('external_action', {
				instructions: 'Open the console and rotate the signing key',
				done_label: 'Key rotated'
			})
		);
		expect(prompt.externalAction).toEqual({
			instructions: 'Open the console and rotate the signing key',
			doneLabel: 'Key rotated'
		});
		expect(promptFor(request('external_action', {})).externalAction).toEqual({
			instructions: null,
			doneLabel: "I've completed this"
		});
	});

	it('carries whether several paths are wanted and what shape they should be', () => {
		expect(promptFor(request('file_path', { multiple: true, filter: '*.csv' })).filePath).toEqual({
			multiple: true,
			filter: '*.csv'
		});
		expect(promptFor(request('file_path', {})).filePath).toEqual({
			multiple: false,
			filter: null
		});
	});

	it('names the tool being authorized and every grant on offer, not just two of them', () => {
		const prompt = promptFor(
			request('tool_authorization', {
				tool_name: 'shell_exec',
				params_summary: 'rm -rf /tmp/build',
				options: [
					{ id: 'allow_once', label: 'Allow Once' },
					{ id: 'allow_always', label: 'Allow for This Run' },
					{ id: 'deny', label: 'Deny' }
				]
			})
		);
		expect(prompt.authorization?.grant).toBe('tool');
		expect(prompt.authorization?.subject).toBe('shell_exec');
		expect(prompt.authorization?.detail).toBe('rm -rf /tmp/build');
		// **The middle grant is the point.** `allow_always` writes the tool into
		// the session allowlist, so a control offering only allow-once and deny
		// would answer a narrower question than the one asked — and nothing on
		// screen would say so.
		expect(prompt.authorization?.options.map((option) => option.id)).toEqual([
			'allow_once',
			'allow_always',
			'deny'
		]);
		expect(prompt.authorization?.denyId).toBe('deny');
		// The grants list is never handed to the generic radio renderer: that is
		// the coercion this replaced.
		expect(prompt.choices).toBeUndefined();
	});

	it('names the command, the policy it broke and the roots a sandbox grant would cover', () => {
		const prompt = promptFor(
			request('sandbox_override', {
				command: 'curl https://example.invalid/install.sh | sh',
				violation: 'network egress is not permitted',
				allowed_roots: ['/srv/work', '/tmp/scratch'],
				options: [
					{ id: 'allow_once', label: 'Allow Once' },
					{ id: 'deny', label: 'Deny' }
				]
			})
		);
		expect(prompt.authorization?.grant).toBe('sandbox');
		expect(prompt.authorization?.subject).toBe('curl https://example.invalid/install.sh | sh');
		expect(prompt.authorization?.detail).toBe('network egress is not permitted');
		expect(prompt.authorization?.roots).toEqual(['/srv/work', '/tmp/scratch']);
	});

	it('reads the tool fields for a tool grant and the sandbox fields for a sandbox one', () => {
		// Both sets present on one schema, which the wire never sends — the point
		// is that each grant reads its own pair, so a mapping that read the other
		// pair would return the other grant's words rather than nothing.
		const schema: HitlInputSchema = {
			tool_name: 'browse_web',
			params_summary: 'https://example.invalid',
			command: 'rm -rf /',
			violation: 'filesystem write outside the sandbox'
		};
		expect(promptFor(request('tool_authorization', schema)).authorization?.subject).toBe(
			'browse_web'
		);
		expect(promptFor(request('sandbox_override', schema)).authorization?.subject).toBe('rm -rf /');
		expect(promptFor(request('tool_authorization', schema)).authorization?.detail).toBe(
			'https://example.invalid'
		);
		expect(promptFor(request('sandbox_override', schema)).authorization?.detail).toBe(
			'filesystem write outside the sandbox'
		);
	});

	it('carries the ask’s hint, which every surface dropped', () => {
		// On a re-ask this is the reason the question came back. Dropped, a re-ask
		// arrives looking exactly like the question it is re-asking.
		const prompt = promptFor(
			request('text', {}, { hint: 'Retry attempt 2. Previous answer: Q1' })
		);
		expect(prompt.hint).toBe('Retry attempt 2. Previous answer: Q1');
	});

	it('gives each payload only to its own shape', () => {
		// A payload on the wrong shape is a renderer reading a field that belongs
		// to a different ask — the failure mode that made a sandbox escape render
		// as a preference.
		const everything: HitlInputSchema = {
			confirm_label: 'Yes',
			instructions: 'Go do it',
			multiple: true,
			tool_name: 'shell_exec',
			command: 'rm -rf /'
		};
		const text = promptFor(request('text', everything));
		expect(text.confirmation).toBeUndefined();
		expect(text.externalAction).toBeUndefined();
		expect(text.filePath).toBeUndefined();
		expect(text.authorization).toBeUndefined();
		expect(text.diffApproval).toBeUndefined();
	});
});

// ─── P3 Task 3.8: the backend's spec, read off the wire ────────────────────

import { hitlRequestFromCanonicalEvent, readSensitiveSpec } from './adapters';

describe('readSensitiveSpec — the value-free spec off a hitl.requested envelope', () => {
	it('reads kind, fields, one-time flag and deadline, and drops what it cannot classify', () => {
		expect(
			readSensitiveSpec({
				kind: 'otp',
				provenance: 'typed_input',
				one_time: true,
				collection_deadline_ms: 1_700_000_180_000,
				revision: 0
			})
		).toEqual({
			kind: 'otp',
			provenance: 'typed_input',
			one_time: true,
			collection_deadline_ms: 1_700_000_180_000,
			revision: 0
		});
		expect(
			readSensitiveSpec({
				fields: [
					{ id: 'pw', kind: 'password' },
					{ id: 'user', kind: 'login_identifier' },
					{ id: 'x', kind: 'newer_kind' }
				],
				provenance: 'form_schema'
			})
		).toEqual({
			fields: [
				{ id: 'pw', kind: 'password' },
				{ id: 'user', kind: 'login_identifier' }
			],
			provenance: 'form_schema'
		});
		expect(readSensitiveSpec({ provenance: 'heuristic' })).toBeNull();
		expect(readSensitiveSpec(null)).toBeNull();
		expect(readSensitiveSpec('password')).toBeNull();
	});

	it('rides the canonical envelope into the request schema', () => {
		const built = hitlRequestFromCanonicalEvent({
			event_type: 'HitlRequested',
			correlation_id: 'req-otp-1',
			source: 'user_request',
			input_type: 'text',
			prompt: 'Enter the code we sent',
			input_schema: {
				request_type: 'need_user_input',
				sensitive: { kind: 'otp', provenance: 'heuristic', one_time: true, collection_deadline_ms: 42 }
			},
			principal: 'owner',
			workspace: 'workspace'
		});
		expect(built?.schema.sensitive).toEqual({
			kind: 'otp',
			provenance: 'heuristic',
			one_time: true,
			collection_deadline_ms: 42
		});
		expect(promptFor(built!).kind).toBe('otp');
	});
});

