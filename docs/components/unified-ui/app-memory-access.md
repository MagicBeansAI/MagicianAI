# App memory access UI

Where the owner decides what memory each app may read. Model and backend
contract: [app-memory-access](../magician/app-memory-access.md).

## Install review

`routes/(app)/apps/+page.svelte` renders a **Memory access** fieldset when the
review carries `requested_memory_read`: the app's stated purpose, then one row
per requested user tier and per requested agent ("What <agent> has learned"),
each with two tick boxes — **While you use it** and **In the background**.
Sensitive tiers are marked `sensitive`.

Ticks start from `defaultGrantedNames`, which copies the review's
`default_grant` (non-sensitive items while in use, nothing in the background)
into `granted_memory_read`, echoing the reviewed request digest.
`toggleMemoryGrant` flips one item in one run mode. Approval sends the exact
choice, so what the owner sees is what is granted.

`parseReview` accepts `requested_memory_read` only when it parses completely;
a malformed memory request rejects the whole review rather than being dropped,
because the default grant would otherwise apply to something the owner never
saw. The permission diff parser accepts the optional `memory_read` axis.

## Installed apps

`lib/apps/AppMemoryAccessPanel.svelte` (mounted for enabled and disabled
installations, above "Lifecycle and portability") loads
`GET .../memory-access` when opened, shows the same two-column table seeded
from the grant in force, and saves with `POST .../memory-access` using the
loaded `edit_revision`. A conflict (someone else changed it, or the choice is
outside the request) is reported with a prompt to reload. "Untick all" plus
Save revokes all memory access. Apps that requested no memory say so.

`lib/apps/appMemoryAccess.ts` parses the response strictly: a grant with an
unknown engagement binding or malformed selection throws instead of rendering.
