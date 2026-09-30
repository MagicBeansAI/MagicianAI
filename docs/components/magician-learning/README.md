# Magician Learning

Production dependencies keep `magician/test-fixtures` disabled. This crate's
explicit `test-fixtures` feature forwards it, and development dependencies enable
it for tests, so extracting a test helper does not pull the monolith's fixture
modules into every debug runtime build.

The learning plane as a satellite crate (~21k lines):

* `outcome_learning` — OPC outcome learning, all five phases: recording,
  the maturity policy, sweep, feeders, aggregates.
* `data_room` — the OPC deal-close container and its audit log
  (access/disclosure/custody stores).
* `bots` — the scoped chat-bot runtime, bot config store, and the auth
  HITL broker.
* `execution_panel` — the projector, runtime store, and v3 adapter.

## Bot spawning stays on `posix_spawn`

`ManagedBot` builds its child command in a pure step (`build_bot_command`) and
spawns it separately. The bot's program — often a bare interpreter name — is
resolved to an absolute path with `runtime_core::process::resolve_program`
against the same scoped PATH the child receives, because a bare name plus a
`PATH` override makes Rust `fork` instead of `posix_spawn`, and a forked copy
of the multithreaded server can hang in macOS atfork handlers before exec. A per-bot `PATH` in `config.env` is
the last env layer and wins, so when one is set the program is resolved
against it rather than the scoped augmentation it overrides. The `gws auth
login`/`auth status` probes resolve the same way. Unit tests pin the contract
without starting a process.

## Managed account setup

The scoped bot supervisor owns interactive account state used by Desktop setup.
WhatsApp writes a private PNG QR plus a connected marker; Telegram runs
`tgcli auth --qr`, captures its login URL into a private scope artifact, accepts
optional 2FA over a bounded stdin seam, and verifies `tgcli auth status` before
reporting ready. Google Workspace profiles reuse the existing managed `gws`
status/login and expected-identity checks. Before a Google login, the supervisor
copies the validated runtime-root OAuth client into that scope's profile with
mode 0600. Auth subprocesses are serialized, timed out, drained, and reaped;
successful logins restart a bot that was running before setup.

## Lib-side seam

The panel's serde state vocabulary stays lib-side:
`magician_v2::execution_panel` retains `types.rs` only (361 lines), because
`realtime_events` embeds `ExecutionPanelState` in runtime events. The crate
re-exports those types from `magician_learning::execution_panel` alongside
the engines. `outcome_learning`, `data_room`, and `bots` had zero lib
consumers and moved wholesale.

Consumers: `magician-api` (bot_api, execution_panel_api, data_room_reader_api,
websocket_handler, web_api) and `magician-bin` depend on `magician-learning`.

## Delegated work in the panel feed

`run.activity_log` carries the selected execution's events **and those of its
delegated children**, with each entry attributed to the agent that produced it
rather than to the run on screen. `run.delegations` describes one group per
child (`execution_id`, `agent_id`, the child's own `status`, `entry_count`), so
a client folds each delegation into a single collapsible block; group entries by
`metadata.execution_id`, never by `agent_id`, because one agent can be delegated
to more than once in a run.

Children come from `collect_related_execution_ids`, a recursive walk of
the execution tree (`child_execution_ids` plus `active_child_execution_ids`),
so nested, repeated and already-finished delegations are covered.

Both halves are required: filtering to `selected_execution_id` alone drops every
child event, and a single scalar `agent_id` for the whole log would attribute
child rows to the parent.
