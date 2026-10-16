# Sync All Magician Documentation

Run both API and WebSocket event documentation sync commands.

## Instructions

Execute both documentation sync tasks in sequence:

1. **First**: Run the WebSocket events sync
   - Read `magician/src/magician_v2/realtime_events.rs`
   - Update `docs/components/magician/v2-websocket-events.md`

2. **Then**: Run the REST API sync
   - Read `magician/src/web_api.rs` and `magician/src/magician_v2/api/`
   - Update `docs/components/magician/v2-api-guide.md`

3. **Summarize all changes** in a single report:
   - WebSocket events: added/removed/modified
   - REST endpoints: added/removed/modified

## Quick Reference

| Documentation | Source Files | Target Doc |
|--------------|--------------|------------|
| WebSocket Events | `realtime_events.rs` | `docs/components/magician/v2-websocket-events.md` |
| REST API | `web_api.rs`, `api/` | `docs/components/magician/v2-api-guide.md` |

## Notes

- Run this after making changes to the API or event system
- Useful before releases to ensure docs are up-to-date
- Creates a comprehensive diff report for review
