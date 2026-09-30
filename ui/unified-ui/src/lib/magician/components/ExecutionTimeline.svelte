<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { v2Events, type V2WebSocketEvent, getV2EventSequence } from '$lib/realtime/v2-websocket';
  import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
  import { isRetro16Bit, isRetro16BitDark, isRetro16BitLight } from '$lib/shared/stores/themeStore';
  import Button from '$lib/magician/components/native/Button.svelte';
  import Checkbox from '$lib/magician/components/native/Checkbox.svelte';

  export let executionId: string;
  export let planId: string = '';

  interface TimelineEntry {
    timestamp: Date;
    type: 'llm_request' | 'llm_response' | 'inference'
        | 'parameter_inference_attempted' | 'parameter_inferred' | 'parameter_inference_failed'
        | 'execution_started' | 'execution_step_started' | 'execution_step_completed'
        | 'execution_paused' | 'execution_resumed' | 'execution_failed' | 'execution_completed'
        | 'agentic_loop_detected' | 'agentic_waiting_for_confirmation' | 'agentic_budget_exhausted' | 'agentic_cannot_proceed'
        | 'agentic_delegate_fanout' | 'agentic_refinement_pending' | 'agentic_partial_success'
        | 'sub_goal_requested' | 'sub_goal_outcome';
    title: string;
    details: string;
    status: 'pending' | 'success' | 'warning' | 'error';
    stepId?: string;
    stepIndex?: number;
    capability?: string;
    expanded?: boolean;
    metadata?: Record<string, unknown>;
    providing_agent_id?: string;
  }

  let timeline: TimelineEntry[] = [];
  let unsubscribe: (() => void) | null = null;
  let autoScroll = true;
  let timelineContainer: HTMLElement;
  let previousExecutionId = '';
  let previousScopeKey = '';
  let lastProcessedEventSequence = 0;
  $: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;

  /**
   * Pull `delegation_targets` out of an `AgenticDecisionMade.raw_decision`
   * payload (tactical pattern T4 polish). Returns an empty array when the shape
   * isn't recognised — the caller renders a generic timeline entry
   * in that case instead of a fan-out card. Defensive about the
   * payload because `raw_decision` is typed `any` on the wire.
   */
  type DelegationTargetSummary = {
    target_agent_id: string;
    context: string;
  };
  function extractDelegationTargets(rawDecision: unknown): DelegationTargetSummary[] {
    if (!rawDecision || typeof rawDecision !== 'object') {
      return [];
    }
    const root = rawDecision as Record<string, unknown>;
    const targets =
      (Array.isArray(root.delegation_targets) && root.delegation_targets)
      || (root.delegate_to_agent && typeof root.delegate_to_agent === 'object'
        && Array.isArray((root.delegate_to_agent as Record<string, unknown>).delegation_targets)
        && ((root.delegate_to_agent as Record<string, unknown>).delegation_targets as unknown[]))
      || [];
    if (!Array.isArray(targets)) return [];
    return targets
      .map((entry) => {
        if (!entry || typeof entry !== 'object') return null;
        const obj = entry as Record<string, unknown>;
        const target =
          (typeof obj.target_agent_id === 'string' && obj.target_agent_id)
          || (typeof obj.targetAgentId === 'string' && obj.targetAgentId)
          || (typeof obj.agent_id === 'string' && obj.agent_id);
        if (!target) return null;
        const context =
          (typeof obj.context === 'string' && obj.context)
          || (typeof obj.input_data === 'string' && obj.input_data)
          || '';
        return { target_agent_id: target, context };
      })
      .filter((value): value is DelegationTargetSummary => value !== null);
  }

  function takeUnprocessedEvents(events: V2WebSocketEvent[]): V2WebSocketEvent[] {
    if (events.length === 0) {
      return [];
    }
    const nextEvents = events.filter((event) => getV2EventSequence(event) > lastProcessedEventSequence);
    if (nextEvents.length > 0) {
      lastProcessedEventSequence = getV2EventSequence(nextEvents[nextEvents.length - 1]);
    }
    return nextEvents;
  }

  onMount(() => {
    unsubscribe = v2Events.subscribe((events: V2WebSocketEvent[]) => {
      const nextEvents = takeUnprocessedEvents(events);
      if (nextEvents.length === 0) {
        return;
      }
      const executionEvents = nextEvents.filter(event =>
        'execution_id' in event.data && event.data.execution_id === executionId
      );

      for (const event of executionEvents) {
        handleEvent(event);
      }
    });
  });

  onDestroy(() => {
    unsubscribe?.();
  });

  $: if (executionId !== previousExecutionId || currentScopeKey !== previousScopeKey) {
    timeline = [];
    previousExecutionId = executionId;
    previousScopeKey = currentScopeKey;
    lastProcessedEventSequence = 0;
  }

  /** Settle every still-'pending' timeline entry to a terminal status. Called on
   *  ExecutionCompleted/Failed/AgenticExecutionCompleted: a step's
   *  ExecutionStepCompleted can be dropped mid-stream or arrive after a scope
   *  reset, leaving the step stuck "in progress" after the execution is over.
   *  Mirrors the chat ChatTurnProgress terminal catch-all. */
  function settleAllPending(status: 'success' | 'warning' | 'error'): void {
    timeline = timeline.map((t) => (t.status === 'pending' ? { ...t, status } : t));
  }

  function handleEvent(event: V2WebSocketEvent) {
    const eventData = event.data as { timestamp?: number };
    const existingIdx = timeline.findIndex(t =>
      t.timestamp.getTime() === (eventData.timestamp || 0)
    );
    if (existingIdx >= 0) return;

    switch (event.event_type) {
      case 'LLMRequestSent': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'llm_request',
          title: `LLM: ${data.capability}`,
          details: data.request_summary,
          status: 'pending',
          stepId: data.step_id,
          stepIndex: data.step_index,
          capability: data.capability,
          metadata: { budgetRemaining: data.budget_remaining }
        }];
        scrollToBottom();
        break;
      }

      case 'LLMResponseReceived': {
        const data = event.data;
        const reqIndex = timeline.findIndex(
          t => t.type === 'llm_request' &&
               t.capability === data.capability &&
               t.status === 'pending'
        );
        if (reqIndex >= 0) {
          const existingEntry = timeline[reqIndex];
          timeline[reqIndex] = {
            ...existingEntry,
            status: data.success ? 'success' : 'error',
            details: data.decision_summary,
            metadata: {
              ...existingEntry.metadata,
              cost: data.cost,
              latency: data.latency_ms,
              error: data.error,
            }
          };
          timeline = timeline;
        }
        break;
      }

      case 'InferenceAttempted': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'inference',
          title: `Inference: ${data.parameter}`,
          details: `${data.inferred_value !== undefined ? JSON.stringify(data.inferred_value) : 'null'} (${(data.confidence * 100).toFixed(0)}% confidence)`,
          status: data.accepted ? 'success' : 'warning',
          stepId: data.step_id,
          metadata: { reason: data.reason }
        }];
        scrollToBottom();
        break;
      }

      case 'ParameterInferenceAttempted': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'parameter_inference_attempted',
          title: `Inferring: ${data.parameter_name}`,
          details: `Priority: ${data.priority}`,
          status: 'pending',
          metadata: {
            parameterName: data.parameter_name,
            priority: data.priority,
          }
        }];
        scrollToBottom();
        break;
      }

      case 'ParameterInferred': {
        const data = event.data;
        const inferIdx = timeline.findIndex(
          t => t.type === 'parameter_inference_attempted' &&
               t.metadata?.parameterName === data.parameter_name &&
               t.status === 'pending'
        );
        if (inferIdx >= 0) {
          timeline[inferIdx] = { ...timeline[inferIdx], status: 'success' };
          timeline = timeline;
        }
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'parameter_inferred',
          title: `Inferred: ${data.parameter_name}`,
          details: `Value: ${JSON.stringify(data.inferred_value)} (${(data.confidence * 100).toFixed(0)}% via ${data.method})`,
          status: 'success',
          metadata: {
            parameterName: data.parameter_name,
            inferredValue: data.inferred_value,
            confidence: data.confidence,
            method: data.method,
          }
        }];
        scrollToBottom();
        break;
      }

      case 'ParameterInferenceFailed': {
        const data = event.data;
        const failIdx = timeline.findIndex(
          t => t.type === 'parameter_inference_attempted' &&
               t.metadata?.parameterName === data.parameter_name &&
               t.status === 'pending'
        );
        if (failIdx >= 0) {
          timeline[failIdx] = { ...timeline[failIdx], status: 'error' };
          timeline = timeline;
        }
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'parameter_inference_failed',
          title: `Inference failed: ${data.parameter_name}`,
          details: `${data.reason} (${(data.confidence * 100).toFixed(0)}% confidence)`,
          status: 'error',
          metadata: {
            parameterName: data.parameter_name,
            confidence: data.confidence,
            reason: data.reason,
          }
        }];
        scrollToBottom();
        break;
      }

      case 'SubGoalRequested': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'sub_goal_requested',
          title: 'Delegated sub-goal requested',
          details: data.sub_goal,
          status: 'pending',
          stepId: data.parent_step_id,
          metadata: {
            planId: data.plan_id,
            budgetIterations: data.budget_iterations,
            depth: data.depth
          }
        }];
        scrollToBottom();
        break;
      }

      case 'SubGoalOutcome': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'sub_goal_outcome',
          title: `Delegated sub-goal ${data.outcome}`,
          details: data.sub_goal,
          status: data.outcome === 'success' ? 'success' : data.outcome === 'blocked' ? 'warning' : 'error',
          stepId: data.parent_step_id,
          metadata: {
            planId: data.plan_id,
            iterationsUsed: data.iterations_used,
            durationMs: data.duration_ms
          }
        }];
        scrollToBottom();
        break;
      }

      case 'AgenticDecisionMade': {
        // tactical pattern T4: surface parallel delegation as a distinct timeline
        // entry. Generic Decision entries are noisy and per-iteration;
        // delegate_to_agent decisions are high-signal because they
        // explain how the agent decomposes work. Only render the
        // explicit fan-out card when the decision is delegate_to_agent;
        // other decision types pass through unrendered here (they
        // surface via the lower-level action/iteration events).
        const data = event.data;
        if (data.decision_type !== 'delegate_to_agent') {
          break;
        }
        const targets = extractDelegationTargets(data.raw_decision);
        if (targets.length === 0) {
          break;
        }
        const isParallelFanout = targets.length >= 2;
        const title = isParallelFanout
          ? `Delegating to ${targets.length} agents in parallel`
          : `Delegating to ${targets[0].target_agent_id}`;
        const details = targets
          .map((target, idx) => {
            const ctxExcerpt = (target.context ?? '').replace(/\s+/g, ' ').trim();
            const ctxShort = ctxExcerpt.length > 120
              ? `${ctxExcerpt.slice(0, 117)}…`
              : ctxExcerpt;
            const prefix = isParallelFanout ? `${idx + 1}. ` : '';
            return `${prefix}→ ${target.target_agent_id}: ${ctxShort}`;
          })
          .join('\n');
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'agentic_delegate_fanout',
          title,
          details,
          status: 'pending',
          stepId: data.step_id,
          metadata: {
            ...data,
            fanout_target_count: targets.length,
            fanout_target_agent_ids: targets.map((t) => t.target_agent_id)
          }
        }];
        scrollToBottom();
        break;
      }

      case 'ExecutionStarted': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'execution_started',
          title: `Execution started`,
          details: `${data.steps_total} steps to execute`,
          status: 'pending',
          metadata: { planId: data.plan_id, stepsTotal: data.steps_total }
        }];
        scrollToBottom();
        break;
      }

      case 'ExecutionStepStarted': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'execution_step_started',
          title: `Step ${data.step_index + 1} started`,
          details: `Step ${data.step_index + 1} of ${data.steps_total}`,
          status: 'pending',
          stepId: data.step_id,
          stepIndex: data.step_index,
          providing_agent_id: data.providing_agent_id,
          metadata: { planId: data.plan_id }
        }];
        scrollToBottom();
        break;
      }

      case 'ExecutionStepCompleted': {
        const data = event.data;
        const stepStartIdx = timeline.findIndex(
          t => t.type === 'execution_step_started' && t.stepId === data.step_id && t.status === 'pending'
        );
        if (stepStartIdx >= 0) {
          timeline[stepStartIdx] = { ...timeline[stepStartIdx], status: data.success ? 'success' : 'error' };
          timeline = timeline;
        }
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'execution_step_completed',
          title: `Step ${data.step_index + 1} ${data.success ? 'completed' : 'failed'}`,
          details: data.success ? 'Success' : 'Failed',
          status: data.success ? 'success' : 'error',
          stepId: data.step_id,
          stepIndex: data.step_index,
          providing_agent_id: data.providing_agent_id,
        }];
        scrollToBottom();
        break;
      }

      case 'ExecutionPaused': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'execution_paused',
          title: `Execution paused`,
          details: data.reason,
          status: 'warning',
          stepId: data.step_id,
          stepIndex: data.step_index,
          metadata: { planId: data.plan_id, reason: data.reason }
        }];
        scrollToBottom();
        break;
      }

      case 'ExecutionResumed': {
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'execution_resumed',
          title: `Execution resumed`,
          details: `Mode: ${data.mode}`,
          status: 'success',
          stepId: data.step_id,
          stepIndex: data.step_index,
          metadata: { planId: data.plan_id, mode: data.mode }
        }];
        scrollToBottom();
        break;
      }

      case 'ExecutionFailed': {
        const data = event.data;
        settleAllPending('error');
        const execStartIdx = timeline.findIndex(t => t.type === 'execution_started' && t.status === 'pending');
        if (execStartIdx >= 0) {
          timeline[execStartIdx] = { ...timeline[execStartIdx], status: 'error' };
          timeline = timeline;
        }
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'execution_failed',
          title: `Execution failed`,
          details: data.error,
          status: 'error',
          stepId: data.step_id,
          stepIndex: data.step_index,
          metadata: { planId: data.plan_id, error: data.error }
        }];
        scrollToBottom();
        break;
      }

      case 'ExecutionCompleted': {
        const data = event.data;
        // Settle any step still stuck 'pending' (dropped/mismatched completion).
        settleAllPending(data.success ? 'success' : 'error');
        const execStartIdx = timeline.findIndex(t => t.type === 'execution_started' && t.status === 'pending');
        if (execStartIdx >= 0) {
          timeline[execStartIdx] = { ...timeline[execStartIdx], status: data.success ? 'success' : 'error' };
          timeline = timeline;
        }
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'execution_completed',
          title: `Execution ${data.success ? 'completed' : 'failed'}`,
          details: `${data.steps_total} steps ${data.success ? 'completed successfully' : 'with errors'}`,
          status: data.success ? 'success' : 'error',
          metadata: {
            planId: data.plan_id,
            stepsTotal: data.steps_total,
            success: data.success,
          }
        }];
        scrollToBottom();
        break;
      }

      case 'AgenticExecutionCompleted': {
        const data = event.data;
        if (data.outcome === 'loop_detected') {
          let loopDetails = data.summary || 'Execution stopped due to detected loop pattern';
          if (data.loop_recommendation) loopDetails += `\nRecommendation: ${data.loop_recommendation}`;
          timeline = [...timeline, {
            timestamp: new Date(data.timestamp),
            type: 'agentic_loop_detected',
            title: `Loop Detected: ${data.loop_detection_type || 'unknown'}`,
            details: loopDetails,
            status: 'warning',
            stepId: data.step_id,
            metadata: { ...data }
          }];
          scrollToBottom();
        } else if (data.outcome === 'waiting_for_confirmation') {
          timeline = [...timeline, {
            timestamp: new Date(data.timestamp),
            type: 'agentic_waiting_for_confirmation',
            title: 'Waiting for Confirmation',
            details: data.summary || 'Action requires user confirmation before proceeding',
            status: 'warning',
            stepId: data.step_id,
            metadata: { ...data }
          }];
          scrollToBottom();
        } else if (data.outcome === 'budget_exhausted') {
          let budgetDetails = data.summary || 'Execution stopped due to budget limit';
          if (data.budget_dimension) budgetDetails += `\nExceeded: ${data.budget_dimension}`;
          timeline = [...timeline, {
            timestamp: new Date(data.timestamp),
            type: 'agentic_budget_exhausted',
            title: `Budget Exhausted: ${data.budget_dimension || 'unknown'}`,
            details: budgetDetails,
            status: 'error',
            stepId: data.step_id,
            metadata: { ...data }
          }];
          scrollToBottom();
        } else if (data.outcome === 'cannot_proceed') {
          timeline = [...timeline, {
            timestamp: new Date(data.timestamp),
            type: 'agentic_cannot_proceed',
            title: 'Cannot Proceed',
            details: data.cannot_proceed_reason || data.summary || 'Agent determined it cannot complete the task',
            status: 'error',
            stepId: data.step_id,
            metadata: { ...data }
          }];
          scrollToBottom();
        } else if (
          (data.outcome === 'goal_achieved_partial' || data.outcome === 'partial_progress') &&
          data.refinement_pending === true &&
          (data.refinement_pass_index ?? 0) === 0
        ) {
          // tactical pattern T1: intermediate pass-0 completion. Refinement is
          // about to run — render as an in-progress refinement entry
          // rather than a terminal state. The authoritative pass-N
          // event will arrive after the refinement pass completes
          // and overwrites the task's final outcome.
          timeline = [...timeline, {
            timestamp: new Date(data.timestamp),
            type: 'agentic_refinement_pending',
            title: 'Refining (pass 1 partial — second pass commissioned)',
            details:
              data.summary ||
              'First pass closed with gaps; runtime is automatically commissioning a focused second pass on the missing artifacts.',
            status: 'warning',
            stepId: data.step_id,
            metadata: { ...data }
          }];
          scrollToBottom();
        } else if (data.outcome === 'goal_achieved_partial' || data.outcome === 'partial_progress') {
          // Final partial outcome (no further refinement). Real work
          // landed; some gaps remain. Surface as a warning-styled
          // "partial" entry, not an error. When the executor terminated
          // via Decision::Yield, prefer the structured yield_payload —
          // it carries completed / open / blockers lists the timeline
          // can render directly without parsing the partial_findings.md
          // artifact.
          const yieldPayload = data.yield_payload;
          let partialDetails: string;
          if (yieldPayload) {
            const lines: string[] = [];
            if (yieldPayload.summary) lines.push(yieldPayload.summary);
            if (yieldPayload.completed && yieldPayload.completed.length > 0) {
              lines.push('Completed:');
              for (const item of yieldPayload.completed) lines.push(`  • ${item}`);
            }
            if (yieldPayload.open && yieldPayload.open.length > 0) {
              lines.push('Open:');
              for (const item of yieldPayload.open) lines.push(`  • ${item}`);
            }
            if (yieldPayload.blockers && yieldPayload.blockers.length > 0) {
              lines.push('Blockers:');
              for (const b of yieldPayload.blockers) {
                lines.push(`  • [${b.kind}] ${b.description}`);
              }
            }
            if (yieldPayload.next_step_hint) {
              lines.push(`Next: ${yieldPayload.next_step_hint}`);
            }
            partialDetails = lines.length > 0
              ? lines.join('\n')
              : (data.summary || 'Some declared artifacts produced; gaps remain.');
          } else {
            partialDetails =
              data.summary ||
              'Some declared artifacts produced; gaps remain. Inspect partial_findings.md for details.';
          }
          timeline = [...timeline, {
            timestamp: new Date(data.timestamp),
            type: 'agentic_partial_success',
            title: yieldPayload ? 'Partial Success (yield)' : 'Partial Success',
            details: partialDetails,
            status: 'warning',
            stepId: data.step_id,
            metadata: { ...data }
          }];
          scrollToBottom();
        }
        break;
      }

      case 'AgenticWaitingForConfirmation': {
        // Post-H7.4 slim: action_summary / reason / action_type live on
        // canonical HitlRequested.input_schema. This timeline row is now
        // a lifecycle marker; subscribe to HitlRequested for the
        // human-response payload.
        const data = event.data;
        timeline = [...timeline, {
          timestamp: new Date(data.timestamp),
          type: 'agentic_waiting_for_confirmation',
          title: 'Confirmation Required',
          details: 'Agent is awaiting human confirmation.',
          status: 'warning',
          stepId: data.step_id,
          metadata: { ...data }
        }];
        scrollToBottom();
        break;
      }
    }
  }

  async function scrollToBottom() {
    if (autoScroll && timelineContainer) {
      await tick();
      timelineContainer.scrollTop = timelineContainer.scrollHeight;
    }
  }

  function toggleExpand(index: number) {
    timeline[index].expanded = !timeline[index].expanded;
    timeline = timeline;
  }

  function handleEntryClick(entry: TimelineEntry, index: number) {
    toggleExpand(index);
  }

  function getIcon(type: string, isRetro: boolean): string {
    if (isRetro) {
      switch (type) {
        case 'llm_request': return '[L]';
        case 'llm_response': return '[R]';
        case 'inference': return '[?]';
        case 'parameter_inference_attempted': return '[?]';
        case 'parameter_inferred': return '[!]';
        case 'parameter_inference_failed': return '[X]';
        case 'execution_started': return '[>]';
        case 'execution_step_started': return '-->';
        case 'execution_step_completed': return '[V]';
        case 'execution_paused': return '[P]';
        case 'execution_resumed': return '[>]';
        case 'execution_failed': return '[E]';
        case 'execution_completed': return '[F]';
        case 'agentic_loop_detected': return '(O)';
        case 'agentic_waiting_for_confirmation': return '[W]';
        case 'agentic_budget_exhausted': return '[$]';
        case 'agentic_cannot_proceed': return '[!]';
        case 'agentic_delegate_fanout': return '[D]';
        case 'agentic_refinement_pending': return '[~]';
        case 'agentic_partial_success': return '[/]';
        default: return '-';
      }
    }

    switch (type) {
      case 'llm_request': return '🤖';
      case 'llm_response': return '💬';
      case 'inference': return '🔮';
      case 'parameter_inference_attempted': return '🔮';
      case 'parameter_inferred': return '✨';
      case 'parameter_inference_failed': return '❓';
      case 'execution_started': return '▶';
      case 'execution_step_started': return '→';
      case 'execution_step_completed': return '✓';
      case 'execution_paused': return '⏸';
      case 'execution_resumed': return '▶';
      case 'execution_failed': return '❌';
      case 'execution_completed': return '🏁';
      case 'agentic_loop_detected': return '🔄';
      case 'agentic_waiting_for_confirmation': return '⚠️';
      case 'agentic_budget_exhausted': return '💸';
      case 'agentic_cannot_proceed': return '🚫';
      case 'agentic_delegate_fanout': return '🤝';
      case 'agentic_refinement_pending': return '✏️';
      case 'agentic_partial_success': return '◐';
      default: return '•';
    }
  }

  function getStatusColor(status: string): string {
    switch (status) {
      case 'success': return 'status-success';
      case 'warning': return 'status-warning';
      case 'error': return 'status-error';
      case 'pending': return 'status-pending';
      default: return 'status-neutral';
    }
  }

  function clearTimeline() {
    timeline = [];
  }
