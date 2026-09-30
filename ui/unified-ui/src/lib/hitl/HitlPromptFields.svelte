<script lang="ts" context="module">
	import type { AttentionPromptKind } from '$lib/stores/attentionPromptStore';

	/**
	 * What gets the caret when a prompt of this shape appears.
	 *
	 * A `Record<AttentionPromptKind, string>` rather than the if-chain this
	 * replaces, which named three shapes and let the rest fall through to a
	 * selector for a field they do not have — so the two shapes that render no
	 * field at all opened with focus nowhere.
	 *
	 * **`authorization` lands on Deny**, and that is deliberate rather than
	 * alphabetical: it is the one shape here where the reflexive answer and the
	 * safe answer differ, and the caret should start on the safe one. Every other
	 * shape lands on the thing the reader has to fill in or the first thing they
	 * have to read.
	 */
	export const PROMPT_FOCUS_SELECTOR: Record<AttentionPromptKind, string> = {
		text: '.hitl-fields input, .hitl-fields textarea',
		multiline: '.hitl-fields input, .hitl-fields textarea',
		password: '.hitl-fields input, .hitl-fields textarea',
		otp: '.hitl-fields input, .hitl-fields textarea',
		guidance: '.hitl-fields input, .hitl-fields textarea',
		file_path: '.hitl-fields input, .hitl-fields textarea',
		choice: 'input[type="radio"]',
		multi_choice: 'input[type="checkbox"]',
		confirmation: '.hitl-decision',
		external_action: '.hitl-fields textarea',
		authorization: '.hitl-decision__deny',
		diff_approval: '.diff-approval',
		form: '.hitl-fields input, .hitl-fields textarea'
	};
</script>

