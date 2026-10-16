<script lang="ts">
	import MiddleEllipsis from '$lib/magician/components/chat/MiddleEllipsis.svelte';
	import { tick } from 'svelte';

	import { isSafeHttpUrl } from '$lib/magician/structuredResponse/schema';
	import type {
		StructuredArtifactRefV1,
		StructuredArtifactsBlockV1,
		StructuredResponseActionV1
	} from '$lib/magician/structuredResponse/types';
	import { morph, rise } from '../motion';
	import { tintVars } from '../tint';

	export let block: StructuredArtifactsBlockV1;
	export let onAction: ((action: StructuredResponseActionV1) => void | Promise<void>) | null = null;

	/** Enter stagger per card, capped so a long grid does not trail. */
	const STAGGER_MS = 40;
	const MAX_STAGGER_MS = 320;

	type Glyph = 'pdf' | 'table' | 'doc' | 'code' | 'zip' | 'file';
	const GLYPH_TEXT: Record<Glyph, string> = {
		pdf: 'PDF',
		table: 'CSV',
		doc: 'DOC',
		code: '</>',
		zip: 'ZIP',
		file: 'FILE'
	};
	const EXT_GLYPH: Record<string, Glyph> = {
		pdf: 'pdf',
		csv: 'table', tsv: 'table', xls: 'table', xlsx: 'table', ods: 'table',
		doc: 'doc', docx: 'doc', odt: 'doc', rtf: 'doc', txt: 'doc', md: 'doc',
		zip: 'zip', gz: 'zip', tgz: 'zip', tar: 'zip', '7z': 'zip', rar: 'zip',
		js: 'code', ts: 'code', py: 'code', rs: 'code', go: 'code', java: 'code', json: 'code',
		yaml: 'code', yml: 'code', html: 'code', css: 'code', sh: 'code', svelte: 'code', sql: 'code'
	};

	type Kind = 'image' | 'video' | 'audio' | 'file';

	function kindOf(artifact: StructuredArtifactRefV1, src: string | null): Kind {
		// Inline media only from a safe http(s) href, the same check the schema applies.
		if (!src) return 'file';
		const mime = artifact.mime_type?.toLowerCase() ?? '';
		if (mime.startsWith('image/')) return 'image';
		if (mime.startsWith('video/')) return 'video';
		if (mime.startsWith('audio/')) return 'audio';
		return 'file';
	}

	function glyphOf(artifact: StructuredArtifactRefV1): Glyph {
		const mime = artifact.mime_type?.toLowerCase() ?? '';
		if (mime === 'application/pdf') return 'pdf';
		if (/csv|spreadsheet|excel/.test(mime)) return 'table';
		if (/zip|gzip|x-tar|compressed/.test(mime)) return 'zip';
		if (/wordprocessing|msword|opendocument\.text|rtf/.test(mime)) return 'doc';
		if (/json|javascript|typescript|x-python|x-rust|x-sh|yaml|xml|html/.test(mime)) return 'code';
		const name = (artifact.label || artifact.href || '').split(/[?#]/)[0];
		const ext = name.match(/\.([a-z0-9]+)$/i)?.[1]?.toLowerCase() ?? '';
		return EXT_GLYPH[ext] ?? (mime.startsWith('text/') ? 'doc' : 'file');
	}

	function humanSize(bytes: number): string {
		if (bytes < 1024) return `${bytes} B`;
		const units = ['KB', 'MB', 'GB', 'TB'];
		let value = bytes / 1024;
		let unit = 0;
		while (value >= 1024 && unit < units.length - 1) {
			value /= 1024;
			unit++;
		}
		return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
	}

	// Same routing as the classic ArtifactsBlock: the host resolves the action.
	function artifactAction(artifact: StructuredArtifactRefV1): StructuredResponseActionV1 | null {
		if (artifact.href) return { kind: 'open_url', label: artifact.label, url: artifact.href };
		if (artifact.artifact_id) {
			return { kind: 'open_artifact', label: artifact.label, artifact_id: artifact.artifact_id };
		}
		return null;
	}

	$: cards = block.items.map((artifact) => {
		const src = isSafeHttpUrl(artifact.href) ? artifact.href : null;
		return {
			artifact,
			src,
			kind: kindOf(artifact, src),
			glyph: glyphOf(artifact),
			action: artifactAction(artifact)
		};
	});

	// Lightbox
	let lightbox: { src: string; label: string } | null = null;
	let lightboxImg: HTMLImageElement | null = null;
	let closeButton: HTMLButtonElement | null = null;
	let opener: HTMLElement | null = null;

	async function openLightbox(event: MouseEvent, src: string, label: string): Promise<void> {
		opener = event.currentTarget as HTMLElement;
		const thumb = opener.querySelector('img');
		await morph(thumb, () => lightboxImg, async () => {
			lightbox = { src, label };
			await tick();
		});
		closeButton?.focus();
	}

	function closeLightbox(): void {
		lightbox = null;
		opener?.focus();
		opener = null;
	}

	function onKeydown(event: KeyboardEvent): void {
		if (!lightbox) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			closeLightbox();
		} else if (event.key === 'Tab') {
			// The close button is the dialog's only control: keep focus on it.
			event.preventDefault();
			closeButton?.focus();
		}
	}
</script>

<svelte:window on:keydown={onKeydown} />

<section class="cn-artifacts" data-next-block="artifacts">
	{#if block.title}<h4 class="cn-artifacts__title">{block.title}</h4>{/if}
	<ul class="cn-artifacts__grid">
		<!-- Local `rise`: a card animates only when it joins an already-rendered
		     grid. A whole message arriving rises via ChatPanel's message entrance;
		     a history load or session switch renders the grid without replaying. -->
		{#each cards as card, i}
			<li
				class="cn-artifact"
				style={tintVars(card.artifact.label)}
				in:rise={{ delay: Math.min(i * STAGGER_MS, MAX_STAGGER_MS) }}
			>
				{#if card.kind === 'image' && card.src}
					{@const src = card.src}
					<button
						type="button"
						class="cn-artifact__thumb"
						aria-label="View {card.artifact.label}"
						on:click={(event) => openLightbox(event, src, card.artifact.label)}
					>
						<img {src} alt={card.artifact.label} loading="lazy" />
					</button>
				{:else if card.kind === 'video' && card.src}
					<!-- svelte-ignore a11y_media_has_caption -->
					<video class="cn-artifact__media" src={card.src} controls preload="metadata"></video>
				{:else if card.kind === 'audio' && card.src}
					<audio class="cn-artifact__audio" src={card.src} controls preload="metadata"></audio>
				{:else}
					<span class="cn-artifact__glyph" data-glyph={card.glyph} aria-hidden="true"
						>{GLYPH_TEXT[card.glyph]}</span
					>
				{/if}
				<div class="cn-artifact__meta">
					<span class="cn-artifact__label"><MiddleEllipsis text={card.artifact.label} /></span>
					{#if card.artifact.size !== undefined || card.artifact.mime_type}
						<span class="cn-artifact__sub">
							{[
								card.artifact.size !== undefined ? humanSize(card.artifact.size) : '',
								card.artifact.mime_type ?? ''
							]
								.filter(Boolean)
								.join(' · ')}
						</span>
					{/if}
				</div>
				{#if card.action}
					{@const action = card.action}
					<button
						type="button"
						class="cn-artifact__open"
						aria-label="Open {card.artifact.label}"
						disabled={!onAction}
						on:click={() => onAction && void onAction(action)}
					>
						Open
					</button>
				{/if}
			</li>
		{/each}
	</ul>
</section>

{#if lightbox}
	<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
	<div class="cn-lightbox" on:click={closeLightbox}>
		<div role="dialog" aria-modal="true" aria-label={lightbox.label} class="cn-lightbox__frame">
			<img bind:this={lightboxImg} src={lightbox.src} alt={lightbox.label} />
			<button
				bind:this={closeButton}
				type="button"
				class="cn-lightbox__close"
				aria-label="Close preview"
				on:click|stopPropagation={closeLightbox}
			>
				×
			</button>
		</div>
	</div>
{/if}

<style>
	.cn-artifacts {
		display: grid;
		gap: 0.45rem;
	}

	.cn-artifacts__title {
		margin: 0;
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.cn-artifacts__grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
		gap: 0.6rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.cn-artifact {
		display: grid;
		grid-template-rows: auto 1fr auto;
		gap: 0.45rem;
		min-width: 0;
		padding: 0.55rem;
		border-radius: var(--radius-md);
		background: color-mix(in srgb, var(--cn-tint) 45%, var(--bg-card));
		color: var(--cn-tint-ink);
		box-shadow: var(--shadow-float);
	}

	.cn-artifact__thumb {
		display: block;
		padding: 0;
		border: 0;
		border-radius: calc(var(--radius-md) * 0.6);
		overflow: hidden;
		background: var(--cn-tint);
		cursor: zoom-in;
	}

	.cn-artifact__thumb img,
	.cn-artifact__media {
		display: block;
		width: 100%;
		aspect-ratio: 4 / 3;
		object-fit: cover;
		border-radius: calc(var(--radius-md) * 0.6);
		transition: transform var(--dur-base) var(--spring-snappy);
	}

	.cn-artifact__thumb:hover img {
		transform: scale(1.03);
	}

	.cn-artifact__audio {
		width: 100%;
	}

	.cn-artifact__glyph {
		display: grid;
		place-items: center;
		aspect-ratio: 4 / 3;
		border-radius: calc(var(--radius-md) * 0.6);
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
		font-family: var(--font-display);
		font-size: 1.35rem;
		font-weight: 700;
		letter-spacing: 0.04em;
	}

	.cn-artifact__meta {
		display: grid;
		gap: 0.1rem;
		min-width: 0;
	}

	.cn-artifact__label {
		min-width: 0;
		font-size: 0.78rem;
		font-weight: 650;
	}

	.cn-artifact__sub {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.cn-artifact__open {
		justify-self: start;
		padding: 0.2rem 0.7rem;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-chip);
		background: var(--surface-glass);
		color: var(--accent-primary);
		font-size: var(--text-xs);
		font-weight: 650;
		cursor: pointer;
	}

	.cn-artifact__open:disabled {
		color: var(--text-muted);
		cursor: default;
	}

	.cn-artifact__thumb:focus-visible,
	.cn-artifact__open:focus-visible,
	.cn-lightbox__close:focus-visible {
		outline: none;
		box-shadow: var(--ring-focus);
	}

	.cn-lightbox {
		position: fixed;
		inset: 0;
		z-index: 1000;
		display: grid;
		place-items: center;
		padding: 1.5rem;
		background: color-mix(in srgb, var(--bg-base) 70%, transparent);
		backdrop-filter: blur(var(--blur-glass));
		cursor: zoom-out;
	}

	.cn-lightbox__frame {
		position: relative;
		max-width: min(92vw, 72rem);
		max-height: 88vh;
	}

	.cn-lightbox__frame img {
		display: block;
		max-width: 100%;
		max-height: 88vh;
		border-radius: var(--radius-md);
		box-shadow: var(--shadow-float);
	}

	.cn-lightbox__close {
		position: absolute;
		top: 0.5rem;
		right: 0.5rem;
		width: 2rem;
		height: 2rem;
		border: 0;
		border-radius: 50%;
		background: var(--surface-glass);
		color: var(--text-primary);
		font-size: 1.25rem;
		line-height: 1;
		cursor: pointer;
	}
</style>
