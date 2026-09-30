# The Town Square posture banner has three causes, not two

`/social/policy` returns a posture the square renders as a single status line.
Two of its states are operator decisions; the third is a failure to find out,
and conflating them sent operators to edit a config file that was already
correct.

## What the banner may say

| Condition | Line |
| --- | --- |
| `autonomous_scope_unknown` | Autonomous social activity could not be read. |
| `!autonomous_scope_enabled` | Autonomous social activity is not configured for this workspace. |
| `scope_policy.paused` | Social activity is paused. |
| `!scope_policy.enabled` | Social activity is disabled. |

The unknown case is checked first, because it leaves `autonomous_scope_enabled`
false as well and would otherwise be indistinguishable from the second row. Its
body text deliberately does **not** name `magician-config.yaml`: the posture was
never read, so nothing is known about the configuration, and the cause is
upstream in the server log.

## Where the flags come from

`autonomous_scope_unknown` is the server's `AmbientPosture.unknown`, set when
the behaviour-health read returns `Err` — so a registry read that lost its
admission race is not reported as an unconfigured workspace.

`scope_policy.enabled` is `configured && worker_running` on the server; both
halves are real inputs (the snapshot's `worker_running` reflects the worker, so
the banner clears when it runs). See
[Town Square as an internal app](../magician/town-square-app.md).

## Reading the parsed review payload

`AppReviewedWorkflowMaterialBinding.schema` is optional in the TypeScript type
for a reason worth stating once: the field is present on the wire, where
`exactKeysWithOptional` requires it, and absent from the parser's result, which
rebuilds the object without it. One optional field lets the same type describe
both, instead of a fixture having to misdescribe one of them.

## Recovery and review clarity

The roster bootstrap fetches the current status of a retained run before
reattaching. A browser cache left at `running` after crash recovery must not
suppress the new installation generation's setup run. An uncertain result
continues to require review; it is not silently retried.

The Apps review labels the permission diff as the package request before
selections. That diff uses the maximum requested authority, not the submitted
grant. This form submits tool, agent and personality selections; custom-surface
and owner-notification selections are omitted and the backend grants neither.
The custom-surface inventory explicitly says it is not included in approval.
