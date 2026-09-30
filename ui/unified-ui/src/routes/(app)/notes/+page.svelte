<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { page } from '$app/stores';

	import { showError } from '$lib/shared/stores/notifications';
	import {
		fetchAudioNoteRecording,
		fetchAudioNotes,
		type AudioNoteItem
	} from '$lib/notes/audioNotes';
	import {
		audioNoteOutbox,
		type AudioNoteOutboxStatus
	} from '$lib/notes/audioNoteOutbox';
	import {
		archiveChatDictationStore,
		setArchiveChatDictation
	} from '$lib/notes/audioNotePreferences';
	import NotesLibrary from '$lib/notes/NotesLibrary.svelte';
	import { isVoiceNotePath } from '$lib/notes/voiceNotePath';
	import {
		highlightSegments,
		searchNotes,
		type NoteSearchHit,
		type NoteSearchResults
	} from '$lib/notes/noteSearch';
	import { refreshNotesSettings } from '$lib/stores/notesSettingsStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	const WRITTEN_SEARCH_STEP = 10;
	const WRITTEN_SEARCH_MAX = 50;
	const SEARCH_PAUSE_MS = 200;
	const RECORDING_PAGE = 100;
	let recordings: AudioNoteItem[] = [];
	let loading = true;
	let requestId = 0;
	let controller: AbortController | null = null;
	let loadedScope = '';
	let mounted = false;
	let playingId: string | null = null;
	let playbackUrl: string | null = null;
	let previousOutboxIds = new Set<string>();
	let unsubscribeOutbox: (() => void) | null = null;
	let libraryRevision = 0;
	let openPath = '';
	let appliedNotePath = '';

	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: {
		const requested = $page.url?.searchParams.get('path') ?? '';
		if (requested && requested !== appliedNotePath) {
			appliedNotePath = requested;
			openPath = requested;
		}
	}
	$: if (mounted && scopeKey !== loadedScope) {
		loadedScope = scopeKey;
		libraryRevision += 1;
		void load();
	}
	let writtenDraft = '';
	let writtenQuery = '';
	let writtenLimit = WRITTEN_SEARCH_STEP;
	let writtenResults: NoteSearchResults | null = null;
	let writtenSearching = false;
	let writtenError = '';
	let searchTookMs: number | null = null;
	let writtenController: AbortController | null = null;
	let searchTimer: ReturnType<typeof setTimeout> | null = null;

	/**
	 * Run a written-notes search, superseding any in flight.
	 *
	 * Aborting the previous request is what keeps a fast typist from seeing an
	 * older, slower response land on top of a newer one.
	 *
	 * `source` defaults to the input box but `Show more` passes the query that
	 * produced the results on screen: expanding a result set must not quietly
	 * search whatever has been typed since.
	 */
	async function runWrittenSearch(limit: number, source = writtenDraft): Promise<void> {
		const query = source.trim();
		if (!query) {
			clearWrittenSearch();
			return;
		}
		writtenController?.abort();
		const controller = new AbortController();
		writtenController = controller;
		writtenSearching = true;
		writtenError = '';
		const started = performance.now();
		try {
			const results = await searchNotes(query, { limit, signal: controller.signal });
			if (writtenController !== controller) return;
			writtenResults = results;
			writtenQuery = query;
			writtenLimit = limit;
			searchTookMs = Math.max(0, Math.round(performance.now() - started));
		} catch (error) {
			if (error instanceof DOMException && error.name === 'AbortError') return;
			if (writtenController !== controller) return;
			writtenResults = null;
			writtenError = error instanceof Error ? error.message : 'Notes search failed';
		} finally {
			if (writtenController === controller) {
				writtenSearching = false;
				writtenController = null;
			}
		}
	}

	function clearWrittenSearch(): void {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = null;
		writtenController?.abort();
		writtenController = null;
		writtenDraft = '';
		writtenQuery = '';
		writtenResults = null;
		writtenError = '';
		writtenSearching = false;
		writtenLimit = WRITTEN_SEARCH_STEP;
		searchTookMs = null;
	}

	function queueSearch(value: string): void {
		writtenDraft = value;
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = null;
		const query = value.trim();
		if (!query) {
			clearWrittenSearch();
			return;
		}
		searchTimer = setTimeout(() => {
			searchTimer = null;
			if (query === writtenQuery && writtenResults) return;
			void runWrittenSearch(WRITTEN_SEARCH_MAX, query);
		}, SEARCH_PAUSE_MS);
	}

	function flushSearch(): void {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = null;
		void runWrittenSearch(WRITTEN_SEARCH_MAX);
	}

	function openWrittenNote(hit: NoteSearchHit): void {
		openPath = hit.relative_path;
	}

	function sameNotePath(left: string, right: string): boolean {
		const normalize = (path: string) => path.replaceAll('\\', '/').replace(/^\/+|\/+$/g, '');
		return normalize(left) === normalize(right);
	}

	async function findRecording(path: string): Promise<AudioNoteItem | null> {
		const cached = recordings.find((item) => sameNotePath(item.note_path, path));
		if (cached) return cached;
		for (let offset = 0; offset < 500; offset += RECORDING_PAGE) {
			const result = await fetchAudioNotes({ offset, limit: RECORDING_PAGE });
			if (offset === 0) recordings = result.items;
			const match = result.items.find((item) => sameNotePath(item.note_path, path));
			if (match) return match;
			if (!result.has_more) return null;
		}
		return null;
	}

	async function playVoicePath(path: string): Promise<void> {
		try {
			const match = await findRecording(path);
			if (!match) {
				showError('No recording is saved for this transcript yet');
				return;
			}
			await playSaved(match);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Recording could not be played');
		}
	}

	function noteFileName(path: string): string {
		const parts = path.split('/').filter((part) => part.length > 0);
		return parts.at(-1) || path;
	}

	function noteFolder(path: string): string {
		const parts = path.split('/').filter((part) => part.length > 0);
		return parts.length > 1 ? parts.slice(0, -1).join('/') : '';
	}

	function showNoteTitle(hit: NoteSearchHit): boolean {
		const title = hit.title?.trim() ?? '';
		if (!title) return false;
		const lower = title.toLowerCase();
		return lower !== noteFileName(hit.relative_path).toLowerCase() && lower !== hit.relative_path.toLowerCase();
	}

	function providerLabel(provider: string): string {
		return provider === 'silverbullet' ? 'Notes folder' : 'Local Markdown';
	}

	function searchSummary(results: NoteSearchResults, tookMs: number | null): string {
		const count = results.hits.length;
		const total = results.more_available ? `${count}+` : String(count);
		const noun = count === 1 && !results.more_available ? 'result' : 'results';
		const timing = tookMs == null ? '' : ` · ${tookMs < 1000 ? `${tookMs} ms` : `${(tookMs / 1000).toFixed(1)} s`}`;
		return `${total} ${noun}${timing}`;
	}

	function resultsUseSeveralProviders(results: NoteSearchResults): boolean {
		return new Set(results.hits.map((hit) => hit.provider)).size > 1;
	}

	async function load(): Promise<void> {
		const id = ++requestId;
		controller?.abort();
		const nextController = new AbortController();
		controller = nextController;
		loading = true;
		try {
			const result = await fetchAudioNotes({
				offset: 0,
				limit: RECORDING_PAGE,
				signal: nextController.signal
			});
			if (id !== requestId) return;
			recordings = result.items;
		} catch (error) {
			if (id !== requestId || nextController.signal.aborted) return;
			showError(error instanceof Error ? error.message : 'Audio Notes could not be loaded');
		} finally {
			if (id === requestId) loading = false;
		}
	}

	function refreshPage(): void {
		libraryRevision += 1;
		void load();
	}

	async function removePending(note: AudioNoteOutboxStatus): Promise<void> {
		if (!confirm('Discard this pending recording permanently?')) return;
		try {
			await audioNoteOutbox.discard(note.id);
			if (playingId === note.id) stopPlayback();
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Pending recording could not be discarded');
		}
	}

	function stopPlayback(): void {
		if (playbackUrl) URL.revokeObjectURL(playbackUrl);
		playbackUrl = null;
		playingId = null;
	}

	async function playSaved(note: AudioNoteItem): Promise<void> {
		if (playingId === note.note_id) {
			stopPlayback();
			return;
		}
		try {
			const blob = await fetchAudioNoteRecording(note.note_id);
			stopPlayback();
			playbackUrl = URL.createObjectURL(blob);
			playingId = note.note_id;
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Recording could not be played');
		}
	}

	async function playPending(note: AudioNoteOutboxStatus): Promise<void> {
		if (playingId === note.id) {
			stopPlayback();
			return;
		}
		try {
			const blob = await audioNoteOutbox.recording(note.id);
			if (!blob) throw new Error('The pending recording is no longer available');
			stopPlayback();
			playbackUrl = URL.createObjectURL(blob);
			playingId = note.id;
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Recording could not be played');
		}
	}

	function displayDate(value: string): string {
		const date = new Date(value);
		return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
	}

	function duration(value: number): string {
		const seconds = Math.max(0, Math.floor(value / 1000));
		return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
	}

	onMount(() => {
		mounted = true;
		loadedScope = scopeKey;
		unsubscribeOutbox = audioNoteOutbox.subscribe((items) => {
			const currentIds = new Set(items.map((item) => item.id));
			if (Array.from(previousOutboxIds).some((id) => !currentIds.has(id))) void load();
			previousOutboxIds = currentIds;
		});
		void audioNoteOutbox.start().catch(() => undefined);
		void refreshNotesSettings().catch(() => undefined);
		void load();
	});
	onDestroy(() => {
		if (searchTimer) clearTimeout(searchTimer);
		unsubscribeOutbox?.();
		controller?.abort();
		writtenController?.abort();
		stopPlayback();
	});
