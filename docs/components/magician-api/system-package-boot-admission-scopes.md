# Which scopes system-package boot admission runs in

`AppPlatformApi::admit_system_packages_at_boot` is described in
[the component README](README.md#appplatformapiadmit_system_packages_at_boot-2026-09-03);
this page covers *which scopes it touches*, which is a contract rather than a
detail.

## The rule

Admission enumerates **live workspaces** with
`auth::workspace_registry::enumerate_live_scopes(scopes_root)`: for each
principal directory under `scopes/`, the workspaces registered in its
`workspaces.json`, plus that principal's `default` workspace (always, even
with no registry file yet), minus ids queued in `pending_purge`. It then admits
only into scopes that host user subsystems (`scope_hosts_user_subsystems()`
from `artifact_v2::workspace`). Reserved sinks (`system`/`system`,
`_quarantine`/`_quarantine`) are skipped, with the count logged at debug.

The default scope is never skipped: a first boot that has written nothing
must still ship with its apps, and it is the scope a single-user deployment
uses.

A scope directory no registry vouches for (residue of a deleted workspace,
recreated by a writer that never consults the registry) is not admitted. Boot
logs it once at INFO (`unregistered workspace directories left
on disk; not admitted`, with the count and `principal/workspace` ids) and
leaves it on disk. A principal whose registry cannot be read fails open: all
its directories are admitted, with a warning. Use the same predicate
(`is_live_workspace`, `live_workspaces`) for any other per-scope opener.

## Which scopes readiness waits for

Only each principal's `default` workspace is admitted before `/health` turns
ready. Every other live workspace is admitted by a background task
(`start_system_package_boot_admission` returns its `JoinHandle`) after HTTP
readiness, `SYSTEM_BOOT_ADMISSION_BACKGROUND_CONCURRENCY` (3) scopes at a
time. While a background scope is pending, `AppProjectionWorker` skips it;
when it finishes, it is released and its request-path caches are evicted (see
[the boot admission page](../magician/system-boot-admission.md#behaviour)).

## Why enumeration alone is wrong

A directory scan (`list_scopes()`) treats a reserved sink, and the leftover
directory of a deleted workspace, as a scope like any other, and admitting
into one copies the whole system package set (~7.5 MB) plus an
`app_store.sqlite3` into a bucket with no owner to approve an app and no
surface to run one on, while delaying `/health`. Eager initialization is fine;
materializing a subsystem in a scope that cannot use it is what the gate
removes. The wider review is
every scope pays for every subsystem.

## The projection worker follows the same rule

`AppProjectionWorker` rediscovers scopes on a cadence and opens each one's app
store for the life of the process, so it enumerates with
`list_tenant_scopes()` to avoid holding a reserved sink's `app_store.sqlite3`
open.

Known gap: the worker still enumerates *directories*, not live workspaces, so
a residue scope that already holds an `app_store.sqlite3` from an earlier boot
is still opened and drained. Switching it (and the other lazy per-scope
openers) to `workspace_registry::is_live_workspace` is the follow-up.

Known gap: another, unidentified opener still opens a sink's app store about
thirty seconds into startup.

## Tests

`apps_api::tests::boot_admission_skips_reserved_sinks_and_keeps_every_tenant`
creates four scope directories (the default scope, another principal's
default workspace, and both reserved sinks), runs admission, and asserts the
summaries cover exactly the two tenants.

`apps_api::tests::boot_admission_follows_the_registry_and_backgrounds_non_default_workspaces`
registers `work`, purges `eval-1`, leaves an unregistered directory, and
asserts the foreground pass covers only `default`, the background pass only
`work`, and no scope stays pending. The predicate is pinned by
`auth::workspace_registry::tests::{the_default_workspace_is_live_without_a_registry,
enumeration_follows_the_registry_and_excludes_pending_purges,
an_unreadable_registry_keeps_every_directory_live}`.
