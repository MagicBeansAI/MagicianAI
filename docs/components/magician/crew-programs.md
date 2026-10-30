# Crew operating Programs

Programs supply ongoing instructions to existing agent focus areas. The scoped
runtime reads `scopes/<principal>/<workspace>/programs/<name>.md`; a similarly
named Notes document is not its runtime source. Git-backed operating baselines
live under `magician_data_v3/scopes/anonymous/default/programs/`.

## Templates and live ownership

Repository Programs are reusable starting points. A deployment instantiates and
adapts them under `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/programs/`;
the runtime loader reads that live folder, with no fallback to the repository
copy when a live document is missing. Reading a Program does not install a seed.
The Web goal picker lists live Programs, not repository templates.

Current installers and launchers seed `harness_reliability.md` only when its live
file is absent, preserving an existing operator document. The nine role operating
templates below are not automatically installed for every workspace. They need
an explicit setup or copy into the intended live scope.

A newly instantiated live document may initially match its template. The loader
does not require arbitrary customization before reading it; content, approvals
and execution policy determine whether a particular action is appropriate.
Personal goals, filled public charters, business facts and progress remain
deployment-owned and must not be copied back into the generic repository seeds.

| Runtime filename | Owner | Existing focus areas |
| --- | --- | --- |
| `daily_ops.md` | CEO | Morning briefing, evening wrap |
| `company_strategy.md` | CEO | Board review |
| `product_ops.md` | CPO | Backlog grooming |
| `product_strategy.md` | CPO | Product strategy review |
| `sprint_plan.md` | CTO | Morning engineering standup |
| `engineering_strategy.md` | CTO | Sprint review, agent improvement |
| `content_calendar.md` | CMO | Content calendar |
| `marketing_strategy.md` | CMO | Marketing review |
| `revenue_strategy.md` | CRO | Weekly GTM review |
| `harness_reliability.md` | Harness SRE | Harness reliability audit |
| `outward_charter.md` | Ambassador | Inbound sweep, follow through |

The operating baselines define inputs, bounded reviews, handoffs, output evidence
and success checks. They use current owner-approved priorities instead of
inventing business targets, prices, dates or customer facts. Missing business
inputs gate the specific decision or action; they need not prevent internal
evidence review. An unfilled public charter still blocks outward claims and sends.

Each [goal note](../../features/goal-workspaces.md#goal-notes) can link one Program
and an existing `harness:<agent>:<focus-slug>` identity. Several sub-goals can
therefore connect a main goal to several Programs. Clearing a missing-document
blocker makes a goal ready for consideration; it does not prove completion or
activate a schedule. Keep adopted status and results distinct from file existence.

Program content is not an authority grant. Current agent tools, delegation
targets, task/step limits, approval policy, privacy boundaries, schedules and
global pause state remain authoritative. Keep mutable progress in the existing
state lane; owner saves and approved revisions use the
[Program editor contract](ceo-decomposition.md#playbooks-program-documents).

Seeds are deployment starting points, not an automatic replacement for edited
live Programs. When repairing or synchronizing a deployment, compare and preserve
its current documents, back up replaced content and verify exact goal/focus
references. Never silently overwrite live strategy from a seed or infer that a
seed was consumed by a run merely because it exists in git.
