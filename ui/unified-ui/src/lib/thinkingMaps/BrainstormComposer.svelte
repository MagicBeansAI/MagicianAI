<!--
  BrainstormComposer — the capture + AI-actions bar for the Live Thinking Map
  detail surface.

  Three ways to grow the map:
    · Add       — build an OWNER `add_node` op client-side (a fresh uuid node,
                  a chosen kind, `owner_spoken` origin) and apply it via
                  `applyOperations`. If a node is selected, the new node's
                  `parent_id` is set to it so captures grow the active branch.
    · Interpret — LLM-fold the composer text (or the selected node's label) into
                  the map with `intent: 'continue_thinking'`.
    · Break open — the same, with `intent: 'break_open'` (surface tensions /
                  alternatives). Both add provisional / model_inferred nodes the
                  canvas renders dashed.

  This component OWNS no map state: it calls up through the passed-in async
  handlers (which live on the page next to the sharedPoll) and reports its own
  in-progress / error / note text. `busy` is shared with the page so the whole
  action set locks together during any mutation.
-->
<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type {
		InterpretIntent,
		MapOperation,
		NodeKind,
		ThinkingNode
	} from '$lib/types/thinkingMap';
	import Button from '$lib/magician/components/native/Button.svelte';

	/** The selected node id (if any) — new captures parent under it. */
	export let selectedNodeId = '';
	/** Its label — the default text `interpret` folds when the composer is empty. */
	export let selectedNodeLabel = '';
	/** Shared with the page so every AI action locks together while one runs. */
	export let busy = false;

	/**
	 * Handlers wired on the page (next to the sharedPoll). Each throws on failure;
	 * we surface the message. `applyOps` returns nothing meaningful here — the
	 * poll reconciles the canvas.
	 */
	export let applyOps: (operations: MapOperation[]) => Promise<void>;
	export let runInterpret: (text: string, intent: InterpretIntent) => Promise<'ok' | 'empty'>;

	const dispatch = createEventDispatcher<{ mutated: void }>();

	/** The 11 first-class node kinds, `idea` first (the capture default). */
	const KINDS: NodeKind[] = [
		'idea',
		'question',
		'fact',
		'decision',
		'option',
		'risk',
		'action',
		'metric',
		'assumption',
		'evidence',
		'group'
	];

	let text = '';
	let kind: NodeKind = 'idea';
	let localError: string | null = null;
	let note: string | null = null;

	function uuid(): string {
		if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
			return crypto.randomUUID();
		}
		// Extremely defensive fallback (browsers here all have crypto.randomUUID).
		return `n-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
	}

	function buildAddNodeOp(label: string): MapOperation {
		const now = new Date().toISOString();
		const node: ThinkingNode = {
			node_id: uuid(),
			kind,
			label,
			epistemic_state: 'asserted',
			assertion_origin: 'owner_spoken',
			confidence: 0.9,
			source_refs: [],
			position_locked: false,
			promoted_refs: [],
			tombstoned: false,
			created_at: now,
			updated_at: now,
			// Grow the active branch when a node is selected; otherwise a root.
			...(selectedNodeId ? { parent_id: selectedNodeId } : {})
		};
		return { op: 'add_node', node };
	}

	async function onAdd(): Promise<void> {
		const label = text.trim();
		if (!label || busy) return;
		localError = null;
		note = null;
		try {
			await applyOps([buildAddNodeOp(label)]);
			text = '';
			dispatch('mutated');
		} catch (err) {
			localError = err instanceof Error ? err.message : String(err);
		}
	}

	async function onInterpret(intent: InterpretIntent): Promise<void> {
		// Prefer the typed text; fall back to the selected node's label so
		// "Continue" / "Break open" work as one-tap actions on a node.
		const source = text.trim() || selectedNodeLabel.trim();
		if (!source || busy) return;
		localError = null;
		note = null;
		try {
			const outcome = await runInterpret(source, intent);
			if (outcome === 'empty') {
				note = 'AI had nothing to add.';
			} else {
				text = '';
			}
			dispatch('mutated');
		} catch (err) {
			localError = err instanceof Error ? err.message : String(err);
		}
	}

	function onKeydown(e: KeyboardEvent): void {
		if (e.key === 'Enter' && !e.shiftKey) {
			e.preventDefault();
			void onAdd();
		}
	}

	$: canAct = !busy && (text.trim().length > 0 || selectedNodeLabel.trim().length > 0);
	$: canAdd = !busy && text.trim().length > 0;
	$: parentHint = selectedNodeId
		? `under “${selectedNodeLabel || 'selected node'}”`
		: 'as a new root';
</script>

<div class="bc" aria-label="Capture composer">
	<div class="bc__row">
		<select
			class="bc__kind"
			bind:value={kind}
			disabled={busy}
			title="Node kind for the captured thought"
			aria-label="Node kind"
		>
			{#each KINDS as k}
				<option value={k}>{k}</option>
			{/each}
		</select>

		<input
			class="bc__input"
			type="text"
			placeholder="Capture a thought…"
			bind:value={text}
			on:keydown={onKeydown}
			disabled={busy}
			aria-label="Thought to capture or interpret"
		/>

		<Button
			variant="primary"
			size="sm"
			label={busy ? 'Working…' : 'Add'}
			interactive={canAdd}
			on:click={onAdd}
		/>
	</div>

	<div class="bc__row bc__row--ai">
		<span class="bc__hint">{parentHint}</span>
		<span class="bc__spacer"></span>
		<Button
			variant="outline"
			size="sm"
			label="✦ Continue"
			title="Fold this into the map (continue_thinking)"
			interactive={canAct}
			on:click={() => onInterpret('continue_thinking')}
		/>
		<Button
			variant="outline"
			size="sm"
			label="✦ Break open"
			title="Surface tensions / alternatives (break_open)"
			interactive={canAct}
			on:click={() => onInterpret('break_open')}
		/>
	</div>

	{#if note}
		<div class="bc__note" role="status">{note}</div>
	{/if}
	{#if localError}
		<div class="bc__error" role="alert">{localError}</div>
	{/if}
</div>

<style>
	.bc {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		padding: 0.85rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		transition: border-color var(--transition-base, 0.25s ease), box-shadow var(--transition-base, 0.25s ease);
	}

	.bc:focus-within {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 40%, var(--border-soft));
		box-shadow: var(--shadow-md);
	}

	.bc__row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		flex-wrap: wrap;
	}

	.bc__row--ai {
		gap: 0.4rem;
	}

	.bc__kind {
		flex-shrink: 0;
		padding: 0.45rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.78rem;
		text-transform: capitalize;
		cursor: pointer;
		transition: border-color var(--transition-fast, 0.15s ease), background var(--transition-fast, 0.15s ease);
	}

	.bc__kind:hover {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 35%, var(--border-soft));
	}

	.bc__input {
		flex: 1;
		min-width: 10rem;
		padding: 0.5rem 0.7rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.85rem;
		transition: border-color var(--transition-fast, 0.15s ease), background var(--transition-fast, 0.15s ease);
	}

	.bc__input::placeholder {
		color: var(--text-faint);
	}

	.bc__input:hover {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 30%, var(--border-soft));
	}

	.bc__input:focus-visible,
	.bc__kind:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.bc__input:focus {
		background: var(--bg-card);
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 45%, var(--border-soft));
	}

	.bc__hint {
		display: inline-flex;
		align-items: center;
		padding: 0.15rem 0.55rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 8%, transparent);
		font-size: 0.72rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.bc__spacer {
		flex: 1;
	}

	.bc__note {
		font-size: 0.78rem;
		color: var(--color-info, var(--text-secondary));
		padding: 0.35rem 0.6rem;
		border-radius: var(--radius-sm, 6px);
		background: var(--color-info-soft, color-mix(in srgb, var(--color-info) 12%, transparent));
	}

	.bc__error {
		padding: 0.4rem 0.6rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.78rem;
	}
</style>
