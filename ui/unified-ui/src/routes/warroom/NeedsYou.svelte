<script lang="ts">
	/**
	 * NEEDS YOU — the deck's left rail: the live human-input queue.
	 *
	 * This is the panel that makes the deck a tool instead of a screensaver:
	 * when the system is blocked on a person, the block is HERE, aged and
	 * actionable. Confirmation-shaped requests resolve inline through the
	 * canonical `/v2/hitl/{id}/respond`; everything richer deep-links to
	 * `/attention`, which owns the full input surfaces (choice, diff, file).
	 */
	import { goto } from '$app/navigation';
	import type { HitlRequest } from '$lib/hitl/types';
	import { postHitlResponse } from '$lib/hitl/adapters';
	import { fmtAge } from './deck';

	export let requests: HitlRequest[] = [];
	export let headers: Record<string, string> = {};
	export let nowMs = Date.now();
	/** Called after a successful inline resolve so the page can refresh. */
	export let onResolved: (id: string) => void = () => {};

	let busyId: string | null = null;
	let failedId: string | null = null;

	const SOURCE_LABEL: Record<string, string> = {
		agentic: 'AGENT',
		user_request: 'REQUEST',
		approval: 'APPROVAL',
		plan_approval: 'PLAN',
		clarification: 'CLARIFY',
		escalation: 'ESCALATION',
		diff_approval: 'DIFF',
		service_health: 'SERVICE',
		bot_auth: 'AUTH'
	};

	async function confirm(request: HitlRequest, confirmed: boolean): Promise<void> {
		busyId = request.id;
		failedId = null;
		try {
			const outcome = await postHitlResponse(request, { type: 'confirmation', confirmed }, headers);
			if (outcome.ok) onResolved(request.id);
			else failedId = request.id;
		} catch {
			failedId = request.id;
		} finally {
			busyId = null;
		}
	}

	function open(request: HitlRequest): void {
		void goto(`/attention#${encodeURIComponent(request.id)}`);
	}
</script>

