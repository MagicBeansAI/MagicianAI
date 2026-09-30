# Settings: critical alerts

Secure HITL P5. Server seam:
`GET`/`PUT /api/magician/v2/settings/critical-delivery`,
`POST /api/magician/v2/settings/critical-delivery/test`,
`GET /api/magician/v2/hitl/deliveries`
([critical-request delivery](../magician/critical-request-delivery.md)).

Settings hosts `$lib/settings/CriticalDeliveryPanel.svelte` (fed by
`$lib/stores/criticalDeliveryStore.ts`) as **Critical alerts** — where a
credential request reaches you. It shows every channel type that has an
owner identity (masked to its last two characters; the addresses themselves
live in `envoy.owner_identities` and are never edited here), lets the owner
enable channels and order them, choose simultaneous or staged fan-out with
the staged fallback delay, keep or drop the phone push, and set a daily
quiet window with the time-bound interrupt. A channel enabled without an
owner identity is flagged, as is a missing link origin (the alert then
says to open Magician → Attention instead of linking the request). The link
origin is not editable here — it is deployment topology, read from
`hitl.critical_delivery.owner_ui_origin` and falling back to
`mobile_access.public_origin`, and a save from this panel leaves it alone.

Below the settings the panel lists recent deliveries — request, masked
destination, state (`queued`, `claimed`, `provider_accepted`, `failed`,
`unavailable`, `ambiguous`, `expired`, `resolved`, `relayed_by_origin`,
`skipped`), attempts, a value-free note — plus the request → queued and
queued → provider-accepted p50/p95 and when each channel's bot last claimed.

**Save** writes the `hitl.critical_delivery` section and reloads the live
config; it sends nothing. **Send test alert** is the only sender: a real
`test` delivery to every enabled destination, never held by quiet hours, so
the owner sees exactly what a real alert looks like on each channel.
Tests: `CriticalDeliveryPanel.component.test.ts`.

## Devices: notifications for verification codes (secure HITL P6)

The Devices panel (`$lib/devices/DevicePairingPanel.svelte`) shows an
Android phone a second grant, **Use notifications for verification codes**
(`setDeviceVerificationCodes` → `PUT /api/magician/v2/devices/policy/verification-codes`,
read back from `GET /devices/policy` `verification_code_devices`). While a
verification request is open the phone judges its own notifications
(protected apps excluded) and answers the request over its paired
credential; the code never leaves the phone through a tool result. Pairing
alone grants nothing; the grant is reversible at once.
