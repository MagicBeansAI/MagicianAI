# App installation review — parsing and rejection

`fetchAppInstallationReview` validates the whole review payload before showing
it. A payload that fails validation is rejected outright rather than rendered
with the unparseable parts dropped: approval grants every requested behaviour
when the grant field is omitted, so a silently dropped entry is authority the
owner never saw.

## Rejection names the failing clause

Rejections carry a `consistency:` reason listing every checked quantity — for
example:

```
behaviors=1 eventBehaviors=0 customSurface=ok workflows=1/1 toolDispatch=2/2
inert=0/0 bindings=0 dataHandling=ok background=ok network=ok resources=ok
permissionDiff=ok
```

The counts are the diagnostic. `bindings=0` beside `workflows=1/1` says which
of a dozen `||` clauses failed, turning a bisection into a read. `BAD` marks a
value that failed to parse at all; `n/m` marks a collection where `n` entries
survived parsing out of `m` received, so a partial parse is visible rather than
silently truncating.

## Workflow material bindings are one per workflow

The parser requires `workflow_material_bindings.length === workflows.length`,
and that every binding names a known workflow.

This mirrors the server exactly: `resolve_reviewed_workflow_material` builds
`Vec::with_capacity(manifest.app.workflows.len())` and pushes once per workflow,
so a payload with an unbound workflow is one the server cannot produce. The
check is not defensive tidiness — a workflow with no material binding is
material access the review never disclosed.

Fixtures must therefore carry a binding for every workflow they declare. A
fixture that declares a workflow and leaves `workflow_material_bindings` empty
is invalid input, not a lenient case.

## Interactive-page approval

`AppCustomSurfaceReview` renders an unchecked choice for each reviewed page.
The expandable code review shows entry documents and digests, the complete
script inventory, static-scan findings, and the sandbox/content policy.
`parseCustomSurface` retains those fields and the request digest rather than
reducing them to counts; incomplete entries, duplicate routes or script paths,
and inconsistent byte totals reject the review.

Selecting a page adds its exact route, document, and reviewed request digest to
`granted_custom_surface_entry_points`. Unchecked pages receive no hosting grant.
The approval client rejects substituted routes/documents, duplicate selections,
and stale request digests before sending; the backend independently enforces
the same reviewed subset. Existing enabled apps need a fresh reviewed update
to add these grants. The operator switch alone does not grant page hosting.

Verification: `installationReview.test.ts` covers parsing and approval transport;
`AppCustomSurfaceReview.component.test.ts` mounts the actual page choices and
checks selection, removal, and code disclosure.
