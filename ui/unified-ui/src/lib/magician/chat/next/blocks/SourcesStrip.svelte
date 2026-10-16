<script lang="ts">
	import { isSafeHttpUrl } from '$lib/magician/structuredResponse/schema';
	import type {
		StructuredResponseActionV1,
		StructuredSourceRefV1,
		StructuredSourcesBlockV1
	} from '$lib/magician/structuredResponse/types';
	import { tintVars } from '../tint';

	/** Chips shown before the "+N" expander. */
	const VISIBLE = 6;

	export let block: StructuredSourcesBlockV1;
	export let onAction: ((action: StructuredResponseActionV1) => void | Promise<void>) | null = null;

	let expanded = false;

	interface Chip {
		label: string;
		host: string;
		letter: string;
		href: string | null;
	}

	// Lettermark, never a favicon: fetching favicons from a third party would
	// leak every source the user reads, and there is no local favicon endpoint.
	function toChip(source: StructuredSourceRefV1): Chip {
		const href = isSafeHttpUrl(source.href) ? source.href : null;
		let host = source.label;
		if (href) {
			try {
				host = new URL(href).hostname.replace(/^www\./i, '') || source.label;
			} catch {
				host = source.label;
			}
		}
		return { label: source.label, host, letter: (host.charAt(0) || '?').toUpperCase(), href };
	}

	function open(event: MouseEvent, chip: Chip): void {
		if (!onAction || !chip.href) return;
		event.preventDefault();
		void onAction({ kind: 'open_url', label: chip.label, url: chip.href });
	}

	$: chips = block.items.map(toChip);
	$: shown = expanded ? chips : chips.slice(0, VISIBLE);
	$: hidden = chips.length - shown.length;
</script>

<section class="cn-sources" data-next-block="sources" aria-label={block.title ?? 'Sources'}>
	{#if block.title}<h4 class="cn-sources__title">{block.title}</h4>{/if}
	<div class="cn-sources__row">
		<span class="cn-sources__stack" aria-hidden="true">
			{#each chips.slice(0, VISIBLE) as chip, i}
				<span class="cn-lettermark cn-lettermark--stacked" style="{tintVars(chip.host)} --i: {i};"
					>{chip.letter}</span
				>
			{/each}
		</span>
		<ul class="cn-sources__chips">
			{#each shown as chip}
				<li>
					{#if chip.href}
						<a
							class="cn-chip"
							href={chip.href}
							target="_blank"
							rel="noopener noreferrer"
							title={chip.label}
							aria-label="{chip.label} ({chip.host})"
							on:click={(event) => open(event, chip)}
						>
							<span class="cn-lettermark" style={tintVars(chip.host)} aria-hidden="true">{chip.letter}</span>
							<span class="cn-chip__host">{chip.host}</span>
						</a>
					{:else}
						<span class="cn-chip cn-chip--inert" title={chip.label}>
							<span class="cn-lettermark" style={tintVars(chip.host)} aria-hidden="true">{chip.letter}</span>
							<span class="cn-chip__host">{chip.label}</span>
						</span>
					{/if}
				</li>
			{/each}
			{#if hidden > 0}
				<li>
					<button
						type="button"
						class="cn-chip cn-chip--more"
						aria-expanded={expanded}
						aria-label="+{hidden} more sources"
						on:click={() => (expanded = true)}
					>
						+{hidden}
					</button>
				</li>
			{/if}
		</ul>
	</div>
</section>

<style>
	.cn-sources {
		display: grid;
		gap: 0.4rem;
	}

	.cn-sources__title {
		margin: 0;
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.cn-sources__row {
		display: flex;
		align-items: flex-start;
		gap: 0.6rem;
	}

	.cn-sources__stack {
		display: inline-flex;
		flex: none;
		padding-top: 0.2rem;
	}

	.cn-lettermark {
		display: inline-grid;
		place-items: center;
		flex: none;
		width: 1.35rem;
		height: 1.35rem;
		border-radius: 50%;
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
		font-family: var(--font-display);
		font-size: var(--text-xs);
		font-weight: 700;
		line-height: 1;
	}

	.cn-lettermark--stacked {
		box-shadow: 0 0 0 2px var(--bg-card);
		transition: margin var(--dur-base) var(--spring-snappy);
	}

	.cn-lettermark--stacked + .cn-lettermark--stacked {
		margin-inline-start: -0.5rem;
	}

	/* Fan the stack out while the strip is hovered or a chip has focus. */
	.cn-sources__row:hover .cn-lettermark--stacked + .cn-lettermark--stacked,
	.cn-sources__row:focus-within .cn-lettermark--stacked + .cn-lettermark--stacked {
		margin-inline-start: 0.15rem;
	}

	.cn-sources__chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		margin: 0;
		padding: 0;
		list-style: none;
		min-width: 0;
	}

	.cn-chip {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		max-width: 16rem;
		padding: 0.15rem 0.6rem 0.15rem 0.2rem;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-chip);
		background: var(--surface-glass);
		color: var(--text-primary);
		font-size: var(--text-xs);
		text-decoration: none;
		transition:
			transform var(--dur-base) var(--spring-snappy),
			box-shadow var(--dur-base) var(--spring-snappy);
	}

	a.cn-chip:hover {
		transform: translateY(-1px);
		box-shadow: var(--shadow-float);
	}

	.cn-chip:focus-visible {
		outline: none;
		box-shadow: var(--ring-focus);
	}

	.cn-chip--inert {
		color: var(--text-muted);
	}

	.cn-chip--more {
		padding-inline: 0.6rem;
		font-weight: 650;
		cursor: pointer;
	}

	.cn-chip__host {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
