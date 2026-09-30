# App OS-jail egress (`app_egress` v1) and in-place skills

A jailed app tool normally has no network. A skill whose reviewed source
declares its **HTTPS hosts**, or an in-place skill the owner grants network
to, runs instead in MagicRun's brokered-egress jail, and its only way out is
a broker Magician starts for that one call.

## Trust model

- Installed skills and their runtimes are **trusted**: a compromised skill has
  already compromised the system through agents.
- The untrusted party is the **app** (shared, imported or LLM-generated),
  which chooses the arguments.
- The app layer therefore confines **authority and data flow**, not the
  skill's code. A jailed tool may use only its own skill files and runtimes
  (read and execute), a fresh private workdir (writes), the hosts the owner
  granted that app, and the keys the owner ticked for that (app, tool) pair.
  Never the home directory, another skill's `config/.env` or the Magician
  data root.
- Everything is generic: no per-skill code, recipes or copies. The owner
  approves per app, at install: tools, hosts (or "any public host") and keys.

## Declaring it (skill author)

```yaml
metadata:
  magician:
    app_egress:
      schema_version: 1
      destination: export.arxiv.org      # one host, or:
      # destinations: [api.example.com, uploads.example.com]
```

- One `destination`, or `destinations` with ≤ 16 hosts (never both), or
  exactly `destinations: ["*"]` for a skill that contacts arbitrary sites
  (network only under the app's "any public host" grant). Each is a lowercase
  DNS name; duplicates, IP literals, ports, paths, wildcards, local names
  (`localhost`, `*.local`, `*.internal`), unknown fields and other schema
  versions refuse the skill rather than being ignored.
- Declaring nothing: a copied single-file skill has no network; an in-place
  skill reaches only the hosts the owner grants the app.
- The block is in the reviewed source, so the source digest covers it; a host
  change is a new review.
- Examples: `arxiv-search` (`export.arxiv.org`), `hackernews-search`
  (`hn.algolia.com`), the API-key skills (see [skills-spec](skills-spec.md)),
  the MiniMax skills (`api.minimax.io`, in place with the vendored `mmx` CLI),
  `youtube-search` (`www.youtube.com`, in place with its `yt-dlp` companion
  from `skillshub/.venv`).

## Secrets (API-key skills, `app_secret_use_v1`)

Some web skills need one of the owner's saved keys (Tavily, Exa, Klipy,
GitHub). An app tool may use a secret only in this shape:

- **Supported contract.** `auth.kind: secrets`, `required` or `optional`. A
  copied single-file skill takes every key as an env var and must declare its
  hosts. An in-place skill (`supported_in_place_secret_requirement`) may also
  carry a `provider` label (no profile selection) and take a key as a
  **config file** (`config_directory` named `MMX_CONFIG_DIR`, the one format
  MagicRun materializes). No profile, lifecycle, storage, identity, stdin or
  scoped-file injection.
