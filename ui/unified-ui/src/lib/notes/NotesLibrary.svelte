<script lang="ts">
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Markdown from '$lib/magician/components/generative/Markdown.svelte';

	import { linkCandidates, rewriteWikiLinks, targetFromHref } from './noteLinks';
	import { isVoiceNotePath } from './voiceNotePath';

	type Entry = {
		name: string;
		relative_path: string;
		kind: 'dir' | 'file';
		has_children?: boolean;
	};
	type Node = Entry & { children?: Node[]; open: boolean; loading: boolean };
	type Selection = { path: string; kind: 'dir' | 'file' };
	type NoteFile = { title: string; relative_path: string; markdown: string };
	type Backlink = { relative_path: string; title: string; line: number; text: string };

	let roots = $state<Node[]>([]);
	let treeError = $state('');
	let loading = $state(true);
	let file = $state<NoteFile | null>(null);
	let fileError = $state('');
	let opening = $state(false);
	let find = $state('');
	let backlinks = $state<Backlink[]>([]);
	let backlinksReady = $state(false);
	let prose = $state<HTMLDivElement | null>(null);
	let findCount = $state(0);
	let selected = $state<Selection | null>(null);
	let draftName = $state('');
	let naming = $state<'note' | 'folder' | null>(null);
	let actionError = $state('');
	let explorerOpen = $state(false);
	let editing = $state(false);
	let draftMarkdown = $state('');
	let saving = $state(false);

	let {
		openPath = '',
		revision = 0,
		onPlayVoice = undefined
	}: {
		openPath?: string;
		revision?: number;
		onPlayVoice?: (path: string) => void;
	} = $props();

	let treeRequest = 0;

	const findQuery = $derived(find.trim());

	function asNodes(entries: Entry[]): Node[] {
		return entries.map((entry) => ({ ...entry, open: false, loading: false }));
	}

	async function load(path: string): Promise<Node[]> {
		const response = await fetch(`/api/magician/v2/notes/tree?path=${encodeURIComponent(path)}`);
		if (!response.ok) {
			throw new Error(
				response.status === 404
					? 'Could not list that folder. This server does not have the notes folder API yet.'
					: `Could not list that folder (${response.status})`
			);
		}
		const body = (await response.json()) as { entries?: Entry[] };
		return asNodes(body.entries ?? []);
	}

	function canExpand(node: Node): boolean {
		return node.kind === 'dir' && node.has_children !== false;
	}

	async function reloadTree(): Promise<void> {
		roots = await load('');
	}

	async function toggle(node: Node): Promise<void> {
		selected = { path: node.relative_path, kind: node.kind };
		naming = null;
		actionError = '';
		if (node.kind === 'file') {
			explorerOpen = false;
			await openFile(node.relative_path);
			return;
		}
		if (!canExpand(node)) return;
		if (node.open) {
			node.open = false;
			return;
		}
		node.open = true;
		if (node.children) return;
		node.loading = true;
		treeError = '';
		try {
			node.children = await load(node.relative_path);
			if (node.children.length === 0) {
				node.open = false;
				node.has_children = false;
			}
		} catch (error) {
			node.open = false;
			treeError = error instanceof Error ? error.message : 'Could not list that folder';
		} finally {
			node.loading = false;
		}
	}

	function destinationFolder(): string {
		if (!selected || selected.kind === 'dir') return selected?.path ?? '';
		const slash = selected.path.lastIndexOf('/');
		return slash < 0 ? '' : selected.path.slice(0, slash);
	}

	async function createNote(): Promise<void> {
		const name = draftName.trim();
		if (!name) return;
		actionError = '';
		const folder = destinationFolder();
		const response = await fetch('/api/magician/v2/notes/file', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ folder, name })
		});
		if (!response.ok) {
			const body = (await response.json().catch(() => null)) as { message?: string } | null;
			actionError = body?.message ?? 'Could not create that note';
			return;
		}
		const created = (await response.json()) as NoteFile;
		draftName = '';
		naming = null;
		await reloadTree();
		await openFile(created.relative_path);
		selected = { path: created.relative_path, kind: 'file' };
	}

	async function createFolder(): Promise<void> {
		const name = draftName.trim();
		if (!name) return;
		actionError = '';
		const folder = destinationFolder();
		const response = await fetch('/api/magician/v2/notes/tree', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ folder, name })
		});
		if (!response.ok) {
			const body = (await response.json().catch(() => null)) as { message?: string } | null;
			actionError = body?.message ?? 'Could not create that folder';
			return;
		}
		const created = (await response.json()) as { relative_path?: string };
		draftName = '';
		naming = null;
		await reloadTree();
		if (created.relative_path) selected = { path: created.relative_path, kind: 'dir' };
	}

	async function deleteSelected(): Promise<void> {
		if (!selected) return;
		const folder = selected.kind === 'dir';
		const label = selected.path || 'this folder';
		if (!confirm(folder ? `Delete the folder ${label} and the notes inside it?` : `Delete ${label}?`)) {
			return;
		}
		actionError = '';
		const query = `path=${encodeURIComponent(selected.path)}`;
		const response = await fetch(
			folder ? `/api/magician/v2/notes/tree?${query}` : `/api/magician/v2/notes/file?${query}`,
			{ method: 'DELETE' }
		);
		if (!response.ok && response.status !== 204) {
			const body = (await response.json().catch(() => null)) as { message?: string } | null;
			actionError = body?.message ?? 'Could not delete that';
			return;
		}
		if (!folder && file?.relative_path === selected.path) file = null;
		selected = null;
		await reloadTree();
	}

	async function openFile(path: string): Promise<void> {
		opening = true;
		fileError = '';
		find = '';
		backlinks = [];
		backlinksReady = false;
		try {
			const response = await fetch(`/api/magician/v2/notes/file?path=${encodeURIComponent(path)}`);
			if (!response.ok) throw new Error('Could not open that note');
			file = (await response.json()) as NoteFile;
			editing = false;
			draftMarkdown = file.markdown;
			void loadBacklinks(file.relative_path);
		} catch (error) {
			file = null;
			fileError = error instanceof Error ? error.message : 'Could not open that note';
		} finally {
			opening = false;
		}
	}

	async function saveNote(): Promise<void> {
		if (!file) return;
		saving = true;
		actionError = '';
		try {
			const response = await fetch('/api/magician/v2/notes/file', {
				method: 'PUT',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ path: file.relative_path, markdown: draftMarkdown })
			});
			if (!response.ok) throw new Error('Could not save that note');
			file = (await response.json()) as NoteFile;
			draftMarkdown = file.markdown;
			editing = false;
		} catch (error) {
			actionError = error instanceof Error ? error.message : 'Could not save that note';
		} finally {
			saving = false;
		}
	}

	async function loadBacklinks(path: string): Promise<void> {
		try {
			const response = await fetch(`/api/magician/v2/notes/backlinks?path=${encodeURIComponent(path)}`);
			if (file?.relative_path !== path) return;
			if (!response.ok) {
				backlinks = [];
				backlinksReady = false;
				return;
			}
			const body = (await response.json()) as { backlinks?: Backlink[] };
			if (file?.relative_path !== path) return;
			backlinks = body.backlinks ?? [];
			backlinksReady = true;
		} catch {
			if (file?.relative_path === path) backlinksReady = false;
		}
	}

	async function openLinked(href: string): Promise<void> {
		const raw = targetFromHref(href);
		const from = file?.relative_path ?? '';
		if (!raw) return;
		fileError = '';
		for (const path of linkCandidates(from, raw)) {
			const response = await fetch(`/api/magician/v2/notes/file?path=${encodeURIComponent(path)}`);
			if (!response.ok) continue;
			file = (await response.json()) as NoteFile;
			find = '';
			backlinks = [];
			backlinksReady = false;
			void loadBacklinks(file.relative_path);
			return;
		}
		fileError = `No note for “${raw}”.`;
	}

	function onProseClick(event: MouseEvent): void {
		const target = event.target;
		if (!(target instanceof Element)) return;
		const anchor = target.closest('a');
		if (!(anchor instanceof HTMLAnchorElement)) return;
		const href = anchor.getAttribute('href') ?? '';
		if (!targetFromHref(href)) return;
		event.preventDefault();
		void openLinked(href);
	}

	function markMatches(root: HTMLElement, query: string): number {
		if (!query) return 0;
		const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
		const nodes: Text[] = [];
		let current = walker.nextNode();
		while (current) {
			nodes.push(current as Text);
			current = walker.nextNode();
		}
		const needle = query.toLowerCase();
		let count = 0;
		for (const textNode of nodes) {
			const text = textNode.data;
			const lower = text.toLowerCase();
			if (!lower.includes(needle)) continue;
			const fragment = document.createDocumentFragment();
			let from = 0;
			while (from < text.length) {
				const at = lower.indexOf(needle, from);
				if (at < 0) {
					fragment.append(text.slice(from));
					break;
				}
				if (at > from) fragment.append(text.slice(from, at));
				const mark = document.createElement('mark');
				mark.textContent = text.slice(at, at + needle.length);
				fragment.append(mark);
				count += 1;
				from = at + needle.length;
			}
			textNode.replaceWith(fragment);
		}
		return count;
	}

	$effect(() => {
		if (openPath) void openFile(openPath);
	});

	$effect(() => {
		const query = findQuery;
		const root = prose;
		const markdown = file?.markdown;
		if (!root || markdown == null) {
			findCount = 0;
			return;
		}
		findCount = markMatches(root, query);
		const onClick = (event: MouseEvent) => onProseClick(event);
		root.addEventListener('click', onClick);
		return () => root.removeEventListener('click', onClick);
	});

	$effect(() => {
		const request = ++treeRequest;
		const listedRevision = revision;
		loading = true;
		treeError = '';
		void load('')
			.then((nodes) => {
				if (request !== treeRequest || listedRevision !== revision) return;
				roots = nodes;
			})
			.catch((error) => {
				if (request !== treeRequest) return;
				treeError = error instanceof Error ? error.message : 'Could not list notes';
			})
			.finally(() => {
				if (request === treeRequest) loading = false;
			});
	});
