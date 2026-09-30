# Public lifecycle proof

The reference set has three independent pack roots:

- `research-planner/app` — base package `0.1.1`;
- `research-planner-composition-destination/app` — typed composition target
  `0.1.0`; and
- `research-planner-update-v0.2.0/app` — complete update candidate `0.2.0`.

No update root is a patch overlay, and no owner fixture sits inside a pack
root. All three checked-in generated-artifact pairs are current owner output
from `magician app check --write-generated`. The D8 Browser selector
set is already frozen in both base and update sources. After the current
Magician binary is available, the deterministic local ordering is:

```sh
npm ci
npm run recipes:materialize
npm run test:static
npm run app:pack-reference-set
```

`app:pack-reference-set` invokes only `magician app check --write-generated`,
`app test`, and `app pack` for each root. Pack writes are create-new; remove or
rename an old test artifact deliberately before rerunning. Generated output
must be reviewed in each root before publication. This is a source/conformance
proof, not live-owner or release qualification.

## Initial publication

Import is inert staging and is not candidate publication. The shortest exact
reviewed path is:

```sh
node scripts/public-lifecycle.mjs publish-reviewed \
  --archive ./dist/packages/research-planner-0.1.1.app.zip \
  --request-id reference:initial-1 \
  --principal anonymous --workspace default
```

The script executes the public `candidate-publish -> review -> approve`
sequence, correlates the returned installation and attempt, and refuses a
publication that claims activation authority before approval. Exact retry must
reuse the request identity and archive bytes.

Publish and approve the composition destination separately, then install the
same immutable destination package into three distinct local installations for
the bounded three-hop proof. Composition remains one of the eight
supported-public operations; the client carries only opaque run references, exact
destination identities, caller-declared mappings, unique hop idempotency keys,
and a payload-free cursor.

## Reviewed destructive update

The `0.2.0` candidate adds nullable `research_plan.reviewed_at` and retires
`research_plan.revision_note`. The exact operation bodies live outside the pack
root in `research-planner-update-v0.2.0/migrations/`. Run:

```sh
node scripts/public-lifecycle.mjs update-reviewed \
  --archive ./dist/packages/research-planner-0.2.0.app.zip \
  --installation-id <enabled-base-installation> \
  --expected-generation <current-generation> \
  --request-id reference:update-1 \
  --operations-file ../research-planner-update-v0.2.0/migrations/v0.1.1-to-v0.2.0.json \
  --passphrase-file ./archive.passphrase \
  --principal anonymous --workspace default
```

The script performs `update-begin`, update-targeted `candidate-publish`, exact
`update-plan`, encrypted `update-backup` when the reviewed plan is destructive,
`review`, and `approve` with the migration-run and update-plan digests. It
requires every examined record to be representable and adds destructive
confirmation only for a plan that reports itself destructive. The same script
supports `reinstall-reviewed`; reinstall starts from an
`uninstalled_retained` installation and therefore does not invent an update
parking transition.

If an update must be abandoned before switch, use public `update-abort` with
the exact parked generation and a retained request ID. After a switched
code-only update, `rollback-code` preserves post-update writes and does not
restore grants. A data rewind is a separate `rewind-preview -> rewind-commit`
decision with explicit confirmation and the encrypted backup passphrase.

## Data and combined portability

Data and combined exports default to authenticated encryption. To exercise a
complete reviewed round-trip into an already enabled compatible destination:

```sh
node scripts/public-lifecycle.mjs portability-roundtrip \
  --source-installation-id <source-installation> \
  --destination-installation-id <compatible-destination-installation> \
  --kind combined \
  --archive ./dist/research-planner-migration.appdata \
  --passphrase-file ./archive.passphrase \
  --request-id reference:portable-1 \
  --principal anonymous --workspace default
```

This executes `data-export -> data-import-preview -> data-import-approve ->
data-import-commit`, retaining the preview digest and approval reference. A
combined archive authenticates package compatibility bytes but never grants
install or update authority; candidate publication and owner approval still
precede import into a new scope. Plaintext export is a separate warned CLI
action and remains denied for Secret-class records.

## Recovery boundaries

Client `AbortSignal` stops only the caller's request/wait. Logical action
cancellation is the generation-bound public operation. Post-dispatch timeout,
cancellation, or decode failure on a keyed operation remains uncertain and may
be retried only with byte-identical input.

`JsonRunRecoveryStore` persists the exact installation, action, input and
idempotency key before launch, then adds the opaque `run_ref`. Completed but
withheld output stays recoverable. Entity-change recovery retains the monotonic
sequence, serializes callbacks, and treats `reset_required` only as an
instruction to refetch. Composition subscription recovery likewise reuses the
exact chain and returned cursor; a gap/expiry reset never authorizes a guessed
next hop.