- **Where a key can travel.**
  - *Tool declares hosts:* the key goes only to the declared hosts the app is
    granted, regardless of "any public host". Those hosts are the vault scope.
  - *Tool declares none:* the owner picks hosts per (app, tool, key) from the
    app's named hosts (`AppSecretUseGrant::hosts`). The picked set is the
    key's vault scope and, while injected, the only hosts the broker admits
    (intersection over injected keys, within granted hosts; overrides "any
    public host"). A key without a choice is not used. In the update diff, an
    added host or "any site" is `secret_uses: expanded` (review); fewer hosts
    is narrowed. A stored grant holding such a key with no choice still loads
    (`validate_stored_secret_use_grant`), but the key stays unused until a
    scope is picked.
  - *Any site (explicit opt-in):* for a `*` tool or one declaring none, the
    owner may tick "allow this key to be sent to any site"
    (`AppSecretUseGrant::any_site`, off by default, warned). Valid only when
    the app asks for "any public host", and the call still needs the effective
    any-host grant. A `*` tool's key is grantable only this way; otherwise the
    review marks it `not_grantable` with the reason (likewise an undeclared
    tool's key when the app names no host). With no grant resolved, a `*` tool
    gets no network.
  - *Vault scope* is exactly the admitted host set (`vault_domains`), passed
    by `credential_route_domains` to MagicVault's scoped grant API: one host
    (`RequestedDomains::One`), a set (`RequestedDomains::hosts`, only if every
    host matches the key's `allowed_domains`), or `RequestedDomains::Any`
    (only for an unrestricted or `*` key policy). Issued with
    `issue_grant_scoped` / `issue_delegated_grant_scoped`, redeemed with the
    same scope. The vault does no public/private filtering; the broker refuses
    every non-public address, under Any too.
- **Explicit owner grant.** Install review lists each (tool, secret) pair with
  its destination and delivery. Nothing is granted unless the owner allows it;
  omitted or empty `granted_secret_uses` grants nothing. The grant is reviewed
  authority (`AppGrantRevision::{requested,granted}_secret_uses`, in the
  authority digest only when present); an added pair is
  `secret_uses: expanded`.
- **Resolved at run time.** Only granted secrets resolve. A required secret not
  granted refuses before launch (`SecretNotGranted`); an optional skill runs
  without.
- **Sealed path.** The value comes via the sealed
  `ScopedCredentialMaterialAdapter`: a short-lived vault grant bound to this
  tool, action and egress host, redeemed once, so the vault's per-secret policy
  (tools, domains, daily cap) still applies.
- **Where the value lives.** An env key exists only in the jailed child's
  environment. A config-file key is written by MagicRun as
  `{"api_key": …, "region": "global"}` to `<dir>/MMX_CONFIG_DIR/config.json`
  in a fresh `0700` per-call directory under system temp (one extra read-only
  root, not in the lock identity), removed with the call. MagicRun has no
  in-workdir path or read-only root kind, so this is an exec root holding only
  the JSON file. The directory name carries the owning pid; at boot
  `sweep_stale_credential_roots` removes orphans. The value never enters a lock
  identity, log, result, receipt, model context or app storage.

## Python skills (interpreter mode)

The jail execs exactly one file. A reviewed private artifact is a native
Mach-O/ELF executable or a single-file Python 3 script whose first line is
exactly `#!/usr/bin/env python3` or `#!/usr/bin/python3`
(`AppOsJailArtifactKind`, read from digest-pinned bytes). Anything else,
`#!/bin/sh` included, runs in place (below).

A script runs through MagicRun `GovernedJailInterpreter::python3_for_host()`
(≥ `0.1.77`): only the host's trusted Python (root-owned, not
group/other-writable, digest-checked again before launch), as
`-I -S -B <script>` with `site-packages` unreadable. On stock macOS that is the
CommandLineTools Python 3.9; a python.org install qualifies once root-owned and
`chmod -R go-w`. Without one the call is refused (`InterpreterUnavailable`).
Scripts must be stdlib-only and single-file.

## In-place skills (`InPlaceSkill`, `app_in_place_skill_v1`)

Most skills cannot be one copied file (e.g. `web-search-via-minimax` is a
Python wrapper spawning the Node `mmx` CLI from `skillshub/node_modules` under
`skillshub/.node`). Such a skill runs **in place** in MagicRun's exec-roots
jail (≥ `0.1.81`, `with_exec_roots`), from `apps/os_jail_in_place.rs`.

**Which skills.** A skill runs in place when it has its own `bin/<exe>` and its
contract needs it (a `provider` label, a config-file key, keys without declared
hosts), it names companions in `requires.bins` beyond the executable and
`python3`, it ships a `package.json`, or its entry point is neither native nor
an exact `python3` script (`#!/usr/bin/env node`, `#!/bin/bash`). Existing
kinds keep their skills and lock identities; a contract only in-place can carry
is never run as a copy.

**The package** is the directory holding the real `SKILL.md` (runtime installs
are often link farms with `config` linked into the data root); `bin/<exe>` must
resolve inside it.

**Roots** (derived from the package, never configured per skill):

- the package, canonical, minus `config/` (created `0700` at review when absent,
  since MagicRun excludes only existing directories);
- the shared `node_modules` beside the skills when the package has a
  `package.json` or a companion resolves into `node_modules/.bin`;
- the install prefix of each companion and `#!` interpreter, resolved on the
  governed runtime `PATH` (`skillshub/node_modules/.bin`,
  `skillshub/.venv/bin`, `skillshub/.node/bin`, then host `PATH`) and
  canonicalized:
  - a directory beside the skills (`skillshub/.node`, `skillshub/.venv`); a
    venv also brings the install its interpreter links into (e.g. a uv Python);
  - under home, only a recognised versioned-runtime directory
    (`~/.nvm/versions/node/X`, `~/.pyenv/versions/X`, `~/.asdf/installs/T/V`,
    uv's Python store `~/.local/share/uv/python` including its minor-version
    alias, `~/.local/share/uv/tools/X`, pipx venvs), else just the file's own
    directory (`~/.local/bin`) — never home or its direct children;
  - a Homebrew prefix (`/opt/homebrew`), a macOS framework version
    (`…/Python.framework/Versions/3.14`), or `<prefix>` of `<prefix>/bin/<name>`
    (`/usr/local`), each minus its `etc` and `var`.

`/usr/bin` and `/bin` are always readable. A missing companion adds nothing. A
runtime with any `SKILL.md` above it refuses the tool.

**Forbidden** (checked by Magician, then by MagicRun's `new_with_forbidden`,
which refuses a root equal to, containing or inside them): the Magician data
root and default `~/MagicianNotes`, `~/.ssh`, `~/.aws`, `~/.config`, `~/.gnupg`,
`~/.docker`, `~/.kube`, `~/.local/share`, `~/.local/state`, `~/.cargo`,
`~/.grok`, every other home dot-directory (`.local` excepted), `~/Library`,
`~/Documents`, `~/Desktop`, `~/Downloads`, and every other skill's directory;
MagicRun refuses `$HOME` and ancestors. The only exception is a recognised
versioned-runtime directory inside a fenced one (a uv Python under
`~/.local/share`), which is exactly the root. `skillshub/` itself is never a
root.

**Search `PATH`:** the skill's `bin`, `node_modules/.bin`, then each runtime's
`bin`, in resolution order.

**Trust checks are MagicRun's, unweakened.** Roots, ancestors and every `PATH`
entry must be owned by root or you and not group/other-writable; a failure makes
the tool unavailable naming the exact root/entry (e.g. Homebrew's
admin-group-writable `bin/`).

**Program.** `bin/<exe>` runs in place (file identity rechecked before spawn).
An exact `python3` script runs under the pinned interpreter as
`<python3> -s -B <script>` so packages beside it load; other `#!` scripts find
their interpreter on the jail `PATH`. Staged inputs go to `in/`
(`stage_input_file` returns `in/<name>`). The child gets
`PYTHONPYCACHEPREFIX=.magician-pycache` (inside the workdir) so no stale `.pyc`
from the skill's `__pycache__` is read. A contract's fixed
`requires.environment` is provided only after MagicRun's call-time manifest
validation, which refuses `DYLD_*`, `LD_*`, `NODE_OPTIONS`, `NODE_PATH`,
`PYTHONPATH`, `PYTHONHOME`, `PYTHONSTARTUP`, `BASH_ENV` and other loader/
interpreter injection names (review refuses skills declaring them); the jail
strips `DYLD_*`/`LD_*` again at launch.

**Lock identity.** `artifact:app-os-jail-in-place:v1:<fingerprint>:<identity>`,
digest = the program's BLAKE3. The identity binds executable, launch, pinned
interpreter line, package fingerprint, program digest and MagicRun's exec-roots
profile identity for both denied and brokered network (roots, exclusions,
`PATH`). Runtime roots are trusted as installed, not fingerprinted.

**Fingerprint and re-approval.** A digest over the package tree (relative path,
size, mode, content hash per file; link targets), excluding only top-level
`config/`, `__pycache__`, `.git`, `.DS_Store` (package-local `node_modules`,
`tests/`, `fixtures/` count). Bounded to 4096 files and 128 MiB; recomputed
cheaply at run time (cached by path, size, mode, mtime, ctime, inode, device). A
different fingerprint refuses with "the skill changed since the app was
approved; re-approve the app to use it" (`SkillChangedSinceApproval`), shown in
install review until re-approval; changed runtime roots or interpreters are
refused likewise. Nothing is copied or pinned.

**Budgets and catalog.** The same 7 KiB / 1 KiB stream budget and result
ceiling apply. The catalog marks the tool dispatchable when its contract is
admitted (a network-capable in-place skill counts as bound-HTTP ready); physical
review derives the roots, and a derivation failure stays non-dispatchable with
the precise reason in review.

## Trusted system tools (pure-data skills)

`SystemTool` lets a pure-data skill such as `jq` run the host's binary in place
(copied Apple platform binaries are killed on launch). The tool must be in
`/usr/bin` or `/bin`, a root-owned native executable not group/other-writable,
on trusted system storage — macOS: the sealed read-only system volume
(`MNT_RDONLY|MNT_ROOTFS`); Linux: every ancestor root-owned and not
group/other-writable. A symlinked `bin/` entry resolves to its target. The lock
pins its digest (`artifact:app-os-jail-system:v1:…`); a changed binary fails
reopen. Used only when the skill ships no `bin/` executable; everything else is
reviewed as for a private artifact.

## Staged file inputs

A read-only `workspace_path` input (e.g. jq's `input_file`) is not a path in an
app: `app_facing_input_schema` turns it into a content string or
`{name, text | base64}`, written into the jail's private directory as
`in-<param>[.ext]` via MagicRun `stage_input_file` (≥ `0.1.78`, quota-checked,
no-follow). The owner's files are never exposed; a `workspace_path` that creates
files or directories is refused. For agent use, jq's `input_file` is a
`workspace_path` with `access: read_file` under the scope's `workdirs`.

## Linux requirements

Every Linux jail needs bubblewrap and MagicRun's root-owned helper at
`/usr/libexec/magicrun/magicrun-jail-egress-forwarder`, which execs each jailed
command and applies the per-jail task ceiling (`RLIMIT_NPROC`) in the jail's
user namespace when exact. Without it every call is refused
(`JailHelperUnavailable`). The Docker image builds the helper from the pinned
MagicRun rev, installs it root-owned, and makes `/usr/bin/bwrap` setuid for
hosts without unprivileged user namespaces. macOS has no per-jail task limit;
the watchdog enforces the app task ceiling (1024) by counting threads.

On both platforms a jailed skill inherits only stdin/stdout/stderr: MagicRun
(≥ `0.1.79`) marks every other host descriptor close-on-exec at launch.

## Granting it (app owner)

Two grants, both per app:

- **Hosts.** The reviewed network policy lists `destination:<host>` entries;
  review suggests each tool's declared hosts. A call is admitted as an
  **external** tool call to every host its broker admits, so tool disclosure
  checks each against the run's effective network and data-handling policy, as
  for the in-process bound-HTTP tool.
- **Any public host.** The manifest data policy must set
  `external_egress: any_public_host` (above `approved_destinations`; gaining it
  is `data_handling: expanded`). Review offers the owner's tick only then, and
  only when a locked in-place tool declares no host. The tick
  (`granted_any_public_host`) is authority-bearing, off by default, in the
  authority digest only when set; gaining it is `any_public_host: expanded`.
  Effective only when
  `grant ∧ effective external_egress = any_public_host` (data-handling policy
  intersects grant, agent, trust and parent by `min`). The call attests
  `destination:any-public-host`, admitted by disclosure only under that
  authority. It never widens a tool that declares hosts, and a tool using a key
  reaches only that key's hosts.

Per call the broker admits (`AppOsJailEgressAdmission::for_call`):

- declared-host tool: the declared hosts the app is granted (or, with none
  granted, all declared hosts, so disclosure refuses), regardless of any-host;
- `*` tool: any public host under that grant, else nothing;
- in-place tool declaring none: the scope of its injected keys (picked hosts,
  or any public host for "any site" keys under the grant), else any public host
  under the grant, else every granted host;
- otherwise nothing.

The workflow resolves the live grant for every network-capable tool; the
admitted set (or any flag) is folded into the attested endpoint configuration.

## What the broker enforces (`apps/os_jail_egress.rs`)

- **Admitted hosts only.** Only `CONNECT <admitted host>:443` (under "any public
  host", any DNS name; never IP literals or local names). Anything else is
  `403`, so plain HTTP and absolute-form proxying never leave the machine.
- **Resolved by Magician**, never the child; every resolved address must be
  public (the bound-HTTP rule).
- **Caps.** 16 connections, 4 concurrent, 1 MiB up / 32 MiB down per call; head,
  resolve, connect and 30 s idle timeouts. Hitting a byte ceiling ends the
  tunnel.
- **Receipt.** The result carries an `egress` receipt: admitted host (or
  `multiple` / `any-public-host`), each contacted host with connections and
  bytes each way, tunnelled/refused/failed counts, totals, whether a ceiling was
  hit, first few refusal reasons. Never payload bytes.
- **Transport.** macOS: loopback TCP on an ephemeral port (sandbox admits IPv4
  TCP only). Linux: a unix socket in a private `0700` directory relayed by the
  in-jail forwarder. Other platforms: refused.
- **Local exposure.** On macOS any same-user process can find the broker port
  during a call; it gains nothing it could not do directly (same public host, no
  injected credentials) and can only spend that call's budget.

## App-run budgets and identity

- **Output budget.** An app result must fit the 56 KiB durable ceiling, so
  jailed skills run with declared stream limits clamped to 7 KiB stdout / 1 KiB
  stderr (tighter limits kept). A skill declaring no stream limit is refused. A
  larger answer fails as invalid output, never truncated. Agent use keeps the
  authored limits.
- **Diagnostics.** JSON-looking stdout must parse within depth/node bounds or
  the result is refused. Stderr is JSON only if it parses; otherwise text.
- **Workspace scope and working directory.** `app_jail_contract`
  (`apps/os_jail.rs`) is compiled, locked and projected by the catalog: a
  `workspace` working directory and resource scope are dropped (no
  `working_dir` in the model schema); the child runs in a fresh empty private
  directory; explicit `approval: ordinary` is accepted; any other scope, grant,
  approval class or resource authority refuses the skill. Skills that never
  declared these keep identical digests.
- **Plan digest.** For an egress skill it adds the brokered-egress profile,
  destination(s), port, broker limits and budgets. An in-place-only contract
  locks its own recipe (exec-roots profile, declared hosts or none, broker
  limits, budgets, exact auth contract). Skills without the block keep their
  previous recipe.
