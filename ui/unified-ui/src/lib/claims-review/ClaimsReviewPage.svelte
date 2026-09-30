<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '$lib/shared/icons/Icon.svelte';
  import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
  import { listClaims, claimsRequest, pendingConfirmation, canConfirm, decisionRequest, deliveryLabel, saveClaimDecision, saveCommitmentDecision, saveConversation, type ConversationImport, type Claim, type ClaimPage, type ClaimStatus, type Commitment } from './api';

  let data: ClaimPage | null = null;
  let selectedId = '';
  let filter: ClaimStatus | 'all' = 'pending';
  let search = '';
  let loading = true;
  let busy = false;
  let error = '';
  let notice = '';
  let reviewer = '';
  let note = '';
  let reviewAction: 'confirm' | 'reject' | null = null;
  let pendingDecision: { claimId: string; action: 'confirm' | 'reject'; body: ReturnType<typeof decisionRequest> } | null = null;
  let controller = new AbortController();
  let scopeKey = '';
  let generation = 0;
  let debounce: ReturnType<typeof setTimeout> | undefined;
  let updated = '';
  let pageAfter: string | undefined;
  let commitments: Commitment[] | null = null;
  let commitmentGeneration = 0;
  let commitmentError = '';
  let activeTab: 'review' | 'import' = 'review';
  let transcript = '';
  let ourSpeaker = '';
  let attendees = '';
  let occurredAt = '';
  let audienceKind = 'person';
  let audienceId = '';
  let commitmentAction: { claim: Claim; term?: Commitment } | null = null;
  let pendingCommitment: { path: string; body: Record<string, unknown> } | null = null;
  let pendingImport: ConversationImport | null = null;
  let importKey = crypto.randomUUID();

  $: selected = data?.claims.find((claim) => claim.claim_id === selectedId) ?? null;
  $: selectedDelivery = selected ? data?.delivery[selected.claim_id] : undefined;
  $: confirmAllowed = selected ? canConfirm(selected, selectedDelivery) : false;

  async function load(after?: string) {
    const current = ++generation;
    loading = true; error = '';
    try {
      const next = await listClaims(filter, search, after, controller.signal);
      if (current !== generation || controller.signal.aborted) return;
      data = next; pageAfter = after;
      if (!next.claims.some((claim) => claim.claim_id === selectedId)) choose(next.claims[0]?.claim_id ?? '');
      updated = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    } catch (cause) {
      if (current === generation && !controller.signal.aborted) error = cause instanceof Error ? cause.message : 'Could not load claims.';
    } finally { if (current === generation) loading = false; }
  }
  function choose(id: string) {
    ++commitmentGeneration;
    selectedId = id; reviewAction = null; pendingDecision = null; note = ''; notice = ''; commitments = null; commitmentError = ''; commitmentAction = null; pendingCommitment = null;
  }
  function setFilter(value: typeof filter) { filter = value; choose(''); void load(); }
  function searchChanged() { clearTimeout(debounce); ++generation; loading = true; choose(''); debounce = setTimeout(() => { debounce = undefined; void load(); }, 250); }
  async function arm(action: 'confirm' | 'reject') {
    const claim = selected;
    if (!claim || busy) return;
    const signal = controller.signal;
    busy = true; error = ''; notice = '';
    try {
      const saved = await pendingConfirmation(claim, signal);
      if (signal.aborted || selectedId !== claim.claim_id) return;
      if (saved) {
        pendingDecision = { claimId: claim.claim_id, action: 'confirm', body: {
          by: saved.by, note: saved.note, expected_revision: saved.expected_revision, decision_id: saved.decision_id
        } };
        reviewer = saved.by; note = saved.note ?? ''; reviewAction = 'confirm';
        notice = 'A previous confirmation was interrupted. Retry the saved decision below to finish it; a different decision cannot replace it.';
      } else { reviewAction = action; pendingDecision = null; }
    } catch (cause) {
      if (!signal.aborted) error = cause instanceof Error ? cause.message : 'Could not check for a saved decision.';
    } finally { if (!signal.aborted) busy = false; }
  }
  async function decide() {
    if (!selected || !reviewAction || !reviewer.trim() || busy) return;
    const request = pendingDecision ?? { claimId: selected.claim_id, action: reviewAction, body: decisionRequest(selected, reviewer, note) };
    pendingDecision = request;
    const scope = scopeKey;
    const signal = controller.signal;
    busy = true; error = '';
    try {
      await saveClaimDecision(request.claimId, request.action, request.body, signal);
      if (scope !== scopeKey || signal.aborted) return;
      pendingDecision = null; reviewAction = null;
      await load();
      if (signal.aborted) return;
      notice = request.action === 'confirm' ? 'Statement confirmed. Its recipients and source are now on the record.' : 'Statement rejected. Your decision is saved in history.';
    } catch (cause) {
      if (scope === scopeKey && !signal.aborted) error = `${cause instanceof Error ? cause.message : 'Decision could not be saved.'} Retry sends the same decision. Refresh to read the current record.`;
    } finally { if (scope === scopeKey && !signal.aborted) busy = false; }
  }
  async function loadCommitments(claim: Claim) {
    if (!claim.audience_ref) return;
    const current = ++commitmentGeneration;
    const id = claim.claim_id;
    const scope = scopeKey;
    const signal = controller.signal;
    commitmentError = '';
    try {
      const query = new URLSearchParams({ audience_kind: claim.audience_ref.kind, audience_id: claim.audience_ref.id });
      const answer = await claimsRequest<{ commitments: Commitment[] }>(`/commitments?${query}`, signal);
      if (current === commitmentGeneration && id === selectedId && scope === scopeKey && !signal.aborted) commitments = answer.commitments;
    } catch (cause) { if (current === commitmentGeneration && id === selectedId && scope === scopeKey && !signal.aborted) commitmentError = String(cause); }
  }
  async function saveCommitment() {
    const action = commitmentAction;
    if (!action || !reviewer.trim() || busy || !action.claim.audience_ref) return;
    const signal = controller.signal;
    const audience = action.claim.audience_ref;
    const request = pendingCommitment ?? (action.term ? {
      path: `/commitments/${encodeURIComponent(action.term.commitment_id)}/confirm`,
      body: { audience_kind: audience.kind, audience_id: audience.id, by: reviewer.trim(), expected_revision: action.term.revision, decision_id: crypto.randomUUID() }
    } : {
      path: `/claims/${encodeURIComponent(action.claim.claim_id)}/commitment`,
      body: { expected_claim_revision: action.claim.revision, decision_id: crypto.randomUUID() }
    });
    pendingCommitment = request; busy = true; commitmentError = '';
    ++commitmentGeneration;
    try {
      const saved = await saveCommitmentDecision(request.path, request.body, action.claim, action.term, signal);
      if (signal.aborted) return;
      pendingCommitment = null; commitmentAction = null;
      await loadCommitments(action.claim);
      if (signal.aborted) return;
      notice = action.term ? `Your confirmation is saved. Current commitment status: ${saved.status}.`
        : saved.status === 'unconfirmed' ? 'Possible commitment recorded. It still needs a separate confirmation.'
        : `The recording request was already completed. Current commitment status: ${saved.status}.`;
    } catch (cause) {
      if (!signal.aborted) commitmentError = `${String(cause)} Retry keeps the same decision.`;
    } finally { if (!signal.aborted) busy = false; }
  }
  async function importTranscript() {
    if (!transcript.trim() || !ourSpeaker.trim() || !attendees.trim() || !occurredAt || busy) return;
    if (!Number.isFinite(Date.parse(occurredAt))) { error = 'Choose a valid date and time.'; return; }
    busy = true; error = ''; const signal = controller.signal;
    const request: ConversationImport = pendingImport ?? {
      transcript_key: `owner-import:${importKey}`, effective_speaker: ourSpeaker.trim(),
      attendees: attendees.split('\n').map((p) => p.trim()).filter(Boolean),
      extracted_by: 'claims-review-manual-import', occurred_at: new Date(occurredAt).toISOString(),
      ...(audienceId.trim() ? { audience_kind: audienceKind, audience_id: audienceId.trim() } : {}),
      utterances: [{ segment_key: 'statement', attribution: 'ours', speaker_id: ourSpeaker.trim(), spoken_text: transcript }]
    };
    pendingImport = request;
    try {
      await saveConversation(request, signal);
      if (signal.aborted) return;
      transcript = ''; pendingImport = null; importKey = crypto.randomUUID(); activeTab = 'review'; filter = 'pending';
      await load(); if (signal.aborted) return; notice = 'Conversation saved. Its current review status is available in the queue and history.';
    } catch (cause) { if (!signal.aborted) error = `${String(cause)} Retry keeps the same conversation.`; }
    finally { if (!signal.aborted) busy = false; }
  }
  function startSeparateImport() {
    pendingImport = null;
    importKey = crypto.randomUUID();
    error = '';
    notice = 'New import started. The previous attempt may already be in the review queue; check it before adding another copy.';
  }
  function formatDate(value?: string) { return value ? new Date(value).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' }) : '—'; }
  onMount(() => {
    const unsubscribe = scopeIdentityStore.subscribe((scope) => {
      const key = JSON.stringify([scope.principal, scope.workspace]);
      if (scopeKey === key) return;
      scopeKey = key; controller.abort(); controller = new AbortController(); ++generation;
      data = null; pageAfter = undefined; updated = ''; choose(''); reviewer = ''; pendingImport = null; audienceId = ''; search = ''; clearTimeout(debounce); debounce = undefined; busy = false; transcript = ''; ourSpeaker = ''; attendees = ''; occurredAt = ''; importKey = crypto.randomUUID();
      void load();
    });
    const refreshTimer = setInterval(() => {
      if (document.visibilityState === 'visible' && activeTab === 'review' && !loading && !busy
        && !reviewAction && !commitmentAction && !debounce) void load(pageAfter);
    }, 30_000);
    return () => { unsubscribe(); controller.abort(); ++generation; clearTimeout(debounce); clearInterval(refreshTimer); };
  });
</script>

<svelte:head><title>Claims Review · Magician</title></svelte:head>

<div class="claims-page">
  <header class="masthead">
    <div>
      <div class="eyebrow"><span class="tag">External conversations</span><span class="updated">{loading ? 'Updating…' : updated ? `Updated ${updated}` : 'Review workspace'}</span></div>
      <h1>Claims Review</h1>
      <p>Know what was said. Check the source. Keep a clear record.</p>
    </div>
    <div class="header-actions"><a class="button" href="/crew"><Icon name="sparkle" size={15} /> Crew</a><button class="button" disabled={loading || busy} on:click={() => { choose(selectedId); void load(); }}><Icon name="rotate-ccw" size={14} /> Refresh</button></div>
  </header>

  <section class="metrics" aria-label="Claim counts">
    {#each [{ key: 'pending', label: 'Needs review', caption: 'Waiting for your decision' }, { key: 'confirmed', label: 'Confirmed', caption: 'Recorded as something said' }, { key: 'rejected', label: 'Rejected', caption: 'Kept for a complete history' }] as metric}
      <button class:chosen={filter === metric.key} on:click={() => { activeTab = 'review'; setFilter(metric.key as ClaimStatus); }} disabled={busy}>
        <span class="metric-label">{metric.label}</span><strong>{data ? data.counts[metric.key as ClaimStatus] : '—'}</strong><span class="muted">{metric.caption}</span>
      </button>
    {/each}
  </section>

  <div class="tabs" role="tablist" aria-label="Claims Review sections">
    <button role="tab" aria-selected={activeTab === 'review'} on:click={() => activeTab = 'review'}>Review queue</button>
    <button role="tab" aria-selected={activeTab === 'import'} on:click={() => activeTab = 'import'}>Add a conversation</button>
  </div>
  {#if error}<div class="notice error" role="alert">{error}</div>{/if}
  {#if notice}<div class="notice success" role="status">{notice}</div>{/if}

  {#if activeTab === 'review'}
    <div class="toolbar">
      <label class="search"><Icon name="search" size={16} /><input aria-label="Search claims" placeholder="Search words, speaker, or recipient…" bind:value={search} on:input={searchChanged} disabled={busy} /></label>
      <select aria-label="Claim status" value={filter} on:change={(event) => setFilter(event.currentTarget.value as typeof filter)} disabled={busy}>
        <option value="pending">Needs review</option><option value="confirmed">Confirmed</option><option value="rejected">Rejected</option><option value="all">All history</option>
      </select>
      <span class="muted">{data ? `${data.count} statement${data.count === 1 ? '' : 's'}` : 'Loading records…'}</span>
    </div>
    <div class="review-layout" aria-busy={loading}>
      <section class="queue panel" aria-label="Statements">
        <div class="panel-heading"><h2>{filter === 'pending' ? 'Needs your review' : 'Statement history'}</h2><span class="tag">{data?.claims.length ?? '—'}</span></div>
        {#if !data && loading}<div class="empty"><span class="loading-dot"></span><h3>Loading your review queue</h3><p>Reading the claim register…</p></div>
        {:else if error && (!data || data.claims.length === 0)}<div class="empty"><h3>Review queue unavailable</h3><p>Refresh when the register can be read again.</p></div>
        {:else if data?.claims.length === 0}<div class="empty"><Icon name="check" size={26} /><h3>{search ? 'No matching statements' : filter === 'pending' ? 'You’re all caught up' : 'No statements here yet'}</h3><p>{search ? 'Try another speaker, recipient, or phrase.' : 'Envoy replies arrive here automatically. You can also add words from an external meeting or call.'}</p></div>
        {:else}
          {#each data?.claims ?? [] as claim (claim.claim_id)}
            <button class="claim-row" class:selected={selectedId === claim.claim_id} aria-pressed={selectedId === claim.claim_id} disabled={busy} on:click={() => choose(claim.claim_id)}>
              <div class="row-top"><span class="source">{claim.transcript_key.startsWith('envoy:') ? 'Envoy' : 'Conversation'}</span><span class="status" class:pending={claim.status === 'pending'}>{claim.status === 'pending' ? 'Needs review' : claim.status}</span></div>
              <p>{claim.stated_text}</p><div class="row-meta"><span>{claim.speaker}</span><span>{formatDate(claim.stated_at)}</span></div>
              <small>{deliveryLabel(data?.delivery[claim.claim_id])}</small>
            </button>
          {/each}
        {/if}
        {#if data?.next_cursor}<div class="pager"><button class="button" disabled={loading || busy} on:click={() => load(data?.next_cursor ?? undefined)}>Next 40 statements →</button></div>{/if}
        {#if data && data.count > data.claims.length}<button class="start-over" disabled={loading || busy} on:click={() => load()}>Back to first page</button>{/if}
      </section>

      <section class="detail panel" aria-label="Statement detail">
        {#if selected}
          <div class="panel-heading"><h2>The exact words</h2><span class="status" class:pending={selected.status === 'pending'}>{selected.status === 'pending' ? 'Needs review' : selected.status}</span></div>
          <div class="detail-body">
            <blockquote>{selected.stated_text}</blockquote>
            <div class="source-grid"><div><span class="field-label">Said by</span><strong>{selected.speaker}</strong></div><div><span class="field-label">When</span><span>{formatDate(selected.stated_at)}</span></div><div><span class="field-label">To</span><span>{selected.audience.join(', ')}</span></div><div><span class="field-label">Channel status</span><span>{deliveryLabel(selectedDelivery)}</span></div></div>
            <div class="review-guidance"><Icon name="info" size={16} /><p>Confirm means these words were said to these people. It does not verify the statement is true or approve a promise.</p></div>
            {#if selected.status === 'pending'}
              {#if selected.transcript_key.startsWith('envoy:') && !confirmAllowed}<div class="notice">The channel has not acknowledged this reply. Confirmation stays unavailable until a send receipt arrives.</div>{/if}
              {#if reviewAction}
                <form class="decision-form" on:submit|preventDefault={decide}>
                  <h3>{reviewAction === 'confirm' ? 'Confirm this statement' : 'Reject this statement'}</h3>
                  <label>Your name<input required bind:value={reviewer} disabled={busy || !!pendingDecision} autocomplete="name" placeholder="Name the person making this decision" /></label>
                  <label>Review note <span class="muted">(optional)</span><textarea bind:value={note} disabled={busy || !!pendingDecision} placeholder="What did you check?" rows="3"></textarea></label>
                  <div class="actions"><button class="button primary" disabled={busy || !reviewer.trim() || (reviewAction === 'confirm' && !confirmAllowed)}>{busy ? 'Saving…' : pendingDecision ? 'Retry decision' : 'Save decision'}</button><button class="button" type="button" disabled={busy} on:click={() => { reviewAction = null; pendingDecision = null; }}>Cancel</button></div>
                </form>
              {:else}<div class="actions"><button class="button primary" disabled={!confirmAllowed || busy || !!error || loading} on:click={() => arm('confirm')}><Icon name="check" size={16} /> Confirm statement</button><button class="button danger" disabled={busy || !!error || loading} on:click={() => arm('reject')}>Reject</button></div>{/if}
            {:else}<div class="decision-record"><span class="field-label">Review decision</span><p>{selected.status === 'confirmed' ? 'Confirmed' : 'Rejected'} by <strong>{selected.decided_by ?? 'Recorded reviewer'}</strong> · {formatDate(selected.decided_at)}</p>{#if selected.decision_note}<p>{selected.decision_note}</p>{/if}</div>{/if}
            <details><summary>Source &amp; evidence</summary><dl><dt>Conversation</dt><dd>{selected.transcript_key}</dd><dt>Statement</dt><dd>{selected.segment_key}</dd><dt>Captured by</dt><dd>{selected.extracted_by}</dd><dt>Evidence</dt><dd>{selected.evidence_refs.length ? selected.evidence_refs.join(', ') : 'No supporting evidence linked'}</dd><dt>Revision</dt><dd>{selected.revision}</dd></dl></details>
            {#if selected.audience_ref}
              <section class="commitments" aria-label="Relationship commitments">
                <h3>Relationship commitments</h3>
                <p class="muted">{selected.audience_ref.kind}: {selected.audience_ref.id}. A confirmed statement does not automatically become a promise.</p>
                <div class="actions"><button class="button" disabled={busy} on:click={() => selected && loadCommitments(selected)}>View commitments</button>
                  {#if selected.status === 'confirmed'}<button class="button" disabled={busy || !!error || loading} on:click={() => { if (selected) commitmentAction = { claim: selected }; pendingCommitment = null; }}>Record a possible commitment</button>{/if}
                </div>
                {#if commitmentError}<p role="alert">{commitmentError}</p>{/if}
                {#if commitments}{#each commitments as term}
                  <div class="term"><span class="tag">{term.status}</span><p>{term.terms}</p>
                    {#if term.status === 'unconfirmed'}<button class="button" disabled={busy} on:click={() => { if (selected) commitmentAction = { claim: selected, term }; pendingCommitment = null; }}>Review commitment</button>
                    {:else if term.confirmed_by}<small>Confirmed by {term.confirmed_by}</small>{/if}
                  </div>
                {:else}<p class="muted">No commitments recorded for this relationship.</p>{/each}{/if}
                {#if commitmentAction}<form class="decision-form" on:submit|preventDefault={saveCommitment}>
                  <h3>{commitmentAction.term ? 'Confirm this commitment' : 'Record these words as a possible commitment'}</h3>
                  <p class="muted">{commitmentAction.term ? 'Confirm only after checking that these terms are real and belong to this relationship.' : 'This records an unconfirmed term. You will still need to review it separately.'}</p>
                  <label>Your name<input required bind:value={reviewer} disabled={busy || !!pendingCommitment} autocomplete="name" /></label>
                  <div class="actions"><button class="button primary" disabled={busy || !reviewer.trim()}>{busy ? 'Saving…' : pendingCommitment ? 'Retry commitment decision' : commitmentAction.term ? 'Confirm commitment' : 'Record unconfirmed commitment'}</button><button class="button" type="button" disabled={busy} on:click={() => { commitmentAction = null; pendingCommitment = null; }}>Cancel</button></div>
                </form>{/if}
              </section>
            {/if}
          </div>
        {:else}<div class="empty detail-empty"><Icon name="search" size={30} /><h3>Select a statement</h3><p>Read the exact words and their source before making a decision.</p></div>{/if}
      </section>
    </div>
  {:else}
    <section class="panel import-panel"><div class="panel-heading"><h2>Add words from a meeting or call</h2><span class="tag">Manual source</span></div><form class="detail-body import-form" on:submit|preventDefault={importTranscript}>
      <p class="muted">Use this for an external conversation that already happened. Enter only words attributed to someone on your side. Envoy channel replies are captured automatically.</p>
      <div class="source-grid"><label>Who spoke for your side?<input required bind:value={ourSpeaker} placeholder="Named speaker" disabled={busy || !!pendingImport} /></label><label>When was it said?<input type="datetime-local" required bind:value={occurredAt} disabled={busy || !!pendingImport} /></label></div>
      <label>Who heard it? <span class="muted">One recipient per line</span><textarea required bind:value={attendees} rows="3" disabled={busy || !!pendingImport} placeholder="Named recipient or address"></textarea></label>
      <label>Exact words<textarea required bind:value={transcript} rows="7" disabled={busy || !!pendingImport} placeholder="Paste one statement, preserving the speaker’s wording."></textarea></label>
      <details><summary>Link to a relationship (optional)</summary><p class="muted">Use an existing relationship’s ID to make these words available to its commitment register.</p><div class="source-grid"><label>Relationship type<select bind:value={audienceKind} disabled={busy || !!pendingImport}><option value="person">Person</option><option value="account">Account</option><option value="engagement">Engagement</option><option value="program">Program</option><option value="panel">Panel</option></select></label><label>Relationship ID<input bind:value={audienceId} disabled={busy || !!pendingImport} placeholder="Existing relationship ID" /></label></div></details>
      <div class="actions"><button class="button primary" disabled={busy}>{busy ? 'Adding…' : pendingImport ? 'Retry conversation' : 'Add for review'}</button>{#if pendingImport}<button class="button" type="button" disabled={busy} on:click={startSeparateImport}>Start a separate import</button>{/if}<span class="muted">Starts unconfirmed. No message will be sent.</span></div>
    </form></section>
  {/if}
</div>

<style>
  .claims-page { box-sizing:border-box; color:var(--text-primary); display:grid; gap:1.25rem; margin:0 auto; max-width:var(--app-content-max,1360px); padding:1.5rem 1.5rem 5rem; width:100%; }
  .masthead { display:flex; flex-wrap:wrap; align-items:flex-start; justify-content:space-between; gap:1.25rem; padding:1.5rem; border:1px solid var(--border-soft); border-radius:12px; background:radial-gradient(ellipse at top right,color-mix(in srgb,var(--accent-primary) 12%,transparent),transparent 60%),var(--bg-card); }
  .eyebrow,.header-actions,.actions,.toolbar,.row-top,.row-meta,.panel-heading { display:flex; align-items:center; gap:.65rem; flex-wrap:wrap; }
  h1 { font-size:1.8rem; letter-spacing:-.035em; margin:.65rem 0 .45rem; } h2 { font-size:.95rem; margin:0; } h3 { font-size:1rem; margin:.5rem 0; } p { line-height:1.6; } .masthead p { margin:0; color:var(--text-secondary); }
  .tag,.source,.field-label,.metric-label { font-size:.68rem; font-weight:650; text-transform:uppercase; letter-spacing:.07em; }
  .tag { border:1px solid var(--border-soft); border-radius:5px; padding:.25rem .5rem; color:var(--text-secondary); } .updated,.muted { color:var(--text-muted); font-size:.78rem; }
  .button { display:inline-flex; align-items:center; justify-content:center; gap:.45rem; min-height:36px; padding:.5rem .8rem; border:1px solid var(--border-soft); border-radius:7px; background:var(--bg-card); color:var(--text-primary); font-size:.8rem; font-weight:600; text-decoration:none; cursor:pointer; }
  .button:hover { background:var(--bg-hover,var(--bg-soft)); } .button.primary { background:var(--accent-primary); border-color:var(--accent-primary); color:var(--text-on-accent,#fff); } .button.danger { color:var(--color-error,#cf645d); }
  button:disabled { opacity:.5; cursor:not-allowed; } button:focus-visible,input:focus-visible,textarea:focus-visible,select:focus-visible,a:focus-visible { outline:2px solid var(--accent-primary); outline-offset:3px; }
  .metrics { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:1rem; } .metrics button { text-align:left; display:grid; gap:.55rem; padding:1.1rem 1.25rem; border:1px solid var(--border-soft); border-radius:10px; background:var(--bg-card); color:var(--text-primary); cursor:pointer; } .metrics button.chosen { border-color:color-mix(in srgb,var(--accent-primary) 55%,var(--border-soft)); background:color-mix(in srgb,var(--accent-primary) 5%,var(--bg-card)); } .metric-label { color:var(--text-secondary); } .metrics strong { font-size:1.9rem; font-weight:650; letter-spacing:-.04em; }
  .tabs { display:flex; gap:1.5rem; border-bottom:1px solid var(--border-soft); } .tabs button { background:none; border:0; border-bottom:2px solid transparent; color:var(--text-muted); padding:.75rem .1rem; cursor:pointer; font-weight:600; font-size:.85rem; } .tabs button[aria-selected="true"] { color:var(--accent-primary); border-bottom-color:var(--accent-primary); }
  input,textarea,select { box-sizing:border-box; min-width:0; border:1px solid var(--input-border,var(--border-soft)); border-radius:7px; color:var(--text-primary); background:var(--input-bg,var(--bg-card)); padding:.65rem; font:inherit; font-size:.82rem; } textarea { resize:vertical; } label { display:grid; gap:.45rem; font-size:.8rem; color:var(--text-secondary); }
  .search { display:flex; align-items:center; gap:.6rem; flex:1 1 260px; min-width:0; padding:0 .7rem; border:1px solid var(--border-soft); border-radius:7px; background:var(--bg-card); } .search input { border:0; width:100%; background:transparent; } .toolbar select { min-width:155px; }
  .review-layout { display:grid; grid-template-columns:minmax(280px,.85fr) minmax(0,1.35fr); gap:1.25rem; align-items:start; } .panel { min-width:0; border:1px solid var(--border-soft); border-radius:10px; background:var(--bg-card); overflow:hidden; } .panel-heading { padding:1rem 1.25rem; border-bottom:1px solid var(--border-soft); justify-content:space-between; } .detail-body { padding:1.4rem; display:grid; gap:1.2rem; }
  .claim-row { width:100%; display:grid; gap:.65rem; text-align:left; padding:1.05rem 1.2rem; background:transparent; color:var(--text-primary); border:0; border-bottom:1px solid var(--border-soft); border-left:3px solid transparent; cursor:pointer; min-width:0; } .claim-row:hover { background:var(--bg-soft); } .claim-row.selected { background:color-mix(in srgb,var(--accent-primary) 7%,var(--bg-card)); border-left-color:var(--accent-primary); } .claim-row p { margin:0; font-size:.88rem; display:-webkit-box; -webkit-line-clamp:3; -webkit-box-orient:vertical; overflow:hidden; overflow-wrap:anywhere; } .row-top,.row-meta { justify-content:space-between; } .row-meta,small { font-size:.7rem; color:var(--text-muted); } .source { color:var(--text-secondary); } .status { font-size:.67rem; font-weight:600; text-transform:capitalize; color:var(--text-secondary); padding:.25rem .5rem; background:var(--bg-soft); border-radius:5px; } .status.pending { color:var(--color-warning,#b17b26); background:color-mix(in srgb,var(--color-warning,#b17b26) 10%,transparent); }
  blockquote { margin:0; padding:1.2rem 1.3rem; background:var(--bg-soft); border-left:3px solid var(--accent-primary); border-radius:0 8px 8px 0; white-space:pre-wrap; overflow-wrap:anywhere; font-size:1rem; line-height:1.75; max-height:45vh; overflow:auto; }
  .source-grid { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:1.1rem; } .source-grid>div { display:grid; gap:.45rem; font-size:.8rem; overflow-wrap:anywhere; } .field-label { color:var(--text-muted); } .review-guidance { display:flex; align-items:flex-start; gap:.7rem; padding:.8rem; border:1px solid var(--border-soft); border-radius:7px; color:var(--text-secondary); } .review-guidance p { margin:0; font-size:.78rem; } .review-guidance :global(svg) { flex-shrink:0; margin-top:.2rem; }
  .notice { padding:.9rem 1rem; border:1px solid var(--border-soft); border-radius:8px; font-size:.82rem; line-height:1.6; color:var(--text-secondary); background:var(--bg-soft); overflow-wrap:anywhere; } .notice.error { border-color:var(--color-error,#cf645d); } .notice.success { border-color:var(--color-success,#419a70); }
  .decision-form,.import-form { display:grid; gap:1rem; } .decision-record { font-size:.82rem; border-top:1px solid var(--border-soft); padding-top:1rem; } .decision-record p { margin:.5rem 0 0; white-space:pre-wrap; } details { border-top:1px solid var(--border-soft); padding-top:1rem; font-size:.78rem; color:var(--text-secondary); } summary { cursor:pointer; } dl { display:grid; grid-template-columns:100px minmax(0,1fr); gap:.65rem; } dt { color:var(--text-muted); } dd { margin:0; overflow-wrap:anywhere; } .term { padding:.8rem 0; border-bottom:1px solid var(--border-soft); } .term p { font-size:.85rem; } .empty { display:grid; place-items:center; text-align:center; padding:3rem 1.5rem; color:var(--text-muted); } .empty p { font-size:.82rem; max-width:28rem; margin:.4rem 0; } .empty h3 { color:var(--text-secondary); } .detail-empty { min-height:300px; align-content:center; } .pager { padding:1rem; } .start-over { background:none; border:0; color:var(--accent-primary); font-size:.78rem; padding:.8rem 1rem; cursor:pointer; } .loading-dot { width:12px; height:12px; border-radius:50%; background:var(--accent-primary); opacity:.5; } .import-panel { max-width:850px; }
  @media(max-width:1000px) { .review-layout { grid-template-columns:1fr; } .queue { max-height:420px; overflow:auto; } }
  @media(max-width:600px) { .claims-page { padding:1rem .8rem 4rem; } .metrics { gap:.5rem; } .metrics button { padding:.8rem; } .metrics .muted { display:none; } .source-grid { grid-template-columns:1fr; } .masthead { padding:1rem; } .detail-body { padding:1rem; } }
</style>