<script lang="ts">
	/**
	 * **The one renderer for a HITL ask**, wherever it is answered.
	 *
	 * It was `AttentionPromptModal`'s middle third. It is a component of its own
	 * because a second surface — the unified task panel — has to answer the same
	 * asks in place, and a panel with renderers of its own would be a second
	 * implementation of "show a HITL question and collect the answer": two places
	 * for a validation floor to drift, two places for a new input type to be
	 * forgotten, and a panel that could answer things the Attention centre could
	 * not, which is backwards.
	 *
	 * What is **not** here is chrome: no title, no eyebrow, no Cancel. A modal
	 * frames this in a dialog with a footer; a task panel puts it inside an act's
	 * body under the verdict that sent the reader there. The four shapes whose
	 * answer *is* a named action carry their own controls — see
	 * `PROMPT_HAS_OWN_ACTIONS` — and everything else is a field the surface
	 * submits.
	 *
	 * See `docs/components/unified-ui/unified-task-panel.md`.
	 */
	import { createEventDispatcher, onDestroy } from 'svelte';

	import Button from '$lib/magician/components/generative/Button.svelte';
	import Input from '$lib/magician/components/generative/Input.svelte';
	import TextArea from '$lib/magician/components/generative/TextArea.svelte';
	import DiffStrip from '$lib/shell/DiffStrip.svelte';
	import {
		PROMPT_HAS_OWN_ACTIONS,
		type AttentionPromptChoice,
		type AttentionPromptRequest,
		type AttentionPromptResult
	} from '$lib/stores/attentionPromptStore';

	/** The ask, as the prompt store models it. Never `null` — the surface gates on that. */
	export let request: AttentionPromptRequest;
	/**
	 * Whether an outside control may submit right now.
	 *
	 * Bindable, and the **single** source of truth for "is this answerable" — the
	 * modal's footer button, the panel's Answer button and the Enter chord all
	 * read this one value, so a floor added here cannot be enforced on one path
	 * and not another. Always `false` for the shapes that answer themselves:
	 * there is nothing for an outside control to submit.
	 */
	export let canSubmit = false;
	/**
	 * Whether the controls are inert because an answer is already in flight. The
	 * surface owns that fact — it is the thing making the request.
	 */
	export let busy = false;

	const dispatch = createEventDispatcher<{ answer: AttentionPromptResult; dismiss: void }>();

	let textValue = '';
	let selectedChoiceId = '';
	let selectedMultiChoiceIds: string[] = [];
	let choiceInputValue = '';
	let formValues: Record<string, string> = {};
	let formSkipped: Record<string, boolean> = {};

	/**
	 * Which request the state above belongs to.
	 *
	 * `-1` rather than `0`, because request ids start at 1 and `0` would make the
	 * first request indistinguishable from "nothing has been rendered". The reset
	 * is keyed on the id alone, so a re-render of the same ask keeps what the
	 * reader has typed.
	 */
	let renderedRequestId = -1;
	$: if (request.id !== renderedRequestId) {
		renderedRequestId = request.id;
		textValue = request.defaultValue ?? '';
		// Decisions require an explicit selection: a confirmation or an
		// authorization must never arrive with the affirmative already chosen.
		selectedChoiceId = '';
		selectedMultiChoiceIds = [];
		choiceInputValue = '';
		formValues = {};
		formSkipped = {};
		nowMs = Date.now();
	}

	/**
	 * A secret's collection window, counted down on screen.
	 *
	 * `nowMs` ticks only while an ask with a deadline is shown; past the
	 * deadline the fields refuse the value (`canSubmit` is `false`) and the
	 * surface offers a fresh ask instead — a code typed after its window is a
	 * code the destination will reject, and the honest thing is to say so
	 * before the reader finishes typing it.
	 */
	let nowMs = Date.now();
	let deadlineTimer: ReturnType<typeof setInterval> | null = null;
	$: deadlineMs = request.sensitive?.deadlineMs;
	$: {
		if (deadlineTimer) {
			clearInterval(deadlineTimer);
			deadlineTimer = null;
		}
		if (typeof deadlineMs === 'number' && deadlineMs > 0) {
			deadlineTimer = setInterval(() => {
				nowMs = Date.now();
			}, 1000);
		}
	}
	onDestroy(() => {
		if (deadlineTimer) clearInterval(deadlineTimer);
		// Whatever was typed into a secret field leaves with the component.
		textValue = '';
		formValues = {};
	});
	$: secretExpired = typeof deadlineMs === 'number' && deadlineMs > 0 && nowMs >= deadlineMs;
	$: secondsLeft =
		typeof deadlineMs === 'number' && deadlineMs > 0
			? Math.max(0, Math.ceil((deadlineMs - nowMs) / 1000))
			: null;

	function isMaskedField(sensitive: string | undefined): boolean {
		return sensitive === 'password' || sensitive === 'otp' || sensitive === 'other';
	}

	function deadlineLabel(seconds: number): string {
		if (seconds >= 120) return `${Math.floor(seconds / 60)} min left`;
		return `${seconds}s left`;
	}

	$: selectedChoice =
		request.choices?.find((choice: AttentionPromptChoice) => choice.id === selectedChoiceId) ?? null;
	$: needsChoiceInput = selectedChoice?.requiresInput ?? false;
	$: canSubmit =
		!secretExpired &&
		computeCanSubmit(
			request,
			textValue,
			selectedChoice,
			choiceInputValue,
			needsChoiceInput,
			selectedMultiChoiceIds,
			formValues,
			formSkipped
		);

	function computeCanSubmit(
		req: AttentionPromptRequest,
		text: string,
		choice: AttentionPromptChoice | null,
		choiceInput: string,
		choiceNeedsInput: boolean,
		multiSelection: string[],
		formVals: Record<string, string>,
		formSkip: Record<string, boolean>
	): boolean {
		// The shapes that answer themselves have no outside submit to gate, and
		// reporting `true` for them would light a Submit button beside a pair of
		// decisions — two controls for one answer, and the reader guessing which
		// ends the prompt.
		if (PROMPT_HAS_OWN_ACTIONS[req.kind]) return false;
		if (req.kind === 'choice') {
			if (!choice) return false;
			if (choiceNeedsInput && !choiceInput.trim()) return false;
			return true;
		}
		if (req.kind === 'multi_choice') {
			const min = Math.max(0, req.minSelections ?? 0);
			const max = req.maxSelections;
			// Require at least one selection regardless of `minSelections`.
			// When `minSelections === 0`, the form-level "valid" check would
			// otherwise pass on zero picks and Submit would dispatch
			// `selected_ids: []` — universally surprising on a selection-driven
			// prompt. If a future "skip this question" case genuinely needs to
			// submit empty, that belongs on its own affordance, not the primary.
			if (multiSelection.length === 0) return false;
			if (multiSelection.length < min) return false;
			if (typeof max === 'number' && max > 0 && multiSelection.length > max) return false;
			return true;
		}
		if (req.kind === 'form') {
			const questions = req.formQuestions ?? [];
			if (questions.length === 0) return false;
			return questions.every((question) => {
				if (formSkip[question.id]) return true;
				return (formVals[question.id] ?? '').trim().length > 0;
			});
		}
		if (req.kind === 'password' || req.kind === 'otp') return text.length > 0;
		// A path is the answer, so an empty one is not an answer — unlike a free
		// text response, where the empty string is a thing a reader may mean.
		if (req.kind === 'file_path') return text.trim().length > 0;
		return true;
	}

	function toggleMultiChoice(choiceId: string): void {
		if (request.kind !== 'multi_choice') return;
		const max = request.maxSelections;
		if (selectedMultiChoiceIds.includes(choiceId)) {
			selectedMultiChoiceIds = selectedMultiChoiceIds.filter((id) => id !== choiceId);
			return;
		}
		// Cap at `maxSelections` if set — the operator can deselect first to swap
		// a pick instead of silently exceeding the limit.
		if (typeof max === 'number' && max > 0 && selectedMultiChoiceIds.length >= max) return;
		selectedMultiChoiceIds = [...selectedMultiChoiceIds, choiceId];
	}

	function multiChoiceConstraintLabel(req: AttentionPromptRequest): string | null {
		if (req.kind !== 'multi_choice') return null;
		const min = req.minSelections ?? 0;
		const max = req.maxSelections;
		if (min > 0 && typeof max === 'number' && max > 0) {
			return min === max ? `Pick exactly ${min}` : `Pick ${min}–${max}`;
		}
		if (min > 0) return `Pick at least ${min}`;
		if (typeof max === 'number' && max > 0) return `Pick up to ${max}`;
		return 'Pick any number';
	}

	function answer(result: AttentionPromptResult): void {
		if (busy) return;
		dispatch('answer', result);
	}

	/**
	 * Answer from an outside control — the modal's footer, the panel's button, the
	 * Enter chord.
	 *
	 * Gated on `canSubmit` here as well as at every call site, because this is the
	 * function that actually posts and a gate the caller can forget is not a gate.
	 * A no-op for the shapes that answer themselves, which is what `canSubmit`
	 * being permanently `false` for them already says.
	 */
	export function submit(): void {
		if (!canSubmit) return;
		if (request.kind === 'choice') {
			if (!selectedChoice) return;
			answer({
				kind: 'choice',
				choiceId: selectedChoice.id,
				input: needsChoiceInput ? choiceInputValue : undefined
			});
			return;
		}
		if (request.kind === 'multi_choice') {
			answer({ kind: 'multi_choice', choiceIds: [...selectedMultiChoiceIds] });
			return;
		}
		if (request.kind === 'form') {
			answer({
				kind: 'form',
				answers: (request.formQuestions ?? []).map((question) => ({
					id: question.id,
					skipped: Boolean(formSkipped[question.id]),
					value: formSkipped[question.id] ? undefined : formValues[question.id]
				}))
			});
			return;
		}
		// `file_path` resolves as text and is split into paths by the response
		// mapping; the field states the separator so the reader is not guessing.
		if (request.kind === 'file_path') {
			answer({ kind: 'text', value: textValue });
			return;
		}
		answer({
			kind: request.kind as 'text' | 'multiline' | 'password' | 'otp' | 'guidance',
			value: textValue
		});
	}

	/**
	 * The reader gave up on a code that expired, or wants a new one. Answering
	 * `null` is the dismissal the surface already turns into an explicit cancel
	 * for a secret ask (`respondToHitl`), which retires the pending operation
	 * so a fresh challenge can be raised.
	 */
	function requestFreshCode(): void {
		if (busy) return;
		textValue = '';
		dispatch('dismiss');
	}

	function diffFilePaths(): string[] {
		return request.diffApproval?.files?.map((file) => file.path).filter(Boolean) ?? [];
	}

	function submitDiffApproval(choiceId: 'apply' | 'reject', selectedPaths?: string[]): void {
		answer({ kind: 'choice', choiceId, selectedPaths });
	}

	function applyDiffFile(path: string): void {
		submitDiffApproval('apply', [path]);
	}

	function rejectDiffFile(path: string): void {
		const remaining = diffFilePaths().filter((candidate) => candidate !== path);
		if (remaining.length === 0) {
			submitDiffApproval('reject');
			return;
		}
		submitDiffApproval('apply', remaining);
	}

	/**
	 * The refusal, and the grants beside it.
	 *
	 * **`denyId` is matched, never the label**: the id is the contract the resume
	 * dispatcher reads, and a backend that relabels `Deny` to `Don't allow`
	 * changes what the reader sees and nothing about which control refuses.
	 *
	 * With no options on the payload the pair falls back to the two ids the
	 * dispatcher has always accepted. That is a floor rather than a guess about
	 * the ask: `allow_once` and `deny` are valid for both grants, and a broader
	 * one is never offered on a payload that did not name it.
	 */
	$: authorizationOptions =
		request.authorization?.options?.length
			? request.authorization.options
			: [
					{ id: 'allow_once', label: 'Allow once' },
					{ id: 'deny', label: 'Deny' }
				];
	$: authorizationDenyId = request.authorization?.options?.length
		? request.authorization.denyId
		: 'deny';
	$: authorizationDeny = authorizationOptions.find((option) => option.id === authorizationDenyId) ?? null;
	$: authorizationGrants = authorizationOptions.filter((option) => option.id !== authorizationDenyId);

	/**
	 * The path field's own label, built from the two schema fields that were on
	 * the wire and reached nobody. It says how many paths are wanted and what
	 * shape they should be, so a prompt for three CSVs stops looking exactly like
	 * one for a single config file.
	 */
	$: filePathLabel =
		request.filePath === undefined
			? 'File path'
			: [
					request.filePath.multiple ? 'File paths, comma-separated' : 'File path',
					request.filePath.filter ? `matching ${request.filePath.filter}` : null
				]
					.filter((part): part is string => part !== null)
					.join(' · ');
