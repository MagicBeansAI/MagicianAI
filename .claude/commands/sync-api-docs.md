# Sync REST API Documentation

Update `docs/components/magician/v2-api-guide.md` to reflect the current state of REST API endpoints in the codebase.

## Instructions

1. **Read the API route definitions**:
   - `magician/src/web_api.rs` - Main API router configuration
   - `magician/src/magician_v2/api/` - V2 API handlers
   - Look for `.route()`, `.get()`, `.post()`, `.put()`, `.delete()` calls

2. **Extract endpoint information**:
   - HTTP method (GET, POST, PUT, DELETE)
   - Path pattern (e.g., `/api/magician/v2/threads/{id}`)
   - Request body type (if POST/PUT)
   - Response type
   - Handler function name

3. **Compare with current documentation**: `docs/components/magician/v2-api-guide.md`
   - Identify NEW endpoints not in the docs
   - Identify REMOVED endpoints still in docs but not in code
   - Identify CHANGED endpoints (path changes, method changes, request/response changes)

4. **Update the documentation**:
   - Add any new endpoints to the appropriate section
   - Remove any endpoints that no longer exist
   - Update request/response schemas for changed endpoints
   - Maintain the existing document structure and formatting style

5. **Report the changes**:
   - List all added endpoints
   - List all removed endpoints
   - List all modified endpoints with what changed

## Endpoint Documentation Format

For each endpoint, document:
```markdown
### Endpoint Name

**Method**: `POST`
**Path**: `/api/magician/v2/path`

**Request Body**:
```json
{
  "field": "type"
}
```

**Response**:
```json
{
  "field": "type"
}
```

**Description**: What this endpoint does
```

## API Categories (maintain these sections)

- Thread Management
- Message Operations
- Task/Execution Control
- Analysis & Results
- WebSocket Connections

## Notes

- Check both the route definitions AND the handler implementations for accurate types
- Look at request/response structs (usually in `types.rs` or inline)
- Preserve existing descriptions where they're still accurate
- Add `(NEW)` marker to newly added endpoints in commit message
- Update the "Last Updated" date at the top of the doc