<section class="rail" aria-label="Requests needing you">
	<h2 class="rail-title"><span class="tick" aria-hidden="true"></span>NEEDS YOU
		{#if requests.length > 0}<span class="count">{requests.length}</span>{/if}
	</h2>

	{#if requests.length === 0}
		<!-- One strip, not a monument. This panel is empty most of the time,
		     and the grid row is `auto`: an empty queue costs ~56px, giving the
		     vertical space back to IN FLIGHT below. -->
		<div class="clear">
			<span class="clear-glyph" aria-hidden="true">◇</span>
			<p>ALL CLEAR<small> — nothing waiting on you</small></p>
		</div>
	{:else}
		<ol class="queue">
			{#each requests as request (request.id)}
				<li class="item" data-failed={failedId === request.id}>
					<div class="item-head">
						<span class="source">{SOURCE_LABEL[request.source] ?? request.source.toUpperCase()}</span>
						{#if request.at}<span class="age">{fmtAge(request.at, nowMs)}</span>{/if}
					</div>
					<p class="prompt">{request.prompt || '(no prompt text)'}</p>
					<div class="actions">
						{#if request.input_type === 'confirmation'}
							<button
								class="act act--yes"
								disabled={busyId === request.id}
								on:click={() => confirm(request, true)}
							>APPROVE</button>
							<button
								class="act act--no"
								disabled={busyId === request.id}
								on:click={() => confirm(request, false)}
							>DENY</button>
						{:else}
							<button class="act" on:click={() => open(request)}>OPEN ↗</button>
						{/if}
						{#if failedId === request.id}<span class="fail">resolve failed — use OPEN</span>{/if}
					</div>
				</li>
			{/each}
		</ol>
	{/if}
</section>

<style>
	.rail {
		grid-area: needs;
		/* Above the deck's scanline film — see `.scan` in +page.svelte. */
		z-index: 1;
		display: flex;
		flex-direction: column;
		border-left: 1px solid var(--deck-line);
		border-bottom: 1px solid var(--deck-line);
		min-height: 0;
		/* The grid row is `auto`: cap it so a deep queue scrolls here rather
		   than squeezing IN FLIGHT out of the column. */
		max-height: min(44vh, 460px);
		background: linear-gradient(270deg, color-mix(in srgb, var(--deck-glow) 3%, transparent), transparent 40%);
	}
	.rail-title {
		display: flex; align-items: center; gap: 8px;
		margin: 0; padding: 14px 16px 10px;
		font: 600 11px/1 var(--font-display);
		letter-spacing: 0.3em;
		color: var(--deck-dim);
	}
	.tick { width: 14px; height: 2px; background: var(--deck-glow); }
	.count {
		margin-left: auto;
		font: 700 12px/1 var(--font-data);
		color: var(--sev-hitl);
		padding: 3px 7px;
		border: 1px solid color-mix(in srgb, var(--sev-hitl) 45%, transparent);
	}

	.clear {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 0 16px 14px;
		color: var(--deck-dim);
	}
	.clear-glyph { font-size: 14px; color: color-mix(in srgb, var(--deck-glow) 55%, transparent); }
	.clear p { margin: 0; font: 600 10px/1 var(--font-display); letter-spacing: 0.26em; }
	.clear small { font: 400 9px/1 var(--font-data); letter-spacing: 0.08em; text-transform: none; }
	.clear small { font: 400 11px/1.4 var(--font-data); }

	.queue {
		list-style: none;
		margin: 0; padding: 2px 12px 14px;
		overflow-y: auto;
		display: flex; flex-direction: column; gap: 10px;
		min-height: 0;
		scrollbar-width: thin;
	}
	.item {
		border: 1px solid color-mix(in srgb, var(--sev-hitl) 30%, transparent);
		border-left: 3px solid var(--sev-hitl);
		padding: 10px 12px;
		background: color-mix(in srgb, var(--sev-hitl) 5%, transparent);
		animation: item-in 360ms cubic-bezier(0.2, 0.9, 0.3, 1);
	}
	.item[data-failed='true'] { border-color: var(--sev-err); }
	@keyframes item-in {
		from { transform: translateX(-10px); opacity: 0; }
		to { transform: none; opacity: 1; }
	}
	.item-head { display: flex; justify-content: space-between; margin-bottom: 6px; }
	.source { font: 700 9px/1 var(--font-data); letter-spacing: 0.2em; color: var(--sev-hitl); }
	.age { font: 500 10px/1 var(--font-data); color: var(--deck-dim); font-variant-numeric: tabular-nums; }
	.prompt {
		margin: 0 0 8px;
		font: 400 12px/1.45 var(--font-body, inherit);
		color: var(--deck-text);
		display: -webkit-box;
		-webkit-line-clamp: 3;
		line-clamp: 3;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}
	.actions { display: flex; gap: 8px; align-items: center; }
	.act {
		font: 600 10px/1 var(--font-data);
		letter-spacing: 0.14em;
		padding: 6px 10px;
		background: transparent;
		border: 1px solid var(--deck-line);
		color: var(--deck-text);
		cursor: pointer;
		transition: background 150ms, border-color 150ms;
	}
	.act:hover:not(:disabled) { background: color-mix(in srgb, var(--deck-glow) 14%, transparent); }
	.act:disabled { opacity: 0.4; cursor: progress; }
	.act--yes { border-color: color-mix(in srgb, var(--sev-ok) 55%, transparent); color: var(--sev-ok); }
	.act--yes:hover:not(:disabled) { background: color-mix(in srgb, var(--sev-ok) 14%, transparent); }
	.act--no { border-color: color-mix(in srgb, var(--sev-err) 45%, transparent); color: var(--sev-err); }
	.act--no:hover:not(:disabled) { background: color-mix(in srgb, var(--sev-err) 12%, transparent); }
	.fail { font: 400 10px/1.2 var(--font-data); color: var(--sev-err); }
</style>
