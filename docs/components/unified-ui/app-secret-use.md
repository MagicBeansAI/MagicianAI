# App secret use in the install review

When an app's locked tools need one of the owner's saved keys, the install
review (`routes/(app)/apps/+page.svelte`) shows a **Secrets** table with one
row per requested key:

| Column | Content |
|---|---|
| Tool | The tool asking for the key |
| Key | The key's vault name, with a *required* badge if the tool cannot run without it, and "(as a config file)" when it reaches the tool as a private config file |
| Only sent to | Where the key can go: the host(s) the tool declares ("any site" for `*`), or the hosts picked for it, or "any site" when that is ticked; "cannot be sent anywhere in this app" with the reason when the request is `not_grantable` |
| Allow | A checkbox for a declared tool's key; for a tool that declares no host, a multi-select of the app's named hosts (`setSecretUseScope`); and, when the app asks for any public host, an "any site" tick with a warning (also the only way to grant a `*` tool's key); nothing for a not-grantable key. All start ungranted |

- **Data model.** `lib/apps/installationReview.ts` parses
  `requested_secret_uses` strictly: a malformed entry rejects the whole
  review payload.
- **Default grant.** `defaultGrantedNames` starts with
  `granted_secret_uses: []`, so nothing is ticked.
- **Toggling.** `toggleSecretUseGrant` ticks or unticks only pairs the review
  requested.
- **Diff.** The permission diff accepts the optional `secret_uses` axis.

## Network

Above the Secrets table, a **Network** table lists each tool that runs in
place or declares hosts (`tool_runtime`): "runs in place from <skill>" (or
"private copy") and what it can reach (its declared hosts, "the hosts this
app is allowed to reach", or "any website" when the grant below is ticked).
When the review `offers_any_public_host`, a checkbox grants **any public
website**; it starts unticked (`defaultGrantedNames` sets
`granted_any_public_host: false`) and `toggleAnyPublicHostGrant` refuses when
not offered. The Network table shows each tool's `reachable_hosts`. `secretReach`
renders the Secrets table's "Only sent to" text; a key reaches "any site"
only with its explicit tick, and a tool using a key reaches only that key's
scope.
The parser accepts `tool_runtime`, `offers_any_public_host`, the new secret
fields and the `any_public_host` diff axis, and rejects a key that names
nowhere it can go.

Tests: `lib/apps/installationReview.test.ts` ("secret uses", "in-place tools
and network").
