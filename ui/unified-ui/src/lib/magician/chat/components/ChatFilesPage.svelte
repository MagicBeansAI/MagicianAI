<script lang="ts">
    import { onDestroy } from 'svelte';
    import ConfirmDialog from '$lib/magician/components/generative/ConfirmDialog.svelte';
    import { timedFetch } from '$lib/shared/fetch';
    import { scopeIdentityStore, scopedRequestHeaders, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
    import { taskStore } from '$lib/stores/taskStore';
    import { workTaskHref } from '$lib/work/routes';
    import { openAuthenticatedTaskOutput, taskOutputUrl, downloadAuthenticatedTaskOutput } from '$lib/magician/tasks/taskOutputs';
    export let sessionId: string | null;
    export let threadId = 'general';
    interface Asset { id: string; name: string; path: string; source: string; task_id?: string; output_id?: string; execution_id?: string; session_id?: string; label?: string; created_at?: string | number; mime_type?: string; size?: number | null; }
    let files: Asset[] = [], error = '', loading = false, partial = false, target = '', chatTitle = '';
    let generation = 0, filter: 'all' | 'artifacts' | 'uploads' = 'all', search = '';
    let controller: AbortController | null = null;
    let deleting = false, hasInventory = false, pendingDelete: Asset | null = null;
    $: chatHref = `/t/${encodeURIComponent(threadId)}/chat${sessionId ? `?session=${encodeURIComponent(sessionId)}` : ''}`;
    $: nextTarget = JSON.stringify([sessionId, $scopeIdentityStore.principal, $scopeIdentityStore.workspace, getCurrentScopeCredentialRevision()]);
    $: if (nextTarget !== target) {
        target = nextTarget; generation++; controller?.abort(); files = []; error = ''; partial = false; chatTitle = ''; loading = false; pendingDelete = null; deleting = false; hasInventory = false;
        if (sessionId) void load();
    }
    function taskTitle(id: string): string { return $taskStore.tasks.find(task => task.id === id)?.title || id; }
    function dateOf(file: Asset): Date | null {
        if (file.created_at == null) return null;
        const date = new Date(file.created_at);
        return Number.isNaN(date.getTime()) ? null : date;
    }
    function sizeOf(file: Asset): number | null {
        return typeof file.size === 'number' && Number.isFinite(file.size) && file.size >= 0 ? file.size : null;
    }
    function formatSize(bytes: number | null): string {
        if (bytes === null) return 'Size unavailable';
        const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
        const unit = bytes > 0 ? Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1) : 0;
        return `${Intl.NumberFormat(undefined, { maximumFractionDigits: unit ? 1 : 0 }).format(bytes / 1024 ** unit)} ${units[unit]}`;
    }
    $: totalSize = files.reduce((sum, file) => sum + (sizeOf(file) ?? 0), 0);
    $: unknownSizes = files.filter(file => sizeOf(file) === null).length;
    $: visible = files.filter(file => (filter === 'all' || (filter === 'uploads') === (file.source === 'upload')) &&
        [file.name, file.path, file.label, file.task_id, file.task_id ? taskTitle(file.task_id) : '', file.execution_id, file.session_id]
            .filter(Boolean).join(' ').toLowerCase().includes(search.trim().toLowerCase()))
        .sort((a, b) => (dateOf(b)?.getTime() ?? 0) - (dateOf(a)?.getTime() ?? 0));
    async function load(): Promise<void> {
        if (!sessionId) return;
        controller?.abort(); controller = new AbortController();
        const version = ++generation;
        loading = true; error = '';
        try {
            const response = await timedFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/assets`, { headers: scopedRequestHeaders(), signal: controller.signal });
            if (!response.ok) throw new Error('Files could not be loaded. Try refreshing.');
            const body = await response.json();
            if (!Array.isArray(body.files)) throw new Error('File inventory is unavailable.');
            if (version === generation) { files = body.files; chatTitle = body.chat_title || ''; partial = !!body.unavailable_tasks?.length || body.unavailable_files > 0; hasInventory = true; }
        } catch (cause) { if (version === generation) error = cause instanceof Error ? cause.message : 'Files could not be loaded.'; }
        finally { if (version === generation) loading = false; }
    }
    function fileUrl(file: Asset): string {
        if (file.source === 'task' && file.task_id) return taskOutputUrl(file.task_id, file.path, $scopeIdentityStore.principal, $scopeIdentityStore.workspace);
        return `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId ?? '')}/outputs/${file.path.split('/').map(encodeURIComponent).join('/')}`;
    }
    async function show(file: Asset, download = false): Promise<void> {
        const version = generation;
        try {
            if (download) await downloadAuthenticatedTaskOutput(fileUrl(file), file.name);
            else await openAuthenticatedTaskOutput(fileUrl(file));
        } catch (cause) { if (version === generation) error = cause instanceof Error ? cause.message : 'The file could not be opened.'; }
    }
    async function remove(file: Asset): Promise<void> {
        const currentTarget = JSON.stringify([sessionId, $scopeIdentityStore.principal, $scopeIdentityStore.workspace, getCurrentScopeCredentialRevision()]);
        if (!sessionId || deleting || target !== currentTarget) return;
        const version = generation;
        const url = `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/assets/${encodeURIComponent(file.id)}`;
        const headers = scopedRequestHeaders();
        deleting = true; error = ''; pendingDelete = null;
        try {
            const response = await timedFetch(url, { method: 'DELETE', headers });
            if (!response.ok) {
                const body = await response.json().catch(() => ({}));
                throw new Error(body.message || 'The file could not be deleted. Refresh and try again.');
            }
            if (version === generation) files = files.filter(candidate => candidate.id !== file.id);
        } catch (cause) { if (version === generation) error = cause instanceof Error ? cause.message : 'The file could not be deleted.'; }
        finally { if (version === generation) deleting = false; }
    }
    onDestroy(() => { generation++; controller?.abort(); });
</script>

<section class="chat-files-page" aria-label="Chat files">
    <header><div><h1>Files</h1><p>{chatTitle || 'Uploads and artifacts from this chat and its tasks.'}</p>
        {#if hasInventory}<p class="size-summary" aria-label="Total file size">{files.length} {files.length === 1 ? 'file' : 'files'} · {unknownSizes || partial ? 'Known size' : 'Total'}: {formatSize(totalSize)}{#if unknownSizes}{' · '}{unknownSizes} {unknownSizes === 1 ? 'size' : 'sizes'} unavailable{/if}</p>{/if}
    </div><button type="button" disabled={loading || deleting || !sessionId} on:click={() => void load()}>Refresh</button></header>
    {#if !sessionId}
        <div class="empty"><h2>Choose a chat to see its files</h2><p>Open a conversation to view its uploads and generated artifacts.</p><a href={chatHref}>Open chat</a></div>
    {:else}
        <div class="filters"><div role="group" aria-label="File types">
            {#each [['all','All files'],['artifacts','Artifacts'],['uploads','Uploads']] as [value,label]}<button type="button" aria-pressed={filter === value} class:chosen={filter === value} on:click={() => filter = value as typeof filter}>{label}</button>{/each}
        </div><input aria-label="Search files" type="search" placeholder="Search files, tasks or executions" bind:value={search} /></div>
        {#if pendingDelete}
            <ConfirmDialog open title="Delete file from this chat?" confirmLabel="Delete file"
                message={`Delete “${pendingDelete.name}” from this chat? Any chat copy will be permanently removed and existing chat links may stop working.${pendingDelete.task_id ? ' The task’s original stays available under Work.' : ''}`}
                on:cancel={() => pendingDelete = null} on:confirm={() => { if (pendingDelete) void remove(pendingDelete); }} />
        {/if}
        {#if error}<p class="error" role="alert">{error}</p>{/if}
        {#if partial}<p role="status">Some files are unavailable. These are the files we could load; refresh to retry.</p>{/if}
        {#if loading && files.length === 0}<p role="status">Loading files…</p>
        {:else if visible.length === 0 && !error}
            <div class="empty"><h2>{search ? 'No matching files' : partial ? 'No files available' : filter === 'uploads' ? 'No uploads yet' : filter === 'artifacts' ? 'No artifacts yet' : 'No files in this chat yet'}</h2>
                <p>{search ? 'Try a different name, task or execution.' : 'Files you upload and artifacts created by this chat or its tasks will appear here.'}</p><a href={chatHref}>Back to chat</a></div>
        {:else}
            <ul class="files">{#each visible as file (file.id)}<li>
                <div class="file-main"><button type="button" class="file-name" on:click={() => void show(file)}>{file.label || file.name}</button><span class="file-path">{file.path}</span>
                <div class="tags"><span>{file.source === 'upload' ? 'Uploaded' : 'Artifact'}</span>
                    <a href={chatHref} title={file.session_id || sessionId}>Chat: {chatTitle || file.session_id || sessionId}</a>
                    {#if file.task_id}<a href={workTaskHref(file.task_id)} title={file.task_id}>Task: {taskTitle(file.task_id)}</a>{/if}
                    {#if file.execution_id}<span title={file.execution_id}>Execution: {file.execution_id}</span>{/if}
                    {#if dateOf(file)}<time datetime={dateOf(file)?.toISOString()} title={dateOf(file)?.toISOString()}>{dateOf(file)?.toLocaleString()}</time>{:else}<span>Date unavailable</span>{/if}
                    <span title={sizeOf(file) === null ? undefined : `${Intl.NumberFormat().format(file.size!)} bytes`}>{formatSize(sizeOf(file))}</span>
                </div></div><div class="file-actions"><button type="button" class="download" aria-label={`Download ${file.name}`} on:click={() => void show(file, true)}>Download</button>
                <button type="button" class="delete" aria-label={`Delete ${file.name}`} disabled={deleting || loading} on:click={() => pendingDelete = file}>Delete</button></div>
            </li>{/each}</ul>
        {/if}
    {/if}
</section>
<style>
    .chat-files-page { flex: 1; min-height: 0; overflow: auto; padding: clamp(1rem, 3vw, 2rem); color: var(--text-primary); font-family: var(--font-primary); }
    header { display: flex; align-items: start; justify-content: space-between; gap: 1rem; margin-bottom: 1.2rem; }
    h1 { font-family: var(--font-display, inherit); font-size: 1.3rem; margin: 0; color: var(--text-primary); } h2 { font-size: 1rem; color: var(--text-primary); }
    p { color: var(--text-secondary); font-size: .85rem; }
    button, input { font: inherit; font-size: .8rem; color: var(--text-secondary); background: var(--bg-card); border: 1px solid var(--border-soft); border-radius: var(--radius-sm); padding: .45rem .7rem; transition: border-color .15s, color .15s, background .15s; }
    input { color: var(--text-body); } input::placeholder { color: var(--text-faint); } input:focus { border-color: var(--accent-primary); }
    button { cursor: pointer; } button:hover:not(:disabled) { color: var(--text-primary); border-color: var(--border-default); background: var(--bg-soft); } button:disabled { opacity: .5; cursor: default; }
    button:focus-visible, input:focus-visible, a:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
    a { color: var(--accent-primary); } a:hover { color: var(--accent-primary-hover); }
    .filters { display: flex; flex-wrap: wrap; gap: .75rem; justify-content: space-between; margin-bottom: 1rem; } .filters [role=group] { display: flex; gap: .3rem; }
    .filters input { width: min(23rem, 100%); }
    .chosen, .chosen:hover:not(:disabled) { color: var(--accent-primary); border-color: var(--accent-primary); background: var(--accent-primary-soft); }
    .files { list-style: none; padding: 0 1rem; margin: 0; background: var(--bg-surface); border: 1px solid var(--border-soft); border-radius: var(--radius-md); }
    li { display: flex; gap: 1rem; align-items: start; padding: 1rem 0; border-bottom: 1px solid var(--border-soft); } li:last-child { border-bottom: 0; }
    .file-main { min-width: 0; flex: 1; }
    .file-name, .file-name:hover:not(:disabled) { border: 0; padding: 0; text-align: left; font-weight: 600; color: var(--accent-primary); background: none; overflow-wrap: anywhere; }
    .file-name:hover:not(:disabled) { text-decoration: underline; }
    .file-path { display: block; color: var(--text-muted); font-family: var(--font-mono); font-size: .72rem; overflow-wrap: anywhere; margin: .3rem 0 .6rem; }
    .tags { display: flex; flex-wrap: wrap; gap: .35rem; font-size: .7rem; }
    .tags > * { border-radius: var(--radius-sm); padding: .25rem .45rem; background: var(--bg-soft); color: var(--text-secondary); border: 1px solid var(--border-soft); overflow-wrap: anywhere; text-decoration: none; }
    .tags > a:hover { color: var(--accent-primary); border-color: var(--accent-primary); }
    .empty { text-align: center; max-width: 34rem; padding: 3rem 1rem; margin: 2rem auto; background: var(--bg-surface); border: 1px dashed var(--border-default); border-radius: var(--radius-md); }
    .error { color: var(--color-error); background: var(--color-error-soft); border-radius: var(--radius-sm); padding: .5rem .7rem; }
    .file-actions { display: flex; flex-wrap: wrap; gap: .4rem; }
    .delete { color: var(--color-error); } .delete:hover:not(:disabled) { color: var(--color-error); border-color: var(--color-error); background: var(--color-error-soft); }
    .size-summary { margin: .5rem 0 0; color: var(--text-muted); }
    @media(max-width: 600px) { li { flex-direction: column; gap: .5rem; } .filters input { width: 100%; } }
</style>
