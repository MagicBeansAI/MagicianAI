# Which scopes system-package boot admission runs in

`AppPlatformApi::admit_system_packages_at_boot` is described in
[the component README](README.md#appplatformapiadmit_system_packages_at_boot-2026-09-03);
this page covers the separate question of *which scopes it touches*, which is a
contract rather than a detail.

## The rule

Admission enumerates every scope, then admits only into scopes that host user
subsystems — `scope_hosts_user_subsystems()` from
`artifact_v2::workspace`. Reserved sinks (`system`/`system`,
`_quarantine`/`_quarantine`) are skipped and the count is logged at debug.

The default scope is explicitly **not** skipped. It is appended to the
enumeration when absent, because a first boot that has written nothing must
still ship with its apps, and it is the scope a single-user deployment uses.

## Why enumeration alone is wrong

`list_scopes()` is a directory scan, so a reserved sink is a directory like any
other. Admitting into one copies the whole system package set (~7.5 MB) plus an
`app_store.sqlite3` into a bucket with no owner to approve an app and no surface
to run one on.

Eager initialization is fine; materializing a subsystem in a scope that cannot
use it is what the gate removes.
The wider review is
every scope pays for every subsystem.

## The projection worker follows the same rule

`AppProjectionWorker` rediscovers scopes on a cadence and opens each one's app
store, holding the connection for the life of the process. Enumerating a
reserved sink therefore kept an `app_store.sqlite3` open for a scope that
admission already refuses to admit anything into, so the worker enumerates with
`list_tenant_scopes()` too.

Known gap: another, unidentified opener still opens a sink's app store about
thirty seconds into startup.

## Test

`apps_api::tests::boot_admission_skips_reserved_sinks_and_keeps_every_tenant`
creates four scope directories — the default scope, an ordinary tenant, and both
reserved sinks — runs admission, and asserts the summaries cover exactly the two
tenants.
