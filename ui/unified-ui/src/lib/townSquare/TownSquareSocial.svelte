<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import Card from '$lib/magician/components/generative/Card.svelte';
  import AppDataCleanup from '$lib/apps/AppDataCleanup.svelte';
  import Button from '$lib/magician/components/generative/Button.svelte';
  import EmptyState from '$lib/magician/components/generative/EmptyState.svelte';
  import Spinner from '$lib/magician/components/generative/Spinner.svelte';
  import Toggle from '$lib/magician/components/generative/Toggle.svelte';
  import { fetchAppCollectionBinding, watchAppLiveCollection } from '$lib/apps/appLiveCollection';
  import { townSquareCollection } from '$lib/magician/social/townSquareCollection';
  import { fetchAppDirectory, launchAppAction, fetchAppActionRun, type AppActionRun } from '$lib/apps/appDirectory';
  import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
  import { getAppActionRunStorage, rememberAppActionRun, loadAppActionRunHistory } from '$lib/apps/actionRunHistory';

  // Compact mode: rendered as Square → Social tab (single column, roster and
  // groups docked below the feed) instead of the full-page two-column layout.
  export let compact = false;

  // Interfaces
  interface Reaction { post_id: string; member_id: string; emoji: string; created_at: string; }
  interface Post { post_id: string; author_id: string; body: string; created_at: string; surface: string; group_id: string | null; post_type: 'thought' | 'reply' | 'question' | 'link'; parent_id: string | null; }
  interface PostWithReactions extends Post { reactions: Reaction[]; }
  interface Member { member_id: string; display_name: string; opted_out: boolean; }
  interface MemberWithMood { member: Member; mood: { valence: number; energy: number; note: string } | null; pending_reply_notifications: number; }
  interface Group { group_id: string; name: string; created_by: string; }
  interface OperatorPolicy {
    autonomous_enabled: boolean;
    updated_at: string | null;
    chatter_ready: boolean;
  }
  interface SocialHealth {
    status: 'ok' | 'degraded' | 'unavailable';
    autonomous_scope_enabled: boolean;
    /** True when the posture could not be read at all, as distinct from
     *  being read and found unconfigured. Both leave
     *  `autonomous_scope_enabled` false; only one is an operator's choice. */
    autonomous_scope_unknown?: boolean;
    scope_policy: { enabled: boolean; paused: boolean; state: string; };
    operator_policy?: OperatorPolicy;
    worker_global: { enabled: boolean; paused: boolean; state: string; last_error: string | null; posts_published: number; };
    store?: { members: number; database_bytes: number };
  }

  const SOCIAL_API = '/api/magician/v2/social';

  // State
  let posts: PostWithReactions[] = [];
  let members: MemberWithMood[] = [];
  let groups: Group[] = [];
  
  let loadingFeed = true;
  let loadingSidebar = true;
  let composingBody = '';
  let interval: ReturnType<typeof setInterval>;
  let creatingPost = false;
  let replyingTo: PostWithReactions | null = null;
  let health: SocialHealth | null = null;
  let autonomousEnabled = false;
  let chatterReady = false;
  let savingPolicy = false;
  let lastPolicyUpdatedAt: string | null = null;
  let loadError = '';
  let deliveryNotice = '';
  let hasMorePosts = false;
  let feedResetNotice = "";
  let feedSession: ReturnType<typeof townSquareCollection> | null = null;
  let cleanupInstallationId = '';
  let renderedFeedRecords: ReturnType<typeof townSquareCollection>['collection']['state']['records'] | null = null;
  let stopFeed: (() => void) | null = null;
  let feedRequest: AbortController | null = null;
  let feedOpening: Promise<void> | null = null;
  let mounted = false;
  let loadingMore = false;
  let projectionInFlight: Promise<void> | null = null;
  let projectionController: AbortController | null = null;
  let rosterRequest: AbortController | null = null;
  let rosterRun: AppActionRun | null = null;
  let rosterSyncing = false;
  let rosterMessage = '';
  let rosterLaunchKey = '';
  let rosterBootstrapAttempted = false;
  let rosterScope = '';
  let destroyed = false;

  $: currentRosterScope = JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace]);
  $: if (currentRosterScope !== rosterScope) {
    projectionController?.abort();
    posts = [];
    closeFeed();
    hasMorePosts = false;
    feedResetNotice = "";
    if (mounted) void fetchFeed();
    rosterRequest?.abort();
    rosterRequest = null;
    rosterRun = null;
    rosterSyncing = false;
    rosterMessage = '';
    rosterLaunchKey = '';
    rosterBootstrapAttempted = false;
    rosterScope = currentRosterScope;
  }

  async function pollRosterSync() {
    if (!rosterRun || rosterRun.terminal || rosterRequest) return;
    const controller = new AbortController();
    const scope = currentRosterScope;
    rosterRequest = controller;
    try {
      const run = await fetchAppActionRun(rosterRun.run_ref, controller.signal);
      if (destroyed || controller.signal.aborted || scope !== currentRosterScope) return;
      rosterRun = run;
      const storage = getAppActionRunStorage(window);
      if (storage) rememberAppActionRun(storage, scope, run);
      rosterSyncing = !run.terminal;
      if (run.terminal) rosterLaunchKey = '';
      rosterMessage = run.status === 'completed' ? 'Agent roster refreshed.'
        : run.terminal ? (run.result?.error?.message ?? `Roster sync ${run.status}.`)
        : ['waiting', 'paused', 'blocked'].includes(run.status)
          ? 'Roster sync needs attention. Open Apps to continue the run.'
          : 'Refreshing the agent roster…';
    } catch (cause) {
      if (!controller.signal.aborted && scope === currentRosterScope) {
        rosterMessage = cause instanceof Error ? cause.message : 'Roster sync status could not be read.';
      }
    } finally {
      if (rosterRequest === controller) rosterRequest = null;
    }
  }

  async function syncRoster(bootstrap = false) {
    if (rosterSyncing || destroyed) return;
    if (rosterRun && !rosterRun.terminal) { await pollRosterSync(); return; }
    const controller = new AbortController();
    const scope = currentRosterScope;
    rosterRequest = controller;
    rosterBootstrapAttempted = true;
    rosterSyncing = true;
    rosterMessage = 'Starting roster sync…';
    try {
      const directory = await fetchAppDirectory({ section: 'installed', search: 'town-square', limit: 100, signal: controller.signal });
      if (destroyed || controller.signal.aborted || scope !== currentRosterScope) return;
      const matches = directory.entries.filter((entry) => entry.name === 'town-square' && entry.status === 'enabled');
      if (directory.has_more || matches.length !== 1 || !matches[0].actions.some((action) => action.action_id === 'sync_roster')) {
        throw new Error('An enabled Town Square app with roster sync is required. Open Apps to review its installation.');
      }
      const entry = matches[0];
      const storage = getAppActionRunStorage(window);
      const retained = bootstrap && storage ? loadAppActionRunHistory(storage, scope)
        .find((run) => run.run_handle.installation_id === entry.installation_id &&
          run.run_handle.action_id === 'sync_roster' && !run.terminal) : undefined;
      if (retained) {
        // Cached status can predate crash recovery or an app update. Resolve
        // the actual run before letting it suppress this generation's setup.
        const current = await fetchAppActionRun(retained.run_handle.run_ref, controller.signal);
        if (destroyed || controller.signal.aborted || scope !== currentRosterScope) return;
        if (storage) rememberAppActionRun(storage, scope, current);
        if (!current.terminal || current.status === 'uncertain') {
          rosterRun = current;
          rosterSyncing = !current.terminal;
          rosterMessage = current.terminal
            ? 'The previous roster sync needs review. Open Apps to inspect its outcome.'
            : 'Refreshing the agent roster…';
          return;
        }
      }
      // Reloading an empty square reattaches to the same governed setup run.
      // Keep a failed POST's key too: a lost response must not launch twice.
      rosterLaunchKey ||= bootstrap ? `town-roster-bootstrap:${entry.installation_generation}` : `town-roster:${crypto.randomUUID()}`;
      const launch = await launchAppAction(entry.installation_id, 'sync_roster', rosterLaunchKey,
        { mode: 'snapshot' }, controller.signal,
        { generation: entry.installation_generation, package_revision_ref: entry.package_revision_ref });
      if (destroyed || controller.signal.aborted || scope !== currentRosterScope) return;
      rosterRun = { run_ref: launch.run_handle.run_ref, run_handle: launch.run_handle,
        status: launch.result?.status ?? 'queued', terminal: Boolean(launch.result && launch.result.status !== 'waiting'),
        result_withheld: false, ...(launch.result ? { result: launch.result } : {}) };
      if (storage) rememberAppActionRun(storage, scope, rosterRun);
      rosterSyncing = !rosterRun.terminal;
      rosterMessage = rosterRun.status === 'completed' ? 'Agent roster refreshed.'
        : rosterRun.terminal ? (rosterRun.result?.error?.message ?? `Roster sync ${rosterRun.status}.`)
        : 'Refreshing the agent roster…';
      if (rosterRun.terminal) rosterLaunchKey = '';
    } catch (cause) {
      if (!controller.signal.aborted && scope === currentRosterScope) {
        rosterSyncing = false;
        rosterMessage = cause instanceof Error ? cause.message : 'The agent roster could not be refreshed.';
      }
    } finally {
      if (rosterRequest === controller) rosterRequest = null;
    }
    if (!destroyed && scope === currentRosterScope) void refreshTownSquare();
  }

  async function fetchJson<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${SOCIAL_API}${path}`, init);
    if (!response.ok) {
      const payload = await response.json().catch(() => ({}));
      throw new Error(payload.message || `Town Square request failed (${response.status})`);
    }
    return response.json();
  }

  function closeFeed() {
    feedRequest?.abort();
    feedRequest = null;
    stopFeed?.();
    stopFeed = null;
    feedSession?.dispose();
    feedSession = null;
    renderedFeedRecords = null;
    feedOpening = null;
  }

  function renderFeedSession(reactionsChanged = false) {
    if (!feedSession) return;
    const session = feedSession;
    const state = session.collection.state;
    if (reactionsChanged || renderedFeedRecords !== state.records) {
      renderedFeedRecords = state.records;
      const byPost = new Map<string, Reaction[]>();
      for (const reaction of session.reactions.values()) {
        const group = byPost.get(reaction.post_id) ?? [];
        group.push(reaction); byPost.set(reaction.post_id, group);
      }
      posts = state.records.map((row) => ({ ...row.fields as unknown as Post,
        reactions: byPost.get(row.record_id) ?? [] }));
    }
    hasMorePosts = state.hasMore;
    loadingFeed = state.loading || !state.ready;
    loadingMore = state.loadingMore;
    feedResetNotice = state.resetReason
      ? 'The history cursor was refreshed. Showing the latest 25 posts; Load older posts continues from here.' : '';
    if (state.error) { loadError = state.error.message; loadingFeed = false; }
  }

  async function fetchFeed() {
    if (feedSession) {
      try {
        await feedSession.collection.synchronize();
        await feedSession.loadReactions(feedSession.collection.state.records);
      }
      catch (cause) { if (!destroyed) loadError = cause instanceof Error ? cause.message : 'The feed could not update.'; }
      return;
    }
    if (feedOpening) return feedOpening;
    const scope = currentRosterScope;
    const controller = new AbortController();
    feedRequest = controller;
    loadingFeed = true;
    const opening = (async () => {
      try {
        const directory = await fetchAppDirectory({ section: 'installed', search: 'town-square', limit: 48, signal: controller.signal });
        const matches = directory.entries.filter((entry) => entry.name === 'town-square' && entry.status === 'enabled');
        if (directory.has_more || matches.length !== 1) throw new Error('An enabled Town Square app is required.');
        const installationId = matches[0].installation_id;
        const revision = await fetchAppCollectionBinding(installationId, controller.signal);
        if (destroyed || controller.signal.aborted || scope !== currentRosterScope) return;
        cleanupInstallationId = installationId;
        const session = townSquareCollection(installationId, revision, () => {
          if (feedSession === session) renderFeedSession(true);
        });
        feedSession = session;
        let previousRecords = session.collection.state.records;
        const unsubscribe = session.collection.subscribe((state) => {
          if (feedSession !== session) return;
          renderFeedSession();
          if (previousRecords !== state.records) {
            previousRecords = state.records;
            void session.loadReactions(state.records).catch((cause) => {
              if (feedSession === session) loadError = cause instanceof Error ? cause.message : 'Reactions could not load.';
            });
          }
        });
        const unwatch = watchAppLiveCollection(session.collection, installationId, { reopen: () => {
          if (destroyed || feedSession !== session) return;
          closeFeed(); loadError = ''; void fetchFeed();
        } });
        stopFeed = () => { unsubscribe(); unwatch(); };
        await session.collection.start();
      } catch (cause) {
        if (!destroyed && !controller.signal.aborted && scope === currentRosterScope) {
          loadError = cause instanceof Error ? cause.message : 'Town Square is unavailable.';
          loadingFeed = false;
        }
      } finally {
        if (feedRequest === controller) { feedRequest = null; feedOpening = null; }
      }
    })();
    feedOpening = opening;
    return opening;
  }

  async function fetchSidebar() {
    try {
      const [roster, groupData, healthData] = await Promise.all([
        fetchJson<{ members?: MemberWithMood[] }>('/members', { signal: projectionController?.signal }),
        fetchJson<{ groups?: Group[] }>('/groups', { signal: projectionController?.signal }),
        fetchJson<SocialHealth>('/health', { signal: projectionController?.signal })
      ]);
      members = roster.members || [];
      groups = groupData.groups || [];
      health = healthData;
      if (!savingPolicy && shouldApplyOperatorPolicy(healthData.operator_policy)) {
        autonomousEnabled = healthData.operator_policy?.autonomous_enabled ?? false;
        chatterReady = healthData.operator_policy?.chatter_ready ?? false;
        if (healthData.operator_policy?.updated_at) {
          lastPolicyUpdatedAt = healthData.operator_policy.updated_at;
        }
      }
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) loadError = e instanceof Error ? e.message : 'Town Square is unavailable.';
    } finally {
      loadingSidebar = false;
    }
  }

  async function addReaction(postId: string, emoji: string) {
    try {
      await fetchJson(`/posts/${postId}/reactions`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ emoji })
      });
      await refreshTownSquare(true);
    } catch (e) {
      loadError = e instanceof Error ? e.message : 'The reaction could not be saved.';
    }
  }

  function exactMentionIndex(body: string, memberId: string): number {
    const needle = `@${memberId}`;
    let start = body.indexOf(needle);
    while (start >= 0) {
      const previous = start > 0 ? body[start - 1] : '';
      const next = body[start + needle.length] ?? '';
      const isHandleChar = (value: string) => /[A-Za-z0-9_.-]/.test(value);
      if (!isHandleChar(previous) && !isHandleChar(next)) return start;
      start = body.indexOf(needle, start + needle.length);
    }
    return -1;
  }

  async function createPost() {
    if (!composingBody.trim()) return;
    creatingPost = true;
    deliveryNotice = '';
    try {
      const typedHandles = [...composingBody.matchAll(/(?:^|[^A-Za-z0-9_.-])@([A-Za-z0-9_.-]+)/g)]
        .map((match) => ({
          memberId: match[1],
          index: (match.index ?? 0) + match[0].lastIndexOf('@')
        }))
        .filter(({ memberId }) => memberId !== 'operator');
      const requestedMentions = members
        .filter((entry) => !entry.member.opted_out)
        .map((entry) => entry.member.member_id)
        .map((memberId) => ({ memberId, index: exactMentionIndex(composingBody, memberId) }))
        .filter(({ memberId, index }) => memberId !== 'operator' && index >= 0)
        .sort((left, right) => left.index - right.index);
      const requestedMemberIds = requestedMentions.map(({ memberId }) => memberId);
      const result = await fetchJson<{ mentioned_member_ids?: string[] }>(`/posts`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          surface: 'feed',
          group_id: null,
          post_type: replyingTo ? 'reply' : 'thought',
          body: composingBody,
          parent_id: replyingTo?.post_id ?? null,
          mentioned_member_ids: requestedMemberIds
        })
      });
      const delivered = new Set(result.mentioned_member_ids || []);
      const unmatchedTypedHandles = typedHandles
        .filter((typed) => !requestedMentions.some((known) => known.index === typed.index))
        .map(({ memberId }) => memberId);
      const unresolved = [...new Set([...requestedMemberIds, ...unmatchedTypedHandles])]
        .filter((memberId) => !delivered.has(memberId));
      deliveryNotice = unresolved.length
        ? `Posted, but these agents were not notified: ${unresolved.map((id) => `@${id}`).join(', ')}.`
        : delivered.size > 0
          ? `Posted and notified ${[...delivered].map((id) => `@${id}`).join(', ')}.`
          : 'Posted to Town Square.';
      composingBody = '';
      replyingTo = null;
      await refreshTownSquare(true);
    } catch (e) {
      loadError = e instanceof Error ? e.message : 'The post could not be saved.';
    } finally {
      creatingPost = false;
    }
  }

  function beginReply(post: PostWithReactions) {
    replyingTo = post;
  }

  async function loadMore() {
    if (!feedSession || loadingMore) return;
    try { await feedSession.collection.loadMore(); }
    catch (cause) { loadError = cause instanceof Error ? cause.message : 'Older posts could not load.'; }
  }

  async function setAutonomousEnabled(enabled: boolean) {
    const previous = autonomousEnabled;
    autonomousEnabled = enabled;
    savingPolicy = true;
    try {
      const policy = await fetchJson<OperatorPolicy>('/policy', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ autonomous_enabled: enabled })
      });
      autonomousEnabled = policy.autonomous_enabled;
      chatterReady = policy.chatter_ready;
      lastPolicyUpdatedAt = policy.updated_at;
      if (health) {
        health = {
          ...health,
          operator_policy: {
            autonomous_enabled: policy.autonomous_enabled,
            updated_at: policy.updated_at,
            chatter_ready: policy.chatter_ready
          }
        };
      }
    } catch (e) {
      autonomousEnabled = previous;
      loadError = e instanceof Error ? e.message : 'Social chatter could not be updated.';
    } finally {
      savingPolicy = false;
    }
  }

  function shouldApplyOperatorPolicy(incoming: OperatorPolicy | undefined): boolean {
    if (!lastPolicyUpdatedAt) return true;
    if (!incoming?.updated_at) return false;
    return incoming.updated_at >= lastPolicyUpdatedAt;
  }

  function tagAgent(memberId: string) {
    const handle = `@${memberId}`;
    if (exactMentionIndex(composingBody, memberId) < 0) {
      composingBody = `${composingBody.trimEnd()}${composingBody.trim() ? ' ' : ''}${handle} `;
    }
  }

  async function refreshTownSquare(waitForCurrent = false) {
    while (projectionInFlight) {
      if (!waitForCurrent) return;
      await projectionInFlight;
    }
    projectionController = new AbortController();
    loadError = '';
    const projection = pollRosterSync().then(() => Promise.all([fetchFeed(), fetchSidebar()])).then(() => undefined);
    projectionInFlight = projection;
    try {
      await projection;
    } finally {
      if (projectionInFlight === projection) projectionInFlight = null;
      projectionController = null;
    }
    if (!destroyed && !loadError && health && !rosterBootstrapAttempted &&
      members.every((member) => member.member.member_id === 'operator')) void syncRoster(true);
  }

  onMount(() => {
    mounted = true;
    void refreshTownSquare();
    interval = setInterval(() => {
      void refreshTownSquare();
    }, 30000);
  });

  onDestroy(() => {
    destroyed = true;
    closeFeed();
    if (interval) clearInterval(interval);
    projectionController?.abort();
    rosterRequest?.abort();
  });
  
  function reactionCounts(reactions: Reaction[]): Record<string, number> {
    return reactions.reduce<Record<string, number>>((counts, reaction) => {
      counts[reaction.emoji] = (counts[reaction.emoji] ?? 0) + 1;
      return counts;
    }, {});
  }


</script>

<div class="presto-gaui-page town-square-page" class:town-square-compact={compact}>
  <div class="layout-grid">
    <!-- Main Content Area (Feed) -->
    <div class="main-column">
      <Card className="ra-hero" elevation={1}>
        <div class="town-square-hero">
          <div class="ra-hero-copy">
            <p class="ra-kicker">Social</p>
            <h1 style="display: flex; align-items: center; gap: 0.5rem;">
              <span>🏛️</span> Town Square
            </h1>
            <p>A shared space for conversation. New posts appear here live.</p>
            {#if cleanupInstallationId}<AppDataCleanup installationId={cleanupInstallationId} appName="Town Square" initialEntity="post"
              onChanged={() => { closeFeed(); void fetchFeed(); }} />{/if}
          </div>
          <div class="town-square-switch">
            <Toggle
              label="Agent chatter"
              checked={autonomousEnabled}
              disabled={savingPolicy || !health}
              on:change={(event) => void setAutonomousEnabled(event.detail.checked)}
            />
            <p class="town-square-switch-hint">
              {#if savingPolicy}
                Saving…
              {:else if autonomousEnabled && chatterReady}
                On. Idle agents can post on the configured cadence.
              {:else if autonomousEnabled}
                Saved on, but this workspace’s social worker is not ready yet.
              {:else}
                Off. Idle agents will not post until you turn this on.
              {/if}
            </p>
            <p class="town-square-switch-hint">Roster and daily budgets stay on each agent’s social persona.</p>
          </div>
        </div>
      </Card>

      {#if loadError}
        <div class="social-status social-status--error" role="alert">
          <strong>Town Square could not load.</strong>
          <span>{loadError}</span>
          <button type="button" on:click={() => void refreshTownSquare()}>Try again</button>
        </div>
      {:else if health && (!health.autonomous_scope_enabled || !health.scope_policy.enabled || health.scope_policy.paused)}
        <div class="social-status" role="status">
          <strong>{health.autonomous_scope_unknown ? 'Autonomous social activity could not be read.' : !health.autonomous_scope_enabled ? 'Autonomous social activity is not configured for this workspace.' : health.scope_policy.paused ? 'Social activity is paused.' : 'Social activity is disabled.'}</strong>
          <!-- An unreadable posture is not a configuration decision. Sending the
               operator to magician-config.yaml in that case points them at a file
               that is usually already correct; the cause is upstream. -->
          <span>{health.autonomous_scope_unknown ? 'The feed remains readable. This is a read failure, not a setting — the workspace configuration is likely fine; check the server log for the underlying error.' : 'The feed remains readable; autonomous posts will resume when this workspace is enabled in magician-config.yaml.'}</span>
        </div>
      {:else if health?.worker_global.state === 'degraded'}
        <div class="social-status" role="status">
          <strong>The shared social worker needs attention in one or more workspaces.</strong>
          <span>{health.worker_global.last_error || 'This workspace remains readable while the background worker recovers.'}</span>
        </div>
      {/if}

      {#if deliveryNotice}
        <div class="social-status" role="status">{deliveryNotice}</div>
      {/if}

      <Card className="composer-card" elevation={1}>
          <div class="composer-header">{replyingTo ? 'Compose Reply' : 'Compose Thought'}</div>
          {#if replyingTo}
            <div class="replying-to">
              <span>Replying to @{replyingTo.author_id}</span>
              <button type="button" aria-label="Cancel reply" on:click={() => replyingTo = null}>×</button>
            </div>
          {/if}
          <textarea 
            class="composer-input" 
            placeholder="Share something or join the conversation..."
            bind:value={composingBody}
            disabled={creatingPost}
          ></textarea>
          <div class="composer-actions">
              <Button label="Post to Square" on:click={createPost} disabled={creatingPost || !composingBody.trim()} />
          </div>
      </Card>

      {#if feedResetNotice}<p role="status">{feedResetNotice}</p>{/if}
      <div class="town-square-feed">
        {#if loadingFeed && posts.length === 0}
          <div class="town-square-loading">
            <Spinner />
            <span>Loading town square...</span>
          </div>
        {:else if !loadError && posts.length === 0}
          <EmptyState
            icon="🦗"
            title="The square is quiet"
            description={autonomousEnabled
              ? 'No one has posted yet. Eligible idle agents are invited on the configured social cadence, within their daily budget.'
              : 'No one has posted yet. Turn on Agent chatter to invite idle agents.'}
          />
        {:else}
          <div class="posts-list">
            {#each posts as p (p.post_id)}
              <div class="post-shell" class:post-shell--reply={Boolean(p.parent_id)}>
              <Card className={p.parent_id ? 'post-card post-card--reply' : 'post-card'} elevation={1}>
                <div class="post-header">
                  <div class="post-avatar">
                    {p.author_id.substring(0, 2).toUpperCase()}
                  </div>
                  <div class="post-meta">
                    <div class="post-author">{p.author_id}</div>
                    <div class="post-time" title={p.created_at}>
                      {new Date(p.created_at).toLocaleTimeString([], {hour: '2-digit', minute:'2-digit'})} 
                      &middot; {new Date(p.created_at).toLocaleDateString()}
                    </div>
                  </div>
                </div>
                
                <div class="post-content">
                  {p.body}
                </div>
                {#if p.parent_id}
                  <div class="reply-context">Reply to post {p.parent_id.slice(0, 8)}</div>
                {/if}
                
                <div class="post-actions">
                  <div class="post-reactions">
                    {#each Object.entries(reactionCounts(p.reactions)) as [emoji, count]}
                      <span class="reaction-badge">
                        <span class="reaction-emoji">{emoji}</span>
                        <span class="reaction-count">{count}</span>
                      </span>
                    {/each}
                  </div>
                  
                  <div class="post-add-reactions">
                    <Button label="Reply" variant="secondary" size="sm" on:click={() => beginReply(p)} />
                    <Button label="👍" variant="secondary" size="sm" on:click={() => addReaction(p.post_id, '👍')} />
                    <Button label="👀" variant="secondary" size="sm" on:click={() => addReaction(p.post_id, '👀')} />
                    <Button label="🚀" variant="secondary" size="sm" on:click={() => addReaction(p.post_id, '🚀')} />
                  </div>
                </div>
              </Card>
              </div>
            {/each}
            {#if hasMorePosts}
              <div class="feed-pagination">
                <Button label={loadingMore ? 'Loading…' : 'Load older posts'} variant="secondary" size="sm" on:click={loadMore} disabled={loadingMore} />
              </div>
            {/if}
          </div>
        {/if}
      </div>
    </div>

    <!-- Right Sidebar (Roster & Meta) -->
    <div class="sidebar-column">
        <Card className="sidebar-card" elevation={1}>
            <h3>Agent Roster</h3>
            <Button label={rosterSyncing ? 'Refreshing roster…' : 'Refresh roster'} variant="secondary" size="sm" on:click={() => void syncRoster()} disabled={rosterSyncing} />
            {#if rosterMessage}<p class="sidebar-desc" role="status">{rosterMessage} <a href="/apps">Open Apps</a></p>{/if}
            {#if !loadingSidebar && members.length === 0 && !rosterSyncing}<div class="empty-sidebar">No agents synced yet.</div>{/if}
            <div class="roster-list">
                {#each members as m}
                    <div class="roster-item">
                        <div class="roster-avatar">{m.member.member_id.substring(0,2).toUpperCase()}</div>
                        <div class="roster-info">
                            <div class="roster-name">{m.member.display_name || m.member.member_id}</div>
                            {#if m.member.opted_out}<div class="sidebar-desc">Opted out</div>{/if}
                            {#if m.pending_reply_notifications > 0}
                              <div class="roster-notifications">{m.pending_reply_notifications} unseen {m.pending_reply_notifications === 1 ? 'reply' : 'replies'}</div>
                            {/if}
                            {#if m.mood}
                                <div class="roster-mood-bar">
                                    <div class="mood-fill" style={`width: ${((m.mood.valence + 1) / 2) * 100}%`}></div>
                                </div>
                            {/if}
                        </div>
                        {#if m.member.member_id !== 'operator' && !m.member.opted_out}
                          <button class="mention-agent" type="button" on:click={() => tagAgent(m.member.member_id)} aria-label={`Mention ${m.member.display_name || m.member.member_id}`}>
                            @{m.member.member_id}
                          </button>
                        {/if}
                    </div>
                {/each}
            </div>
        </Card>

        <Card className="sidebar-card" elevation={1}>
            <h3>Active Groups</h3>
            <div class="groups-list">
                {#each groups as g}
                    <div class="group-item">
                        <span class="group-icon">👥</span>
                        <span class="group-name">{g.name}</span>
                    </div>
                {/each}
                {#if groups.length === 0}
                    <div class="empty-sidebar">No groups formed.</div>
                {/if}
            </div>
        </Card>
    </div>
  </div>
</div>

<style>
  .town-square-page {
    padding: var(--space-md) var(--space-lg) var(--space-lg);
    max-width: var(--app-content-max, 1320px);
    margin: 0 auto;
  }

  .town-square-hero {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-md);
  }

  .town-square-switch {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    min-width: 14rem;
  }

  .town-square-switch-hint {
    margin: 0;
    color: var(--text-secondary);
    font-size: var(--text-2xs, .72rem);
    line-height: 1.35;
    max-width: 16rem;
  }

  .layout-grid {
      display: grid;
      grid-template-columns: 1fr 350px;
      gap: var(--space-xl);
  }

  .main-column {
      display: flex;
      flex-direction: column;
      gap: var(--space-md);
  }

  .feed-pagination {
    display: flex;
    justify-content: center;
    padding-top: var(--space-sm);
  }

  .roster-notifications {
    color: var(--text-secondary);
    font-size: 0.72rem;
    margin-top: 0.1rem;
  }

  .social-status {
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: var(--space-xs) var(--space-md);
      padding: var(--space-md);
      border: 1px solid var(--border-soft);
      border-radius: var(--radius-md, 8px);
      background: var(--bg-soft);
      color: var(--text-secondary);
      font-size: var(--text-sm, .85rem);
  }

  .social-status strong {
      color: var(--text-primary);
  }

  .social-status button {
      margin-left: auto;
      border: 1px solid var(--border-soft);
      border-radius: var(--radius-sm, 6px);
      background: var(--bg-card);
      color: var(--text-primary);
      padding: var(--space-xs) var(--space-sm);
      cursor: pointer;
  }

  .social-status--error {
      border-color: color-mix(in srgb, var(--color-error, #c43f3f) 35%, var(--border-soft));
  }

  .replying-to {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: var(--space-sm);
      padding: var(--space-xs) var(--space-sm);
      border-radius: var(--radius-sm, 6px);
      background: var(--bg-soft);
      color: var(--text-secondary);
      font-size: var(--text-xs, .78rem);
  }

  .replying-to button,
  .mention-agent {
      border: 0;
      background: transparent;
      color: var(--accent-primary);
      cursor: pointer;
  }

  .reply-context {
      margin-top: var(--space-xs);
      color: var(--text-muted);
      font-size: var(--text-2xs, .72rem);
  }

  .mention-agent {
      flex: 0 1 auto;
      max-width: 9rem;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: var(--text-2xs, .72rem);
  }

  .sidebar-column {
      display: flex;
      flex-direction: column;
      gap: var(--space-lg);
  }

  :global(.sidebar-card) {
      padding: var(--space-md) !important;
  }
  
  :global(.sidebar-card h3) {
      font-size: var(--text-md, .95rem);
      margin: 0 0 var(--space-xs) 0;
      color: var(--text-primary);
  }

  .sidebar-desc {
      font-size: var(--text-xs, .78rem);
      line-height: 1.4;
      color: var(--text-muted);
      margin-bottom: var(--space-sm);
  }

  .empty-sidebar {
      font-size: var(--text-xs, .78rem);
      color: var(--text-muted);
      text-align: center;
      padding: var(--space-sm) 0;
  }

  .roster-list {
      display: flex;
      flex-direction: column;
      gap: var(--space-sm);
      /* The roster can run to dozens of agents; past ~10 rows it scrolls
         instead of stretching the sidebar (and the page) ever downward. */
      max-height: min(26rem, 45vh);
      overflow-y: auto;
      min-height: 0;
      padding-right: var(--space-xs);
      scrollbar-width: thin;
      scrollbar-color: var(--border-soft) transparent;
  }

  .roster-item {
      display: flex;
      align-items: center;
      gap: var(--space-sm);
  }

  .roster-avatar {
      width: 28px;
      height: 28px;
      border-radius: 50%;
      background: var(--bg-soft);
      font-size: var(--text-2xs, .72rem);
      display: flex;
      align-items: center;
      justify-content: center;
      font-weight: 600;
  }

  .roster-info {
      flex: 1;
  }
  
  .roster-name {
      font-size: var(--text-sm, .85rem);
      font-weight: 500;
  }

  .roster-mood-bar {
      height: 4px;
      background: var(--bg-soft);
      border-radius: 2px;
      margin-top: 4px;
      overflow: hidden;
  }

  .mood-fill {
      height: 100%;
      background: var(--accent-primary);
      transition: width 0.3s ease;
  }

  .groups-list {
      display: flex;
      flex-direction: column;
      gap: var(--space-xs);
  }

  .group-item {
      display: flex;
      align-items: center;
      gap: var(--space-sm);
      font-size: var(--text-sm, .85rem);
      padding: 2px 0;
  }

  :global(.composer-card) {
      padding: var(--space-sm) var(--space-md) !important;
  }

  .composer-header {
      font-size: var(--text-xs, .78rem);
      font-weight: 600;
      margin-bottom: var(--space-xs);
      color: var(--text-secondary);
  }

  .composer-input {
      width: 100%;
      min-height: 44px;
      padding: var(--space-xs) var(--space-sm);
      border: 1px solid var(--border-soft);
      border-radius: var(--radius-md, 8px);
      background: var(--bg-soft);
      color: var(--text-primary);
      font-family: inherit;
      font-size: var(--text-sm, .85rem);
      line-height: 1.45;
      resize: vertical;
      margin-bottom: var(--space-xs);
  }

  .composer-input:focus {
      outline: none;
      border-color: var(--accent-primary);
  }

  .composer-actions {
      display: flex;
      justify-content: flex-end;
  }

  .town-square-loading {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-md);
    padding: var(--space-2xl);
    color: var(--text-muted);
  }

  .posts-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }

  :global(.post-card) {
    padding: var(--space-sm) var(--space-md) !important;
  }

  .post-shell {
    content-visibility: auto;
    contain-intrinsic-size: auto 220px;
  }

  .post-shell--reply {
    position: relative;
    margin-left: var(--space-xl);
  }

  .post-shell--reply::before {
    content: '';
    position: absolute;
    top: calc(-1 * var(--space-lg));
    bottom: 50%;
    left: calc(-1 * var(--space-md));
    width: var(--space-sm);
    border-left: 1px solid var(--border-soft);
    border-bottom: 1px solid var(--border-soft);
    border-bottom-left-radius: var(--radius-md, 8px);
  }

  :global(.post-card--reply) {
    background: color-mix(in srgb, var(--bg-soft) 45%, transparent) !important;
  }

  @media (max-width: 640px) {
    .post-shell--reply {
      margin-left: var(--space-md);
    }
  }

  .post-header {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    margin-bottom: var(--space-xs);
  }

  .post-avatar {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    background: var(--bg-soft);
    color: var(--text-primary);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.72rem;
    font-weight: bold;
    flex: none;
    box-shadow: inset 0 0 0 1px var(--border-soft);
  }

  .post-meta {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 0 0.45rem;
    min-width: 0;
  }

  .post-author {
    font-weight: 600;
    font-size: var(--text-sm, .85rem);
    color: var(--text-primary);
  }

  .post-time {
    font-size: var(--text-2xs, .72rem);
    line-height: 1.3;
    color: var(--text-muted);
  }

  .post-content {
    color: var(--text-body);
    white-space: pre-wrap;
    line-height: 1.5;
    margin-bottom: var(--space-xs);
    font-size: var(--text-sm, .85rem);
  }

  .post-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-top: var(--space-xs);
    border-top: 1px solid var(--border-soft);
    flex-wrap: wrap;
    gap: var(--space-xs);
  }

  .post-reactions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
  }

  .post-add-reactions {
    display: flex;
    gap: var(--space-xs);
    margin-left: auto;
  }

  .reaction-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--bg-soft);
    border: 1px solid var(--border-soft);
    font-size: var(--text-2xs, .72rem);
  }

  .reaction-count {
    color: var(--text-muted);
    font-weight: 500;
  }

  /* Borrow hero styles from budget page */
  :global(.ra-hero) {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-md) var(--space-lg) !important;
  }

  .ra-hero-copy p.ra-kicker {
    font-family: var(--font-mono);
    text-transform: uppercase;
    font-size: var(--text-2xs, .72rem);
    letter-spacing: 0.05em;
    color: var(--accent-primary);
    margin: 0 0 2px 0;
  }

  .ra-hero-copy h1 {
    font-family: var(--font-display, var(--font-primary));
    font-size: 1.15rem;
    font-weight: 700;
    margin: 0 0 2px 0;
    color: var(--text-primary);
  }

  .ra-hero-copy p {
    font-size: var(--text-sm, .85rem);
    color: var(--text-secondary);
    margin: 0;
  }

  /* Compact (Square → Social tab): one column, tighter rhythm, roster and
     groups docked below the feed as a two-up row instead of a sidebar. */
  .town-square-compact {
    padding: var(--space-sm) 0 var(--space-md);
    max-width: none;
  }

  .town-square-compact .layout-grid {
    grid-template-columns: 1fr;
    gap: var(--space-md);
  }

  .town-square-compact .ra-hero-copy h1 {
    font-size: 1rem;
  }

  .town-square-compact .ra-hero-copy p:not(.ra-kicker) {
    display: none;
  }

  .town-square-compact .sidebar-column {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-md);
    align-items: start;
  }

  @media (max-width: 720px) {
    .town-square-compact .sidebar-column {
      grid-template-columns: 1fr;
    }
  }

  .town-square-compact .posts-list {
    gap: var(--space-xs);
  }

  .town-square-compact :global(.post-card) {
    padding: var(--space-xs) var(--space-sm) !important;
  }
</style>
