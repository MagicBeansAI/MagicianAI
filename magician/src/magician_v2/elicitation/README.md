# Magician V2 Elicitation Manager

The elicitation manager coordinates all parameter/consent collection for a thread so the planner and executor operate on the same slot state.

## Goals

- **Single source of truth**: Slots and consent flags are stored in the V2 conversation store and referenced by plan steps via `prerequisites`.
- **Planner integration**: During outline/plan generation the strategy registers slots using `UnresolvedInput` (`strategy/plan.rs`), automatically pre-filling where inherited parameters exist.
- **Execution integration**: Before a step runs, the executor queries the manager (`pending_slots`, `await_slot`) to decide whether to prompt the user or continue. Observation checkpoints after execution are reported through `record_observation`.
- **Unified analysis**: When `answer_elicitation` intents arrive, the intent processor calls `update_slot`, keeping planner-derived metadata and user answers in sync.
- **Visibility**: Slot updates can be surfaced through the event broadcaster/telemetry to power UI “pending inputs” panels and metrics (slots created, time-to-fill, retry counts).

## Key Types

- `UnresolvedInput` (`strategy/plan.rs`): describes a slot during planning (prompt, schema, linked steps, optional auto-fill).
- `UnresolvedInputUpdate`: carried by the orchestrator/intent processors when a value or status changes.
- `ObservationEvent`: optional post-step reporting used to drive retries/escalation policies.

## Manager Lifecycle

1. `ElicitationManagerBuilder` initialises the manager by scanning existing slots for the thread and recording the mapping between planner slot IDs and storage slot IDs.
2. Strategies call `register_slots` as they emit prerequisites; answers inherited from parameter extraction are stored automatically.
3. Executors call `pending_slots`, `await_slot`, and `update_slot` to coordinate runtime prompts.
4. Observation checkpoints feed into `record_observation` so future retry policies can be triggered consistently.

This module intentionally decouples the slot lifecycle from individual strategies so future planners or multi-agent setups can reuse the same infrastructure.

## Progressive Elicitation Integration

The `ElicitationManager` also serves as a bridge for **progressive parameter resolution**, providing access to intelligent auto-resolution services for strategies that support it (currently `AtomicComposition`).

### Services Available

- **ParameterInferenceService**: LLM-based and rule-based inference of parameter values from context
- **AutonomousDiscoveryService**: Safe autonomous tool execution to discover parameter values

### Integration Flow

1. **Orchestrator Setup**: During turn processing, the orchestrator creates an `ElicitationManager` for the thread and passes it to the strategy via `StrategyContext`.

2. **Service Wiring**: When creating `AtomicComposition`, the orchestrator extracts inference/discovery services from the `ElicitationManager` and wires them up:
   ```rust
   let atomic = if let Some(ref elicitation_mgr) = context.elicitation_manager {
       let inference = elicitation_mgr.get_inference_service();
       let discovery = elicitation_mgr.get_discovery_service();
       atomic.with_elicitation_services(inference, discovery)
   } else {
       atomic
   };
   ```

3. **Progressive Resolution**: Inside `AtomicComposition::compose_with_retry()`, after outline generation:
   - Extract parameters from outline goals
   - For each parameter, try Phase 2.1 (Inference) → Phase 2.2 (Discovery)
   - Defer low-priority parameters to JIT or mark for upfront questions
   - Pass resolved parameters to LLM for plan expansion

4. **Backward Compatibility**: If services are not configured (`None`), progressive resolution is skipped and the strategy falls back to traditional upfront question mode.

### Service Access

The manager provides getter methods for direct service access:

- `get_inference_service() -> Option<Arc<dyn ParameterInferenceService>>`
- `get_discovery_service() -> Option<Arc<dyn AutonomousDiscoveryService>>`

These are primarily used by the orchestrator when wiring up strategies, though strategies could also access them directly if needed.

### Configuration

Progressive services are **optional** and default to `None` for backward compatibility. When fully implemented, they will be configured at manager creation time and made available to all strategies that support progressive resolution.
