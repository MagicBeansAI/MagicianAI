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

The last three are the registry-facing ones and are the interesting split: they
separate "the app platform forgot this task" from "the two sides disagree".

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
canonical JSON, not bytes, because the registry publisher and typed sidecar
serializer may differ in key order or whitespace. It still requires an active
registry control and the exact scoped task marker; altered payload, seal, or
scope is rejected. The mirror is restored from registry bytes.

The same canonical comparison applies when reopening run state and replaying
an existing task binding. Failed execution admission and terminal cleanup log
their underlying error with scoped task/execution IDs for the operator; public
task outcomes remain the bounded, opaque app-workflow status.

Startup recovery boxes its large execution operations, and the bounded sidecar
reader decodes its payload behind a box. This keeps failed app-task recovery
within the ordinary Tokio worker stack in debug builds; byte, depth, HMAC and
registry-head checks remain in force. A recovery stack overflow can otherwise
abort the whole backend and appear in Vite as socket hang-ups or connection
refusals across unrelated endpoints.

Fresh app launches also heap-own the child futures for the execution runner,
app admission, and bounded resource maintenance. A debug crash in
`tool_for_execution` / `maintain_resource_root` can be caused by accumulated
parent poll frames even after the job reaches the dedicated execution runtime.
Keep these boxing boundaries through both launch and recovery; they retain
inline ordering, cancellation, authority checks, and resource accounting.

The subsequent orchestrator/agent-loop handoff uses another structured
execution job. Merely running admission on an execution worker does not reset
its native poll stack before model/tool execution. Fresh, exact sleeping retry,
and accepted in-process recovery share this boundary; the token meter is
carried explicitly and secret scope is installed inside the orchestrator.

Terminal outbox projection also boxes the Artifact acceptance future, nested
outcome persistence, and app cleanup. These stay under the original lifecycle
exclusion and cancellation scope; the general Tokio projector does not carry
the full terminal/sidecar state machines inline while verifying their HMACs.

The development configuration sets the app no-progress interval to 60 seconds
(cold debug preparation can take ~50 s before the first model request); the
code fallback for an omitted setting is 30 seconds. Both stay below the
reviewed foreground lifetime and the 90-second Town Square background limit;
token, dollar, and overall runtime ceilings still apply.

An admitted run must also retain its app catalog through initial owner loading
and every policy refresh. Reviewed workflow dependencies are resolved from the
sealed binding, even when the runner's ordinary tool list omits them. Current
runner denials can still narrow that set. The server-derived `app_store_query`
and `app_commit_mutations` schemas remain attached to the run; neither depends
on a mutable agent grant or the process-wide deferred-tool index. Protected
app decisions offer their complete bounded catalog directly and never add the
general agent's universal tools, including when the admitted catalog is empty
(otherwise the run would be offered HTTP/filesystem tools it cannot dispatch).

## Remedy for actual corruption

A stranded task has no execution to recover — `try_recover_task_synthesis`
fails before it reaches one — so nothing is lost by re-running the action from
the app's surface, which mints a fresh task id.

Do not delete a task to work around a serialization-only mismatch. For a
confirmed orphan, removing its directory is a deletion under the runtime data
root and requires the owner's explicit confirmation. Retiring the orphan automatically would
be the better answer and is not built; it would have to distinguish "the
registry forgot this" from "the registry is temporarily unreadable", and
guessing wrong destroys a governed task.

An interrupted admitted run can retain the installation's foreground slot.
Use Apps → the action → Open existing run to inspect its canonical run reference
and request cancellation. The cancellation writer requires the
`action_cancellation` control kind (registry schema V33+). Operator logs include the
underlying runtime error while the public API keeps its bounded error message.
Physical model admission also logs the underlying refusal with only the
profile, token reservation bounds, and identity-match result; prompts and tool
results stay out of these diagnostics.

Tool admission releases a resource lease if a later progress or lifetime check
refuses dispatch, including on resume. Cancellation also drops the dispatch
lease before acquiring accepted cleanup authority: removing the root map entry
alone is insufficient while the cleanup function still holds an `Arc` to it.
This prevents a refused or cancelled run from blocking subsequent app actions
until the next process restart.

Scheduled goals can reach task-shell creation during startup hydration. Goal
admission runs as a lazily built job on the execution runtime, as does the
later agentic loop, keeping both phases off the hydration/wake caller's stack.