</script>

<svelte:head><title>Notes · Magican</title></svelte:head>

<main class="notes-page">
	<header class="page-head">
		<h1>Notes</h1>
		<form class="search" aria-busy={writtenSearching} on:submit|preventDefault={flushSearch}>
			<input
				value={writtenDraft}
				placeholder="Search notes"
				aria-label="Search notes"
				on:input={(event) => {
				const target = event.target;
				if (target instanceof HTMLInputElement) queueSearch(target.value);
			}}
			/>
			{#if writtenDraft}
				<button class="icon-button" type="button" aria-label="Clear search" title="Clear search" on:click={clearWrittenSearch}>
					<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
						<path d="M6 6l12 12M18 6L6 18" />
					</svg>
				</button>
			{/if}
		</form>
		<div class="page-actions">
			<label class="keep-recordings">
				<input
					type="checkbox"
					checked={$archiveChatDictationStore}
					on:change={() => setArchiveChatDictation(!$archiveChatDictationStore)}
				/>
				<span class="keep-box" aria-hidden="true"></span>
				<span>Keep recordings</span>
			</label>
			<button class="icon-button" type="button" aria-label="Refresh" title="Refresh" disabled={loading} on:click={refreshPage}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<path d="M21 12a9 9 0 1 1-2.2-5.8" />
					<path d="M21 3v6h-6" />
				</svg>
			</button>
		</div>
	</header>

	{#if writtenSearching || writtenError || writtenResults}
	<section class="section results" aria-label="Search results">
		<div class="results-head">
			<p role="status">
				{#if writtenSearching && !writtenResults}
					Searching notes…
				{:else if writtenResults}
					{searchSummary(writtenResults, searchTookMs)}
					{#if writtenSearching}<span class="results-updating"> · updating</span>{/if}
				{/if}
			</p>
			{#if writtenResults && writtenResults.more_available && writtenLimit < WRITTEN_SEARCH_MAX}
				<button
					type="button"
					disabled={writtenSearching}
					on:click={() =>
						void runWrittenSearch(
							Math.min(writtenLimit + WRITTEN_SEARCH_STEP, WRITTEN_SEARCH_MAX),
							writtenQuery
						)}
				>
					Show more
				</button>
			{/if}
		</div>

		{#if writtenError}
			<p class="written-note written-error" role="alert">{writtenError}</p>
		{:else if writtenResults}
			{#if writtenResults.scan_truncated}
				<!-- Never let a partial scan read as a complete answer. -->
				<p class="written-note written-warning" role="status">
					These results are incomplete — the search stopped before reaching the end of your
					notes. A note that is not listed may still exist.
				</p>
			{/if}

			{#if writtenResults.hits.length === 0}
				<p class="written-note">
					Nothing matched “{writtenQuery}”. Try another word from the note, or a shorter
					spelling. Related notes appear when the embedding model is available.
				</p>
			{:else}
				<ul class="written-hits">
					{#each writtenResults.hits as hit (hit.source_ref)}
						<li class="written-hit">
							<div class="written-hit-head">
								<div class="written-id">
									{#if showNoteTitle(hit)}
										<p class="written-title">
											{#each highlightSegments(hit.title, writtenResults.query_terms) as segment}{#if segment.match}<mark
														>{segment.text}</mark
													>{:else}{segment.text}{/if}{/each}
										</p>
									{/if}
									<p class="written-path">
										{#if noteFolder(hit.relative_path)}
											<span class="written-folder">{noteFolder(hit.relative_path)}/</span>
										{/if}
										<button type="button" class="hit-link" on:click={() => openWrittenNote(hit)}>
											{#if isVoiceNotePath(hit.relative_path)}
												<svg class="voice" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-label="Transcription">
													<path d="M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z" />
													<path d="M19 11a7 7 0 0 1-14 0" />
													<path d="M12 18v3" />
												</svg>
											{/if}
											{noteFileName(hit.relative_path)}
										</button>
									</p>
								</div>
								<div class="written-badges">
									{#if resultsUseSeveralProviders(writtenResults)}
										<span class="written-badge">{providerLabel(hit.provider)}</span>
									{/if}
									{#if hit.matched_in_title}<span class="written-badge written-badge-title">Title match</span>{/if}
								</div>
							</div>
							{#if hit.matches.length > 0}
								<div class="written-matches">
									{#each hit.matches as match (match.line)}
										<p class="written-match">
											<span class="written-line">{match.line}</span>
											<span
												>{#each highlightSegments(match.text, writtenResults.query_terms) as segment}{#if segment.match}<mark
															>{segment.text}</mark
														>{:else}{segment.text}{/if}{/each}</span
											>
										</p>
									{/each}
								</div>
							{/if}
							{#if hit.match_count > hit.matches.length}
								<p class="written-more">
									{hit.match_count - hit.matches.length} more matching {hit.match_count -
										hit.matches.length === 1
										? 'line'
										: 'lines'} in this note
								</p>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		{/if}
	</section>
	{/if}

	<NotesLibrary {openPath} revision={libraryRevision} onPlayVoice={playVoicePath} />

	{#if playbackUrl}
		<section class="player" aria-label="Recording player">
			<!-- svelte-ignore a11y_media_has_caption -->
			<audio src={playbackUrl} controls autoplay on:ended={stopPlayback}></audio>
			<button type="button" on:click={stopPlayback}>Close</button>
		</section>
	{/if}

	{#if $audioNoteOutbox.length > 0}
		<section class="section" aria-labelledby="pending-title">
			<div class="section-head">
				<div><p class="eyebrow">Recordings · this browser</p><h2 id="pending-title">Pending and failed</h2></div>
				<span>{$audioNoteOutbox.length} local</span>
			</div>
			<div class="note-grid">
				{#each $audioNoteOutbox as note (note.id)}
					<article class:needs-attention={note.state === 'Needs attention'} class="note-card">
						<div class="card-head">
							<strong>{displayDate(note.capturedAt)}</strong>
							<span>{note.state}</span>
						</div>
						<p>{note.transcript || 'Audio-only note; transcription was not available.'}</p>
						<small>{duration(note.durationMs)} · {(note.bytes / 1024).toFixed(0)} KB{note.detail ? ` · ${note.detail}` : ''}</small>
						<div class="card-actions">
							<button type="button" on:click={() => void playPending(note)}>{playingId === note.id ? 'Stop' : 'Play'}</button>
							{#if note.canRetry}<button type="button" on:click={() => void audioNoteOutbox.retry(note.id)}>Retry</button>{/if}
							{#if note.canDiscard}<button class="danger" type="button" on:click={() => void removePending(note)}>Discard</button>{/if}
						</div>
					</article>
				{/each}
			</div>
		</section>
	{/if}

</main>

<style>
	.notes-page {
		box-sizing: border-box;
		width: min(var(--app-content-max, 1120px), calc(100% - 48px));
		max-width: var(--app-content-max, 1120px);
		margin: 0 auto;
		padding: 12px 0 48px;
		color: var(--text-primary);
	}
	.page-head, .section {
		box-sizing: border-box;
		width: 100%;
		max-width: 100%;
		min-width: 0;
	}
	.page-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px;
		min-height: 44px;
		padding: 6px 8px 6px 14px;
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		background: var(--bg-elevated);
	}
	.section-head, .player { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
	h1, h2, p { margin: 0; }
	h1 { flex: none; font-size: 1.25rem; font-weight: 650; line-height: 1; letter-spacing: 0; }
	h2 { font-size: 1rem; letter-spacing: 0; }
	.eyebrow { margin-bottom: 4px; color: var(--accent-primary); font-size: .72rem; font-weight: 800; letter-spacing: .12em; text-transform: uppercase; }
	.page-actions { display: flex; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: 8px; margin-left: auto; }
	.keep-recordings, .icon-button, button {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card, var(--bg-elevated));
		color: var(--text-primary);
		font: inherit;
		font-size: .8rem;
		text-decoration: none;
		cursor: pointer;
	}
	button { padding: 8px 12px; }
	.icon-button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 0;
		flex: none;
	}
	.icon-button:hover:not(:disabled) {
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
	}
	.keep-recordings {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		padding: 0 12px 0 10px;
		cursor: pointer;
	}
	.keep-recordings input {
		position: absolute;
		opacity: 0;
		width: 16px;
		height: 16px;
		margin: 0;
	}
	.keep-box {
		width: 16px;
		height: 16px;
		flex: none;
		border: 1.5px solid color-mix(in srgb, var(--text-secondary) 55%, var(--border-soft));
		border-radius: 4px;
		background: var(--bg-base, var(--bg-card));
		box-shadow: inset 0 0 0 1px transparent;
	}
	.keep-recordings input:checked + .keep-box {
		border-color: var(--accent-primary);
		background: var(--accent-primary);
		box-shadow: inset 0 0 0 3px var(--bg-elevated);
	}
	.keep-recordings input:focus-visible + .keep-box {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 55%, transparent);
		outline-offset: 2px;
	}
	button:disabled { opacity: .55; cursor: default; }
	.player { position: sticky; top: 64px; z-index: 5; margin-top: 14px; padding: 10px 14px; border-radius: 14px; background: var(--bg-elevated); box-shadow: var(--shadow-md); }
	.player audio { flex: 1; min-width: 0; }
	.section {
		box-sizing: border-box;
		width: 100%;
		max-width: 100%;
		min-width: 0;
		margin-top: 16px;
		padding: 18px;
		border: 1px solid var(--border-soft);
		border-radius: 16px;
		background: var(--bg-elevated);
	}
	.results {
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-height: 240px;
		margin-top: 10px;
		padding: 10px 12px;
		overflow: hidden;
	}
	.results-head {
		display: flex;
		flex: none;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		color: var(--text-secondary);
		font-size: .8rem;
	}
	.results-head p { font-variant-numeric: tabular-nums; }
	.results-updating { color: var(--text-muted); }
	.results .written-hits { flex: 1; min-height: 0; overflow: auto; }
	.written-note { color: var(--text-secondary); font-size: .88rem; line-height: 1.5; max-width: 640px; }
	.written-error { color: var(--status-danger, #d64545); }
	.written-warning { padding: 10px 12px; border: 1px solid var(--border-soft); border-radius: 10px; background: color-mix(in srgb, var(--bg-elevated) 88%, var(--accent-primary) 12%); }
	.written-hits { display: flex; flex-direction: column; gap: 0; margin: 0; padding: 0; list-style: none; }
	.written-hit {
		display: grid;
		gap: 6px;
		padding: 12px 2px;
		border: 0;
		border-radius: 0;
		background: transparent;
	}
	.written-hit + .written-hit { border-top: 1px solid var(--border-soft); }
	.written-hit-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; min-width: 0; }
	.written-id { display: grid; gap: 2px; min-width: 0; }
	.written-title { margin: 0; color: var(--text-primary); font-weight: 650; }
	.written-title mark { padding: 0 1px; border-radius: 3px; background: color-mix(in srgb, var(--accent-primary) 34%, transparent); color: inherit; }
	.written-badges { display: flex; flex: none; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
	button.hit-link {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--accent-primary);
		font-weight: 650;
		text-align: left;
		text-decoration: underline;
		text-decoration-thickness: 1px;
		text-underline-offset: 3px;
	}
	button.hit-link:hover { color: var(--text-primary); }
	.voice { flex: none; color: var(--accent-primary); }
	.written-badge { padding: 1px 7px; border-radius: 999px; background: color-mix(in srgb, var(--bg-elevated) 70%, var(--accent-primary) 30%); color: var(--text-secondary); font-size: .68rem; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; }
	.written-badge-title { background: color-mix(in srgb, var(--bg-elevated) 60%, var(--accent-primary) 40%); }
	/* `min-width: 0` with `overflow-wrap` so a long unbroken path cannot force
	   the whole card into a horizontal scroll. */
	.written-path { display: flex; flex-wrap: wrap; align-items: baseline; min-width: 0; margin: 0; overflow-wrap: anywhere; color: var(--text-muted); font-size: .84rem; }
	.written-folder { color: var(--text-muted); }
	.written-matches { display: grid; gap: 4px; }
	.written-match {
		display: grid;
		grid-template-columns: 2.75rem minmax(0, 1fr);
		column-gap: 12px;
		align-items: baseline;
		min-width: 0;
		margin: 0;
		overflow-wrap: anywhere;
		color: var(--text-secondary);
		font-size: .84rem;
		line-height: 1.45;
	}
	.written-line { color: var(--text-muted); font-size: .74rem; text-align: right; font-variant-numeric: tabular-nums; }
	.written-match mark { padding: 0 1px; border-radius: 3px; background: color-mix(in srgb, var(--accent-primary) 34%, transparent); color: inherit; }
	.written-more { margin: 0 0 0 calc(2.75rem + 12px); color: var(--text-muted); font-size: .74rem; }
	.section-head { margin-bottom: 14px; }
	.section-head > span { color: var(--text-muted); font-size: .78rem; }
	.note-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 12px; }
	.note-card { display: grid; gap: 11px; min-width: 0; padding: 16px; border: 1px solid var(--border-soft); border-radius: 16px; background: var(--bg-elevated); }
	.note-card.needs-attention { border-color: color-mix(in srgb, var(--color-warning, #b45309) 58%, var(--border-soft)); }
	.card-head { display: flex; justify-content: space-between; gap: 12px; font-size: .8rem; }
	.card-head span, .note-card small { color: var(--text-muted); }
	.note-card p { color: var(--text-secondary); line-height: 1.5; white-space: pre-wrap; overflow-wrap: anywhere; }
	.note-card small { line-height: 1.4; overflow-wrap: anywhere; }
	.card-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 7px; }
	.card-actions button { padding: 6px 10px; }
	.card-actions .danger { color: var(--color-error, #b91c1c); }
	.search { position: relative; display: flex; flex: 1 1 220px; align-items: center; min-width: 0; }
	.search input {
		flex: 1;
		min-width: 0;
		height: 32px;
		padding: 0 32px 0 10px;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-base, var(--bg-card));
		color: var(--text-primary);
		font: inherit;
		font-size: .85rem;
	}
	.search .icon-button {
		position: absolute;
		top: 2px;
		right: 2px;
		width: 28px;
		height: 28px;
		border: 0;
		background: transparent;
	}
	@media (max-width: 720px) {
		.notes-page { width: min(100% - 24px, 1120px); }
		.page-head { align-items: stretch; }
		.page-actions { justify-content: flex-start; margin-left: 0; }
		.search { flex-basis: 100%; }
	}
</style>