</script>

<div class="hitl-fields" data-prompt-kind={request.kind}>
	{#if request.hint}
		<!--
			The ask's own supporting text. It sits above the controls rather than
			beside them because on a re-ask it is the reason the question came back,
			and a reader who does not read it answers the same way twice.
		-->
		<p class="hitl-hint">{request.hint}</p>
	{/if}

	{#if request.sensitive}
		<!--
			The backend classified this ask as collecting a secret. Say what will
			happen to the value (custody, never the transcript), count the window
			down when there is one, and past it offer a fresh ask instead of a field
			that would accept a code the destination will reject.
		-->
		<div class="sensitive-banner" data-sensitive-kind={request.sensitive.kind ?? 'form'} data-expired={secretExpired}>
			<span class="sensitive-banner__text">
				{#if secretExpired}
					This code's window has closed — ask for a fresh one.
				{:else if request.sensitive.oneTime}
					Used once, then discarded. Never shown to the assistant.
				{:else}
					Held privately for this run. Never shown to the assistant.
				{/if}
			</span>
			{#if secondsLeft !== null && !secretExpired}
				<span class="sensitive-banner__deadline" aria-live="polite">{deadlineLabel(secondsLeft)}</span>
			{/if}
			{#if secretExpired}
				<button type="button" class="sensitive-banner__fresh" disabled={busy} on:click={requestFreshCode}>
					Request a fresh code
				</button>
			{/if}
		</div>
	{/if}

	{#if request.kind === 'diff_approval'}
		<div class="diff-approval" tabindex="-1">
			{#if request.diffApproval?.rationale}
				<p class="diff-approval__rationale">{request.diffApproval.rationale}</p>
			{/if}
			<DiffStrip
				diff={{
					files: request.diffApproval?.files ?? [],
					note: request.diffApproval?.transactionId
				}}
				lifecycle="pending"
				title="Staged changes"
				showStats={true}
				allowCopy={true}
				autoExpandFirst={true}
				showLineNumbers={true}
				wrapLongLines={true}
				truncateLinesThreshold={400}
				allowPerFileApproval={(request.diffApproval?.files?.length ?? 0) > 1}
				on:applyFile={(event) => applyDiffFile(event.detail.file.path)}
				on:rejectFile={(event) => rejectDiffFile(event.detail.file.path)}
			/>
		</div>
		<div class="hitl-decision">
			<Button
				label="Reject"
				variant="outline"
				size="sm"
				disabled={busy}
				on:click={() => submitDiffApproval('reject')}
			/>
			<Button
				label="Apply"
				variant="primary"
				size="sm"
				disabled={busy || !request.diffApproval?.files?.length}
				on:click={() => submitDiffApproval('apply')}
			/>
		</div>
	{:else if request.kind === 'confirmation'}
		<!--
			Two controls and no field. It used to be a two-option radio group with a
			Submit under it — one extra click on the commonest decision in the
			system, and nothing on screen distinguishing "proceed with deletion?"
			from "which quarter?".

			`data-destructive` carries the backend's own flag to CSS rather than a
			second colour decision here; it changes how the affirmative is banded and
			nothing about what is posted.
		-->
		<div class="hitl-decision" data-destructive={request.confirmation?.destructive ? 'yes' : null}>
			<Button
				label={request.confirmation?.denyLabel ?? 'No'}
				variant="outline"
				size="sm"
				disabled={busy}
				on:click={() => answer({ kind: 'choice', choiceId: 'deny' })}
			/>
			<Button
				label={request.confirmation?.confirmLabel ?? 'Yes'}
				variant={request.confirmation?.destructive ? 'outline' : 'primary'}
				size="sm"
				disabled={busy}
				on:click={() => answer({ kind: 'choice', choiceId: 'confirm' })}
			/>
		</div>
	{:else if request.kind === 'authorization'}
		<!--
			A grant of permission, and the one block here that is styled to stop a
			reader rather than to help them along. What is being authorized is
			printed verbatim in the mono face: the prompt sentence is composed by the
			backend *around* these values, and a grant made against the sentence is a
			grant made against something the reader was never shown.

			Enter does not reach this — see `promptSubmitsOnEnter`.
		-->
		<div class="hitl-grant" data-grant={request.authorization?.grant ?? 'tool'}>
			<p class="hitl-grant__label">
				{request.authorization?.grant === 'sandbox'
					? 'Sandbox override requested'
					: 'Tool authorization requested'}
			</p>
			{#if request.authorization?.subject}
				<p class="hitl-grant__subject">{request.authorization.subject}</p>
			{/if}
			{#if request.authorization?.detail}
				<p class="hitl-grant__detail">{request.authorization.detail}</p>
			{/if}
			{#if request.authorization?.roots?.length}
				<ul class="hitl-grant__roots">
					{#each request.authorization.roots as root}
						<li>{root}</li>
					{/each}
				</ul>
			{/if}
		</div>
		<!--
			**The refusal first and in the primary slot**, then every grant the ask
			actually offers, in the order the backend listed them. A tool
			authorization offers three — allow once, allow for this run, deny — and
			the middle one writes the tool into the session allowlist; collapsing
			them into one Allow button would have quietly answered a broader question
			than the reader was asked.
		-->
		<div class="hitl-decision">
			{#if authorizationDeny}
				<Button
					label={authorizationDeny.label}
					variant="primary"
					size="sm"
					disabled={busy}
					className="hitl-decision__deny"
					on:click={() => answer({ kind: 'choice', choiceId: authorizationDeny.id })}
				/>
			{/if}
			{#each authorizationGrants as grant (grant.id)}
				<Button
					label={grant.label}
					variant="outline"
					size="sm"
					disabled={busy}
					title={grant.description ?? ''}
					on:click={() => answer({ kind: 'choice', choiceId: grant.id })}
				/>
			{/each}
		</div>
	{:else if request.kind === 'external_action'}
		<!--
			`instructions` is the whole content of this input type and was on the
			schema, unread, in every surface. Without it the reader got a bare
			textarea under a prompt that assumed they already knew what to go and do.
		-->
		{#if request.externalAction?.instructions}
			<p class="hitl-instructions">{request.externalAction.instructions}</p>
		{/if}
		<TextArea
			rows={3}
			bind:value={textValue}
			label="Anything to add (optional)"
			placeholder={request.placeholder ?? 'Optional note about what you did…'}
			ariaLabel="Note"
		/>
		<div class="hitl-decision">
			<Button
				label={request.externalAction?.doneLabel ?? "I've completed this"}
				variant="primary"
				size="sm"
				disabled={busy}
				on:click={() =>
					answer({
						kind: 'choice',
						choiceId: 'completed',
						input: textValue.trim() ? textValue : undefined
					})}
			/>
		</div>
	{:else if request.kind === 'choice' && request.choices}
		<div class="choice-list" role="radiogroup">
			{#each request.choices as choice (choice.id)}
				<label class="choice-row">
					<input
						type="radio"
						name="attention-choice"
						value={choice.id}
						disabled={busy}
						bind:group={selectedChoiceId}
					/>
					<span class="choice-copy">
						<strong>{choice.label}</strong>
						{#if choice.description}
							<span class="choice-desc">{choice.description}</span>
						{/if}
						{#if choice.requiresInput}
							<span class="choice-flag">requires input</span>
						{/if}
					</span>
				</label>
			{/each}
		</div>
		{#if needsChoiceInput}
			<TextArea
				label={`Input for "${selectedChoice?.label ?? ''}"`}
				rows={3}
				bind:value={choiceInputValue}
				placeholder={request.placeholder ?? 'Type your response…'}
			/>
		{/if}
	{:else if request.kind === 'multi_choice' && request.choices}
		{@const constraint = multiChoiceConstraintLabel(request)}
		{#if constraint}
			<div class="choice-constraint" aria-live="polite">
				{constraint} · {selectedMultiChoiceIds.length} selected
			</div>
		{/if}
		<div class="choice-list" role="group" aria-label="Select one or more">
			{#each request.choices as choice (choice.id)}
				{@const checked = selectedMultiChoiceIds.includes(choice.id)}
				{@const atCap =
					!checked &&
					typeof request.maxSelections === 'number' &&
					request.maxSelections > 0 &&
					selectedMultiChoiceIds.length >= request.maxSelections}
				<label class="choice-row" class:choice-row--at-cap={atCap}>
					<input
						type="checkbox"
						name="attention-multi-choice"
						value={choice.id}
						{checked}
						disabled={busy || atCap}
						on:change={() => toggleMultiChoice(choice.id)}
					/>
					<span class="choice-copy">
						<strong>{choice.label}</strong>
						{#if choice.description}
							<span class="choice-desc">{choice.description}</span>
						{/if}
					</span>
				</label>
			{/each}
		</div>
	{:else if request.kind === 'multiline' || request.kind === 'guidance'}
		<TextArea
			rows={6}
			bind:value={textValue}
			disabled={busy}
			placeholder={request.placeholder ?? 'Type your response… (⌘/Ctrl+Enter to submit)'}
			ariaLabel="Response"
		/>
	{:else if request.kind === 'password'}
		<Input
			type="password"
			bind:value={textValue}
			disabled={busy || secretExpired}
			placeholder={request.placeholder ?? 'Enter password…'}
			ariaLabel="Password"
			autocomplete="off"
		/>
	{:else if request.kind === 'otp'}
		<Input
			type="password"
			bind:value={textValue}
			disabled={busy || secretExpired}
			placeholder={request.placeholder ?? 'Enter the code…'}
			ariaLabel="Verification code"
			autocomplete="one-time-code"
		/>
	{:else if request.kind === 'form'}
		{#each request.formQuestions ?? [] as question (question.id)}
			<div class="form-question">
				<div class="form-question__head">
					<p class="form-question__prompt">{question.prompt}</p>
					<button
						type="button"
						class="form-skip"
						disabled={busy}
						on:click={() => {
							formSkipped = { ...formSkipped, [question.id]: !formSkipped[question.id] };
						}}
					>
						{formSkipped[question.id] ? 'Unskip' : 'Skip'}
					</button>
				</div>
				{#if !formSkipped[question.id]}
					<Input
						type={isMaskedField(question.sensitive) ? 'password' : 'text'}
						value={formValues[question.id] ?? ''}
						disabled={busy || secretExpired}
						placeholder={question.sensitive === 'otp'
							? 'Enter the code…'
							: isMaskedField(question.sensitive)
								? 'Kept private'
								: 'Your answer'}
						ariaLabel={question.prompt}
						autocomplete={question.sensitive === 'otp'
							? 'one-time-code'
							: question.sensitive
								? 'off'
								: undefined}
						on:change={(event) => {
							// `Input` is a component: it dispatches `change` with the value
							// and forwards no DOM event, so an `on:input` here never fired
							// and no form could be submitted from this surface.
							formValues = { ...formValues, [question.id]: event.detail.value };
						}}
					/>
					{#if question.sensitive === 'login_identifier'}
						<p class="sensitive-note">Kept private: used only to sign in, never shown to the assistant.</p>
					{/if}
				{/if}
			</div>
		{/each}
		<button
			type="button"
			class="form-skip-all"
			disabled={busy}
			on:click={() => {
				const questions = request.formQuestions ?? [];
				formSkipped = Object.fromEntries(questions.map((question) => [question.id, true]));
				answer({
					kind: 'form',
					answers: questions.map((question) => ({
						id: question.id,
						skipped: true,
						value: undefined
					}))
				});
			}}
		>
			Skip all
		</button>
	{:else if request.kind === 'file_path'}
		<Input
			type="text"
			bind:value={textValue}
			disabled={busy}
			label={filePathLabel}
			placeholder={request.placeholder ?? '/path/to/file'}
			ariaLabel={filePathLabel}
		/>
	{:else}
		<Input
			type="text"
			bind:value={textValue}
			disabled={busy}
			placeholder={request.placeholder ?? 'Type your response…'}
			ariaLabel="Response"
		/>
	{/if}
</div>

<style>
	.hitl-fields {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.sensitive-banner {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		font-size: 0.75rem;
		color: var(--text-secondary);
		padding: 6px 8px;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
	}

	.sensitive-banner[data-expired='true'] {
		color: var(--text-body);
	}

	.sensitive-banner__deadline {
		margin-left: auto;
		font-variant-numeric: tabular-nums;
	}

	.sensitive-banner__fresh {
		margin-left: auto;
		font: inherit;
		cursor: pointer;
		background: none;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		color: var(--text-body);
		padding: 2px 8px;
	}

	.sensitive-note {
		margin: 0;
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.form-question {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.form-question__head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.5rem;
	}
	.form-question__prompt {
		margin: 0;
		font-size: 0.85rem;
		line-height: 1.4;
	}
	.form-skip {
		background: none;
		border: 0;
		color: var(--text-secondary);
		cursor: pointer;
		font-size: 0.75rem;
		padding: 0;
	}
	.form-skip-all {
		align-self: flex-end;
		background: none;
		border: 0;
		color: var(--text-secondary);
		cursor: pointer;
		font-size: 0.75rem;
		padding: 0;
	}

	.hitl-hint {
		margin: 0;
		font-size: 0.78rem;
		line-height: 1.45;
		color: var(--text-secondary);
		white-space: pre-wrap;
	}

	.hitl-instructions {
		margin: 0;
		padding: 0.65rem 0.75rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
		color: var(--text-primary);
		font-size: 0.85rem;
		line-height: 1.5;
		white-space: pre-wrap;
	}

	/* The row of controls that *are* the answer, for the four shapes that carry
	   their own. Right-aligned like the modal footer they used to live in, so a
	   reader's eye finds the decision in the same place either way. */
	.hitl-decision {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	/* ── the grant block ──────────────────────────────────────────────────
	   Banded in the attention colour and outlined, which no other field here
	   is. A reader skimming an act body must not approve a sandbox escape the
	   way they answer "which quarter?", and the only thing that stops them is
	   this block not looking like a question. */
	.hitl-grant {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		padding: 0.7rem 0.8rem;
		border: 1px solid color-mix(in srgb, var(--status-attention, currentColor) 45%, transparent);
		border-radius: 10px;
		background: var(--status-attention-soft, var(--bg-soft));
	}

	.hitl-grant__label {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.62rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.16em;
		color: color-mix(in srgb, var(--status-attention, currentColor) 60%, var(--text-primary));
	}

	/* Verbatim, in the mono face, wrapping rather than truncating: a command the
	   reader can only see half of is a command they cannot judge. */
	.hitl-grant__subject {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.8rem;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.hitl-grant__detail {
		margin: 0;
		font-size: 0.78rem;
		line-height: 1.45;
		color: var(--text-secondary);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.hitl-grant__roots {
		margin: 0;
		padding-left: 1.1rem;
		font-family: var(--font-mono);
		font-size: 0.72rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.diff-approval {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-height: min(68vh, 720px);
		overflow: auto;
		padding-right: 0.15rem;
	}

	.diff-approval__rationale {
		margin: 0;
		padding: 0.65rem 0.75rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		font-size: 0.82rem;
		line-height: 1.45;
		white-space: pre-wrap;
	}

	.choice-list {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	.choice-row {
		display: flex;
		align-items: flex-start;
		gap: 0.55rem;
		padding: 0.55rem 0.75rem;
		border-radius: 10px;
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		cursor: pointer;
		transition:
			background 140ms ease,
			border-color 140ms ease;
	}

	.choice-row:hover {
		background: var(--bg-soft);
	}

	.choice-row:has(input:checked) {
		border-color: var(--accent-primary);
		background: var(--accent-primary-soft);
	}

	.choice-row input[type='radio'],
	.choice-row input[type='checkbox'] {
		margin-top: 0.18rem;
	}

	.choice-row--at-cap {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.choice-constraint {
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.7rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.12em;
		color: var(--text-secondary);
	}

	.choice-copy {
		display: flex;
		flex-direction: column;
		gap: 0.18rem;
		font-size: 0.85rem;
		color: var(--text-primary);
	}

	.choice-desc {
		font-size: 0.74rem;
		color: var(--text-secondary);
	}

	.choice-flag {
		font-size: 0.65rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--accent-primary);
	}
</style>