</script>

{#snippet branch(nodes: Node[])}
	<ul class="branch">
		{#each nodes as node (node.relative_path)}
			<li>
				<button
					type="button"
					class="row"
					class:empty={node.kind === 'dir' && !canExpand(node)}
					class:selected={selected?.path === node.relative_path}
					aria-expanded={canExpand(node) ? node.open : undefined}
					onclick={() => void toggle(node)}
				>
					{#if node.kind === 'dir'}
						<span class="mark" class:filled={canExpand(node)} aria-hidden="true">{canExpand(node) ? (node.open ? '▾' : '▸') : '–'}</span>
					{:else}
						<span class="mark" aria-hidden="true"></span>
					{/if}
					{#if isVoiceNotePath(node.relative_path)}
						<svg class="voice" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-label="Transcription">
							<path d="M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z" />
							<path d="M19 11a7 7 0 0 1-14 0" />
							<path d="M12 18v3" />
						</svg>
					{/if}
					<span class="name">{node.name}</span>
					{#if node.loading}<span class="hint">…</span>{/if}
				</button>
				{#if node.open && node.children && node.children.length > 0}
					{@render branch(node.children)}
				{/if}
			</li>
		{/each}
	</ul>
{/snippet}

<section class="library" class:explorer-open={explorerOpen} aria-labelledby="library-title">
	<div class="reader-bar">
		<button type="button" class="icon-button" aria-label="Folders" onclick={() => (explorerOpen = !explorerOpen)}>
			<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
				<path d="M4 7h16M4 12h16M4 17h16" />
			</svg>
		</button>
		{#if explorerOpen}
			<button type="button" class="icon-button" aria-label="Close folders" onclick={() => (explorerOpen = false)}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<path d="M15 6l-6 6 6 6" />
				</svg>
			</button>
		{/if}
	</div>
	{#if explorerOpen}
		<button type="button" class="scrim" aria-label="Close folders" onclick={() => (explorerOpen = false)}></button>
	{/if}
	<div class="tree">
		<h2 id="library-title">Files</h2>
		<p class="place">{destinationFolder() ? `In ${destinationFolder()}` : 'In the notes root'}</p>
		<div class="tree-actions">
			<Button label="New note" variant="secondary" size="sm" on:click={() => (naming = 'note')} />
			<Button label="New folder" variant="secondary" size="sm" on:click={() => (naming = 'folder')} />
			{#if selected && selected.path}
				<Button
					label={selected.kind === 'dir' ? 'Delete folder' : 'Delete'}
					variant="outline"
					size="sm"
					on:click={() => void deleteSelected()}
				/>
			{/if}
		</div>
		{#if naming}
			<form
				onsubmit={(event) => {
					event.preventDefault();
					if (naming === 'folder') void createFolder();
					else void createNote();
				}}
			>
				<label>
					<span class="sr">{naming === 'folder' ? 'Folder name' : 'Note name'}</span>
					<input
						bind:value={draftName}
						placeholder={naming === 'folder' ? 'Folder name' : 'Note name'}
						aria-label={naming === 'folder' ? 'Folder name' : 'Note name'}
					/>
				</label>
				<Button type="submit" label="Create" variant="primary" size="sm" />
			</form>
		{/if}
		{#if actionError}
			<p class="error" role="alert">{actionError}</p>
		{/if}
		{#if loading}
			<p class="hint">Loading folders…</p>
		{:else if treeError}
			<p class="error" role="alert">{treeError}</p>
		{:else if roots.length === 0}
			<p class="hint">This notes folder is empty.</p>
		{:else}
			{@render branch(roots)}
		{/if}
	</div>
	<div class="reader">
		{#if opening}
			<p class="hint">Opening note…</p>
		{:else if fileError}
			<p class="error" role="alert">{fileError}</p>
		{:else if file}
			<header>
				<p>{file.relative_path}</p>
				<div class="reader-tools">
					<Button
						label="View"
						variant={editing ? 'secondary' : 'primary'}
						size="sm"
						on:click={() => (editing = false)}
					/>
					<Button
						label="Edit"
						variant={editing ? 'primary' : 'secondary'}
						size="sm"
						on:click={() => {
							if (!file) return;
							draftMarkdown = file.markdown;
							editing = true;
						}}
					/>
					{#if editing}
						<Button label={saving ? 'Saving' : 'Save'} variant="primary" size="sm" disabled={saving} on:click={() => void saveNote()} />
					{/if}
					{#if isVoiceNotePath(file.relative_path) && onPlayVoice}
						<button type="button" class="icon-button" aria-label="Play recording" onclick={() => file && onPlayVoice?.(file.relative_path)}>
							<svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M8 5v14l11-7z" /></svg>
						</button>
					{/if}
					<label>
						<span class="sr">Find in this note</span>
						<input bind:value={find} placeholder="Find in this note" aria-label="Find in this note" />
					</label>
				</div>
			</header>
			{#if findQuery}
				<p class="hint">{findCount} {findCount === 1 ? 'match' : 'matches'}</p>
			{/if}
			{#if editing}
				<textarea class="editor" bind:value={draftMarkdown} aria-label="Edit note"></textarea>
			{:else}
				{#key `${file.relative_path}\n${findQuery}`}
					<div class="prose" bind:this={prose}>
						<Markdown content={rewriteWikiLinks(file.markdown)} />
					</div>
				{/key}
			{/if}
			{#if backlinksReady}
				<section class="backlinks" aria-label="Backlinks">
					<h3>Backlinks</h3>
					{#if backlinks.length === 0}
						<p class="hint">No other notes link here.</p>
					{:else}
						<ul>
							{#each backlinks as link (link.relative_path)}
								<li>
									<button type="button" onclick={() => void openFile(link.relative_path)}>
										{link.title || link.relative_path}
									</button>
									<p class="hint">{link.text}</p>
								</li>
							{/each}
						</ul>
					{/if}
				</section>
			{/if}
		{:else}
			<p class="hint">Choose a note to read it here.</p>
		{/if}
	</div>
</section>

<style>
	.library {
		box-sizing: border-box;
		display: grid;
		grid-template-columns: minmax(14rem, 22rem) minmax(0, 1fr);
		align-items: start;
		gap: 16px;
		width: 100%;
		max-width: 100%;
		min-width: 0;
		margin-top: 16px;
	}
	.tree,
	.reader {
		min-width: 0;
		padding: 14px;
		border: 1px solid var(--border-soft);
		border-radius: 16px;
		background: var(--bg-elevated);
	}
	.tree {
		position: sticky;
		top: 0;
		z-index: 2;
		max-height: calc(100dvh - var(--v5-topbar-h, 48px));
		overflow: auto;
	}
	h2,
	p { margin: 0; }
	h2 {
		color: var(--text-muted);
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.reader-bar, .scrim { display: none; }
	.place {
		margin: 8px 0 0;
		color: var(--text-muted);
		font-size: 0.75rem;
		line-height: 1.3;
		text-align: left;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.tree .branch {
		margin: 4px 0 0;
		padding: 0 0 0 12px;
		list-style: none;
	}
	.tree .branch > li {
		position: relative;
		margin: 0;
		padding: 0 0 0 12px;
		list-style: none;
	}
	.tree .branch > li::before {
		content: '';
		position: absolute;
		left: 0;
		top: 0;
		bottom: 0;
		border-left: 1px solid var(--border-soft);
	}
	.tree .branch > li::after {
		content: '';
		position: absolute;
		left: 0;
		top: 15px;
		width: 12px;
		border-top: 1px solid var(--border-soft);
	}
	.tree .branch > li:last-child::before { height: 15px; }
	.tree-actions, form {
		display: flex;
		flex-wrap: nowrap;
		align-items: center;
		gap: 6px;
		margin-top: 8px;
	}
	.tree-actions { overflow-x: auto; }
	.tree-actions :global(.muij-button),
	form :global(.muij-button) {
		flex: 0 0 auto;
	}
	button.row {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		min-height: 30px;
		padding: 4px 6px;
		border: 0;
		border-radius: 8px;
		background: transparent;
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.84rem;
		line-height: 1.2;
		text-align: left;
		cursor: pointer;
	}
	button.row:hover { background: var(--button-secondary-hover-bg, var(--bg-soft, transparent)); }
	.mark {
		flex: 0 0 12px;
		width: 12px;
		color: var(--text-muted);
		font-size: 0.72rem;
		line-height: 1;
	}
	.mark.filled { color: var(--accent-primary); }
	.row.empty .name { color: var(--text-muted); }
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	form { min-width: 0; }
	form label { flex: 1 1 auto; min-width: 0; }
	button.row.selected { background: color-mix(in srgb, var(--accent-primary) 16%, transparent); }
	.voice { flex: none; color: var(--accent-primary); }
	.reader-tools { display: flex; align-items: center; gap: 8px; }
	.icon-button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 0;
		border: 1px solid var(--button-secondary-border, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: var(--button-secondary-bg, var(--bg-soft, var(--bg-card)));
		color: var(--button-secondary-color, var(--text-primary));
		font-family: var(--font-primary);
		cursor: pointer;
	}
	.icon-button:hover {
		background: var(--button-secondary-hover-bg, var(--border-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
	}
	.reader header {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: 10px;
		margin-bottom: 10px;
	}
	.reader header p,
	.hint { color: var(--text-muted); font-size: 0.78rem; }
	.error { color: var(--status-danger, #d64545); font-size: 0.84rem; }
	input {
		box-sizing: border-box;
		width: 100%;
		height: 32px;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-base, var(--bg-elevated));
		color: var(--text-primary);
		padding: 0 10px;
		font-family: var(--font-primary);
		font-size: 0.84rem;
	}
	.editor {
		box-sizing: border-box;
		width: 100%;
		min-height: 320px;
		margin-top: 8px;
		padding: 10px;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-base, var(--bg-elevated));
		color: var(--text-primary);
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.84rem;
		line-height: 1.45;
		resize: vertical;
	}
	.prose {
		min-width: 0;
		margin-top: 8px;
	}
	.prose :global(mark) {
		padding: 0 1px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--accent-primary) 34%, transparent);
		color: inherit;
	}
	.backlinks {
		margin-top: 18px;
		padding-top: 12px;
		border-top: 1px solid var(--border-soft);
	}
	.backlinks h3 {
		margin: 0 0 6px;
		color: var(--text-muted);
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	.backlinks button {
		display: flex;
		width: 100%;
		padding: 4px 0;
		border: 0;
		background: transparent;
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.84rem;
		text-align: left;
		cursor: pointer;
	}
	.backlinks .hint { margin-top: 2px; }
	.sr {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip: rect(0 0 0 0);
	}
	@media (max-width: 720px) {
		.library { display: block; }
		.reader-bar {
			display: flex;
			align-items: center;
			gap: 8px;
			margin-bottom: 8px;
		}
		.tree {
			position: fixed;
			z-index: 30;
			top: var(--v5-topbar-h, 48px);
			bottom: 0;
			left: 0;
			width: min(280px, 78vw);
			max-height: none;
			border-radius: 0 16px 16px 0;
			transform: translateX(-105%);
		}
		.library.explorer-open .tree { transform: none; }
		.scrim {
			display: none;
			position: fixed;
			z-index: 29;
			top: var(--v5-topbar-h, 48px);
			right: 0;
			bottom: 0;
			left: 0;
			width: auto;
			min-height: 0;
			padding: 0;
			border: 0;
			border-radius: 0;
			background: color-mix(in srgb, var(--text-primary) 32%, transparent);
		}
		.library.explorer-open .scrim { display: block; }
	}
</style>
