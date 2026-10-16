# Diagnosing a stranded app workflow task

A task under `task_app_<digest>` whose binding cannot be resolved is reported
once per boot by the startup synthesis reconciler and never retired:

```
WARN [ARTIFACT-V2] startup synthesis reconciler skipped task
  principal=anonymous workspace=default
  task_id=task_app_<digest>
  error=runtime error: the app workflow binding is corrupt
```

The reconciler calls `is_workflow_task` first, which is `read_task_binding`, so
this fires before any recovery is attempted. Nothing removes the task, so the
same line returns on every boot.

## Logged reasons

`AppWorkflowError::CorruptBinding` is returned from many places in
`apps/workflows.rs` and is deliberately opaque to callers (a governance
boundary). The operator log is not: `corrupt_task_binding(scope, task_id,
reason)` logs the failing invariant at each rejection inside
`read_task_binding` and its marker check, then returns the same opaque error.
One boot is enough to tell these apart:

| Reason logged | What it means |
| --- | --- |
| no task directory exists for a canonical app workflow task id | the id is server-owned but its scoped directory is gone |
| no task manifest exists for a canonical app workflow task id | the directory survived, `manifest.json` did not |
| the task manifest's app marker and identity do not agree with its task id | `created_by`/`app_workflow` tag or the scoped identity was altered |
| a binding sidecar exists without the server-authored app task marker | a sidecar on a task the server never marked as an app task |
| the sealed task-binding sidecar is unreadable or its seal did not verify | HMAC mismatch or an unreadable sidecar |
| a sealed sidecar exists but the registry holds no task-binding control | disk kept the binding; the registry row that authorises it did not |
| the registry's task-binding control is not active | the row exists but its lifecycle is not `Active` |
| the registry's sealed content does not match the verified sidecar | both exist and disagree |

The last three are registry-facing: they separate "the app platform forgot this
task" from "the two sides disagree".

## Reading the on-disk half

The scoped task directory is enough to clear the manifest-facing rows without
opening the SQLCipher registry:

```
$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/tasks/<task_id>/
  manifest.json                     # created_by must be "app_action",
                                    # tags must include "app_workflow",
                                    # task_id/principal/workspace must match exactly
  state/app_workflow_binding.json   # the sealed sidecar
  state/task_state.json             # status; `ready` with an empty executions/
                                    # means the run never launched
  executions/
```

If those all look right, the failure is on the registry side and the logged
reason names which one.

## Canonical JSON comparison

After HMAC verification the reader compares the complete seal and payload as
canonical JSON, not bytes, because the registry publisher and the typed sidecar
serializer may differ in key order or whitespace. It still requires an active
registry control and the exact scoped task marker; an altered payload, seal or
scope is rejected. The mirror is restored from registry bytes. The same
comparison applies when reopening run state and replaying an existing task
binding.

Failed execution admission and terminal cleanup log their underlying error with
scoped task and execution ids; public task outcomes stay the bounded, opaque
app-workflow status.

## Stack depth in debug builds

A recovery stack overflow aborts the whole backend, which shows in Vite as
socket hang-ups or connection refusals across unrelated endpoints. To stay
within the ordinary Tokio worker stack, these futures are boxed or run as their
own execution jobs:

- startup recovery's large execution operations, and the bounded sidecar
  reader's payload decode;
- on fresh launch, the execution runner, app admission and bounded resource
  maintenance (a debug crash in `tool_for_execution` / `maintain_resource_root`
  can come from accumulated parent poll frames);
- the orchestrator/agent-loop handoff, a separate structured execution job,
  shared by fresh, exact sleeping retry and accepted in-process recovery (the
  token meter is carried explicitly; secret scope is installed inside the
  orchestrator);
- terminal outbox projection: Artifact acceptance, nested outcome persistence
  and app cleanup, still under the original lifecycle exclusion and
  cancellation scope;
- scheduled-goal admission during startup hydration, and the later agentic
  loop, built lazily on the execution runtime.

Byte, depth, HMAC and registry-head checks, ordering, cancellation, authority
checks and resource accounting are unchanged. Keep these boundaries through
both launch and recovery.

## Run limits and catalog

The repo seed `magician-config.yaml` sets `app_platform.resources.max_no_progress_seconds`
to 60 (cold debug preparation can take ~50 s before the first model request);
an omitted setting falls back to 30 (`default_app_resource_max_no_progress_seconds`).
A run whose no-progress interval exceeds its lifetime ceiling is refused;
token, dollar and overall runtime ceilings still apply.

An admitted run keeps its app catalog through initial owner loading and every
policy refresh. Reviewed workflow dependencies resolve from the sealed binding
even when the runner's tool list omits them; current runner denials can still
narrow that set. The server-derived `app_store_query` and
`app_commit_mutations` schemas stay attached and depend on neither a mutable
agent grant nor the process-wide deferred-tool index. Protected app decisions
offer only their bounded catalog and never add the general agent's universal
tools, even when that catalog is empty (otherwise the run would be offered
HTTP/filesystem tools it cannot dispatch).

## Remedy for actual corruption

A stranded task has no execution to recover (`try_recover_task_synthesis`
fails before it reaches one), so re-running the action from the app's surface,
which mints a fresh task id, loses nothing.

Do not delete a task to work around a serialization-only mismatch. For a
confirmed orphan, removing its directory is a deletion under the runtime data
root and needs the owner's explicit confirmation. Automatic retirement is not
built: it would have to tell "the registry forgot this" from "the registry is
temporarily unreadable", and guessing wrong destroys a governed task.

An interrupted admitted run can keep the installation's foreground slot. Use
Apps → the action → Open existing run to inspect its canonical run reference
and request cancellation. The cancellation writer requires the
`action_cancellation` control kind (registry schema V33+). Operator logs carry
the underlying runtime error; the public API keeps its bounded message.
Physical model admission logs its refusal with only the profile, token
reservation bounds and identity-match result; prompts and tool results stay
out.

Tool admission releases its resource lease if a later progress or lifetime
check refuses dispatch, including on resume. Cancellation drops the dispatch
lease before taking cleanup authority (removing the root map entry alone is not
enough while cleanup still holds an `Arc` to it). A refused or cancelled run
therefore does not block later app actions until restart.
