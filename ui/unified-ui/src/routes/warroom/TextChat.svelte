<script lang="ts">
	/**
	 * TEXT CHANNEL — the deck's left rail.
	 *
	 * The rail used to hold NEEDS YOU, which is empty most of the time; a
	 * permanently-blank third of the screen is worse than no panel. Text chat
	 * earns the space because it always has something to show, and it keeps the
	 * centre free for voice.
	 */
	export let bubbles: Array<{ id: string; role: 'user' | 'assistant'; text: string; voice?: boolean }> = [];
	export let sessionLabel = '';
	export let canSend = false;
	export let sending = false;
	export let chatError: string | null = null;
	export let onSend: (text: string) => void = () => {};
	/** Opens the history drawer (search / pagination / personal vs automated
	 *  — the same surface the whole app uses). */
	export let onOpenHistory: () => void = () => {};

	let draft = '';
	let scroller: HTMLElement | null = null;

	function submit(): void {
		const text = draft.trim();
		if (!text || !canSend || sending) return;
		draft = '';
		onSend(text);
	}

	/**
	 * Pin to the newest message.
	 *
	 * `requestAnimationFrame`, never `tick()`. Calling `tick()` from a reactive
	 * statement re-enters Svelte's flush scheduler from the microtask it just
	 * queued, re-running the statement forever — a silent 100% CPU hang that
	 * `effect_update_depth_exceeded` does not catch, because each iteration is
	 * a separate flush. rAF schedules outside the scheduler. The guard lives on
	 * a plain object so writing it is untracked. `deck.test.ts` enforces this.
	 */
	const mark = { count: -1 };
	$: if (scroller && bubbles.length !== mark.count) {
		mark.count = bubbles.length;
		requestAnimationFrame(() => {
			if (scroller) scroller.scrollTop = scroller.scrollHeight;
		});
	}
</script>

<section class="chat" aria-label="Text channel">
	<!-- The header IS the channel identity — thread, then session name — and
	     clicking it opens the app's history drawer (search, pagination,
	     personal vs automated: full parity, same component). -->
	<button class="head" on:click={onOpenHistory} title="Open history" aria-haspopup="dialog">
		<span class="rule" aria-hidden="true"></span>
		<h2>{sessionLabel || 'TEXT CHANNEL'}</h2>
		<span class="head-caret" aria-hidden="true">▾</span>
	</button>

	<div class="talk" bind:this={scroller}>
		{#if bubbles.length === 0}
			<p class="empty">no messages yet</p>
		{:else}
			{#each bubbles as bubble (bubble.id)}
				<div class="bubble" data-role={bubble.role} data-voice={bubble.voice ?? false}>
					<span class="tag">{bubble.voice ? '≋ ' : ''}{bubble.role === 'user' ? 'YOU' : 'AGENT'}</span>
					<p>{bubble.text}</p>
				</div>
			{/each}
		{/if}
	</div>

	<form class="command" on:submit|preventDefault={submit}>
		<span class="glyph" aria-hidden="true">▸</span>
		<input
			class="input"
			type="text"
			bind:value={draft}
			placeholder={canSend ? 'type a command…' : 'channel offline'}
			disabled={!canSend || sending}
			aria-label="Command input"
		/>
		<button class="send" type="submit" disabled={!canSend || sending || !draft.trim()}>
			{sending ? '…' : 'SEND'}
		</button>
	</form>

	{#if chatError}
		<span class="session session--bad">channel: {chatError}</span>
	{:else if !canSend}
		<span class="session">channel: connecting…</span>
	{/if}
</section>

<style>
	.chat {
		grid-area: chat;
		z-index: 1;
		display: flex;
		flex-direction: column;
		gap: 10px;
		min-height: 0;
		min-width: 0;
		padding: 16px 18px 14px;
		border-right: 1px solid var(--deck-line);
	}

	.head {
		display: flex;
		align-items: center;
		gap: 9px;
		flex: none;
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		min-width: 0;
		text-align: left;
	}
	.head:hover h2 { color: var(--deck-glow); }
	.head:hover .head-caret { color: var(--deck-glow); }

	.head-caret {
		font-size: 8px;
		color: var(--deck-dim);
		flex: none;
	}

	.rule {
		width: 18px;
		height: 1px;
		background: var(--deck-glow);
	}

	h2 {
		margin: 0;
		font: 700 10px/1 var(--font-display);
		letter-spacing: 0.22em;
		color: var(--deck-dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		min-width: 0;
		transition: color 120ms ease;
	}

	.talk {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		/* Fade the top edge. A half-scrolled line hard-clipped against the
		   panel header reads as two rows of text colliding; fading it makes
		   the scroll obvious instead. */
		mask-image: linear-gradient(to bottom, transparent 0, #000 14px);
		-webkit-mask-image: linear-gradient(to bottom, transparent 0, #000 14px);
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding-right: 4px;
	}

	.empty {
		margin: auto 0;
		font: 400 11px/1 var(--font-data);
		letter-spacing: 0.1em;
		color: var(--deck-dim);
	}

	.bubble {
		max-width: 94%;
	}

	.bubble[data-role='user'] {
		align-self: flex-end;
		text-align: right;
	}

	.tag {
		font: 700 8px/1 var(--font-data);
		letter-spacing: 0.24em;
		color: var(--deck-dim);
	}

	.bubble[data-role='user'] .tag {
		color: var(--deck-glow);
	}

	/* Voice turns wear the same speaker code as the stage caption and the
	   orb: green = you, accent = agent. The ≋ glyph marks the channel. */
	.bubble[data-voice='true'][data-role='user'] .tag,
	.bubble[data-voice='true'][data-role='user'] p {
		color: color-mix(in srgb, var(--sev-ok) 70%, var(--deck-text));
	}
	.bubble[data-voice='true'][data-role='assistant'] p {
		color: color-mix(in srgb, var(--deck-glow) 55%, var(--deck-text));
	}

	.bubble p {
		margin: 4px 0 0;
		font: 400 12px/1.5 var(--font-body);
		color: var(--deck-text);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.command {
		flex: none;
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		gap: 8px;
		border: 1px solid var(--deck-line);
		padding: 3px 8px;
	}

	.glyph {
		font: 700 11px/1 var(--font-data);
		color: var(--deck-glow);
	}

	.input {
		background: transparent;
		border: none;
		outline: none;
		min-width: 0;
		padding: 4px 0;
		font: 400 11.5px/1.2 var(--font-data);
		color: var(--deck-text);
	}

	.input::placeholder {
		color: var(--deck-dim);
	}

	.send {
		background: transparent;
		border: none;
		cursor: pointer;
		font: 700 9px/1 var(--font-display);
		letter-spacing: 0.2em;
		color: var(--deck-glow);
	}

	.send:disabled {
		color: var(--deck-dim);
		cursor: not-allowed;
	}

	.session {
		flex: none;
		font: 400 9px/1.3 var(--font-data);
		letter-spacing: 0.06em;
		color: var(--deck-dim);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.session--bad {
		color: var(--sev-err);
	}
</style>
