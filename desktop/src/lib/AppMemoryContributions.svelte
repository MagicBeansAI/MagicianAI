<script lang="ts">
  import { magicianFetch } from "./magicianAuth.js";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { readBoundedResponseText } from "./macosPairingUiModel.js";

  interface Props { magicianPort: number }
  interface SourceRef {
    entity_name: string;
    record_id: string;
    record_revision: number;
    selected_fields: string[];
    canonical_source_ref: string;
    canonical_source_digest: string;
  }
  interface ContributionHeader {
    proposal_id: string;
    scope_binding_ref: string;
    installation_id: string;
    workflow_id: string;
    action_id: string;
    contribution_port_id: string;
    purpose: string;
    expires_at_ms: number;
    audiences: string[];
    sources: SourceRef[];
    handling_labels: {
      classification: string;
      model_processing: string;
      policy_digest: string;
      provenance_digest: string;
    };
  }
  interface MemoryProposal {
    header: ContributionHeader;
    claim_or_summary: string;
    claim_digest: string;
    evidence_refs: string[];
    proposal_digest: string;
  }
  interface OwnerReview {
    contract_version: number;
    review_id: string;
    destination_generation: number;
    destination_receipt_digest?: string;
    desktop_identity_key_id: string;
    desktop_identity_digest: string;
    proposal: MemoryProposal;
    display_digest: string;
  }
  interface ReviewList { reviews: OwnerReview[] }
  type ContributionState = "proposed" | "accepted" | "rejected" | "stale" | "tombstoned";
  interface ContributionStateItem {
    proposal_id: string;
    proposal_digest: string;
    installation_id: string;
    workflow_id?: string;
    action_id?: string;
    contribution_port_id?: string;
    source_event_ref: string;
    source_event_revision: number;
    state: ContributionState;
    reason: string;
    source_invalidation_reason?: string;
    state_changed_at_ms: number;
    expires_at_ms?: number;
    retained_until_ms?: number;
    claim_or_summary?: string;
    details_compacted: boolean;
    revoke_review?: OwnerReview;
  }
  interface ContributionStateResponse {
    memory: {
      destination_generation: number;
      destination_receipt_digest?: string;
      items: ContributionStateItem[];
    };
    retrieval?: {
      destination_generation: number;
      destination_receipt_digest?: string;
      items: Array<{
        proposal_id: string;
        proposal_digest: string;
        installation_id: string;
        source_event_ref: string;
        source_event_revision: number;
        state: "accepted" | "stale" | "tombstoned";
        source_invalidation_reason?: string;
        state_changed_at_ms: number;
        expires_at_ms?: number;
        target_agent_id: string;
        target_goal_id?: string;
        details_compacted: boolean;
      }>;
    };
    retrieval_available: boolean;
  }
  interface SignedDecision extends Record<string, unknown> {
    receipt_digest: string;
  }
  interface DestinationDecisionReceipt {
    contract_version: number;
    receipt_id: string;
    generation: number;
    previous_receipt_digest?: string;
    operation: {
      kind: "decide";
      proposal_id: string;
      proposal_digest: string;
      decision: "accept" | "reject" | "revoke";
      owner_decision_receipt_digest: string;
      retained_until_ms?: number;
    };
    resulting_projection_digest: string;
    recorded_at_ms: number;
    receipt_digest: string;
  }

  const MAX_RESPONSE_BYTES = 800 * 1024;
  let { magicianPort }: Props = $props();
  let reviews = $state<OwnerReview[]>([]);
  let contributionState = $state<ContributionStateResponse | null>(null);
  let busy = $state(false);
  let error = $state("");
  let message = $state("");
  let confirmedDigest = $state("");
  let confirmedRevokeDigest = $state("");
  let selectedDecision = $state<"accept" | "reject">("accept");

  function apiUrl(path: string): string {
    return `http://127.0.0.1:${magicianPort}/api/magician/v2/apps/memory-contributions${path}`;
  }

  async function runtimeJson<T>(path: string, method = "GET", body?: unknown): Promise<T> {
    const abort = new AbortController();
    const deadline = setTimeout(() => abort.abort(), 15_000);
    try {
      const response = await magicianFetch(apiUrl(path), {
        method,
        cache: "no-store",
        redirect: "error",
        headers: {
          "Accept": "application/json",
          "Content-Type": "application/json",
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: abort.signal,
      });
      const declared = Number(response.headers.get("content-length") ?? "0");
      if (declared > MAX_RESPONSE_BYTES) throw new Error("oversized response");
      const text = await readBoundedResponseText(response.body, MAX_RESPONSE_BYTES);
      const value = JSON.parse(text) as T & { message?: string };
      if (!response.ok) throw new Error(value.message ?? "app-memory owner request failed");
      return value;
    } finally {
      clearTimeout(deadline);
    }
  }

  function assertDecisionReceipt(
    value: DestinationDecisionReceipt,
    signed: SignedDecision,
    review: OwnerReview,
    decision: "accept" | "reject" | "revoke",
  ): void {
    const exactKeys = (candidate: object, allowed: string[]) =>
      Object.keys(candidate).every((key) => allowed.includes(key));
    const digest = (candidate: unknown) =>
      typeof candidate === "string" && /^blake3:[0-9a-f]{64}$/.test(candidate);
    const operation = value?.operation;
    if (!value || !exactKeys(value, ["contract_version", "receipt_id", "generation", "previous_receipt_digest", "operation", "resulting_projection_digest", "recorded_at_ms", "receipt_digest"])
      || !operation || !exactKeys(operation, ["kind", "proposal_id", "proposal_digest", "decision", "owner_decision_receipt_digest", "retained_until_ms"])
      || value.contract_version !== 1 || !Number.isSafeInteger(value.generation)
      || value.generation !== review.destination_generation + 1
      || value.previous_receipt_digest !== review.destination_receipt_digest
      || operation.kind !== "decide" || operation.proposal_id !== review.proposal.header.proposal_id
      || operation.proposal_digest !== review.proposal.proposal_digest
      || operation.decision !== decision
      || operation.owner_decision_receipt_digest !== signed.receipt_digest
      || (decision === "accept"
        ? operation.retained_until_ms !== review.proposal.header.expires_at_ms
        : operation.retained_until_ms !== undefined)
      || !digest(signed.receipt_digest) || !digest(value.resulting_projection_digest)
      || !digest(value.receipt_digest) || !Number.isSafeInteger(value.recorded_at_ms)
      || value.recorded_at_ms < 0) {
      throw new Error("the destination decision receipt was substituted");
    }
  }

  async function refresh(): Promise<void> {
    busy = true;
    error = "";
    try {
      const [payload, state] = await Promise.all([
        runtimeJson<ReviewList>("/owner-reviews?limit=16"),
        runtimeJson<ContributionStateResponse>("/state?limit=32"),
      ]);
      reviews = Array.isArray(payload.reviews) ? payload.reviews : [];
      contributionState = state;
      if (!reviews.some((review) => review.display_digest === confirmedDigest)) {
        confirmedDigest = "";
      }
      if (!state.memory.items.some((item) =>
        item.revoke_review?.display_digest === confirmedRevokeDigest
      )) {
        confirmedRevokeDigest = "";
      }
    } catch (cause) {
      reviews = [];
      contributionState = null;
      error = `App-memory reviews unavailable: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function revoke(item: ContributionStateItem): Promise<void> {
    const review = item.revoke_review;
    if (!review || confirmedRevokeDigest !== review.display_digest) {
      error = "Confirm the exact accepted-memory review digest before revoking it.";
      return;
    }
    busy = true;
    error = "";
    message = "";
    try {
      const signed = await invoke<SignedDecision>("sign_app_memory_owner_decision", {
        review,
        decision: "revoke",
        retainedUntilMs: null,
        expectedDisplayDigest: review.display_digest,
      });
      const receipt = await runtimeJson<DestinationDecisionReceipt>("/owner-decisions", "POST", signed);
      assertDecisionReceipt(receipt, signed, review, "revoke");
      message = "The signed owner receipt tombstoned this exact accepted memory source.";
      confirmedRevokeDigest = "";
      await refresh();
    } catch (cause) {
      error = `The signed revoke is incomplete; retry the same displayed digest: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function decide(review: OwnerReview): Promise<void> {
    if (confirmedDigest !== review.display_digest) {
      error = "Confirm the complete displayed review digest before signing.";
      return;
    }
    busy = true;
    error = "";
    message = "";
    const decision = selectedDecision;
    try {
      const signed = await invoke<SignedDecision>("sign_app_memory_owner_decision", {
        review,
        decision,
        retainedUntilMs: decision === "accept"
          ? review.proposal.header.expires_at_ms
          : null,
        expectedDisplayDigest: review.display_digest,
      });
      const receipt = await runtimeJson<DestinationDecisionReceipt>("/owner-decisions", "POST", signed);
      assertDecisionReceipt(receipt, signed, review, decision);
      message = decision === "accept"
        ? "The signed owner receipt promoted this exact source-linked candidate."
        : "The signed owner receipt rejected and tombstoned this exact candidate.";
      confirmedDigest = "";
      await refresh();
    } catch (cause) {
      error = `The signed owner transition is incomplete; retry the same displayed digest: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  onMount(() => { void refresh(); });
</script>

<section class="card" aria-labelledby="app-memory-owner-title">
  <div class="title-row">
    <div>
      <h2 id="app-memory-owner-title">App Memory Contributions</h2>
      <p class="section-desc">Review source-linked candidate text and handling policy before the Keychain desktop identity signs an accept, reject, or accepted-memory revoke receipt.</p>
    </div>
    <button type="button" onclick={refresh} disabled={busy}>Refresh</button>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if message}<p class="message" role="status">{message}</p>{/if}
  {#if !busy && reviews.length === 0 && !error}
    <p>No unexpired app-memory candidates await owner review.</p>
  {/if}
  {#each reviews as review (review.review_id)}
    <article class="memory-review" aria-labelledby={`memory-review-${review.review_id}`}>
      <h3 id={`memory-review-${review.review_id}`}>{review.proposal.header.installation_id}</h3>
      <p>{review.proposal.claim_or_summary}</p>
      <dl>
        <dt>Workflow / action</dt><dd><code>{review.proposal.header.workflow_id} / {review.proposal.header.action_id}</code></dd>
        <dt>Purpose</dt><dd>{review.proposal.header.purpose}</dd>
        <dt>Audience</dt><dd>{review.proposal.header.audiences.join(", ")}</dd>
        <dt>Handling</dt><dd>{review.proposal.header.handling_labels.classification}; {review.proposal.header.handling_labels.model_processing}</dd>
        <dt>Retention ceiling</dt><dd><code>{review.proposal.header.expires_at_ms}</code></dd>
        <dt>Source</dt><dd>{review.proposal.header.sources.map((source) => `${source.entity_name}/${source.record_id}@${source.record_revision}`).join(", ")}</dd>
        <dt>Selected fields</dt><dd>{review.proposal.header.sources.flatMap((source) => source.selected_fields).join(", ")}</dd>
        <dt>Desktop identity</dt><dd><code>{review.desktop_identity_digest}</code></dd>
        <dt>Destination head</dt><dd>{review.destination_generation}</dd>
        <dt>Complete review digest</dt><dd><code>{review.display_digest}</code></dd>
      </dl>
      <details>
        <summary>Complete signed review document</summary>
        <pre>{JSON.stringify(review, null, 2)}</pre>
      </details>
      <label class="confirmation">
        <input
          type="checkbox"
          checked={confirmedDigest === review.display_digest}
          onchange={(event) => {
            confirmedDigest = (event.currentTarget as HTMLInputElement).checked
              ? review.display_digest
              : "";
          }}
        />
        I reviewed the exact source, text, policy, destination head, and digest above.
      </label>
      <div class="decision-row">
        <label>Decision
          <select bind:value={selectedDecision} disabled={busy}>
            <option value="accept">Accept</option>
            <option value="reject">Reject</option>
          </select>
        </label>
        <button
          type="button"
          class="primary"
          disabled={busy || confirmedDigest !== review.display_digest}
          onclick={() => decide(review)}
        >Sign exact {selectedDecision}</button>
      </div>
    </article>
  {/each}
  {#if contributionState}
    <div class="history-title">
      <h3>Current and compacted history</h3>
      <p>Memory head {contributionState.memory.destination_generation}; retrieval {contributionState.retrieval_available ? `head ${contributionState.retrieval?.destination_generation ?? 0}` : "unavailable"}.</p>
    </div>
    {#if contributionState.memory.items.length === 0 && (contributionState.retrieval?.items.length ?? 0) === 0}
      <p>No source-linked memory or retrieval contribution state is retained.</p>
    {/if}
    {#each contributionState.memory.items as item (`memory:${item.proposal_digest}`)}
      <article class="memory-review history" aria-label={`Memory ${item.state} ${item.proposal_id}`}>
        <h4>{item.installation_id} · {item.state}</h4>
        {#if item.claim_or_summary}<p>{item.claim_or_summary}</p>{/if}
        <dl>
          <dt>Reason</dt><dd>{item.source_invalidation_reason ?? item.reason}</dd>
          <dt>Source</dt><dd><code>{item.source_event_ref}@{item.source_event_revision}</code></dd>
          <dt>Retention</dt><dd>{item.retained_until_ms ? new Date(item.retained_until_ms).toLocaleString() : item.expires_at_ms ? `ceiling ${new Date(item.expires_at_ms).toLocaleString()}` : "compacted"}</dd>
          <dt>State changed</dt><dd>{item.state_changed_at_ms > 0 ? new Date(item.state_changed_at_ms).toLocaleString() : "legacy compacted state"}</dd>
        </dl>
        {#if item.revoke_review}
          <details><summary>Complete revoke review document</summary><pre>{JSON.stringify(item.revoke_review, null, 2)}</pre></details>
          <label class="confirmation"><input type="checkbox" checked={confirmedRevokeDigest === item.revoke_review.display_digest} onchange={(event) => confirmedRevokeDigest = (event.currentTarget as HTMLInputElement).checked ? item.revoke_review?.display_digest ?? "" : ""} />I reviewed the accepted text, exact source, retention, destination head, and revoke digest.</label>
          <button type="button" class="danger" disabled={busy || confirmedRevokeDigest !== item.revoke_review.display_digest} onclick={() => revoke(item)}>Sign exact revoke</button>
        {/if}
      </article>
    {/each}
    {#each contributionState.retrieval?.items ?? [] as item (`retrieval:${item.proposal_digest}`)}
      <article class="memory-review history" aria-label={`Retrieval ${item.state} ${item.proposal_id}`}>
        <h4>{item.installation_id} · retrieval {item.state}</h4>
        <dl><dt>Reason</dt><dd>{item.source_invalidation_reason ?? "reviewed projection"}</dd><dt>Source</dt><dd><code>{item.source_event_ref}@{item.source_event_revision}</code></dd><dt>Target</dt><dd>{item.target_agent_id}{item.target_goal_id ? ` / ${item.target_goal_id}` : ""}</dd><dt>Retention</dt><dd>{item.expires_at_ms ? new Date(item.expires_at_ms).toLocaleString() : "compacted"}</dd></dl>
      </article>
    {/each}
  {/if}
</section>

<style>
  .title-row, .decision-row { display: flex; justify-content: space-between; gap: 1rem; align-items: center; }
  .memory-review { border: 1px solid var(--border); border-radius: 10px; padding: 1rem; margin-top: 1rem; }
  dl { display: grid; grid-template-columns: minmax(9rem, auto) 1fr; gap: .4rem 1rem; }
  dt { font-weight: 600; }
  dd { margin: 0; overflow-wrap: anywhere; }
  code { font-size: .78rem; overflow-wrap: anywhere; }
  pre { max-height: 18rem; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; font-size: .72rem; }
  .confirmation { display: flex; gap: .6rem; align-items: flex-start; margin: 1rem 0; }
  .error { color: var(--danger); }
  .message { color: var(--success); }
  .history-title { margin-top: 1.5rem; }
  .history-title p { color: var(--text-muted); }
  .history h4 { margin: 0 0 .6rem; }
  button.danger { color: var(--danger); border-color: currentColor; }
</style>
