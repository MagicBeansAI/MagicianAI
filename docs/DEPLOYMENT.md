# Deployment Guide (No Magictunnel)

## Scope
This guide covers deployment of the current runtime stack:
- `magician`
- `magicutor`
- `magic-supervisor` (optional but recommended)

## Build

The workspace consumes exact MagicRun/MagicVault Git revisions and a
tracked root lockfile. Both repositories are public HTTPS Git dependencies.
Do not embed access tokens in dependency URLs or container layers. See the
archived Phase 1 ledger
for the original extraction mapping and source-attestation re-review notes.

```bash
make build-all-release
```

## Run With Supervisor (recommended)
```bash
./target/release/magic-supervisor
```

Control commands:
```bash
./supervisor-ctl status
./supervisor-ctl restart-magician
./supervisor-ctl restart-magicutor
./supervisor-ctl health
```

## Run Services Individually
```bash
./target/release/magicutor
./target/release/magician --config tool-runtime-config.yaml
```

## Health Checks
```bash
curl -s http://127.0.0.1:3002/health
curl -s http://127.0.0.1:3003/health
```

## Configuration
- `magician` default config: `tool-runtime-config.yaml`
- capability sources: workspace-scoped skills under
  `$MAGICIAN_ROOT_DIR/scopes/<scope>/skills/` (installed from `skillshub/`),
  plus any extra system roots listed in `tool-runtime-config.yaml :: registry.paths`

## Environment Variables
Common knobs:
- `MAGICIAN_PORT`
- `MAGICUTOR_PORT`
- `MAGICUTOR_CONFIG_PATH`
- `RUST_LOG`

## Migration Notes
From the old stack:
- remove assumptions about a `magictunnel` process on `3001`
- remove calls to `/magictunnel` routes
- use local capability/skill files as the source of tool metadata

## Historical Material
Legacy two-service/tunnel deployment content has been superseded. Use `docs/archive/` for historical reference only.
