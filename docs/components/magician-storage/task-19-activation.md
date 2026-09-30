# Task 19 — Track B operator activation

Default Magician startup does **not** inventory, plan, export, or cut over.
Remote selection is an explicit operator workflow. Decision Gates 1, 2, and
3 are closed. Cutover still requires a recent backup, remote health, no
competing lease, and confirmation.

## Surfaces

- CLI: `magician storage inventory|status|plan|export|import|verify|checkpoint|resume|cancel|cutover|rollback`
- HTTP: `GET /api/magician/v2/storage/activation` (evidence);
  `POST /api/magician/v2/storage/activation/cutover` (conflict unless every precondition holds)
- `GET /health/storage` remains the live adapter/lease health snapshot
- Settings → Open storage, command palette **Storage** / **Storage activation**
  (inventory and status only; no qualified cutover action)

## Preconditions (all required for cutover)

- every selected Tier 1 owner is `remote_ready`
- every Tier 2 / device-local owner is in the capability-support matrix
- Gates 1, 2, and 3 closed
- source `local_embedded` → target `remote_durable`
- backup younger than 24 h
- remote health passing
- no other migration lease
- rollback retention ≥ 7 days
- destructive confirmation `CUT OVER STORAGE` / `ROLLBACK STORAGE`
- fencing generation matches the ledger

A failed copy or verify leaves local canonical. Changing a provider field
does not move data. The desktop Runtime provider control is unchanged.

## Operator commands

```
magician storage inventory --principal alice --workspace home
magician storage status
magician storage plan --owner programs --dry-run
magician storage cutover --migration-id <uuid> --fence 1 --confirm "CUT OVER STORAGE"
```

JSON reports are secret-redacted. Default startup still calls
`StorageRuntime::open_local` only.
