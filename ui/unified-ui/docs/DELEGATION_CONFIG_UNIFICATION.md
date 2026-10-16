# Delegation Config Unification

**Date:** 2026-03-05

## Summary

The UI previously read delegation targets from two sources:
- `coordination.delegate_to` (nested in constraints, extracted by `agentStore.ts`)
- `delegation_targets` (top-level on `AgentDefinition`)

These have been unified. All delegation configuration now uses the single
top-level `delegation_targets` field.

## Changes

- `CoordinationView.delegate_to` removed from `types/agents.ts`
- `AgentSummary.delegate_to` removed from `agentStore.ts`
- Agent create/edit forms (`new/+page.svelte`, `[id]/+page.svelte`) emit
  `delegation_targets` at top level in YAML instead of `coordination.delegate_to`
- `DEFAULT_NEW_AGENT_YAML` in both `definitionApi.ts` files updated

## Backward Compatibility

Old agent YAML with `delegate_to` under `constraints.coordination` is silently
ignored by the backend (serde skips unknown fields on `CoordinationConfig`).
