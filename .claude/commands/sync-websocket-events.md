# Sync WebSocket Events Documentation

Update `docs/components/magician/v2-websocket-events.md` to reflect the current state of WebSocket events in the codebase.

## Instructions

1. **Read the source of truth**: `magician/src/magician_v2/realtime_events.rs`
   - Extract all `V2RealtimeEvent` enum variants
   - Note all fields and their types for each event
   - Identify any `#[serde(skip_serializing_if)]` annotations

2. **Compare with current documentation**: `docs/components/magician/v2-websocket-events.md`
   - Identify NEW events not in the docs
   - Identify REMOVED events still in docs but not in code
   - Identify CHANGED events (field additions/removals/type changes)

3. **Update the documentation**:
   - Add any new events to the appropriate category section
   - Remove any events that no longer exist
   - Update field definitions for changed events
   - Maintain the existing document structure and formatting style

4. **Report the changes**:
   - List all added events
   - List all removed events
   - List all modified events with what changed

## Event Categories (maintain these sections)

- Message Lifecycle Events
- LLM Analysis & Processing Events
- Agentic Execution Events
- Task/Step Execution Events
- Error & Retry Events
- System Events

## Field Documentation Format

For each event, document:
```markdown
### EventName
**Type**: `event_type_string`

| Field | Type | Description |
|-------|------|-------------|
| field_name | type | description |
```

## Notes

- Preserve existing descriptions where they're still accurate
- Add `(NEW)` marker to newly added events in commit message
- Update the "Last Updated" date at the top of the doc
