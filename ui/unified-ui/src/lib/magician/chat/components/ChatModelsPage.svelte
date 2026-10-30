<script lang="ts">
    import { onDestroy } from 'svelte';
    import { browser } from '$app/environment';
    import { scopeIdentityStore, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
    import { readUsageModels, type UsageModelTotals } from '$lib/llm/usageTotals';
    import { createVisiblePoll, DEFAULT_POLL_INTERVAL_MS, POLL_INTERVAL_CHOICES_MS } from '$lib/stores/visiblePoll';
    export let sessionId: string | null;
    const INTERVAL_KEY = 'magician.chat.models.refreshIntervalMs';
    let rows: UsageModelTotals[] = [], loading = false, error = '', key = '', generation = 0, search = '';
    let destroyed = false, now = Date.now();
    let intervalMs = readInterval();
    const poll = createVisiblePoll({ run: () => load(generation), intervalMs });
    const { nextAt } = poll;
    // One-second clock for the countdown, only while a refresh is scheduled.
    const clock = browser ? setInterval(() => { if ($nextAt !== null) now = Date.now(); }, 1000) : undefined;
    $: target = JSON.stringify([sessionId, $scopeIdentityStore.principal, $scopeIdentityStore.workspace, getCurrentScopeCredentialRevision()]);
    $: if (target !== key) { key = target; generation++; rows = []; error = ''; loading = false; if (browser && sessionId) poll.start(); else poll.stop(); }
    $: visible = rows.filter(row => `${row.provider} ${row.model}`.toLowerCase().includes(search.trim().toLowerCase()));
    $: if ($nextAt !== null) now = Date.now();
    $: secondsLeft = $nextAt === null ? null : Math.max(0, Math.ceil(($nextAt - now) / 1000));
    const number = (value: number | null) => value === null ? 'Not reported' : Intl.NumberFormat().format(value);
    const cost = (row: UsageModelTotals) => row.cost === null ? 'Not reported' : `${row.unpricedCalls ? '≥ ' : ''}$${row.cost.toFixed(row.cost < 0.01 ? 6 : 4)}`;
    const intervalLabel = (ms: number) => ms < 60_000 ? `${ms / 1000} s` : `${ms / 60_000} min`;
    const countdown = (seconds: number) => seconds >= 60 ? `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}` : `${seconds} s`;
    function readInterval(): number {
        try {
            const saved = Number(localStorage.getItem(INTERVAL_KEY));
            return (POLL_INTERVAL_CHOICES_MS as readonly number[]).includes(saved) ? saved : DEFAULT_POLL_INTERVAL_MS;
        } catch { return DEFAULT_POLL_INTERVAL_MS; }
    }
    function chooseInterval(ms: number): void {
        intervalMs = ms;
        try { localStorage.setItem(INTERVAL_KEY, String(ms)); } catch { /* per-viewer convenience only */ }
        poll.setIntervalMs(ms);
    }
    async function load(version: number): Promise<void> {
        if (!sessionId || destroyed || version !== generation) return;
        loading = true; error = '';
        try {
            const result = await readUsageModels('chat', sessionId);
            if (version === generation && !destroyed) rows = result;
        } catch (cause) {
            if (version === generation && !destroyed) error = cause instanceof Error ? cause.message : 'Model usage could not be loaded.';
            throw cause;
        } finally { if (version === generation && !destroyed) loading = false; }
    }
    onDestroy(() => { destroyed = true; generation++; poll.stop(); if (clock) clearInterval(clock); });
</script>
<section class="chat-models-page" aria-label="Chat model analytics">
    <header><div><h1>Models</h1><p>Recorded usage across this chat and its delegated work.</p></div>
        {#if sessionId}<div class="refresh">
            <span class="next" role="timer" aria-live="off">{loading ? 'Refreshing…' : secondsLeft === null ? 'Paused while this tab is hidden' : `Next refresh in ${countdown(secondsLeft)}`}</span>
            <label>Every <select aria-label="Refresh interval" value={intervalMs} on:change={(event) => chooseInterval(Number(event.currentTarget.value))}>
                {#each POLL_INTERVAL_CHOICES_MS as choice}<option value={choice}>{intervalLabel(choice)}</option>{/each}
            </select></label>
            <button type="button" disabled={loading} on:click={() => poll.pollNow()}>Refresh</button>
        </div>{/if}
    </header>
    {#if !sessionId}<div class="empty"><h2>Choose a chat to see model usage</h2><p>Model activity will appear here after usage is recorded.</p></div>
    {:else}
        <input type="search" aria-label="Search models" placeholder="Search models or providers" bind:value={search} />
        {#if error}<p role="alert">{error}{#if rows.length} Showing the last recorded breakdown.{/if}</p>{/if}
        {#if loading && !rows.length}<p role="status">Loading model usage…</p>
        {:else if !visible.length && !error}<div class="empty"><h2>{search ? 'No matching models' : 'No model usage recorded yet'}</h2><p>{search ? 'Try another model or provider.' : 'Usage will appear after this chat or its tasks make a measured model call.'}</p></div>
        {:else if visible.length}
            <div class="table-scroll"><table><caption class="sr-only">Model tokens, cache usage and cost for this chat</caption>
                <thead><tr><th scope="col">Model / provider</th><th scope="col">Calls</th><th scope="col">Input</th><th scope="col">Output</th><th scope="col">Total tokens</th><th scope="col">Cache read</th><th scope="col">Cache write</th><th scope="col">Cost</th></tr></thead>
                <tbody>{#each visible as row}<tr><th scope="row">{row.model}<span>{row.provider}</span></th><td>{number(row.calls)}</td><td>{number(row.input)}</td><td>{number(row.output)}</td><td>{row.input === null && row.output === null ? 'Not reported' : number((row.input ?? 0) + (row.output ?? 0))}</td><td>{number(row.cacheRead)}</td><td>{number(row.cacheWrite)}</td><td>{cost(row)}{#if row.unpricedCalls}<span>{row.unpricedCalls} unpriced</span>{/if}</td></tr>{/each}</tbody>
            </table></div>
            <p class="note">Cached tokens are included in input, so they are not added again to the total. Speech services can report cost without token counts. Unknown usage stays explicit.</p>
        {/if}
    {/if}
</section>
<style>
    .chat-models-page { flex: 1; min-height: 0; overflow: auto; padding: clamp(1rem, 3vw, 2rem); color: var(--text-primary); font-family: var(--font-primary); }
    header { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: start; gap: 1rem; margin-bottom: 1rem; }
    h1 { font-family: var(--font-display, inherit); font-size: 1.3rem; margin: 0; color: var(--text-primary); } h2 { font-size: 1rem; color: var(--text-primary); }
    p, th span, td span { color: var(--text-secondary); font-size: .8rem; }
    .refresh { display: flex; flex-wrap: wrap; align-items: center; gap: .6rem; font-size: .78rem; color: var(--text-secondary); }
    .next { font-variant-numeric: tabular-nums; color: var(--text-muted); }
    label { display: inline-flex; align-items: center; gap: .35rem; }
    button, input, select { font: inherit; font-size: .8rem; color: var(--text-body); background: var(--bg-card); border: 1px solid var(--border-soft); border-radius: var(--radius-sm); padding: .45rem .7rem; transition: border-color .15s, color .15s, background .15s; }
    select { padding-right: 1.6rem; } input::placeholder { color: var(--text-faint); }
    button { cursor: pointer; color: var(--text-secondary); } button:hover:not(:disabled) { color: var(--text-primary); border-color: var(--border-default); background: var(--bg-soft); } button:disabled { opacity: .5; cursor: default; }
    button:focus-visible, input:focus-visible, select:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; } input:focus, select:focus { border-color: var(--accent-primary); }
    input { width: min(24rem, 100%); box-sizing: border-box; margin-bottom: 1rem; }
    .table-scroll { overflow-x: auto; background: var(--bg-surface); border: 1px solid var(--border-soft); border-radius: var(--radius-md); }
    table { border-collapse: collapse; width: 100%; font-size: .8rem; color: var(--text-body); }
    th, td { padding: .7rem .6rem; text-align: right; border-bottom: 1px solid var(--border-soft); font-variant-numeric: tabular-nums; white-space: nowrap; }
    thead th { background: var(--bg-soft); color: var(--text-muted); font-weight: 600; font-size: .72rem; } tbody tr:last-child > * { border-bottom: 0; } tbody tr:hover { background: var(--bg-soft); }
    tbody th { color: var(--text-primary); }
    th:first-child { text-align: left; min-width: 11rem; white-space: normal; overflow-wrap: anywhere; } th span, td span { display: block; margin-top: .25rem; font-weight: 400; }
    .empty { text-align: center; padding: 3rem 1rem; background: var(--bg-surface); border: 1px dashed var(--border-default); border-radius: var(--radius-md); }
    [role=alert] { color: var(--color-error); background: var(--color-error-soft); border-radius: var(--radius-sm); padding: .5rem .7rem; }
    .note { margin-top: .8rem; color: var(--text-muted); }
</style>
