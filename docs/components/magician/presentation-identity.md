# Presentation Identity

Product-facing copy must not depend on the backend service name or on a live
network connection. The canonical checked-in identity is
[`data/presentation_identity.json`](../../../data/presentation_identity.json):

- `product_name` names the product in ordinary UI copy;
- `host_app_name` names the native desktop host where an OS permission prompt
  needs the installed app identity;
- `assistant_fallback_name` is the neutral fallback when no scoped primary
  agent identity is available.

`make presentation-identity-codegen` generates committed constants for Rust,
Unified UI, Magios, Magdroid, and the Tauri Rust/Svelte surfaces. Consumers use
those constants instead of spelling the product name again. `make
presentation-identity-check` validates the manifest strictly, compares every
generated file byte-for-byte, confirms the migrated consumers still import the
generated seam, and rejects raw product-name literals in those migrated files.
The check is a prerequisite of `make check-all` and both all-binary build lanes.

Native first-run, QR enrollment, widgets, extensions, notifications, and
offline errors use the generated local value. They deliberately do not fetch a
server value: several of those surfaces run before enrollment or while the
server is unavailable. Platform metadata that the OS reads before application
code—bundle names, Tauri metadata, URL schemes, entitlement identifiers, and
brand assets—remains compile-time metadata and is not a runtime-configurable
presentation surface. Existing storage paths and compatibility identifiers are
also stable operational values: they must not be assembled from
`product_name`. A migrated consumer may retain one only with the narrow
`presentation-identity-allow: stable-operational-identifier` marker enforced by
the generator check.

The manifest is a product source of truth, not an operator setting. If
white-label deployment is introduced later, it should be an additive,
authenticated presentation identity returned during bootstrap/enrollment,
cached in App Group/DataStore storage, and bounded by the generated identity as
the offline fallback. Backend operational identifiers, storage directory names,
API paths, and compatibility keys remain separate and must never be derived
from presentation identity.