</script>

<div class="execution-timeline" class:retro={$isRetro16BitDark} class:retro-light={$isRetro16BitLight} data-plan-id={planId}>
  {#if $isRetro16Bit}
    <div class="retro-border">+--------------------------------------------------+</div>
  {/if}
  
  <div class="timeline-header">
    <h3 class="timeline-title">
      {$isRetro16Bit ? '> EXECUTION_LOG' : 'Execution Timeline'}
    </h3>
    <div class="timeline-controls">
      <Checkbox
        label={$isRetro16Bit ? 'AUTO_SCR' : 'Auto-scroll'}
        checked={autoScroll}
        on:change={(e) => autoScroll = e.detail.checked}
      />
      <Button
          label={$isRetro16Bit ? '[CLR]' : 'Clear'}
          variant="outline"
          size="sm"
          on:click={clearTimeline}
        />
    </div>
  </div>

  <div bind:this={timelineContainer} class="timeline-container">
    {#each timeline as entry, index}
      <div
        class="timeline-entry"
        on:click={() => handleEntryClick(entry, index)}
        on:keydown={(e) => e.key === 'Enter' && handleEntryClick(entry, index)}
        role="button"
        tabindex="0"
      >
        <span class="entry-icon">{getIcon(entry.type, $isRetro16Bit)}</span>
        <div class="entry-content">
          <div class="entry-header">
            <span class={`entry-title ${getStatusColor(entry.status)}`}>
              {entry.title}
            </span>
            {#if entry.stepIndex !== undefined}
              <span class="step-badge">{$isRetro16Bit ? 'STP ' : 'Step '}{entry.stepIndex + 1}</span>
            {/if}
            {#if entry.providing_agent_id}
              <span class="delegated-agent-badge">{$isRetro16Bit ? '@' : 'via '}{entry.providing_agent_id}</span>
            {/if}
            <span class="entry-time">{entry.timestamp.toLocaleTimeString()}</span>
          </div>
          <p class="entry-details">{entry.details}</p>

          {#if entry.expanded && entry.metadata}
            <div class="entry-metadata">
              <pre>{JSON.stringify(entry.metadata, null, 2)}</pre>
            </div>
          {/if}
        </div>
      </div>
    {/each}
  </div>

  {#if timeline.length === 0}
    <p class="empty-state">
      {$isRetro16Bit ? '... WAITING_FOR_DATA ...' : 'Waiting for execution events...'}
    </p>
  {/if}

  {#if $isRetro16Bit}
    <div class="retro-border">+--------------------------------------------------+</div>
  {/if}
</div>

<style>
  .execution-timeline {
    background: var(--bg-card);
    border: 1px solid var(--border-soft);
    border-radius: var(--radius-lg);
    padding: var(--space-lg);
    box-shadow: var(--shadow-sm);
  }

  .execution-timeline.retro {
    background: var(--bg-base);
    border: 2px solid var(--border-default);
    border-radius: 0;
    box-shadow: 8px 8px 0 color-mix(in srgb, var(--border-default) 42%, transparent);
    color: var(--text-primary);
    font-family: var(--font-mono);
  }

  .retro-border {
    font-family: var(--font-mono);
    color: var(--text-primary);
    opacity: 0.5;
    white-space: pre;
    font-size: 10px;
    overflow: hidden;
  }

  .timeline-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-md);
    padding-bottom: var(--space-md);
    border-bottom: 1px solid var(--border-soft);
  }

  .retro .timeline-header {
    border-bottom: 2px dashed var(--border-default);
  }

  .timeline-title {
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-primary);
  }

  .retro .timeline-title {
    color: var(--text-primary);
  }

  .timeline-controls {
    display: flex;
    align-items: center;
    gap: var(--space-md);
  }

  /* Clear button uses the native Button component. */

  .timeline-container {
    max-height: 400px;
    overflow-y: auto;
    scrollbar-width: thin;
    scrollbar-color: var(--border-default) var(--bg-soft);
  }

  .retro .timeline-container::-webkit-scrollbar {
    width: 8px;
  }

  .retro .timeline-container::-webkit-scrollbar-thumb {
    background-color: var(--border-default);
  }

  .timeline-entry {
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-radius: var(--radius-md);
    cursor: pointer;
    transition: all 0.2s ease;
  }

  .retro .timeline-entry {
    border-radius: 0;
    border-left: 2px solid transparent;
  }

  .retro .timeline-entry:hover {
    background-color: var(--bg-soft);
    border-left: 2px solid var(--border-default);
  }

  .entry-icon {
    font-size: 1.1rem;
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg-soft);
    border-radius: var(--radius-sm);
  }

  .retro .entry-icon {
    background: transparent;
    border: 1px solid var(--border-soft);
    font-family: var(--font-mono);
    font-size: 0.8rem;
    color: var(--text-primary);
  }

  .entry-header {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    flex-wrap: wrap;
    margin-bottom: 0.25rem;
  }

  .entry-title {
    font-weight: 500;
    font-size: 0.875rem;
  }

  .retro .entry-title {
    font-family: var(--font-mono);
  }

  .step-badge {
    font-size: 0.7rem;
    color: var(--text-muted);
    background: var(--bg-soft);
    padding: 0.125rem 0.5rem;
    border-radius: var(--radius-full);
  }

  .retro .step-badge {
    background: var(--bg-soft);
    color: var(--text-primary);
    border-radius: 0;
  }

  .delegated-agent-badge {
    font-size: 0.65rem;
    font-weight: 600;
    color: var(--accent-secondary);
    background: var(--accent-secondary-soft);
    border: 1px solid color-mix(in srgb, var(--accent-secondary) 34%, transparent);
    padding: 0.1rem 0.5rem;
    border-radius: var(--radius-full);
  }

  .retro .delegated-agent-badge {
    color: var(--text-primary);
    background: var(--bg-base);
    border: 1px dashed var(--border-default);
    border-radius: 0;
  }

  .entry-time {
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-left: auto;
  }

  .retro .entry-time {
    color: var(--text-muted);
  }

  .entry-details {
    font-size: 0.8rem;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin: 0;
  }

  .retro .entry-details {
    color: var(--text-secondary);
    opacity: 0.8;
  }

  .entry-metadata {
    margin-top: 0.5rem;
    padding: 0.5rem;
    background: var(--bg-soft);
    border-radius: var(--radius-sm);
    font-size: 0.7rem;
    font-family: var(--font-mono);
  }

  .retro .entry-metadata {
    background: var(--bg-base);
    border: 1px solid var(--border-soft);
    border-radius: 0;
    color: var(--text-primary);
  }

  .status-success { color: var(--color-success); }
  .status-warning { color: var(--color-warning); }
  .status-error   { color: var(--color-error); }
  .status-pending { color: var(--status-running); animation: pulse 1.5s infinite; }

  .retro .status-success { color: var(--text-primary); }
  .retro .status-warning { color: var(--text-primary); font-weight: bold; }
  .retro .status-error   { color: var(--text-primary); text-decoration: underline; }
  .retro .status-pending { color: var(--text-primary); opacity: 0.5; }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
  }

  /* ── Retro 16-bit Light Theme ── */
  .execution-timeline.retro-light {
    background: var(--bg-base);
    border: 2px solid var(--border-default);
    border-radius: 0;
    box-shadow: 4px 4px 0 color-mix(in srgb, var(--border-default) 34%, transparent);
    color: var(--text-primary);
    font-family: var(--font-mono);
  }

  .retro-light .timeline-header {
    border-bottom: 2px dashed var(--border-default);
  }

  .retro-light .timeline-title {
    color: var(--text-primary);
  }

  /* Clear button uses the native Button component. */

  .retro-light .timeline-entry {
    border-radius: 0;
    border-left: 2px solid transparent;
  }

  .retro-light .timeline-entry:hover {
    background-color: var(--bg-soft);
    border-left: 2px solid var(--border-default);
  }

  .retro-light .entry-icon {
    background: transparent;
    border: 2px solid var(--border-default);
    font-family: var(--font-mono);
    font-size: 0.8rem;
    color: var(--text-primary);
    border-radius: 0;
  }

  .retro-light .entry-title {
    font-family: var(--font-mono);
    color: var(--text-primary);
  }

  .retro-light .step-badge {
    background: var(--bg-soft);
    color: var(--text-primary);
    border-radius: 0;
    border: 1px solid var(--border-default);
  }

  .retro-light .delegated-agent-badge {
    color: var(--text-primary);
    background: var(--bg-soft);
    border: 1px dashed var(--border-default);
    border-radius: 0;
  }

  .retro-light .entry-time {
    color: var(--text-muted);
  }

  .retro-light .entry-details {
    color: var(--text-secondary);
    opacity: 0.8;
  }

  .retro-light .entry-metadata {
    background: var(--bg-soft);
    border: 1px solid var(--border-default);
    border-radius: 0;
    color: var(--text-primary);
  }

  .retro-light .status-success { color: var(--text-primary); }
  .retro-light .status-warning { color: var(--text-primary); font-weight: bold; }
  .retro-light .status-error   { color: var(--text-primary); text-decoration: underline; }
  .retro-light .status-pending { color: var(--text-primary); opacity: 0.5; }

  .retro-light .timeline-container::-webkit-scrollbar {
    width: 8px;
  }

  .retro-light .timeline-container::-webkit-scrollbar-thumb {
    background-color: var(--border-default);
  }
</style>
