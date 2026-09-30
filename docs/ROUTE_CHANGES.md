# Route Changes (Current)

## Frontend Routes
Active user routes:
- `/`
- `/presto/spells`
- `/presto/legend`
- `/presto/*` feature routes

Removed:
- `/magictunnel`
- `/magictunnel/*`

## Backend API Routes
Active API surface remains under:
- `/api/magician/v2/*`
- `/api/magician/v2/realtime/ws`

## Notes
- Any legacy links to `/magictunnel` should be removed from docs, tests, and scripts.
- The UI no longer imports code from `$lib/magictunnel/*`.
