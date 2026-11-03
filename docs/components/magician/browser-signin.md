# Browser sign-in across devices

Settings → Service accounts lists supported installed skills in the active
workspace (Settings → Services on Android and iOS). MCP sign-in requests also
offer **Finish sign-in here** and **Open on computer** in Attention and the
shared composer/task question controls. A client-registration failure returns
a secret-free error, shown by web, Android and iOS, before any browser or
pending card exists. Status reads never start sign-in, and opening a page does
not resolve the request; only a verified provider callback does. Reopening an
account reuses its pending OAuth flow instead of starting a competing exchange.

Each `POST /auth/browser-signin/accounts` row is `{name, label, kind, status,
detail, endpoint_alias?}`; `kind` is `governed_oauth`, `browser_cli` or
`managed_bot`. `status` is `connected` (login active, or expired with a
refresh grant the runtime will use), `needs_sign_in`, `authenticating`,
`unknown` or (managed bots) `unavailable`; `detail` is a short secret-free
reason. Checks run concurrently, three seconds each; a slow or failed check
reads `unknown`. A multi-endpoint governed MCP skill (Swiggy
food/instamart/dineout) lists one row per declared endpoint alias, since each
alias is its own credential binding; `/launch` accepts that optional
`endpoint_alias`, validated against the manifest (omitted = the default
alias). Connect and governed runs build the binding with the same helper
(`magician_v2::mcp_oauth_binding`), so a run reads exactly the login Connect
saved; a `${NAME}` endpoint or issuer resolves from the scope's own skill
`config/.env` on both sides (`skills::scope_env`), and a scope that has not set
it gets no row. Clients label connected rows **Reconnect** and re-check on
focus/resume. Callback completion is reconciled after the pending card is
published; logout and definitive rejection clear stale connection receipts.

## Managed-bot accounts

Google Workspace (one row per `gmail-{profile}` bot), Telegram
(`telegram-self`) and WhatsApp (`whatsapp`) rows appear when a skill bound to
that `managed_bot` setup is installed in the scope and the bot is in the scope's
bot config. Labels are `Google Workspace (work)`, `Telegram`, `WhatsApp`. Rows
add `bot`, `profile` (Google) and `methods` in preference order: `login_url`
(Google), `code` then `qr` (Telegram), `pairing_code` then `qr` (WhatsApp). A
row reads `unavailable`, naming the missing piece in `detail`, when `gws`, the
Google OAuth Desktop client, the profile's Desktop setup, `tgcli`,
`TGCLI_API_ID`/`TGCLI_API_HASH` or the WhatsApp bot bundle is missing. Bot auth
`ok` maps to `connected`; `needs_auth`/`account_mismatch` to `needs_sign_in`
(the mismatch detail names no address); an active or queued login to
`authenticating`; an error to `unknown`.

`POST /auth/browser-signin/bot/{start,input,state,cancel}` drive the
supervisor logins (`BotManager::start_auth_with`). They take an owner session
or verified phone, answer `Cache-Control: no-store`, refuse unknown body
fields, accept only bots that are rows of the scope, and fail with
`{error, message}` (clients show `message`). Bodies: `start {bot, method,
phone?, open_on_computer?}`, `input {bot, step, value}` (`step`: `phone`,
`code`, `password`; phone `+`/digits ≤ 20, code ≤ 16, password ≤ 4096 bytes),
`state {bot}`, `cancel {bot}`. Each returns `BotConnectState {bot, status,
detail, pending}`; `pending` is `null` or `{kind, label, pairing_code,
qr_data_url, sign_in}` with `kind` `phone`, `code`, `password`,
`pairing_code`, `login_url` or `qr`. `qr_data_url` is always
`data:image/png;base64,…` (Telegram's `tg://` link is rendered to PNG). Clients
poll `state` every 2 s while `authenticating`.

A Google row also carries `project_access: true`: the account needs Service
Usage Consumer and Pub/Sub Editor on the Cloud project behind its OAuth client
(the bot env's `GOOGLE_WORKSPACE_PROJECT_ID`, else the profile's
`client_secret.json`), unless it holds Owner or Editor. `POST
/auth/browser-signin/bot/project-access {bot, fresh?}` reads the project policy
with the host's `gcloud` (`magician_components::google_project_access`) and
returns `{bot, project_id, email, status, missing_roles, detail, grant_account,
commands, console_url, can_grant}`, `status` being `granted`, `missing` or
`unknown` (policy unreadable). Definite answers are cached for 10 minutes;
`fresh: true` rechecks. `POST .../bot/project-access/grant {bot}` adds the
missing roles with the host's gcloud account when it holds Owner, Project IAM
Admin or Security Admin, and returns the rechecked report; otherwise it fails
with `project_access_grant_failed` and clients show `commands`. Setup scripts
and the terminal wizard run the same check through `magician.bin
google-project-access` ([setup](../../setup/google-workspace.md#6-roles-for-every-other-account)).

- **Google**: `start` runs `gws auth login` without opening the host browser.
  The supervisor keeps the scraped `accounts.google.com` URL out of the bot's
  logs; the API turns it into a computer-mode viewer session
  (`sign_in = {mode: "computer", ticket, viewer_path, expires_in}`, issued
  once; a repeated `start` reissues a ticket for the same login), which ends
  connected when the login settles with a working account.
  `open_on_computer: true` opens the URL in the host's own browser instead
  (`sign_in = {mode: "computer_opened"}`). The private browser presents the
  engine's Chrome user agent without the `HeadlessChrome` token.
- **Telegram**: for both methods the engine seeds tgcli's store `config.json`
  (`0600`, where tgcli reads it: `TGCLI_STORE` or the app-data dir under the
  scope HOME) with the bot env's API id/hash and the requested phone, so tgcli
  never stalls on its own API-key prompt. `code` runs `tgcli auth` without
  `--qr` and maps its stdin prompts to `pending.kind` (`phone`, `code`, then
  `password` with 2FA); answers go only to the login's stdin, and a missing
  phone surfaces as `phone`.
- **WhatsApp** (`pairing_code`): the phone is handed to the next bot spawn as
  `WHATSAPP_PHONE` and dropped; the bot requests one code per login and writes
  it to `WHATSAPP_PAIRING_CODE_FILE` beside the QR. Its QR or code counts as an
  in-progress sign-in for five minutes after a `start`. An already linked bot
  is not restarted.
- `cancel` dequeues or kills the login and revokes its viewer sessions.

## Callbacks and the private viewer

Tools declare `metadata.magician.browser_signin.callback_mode` in their own
`SKILL.md`: `loopback` (`127.0.0.1`, the default when undeclared),
`loopback_localhost`, or `public_when_available` (the engine's reviewed public
origin, else loopback); e.g. Zepto localhost, Swiggy loopback, Metabase
public-when-available. The engine owns the port, scoped path, state and PKCE;
arbitrary callback URLs and unknown declaration fields are rejected. Setup,
chat and apps read the same typed declaration; there is no server-config
override or built-in provider map. In-flight flows keep their original
callback.

A browser-CLI login may hand its link through a local HTML page instead of
printing it (Higgsfield writes `sign-in.html` and prints a `file://` path);
the engine reads only a regular file inside that login's private scratch
`TMPDIR` and uses only an allowed-host link from it. On the Magician computer
itself (a loopback page or the desktop app) the web client shows one sign-in
action that opens the computer's own browser; elsewhere it offers both
“Connect on this device” and “Open on computer”.

Direct sign-in requires public HTTPS authorization and callback addresses.
Loopback, private IPs and recognized local hostnames stay on the engine, even
with a public callback, where the user controls a private Chromium instance:
ephemeral profile, no extensions, downloads disabled, headless rendering, not
attached to Magicutor or agent recording. Private viewers require HTTPS or a
loopback connection, so passwords never cross plain-HTTP LAN connections. Only
rendered page images and bounded tap, scroll, text and key inputs pass through
the owner viewer. The backend owns the callback and exchanges tokens through
the existing SDK.

The private viewer needs Chrome/Chromium on the engine; nonstandard installs set
an absolute `browser_signin.browser_executable`. The browser sandbox stays on.
Provider limits on headless browsers, system dialogs, hardware keys and
computer biometrics still apply; **Open on computer** keeps the normal browser
route. The viewer is not a general remote desktop.

`browser_cli` setup definitions declare executable, fixed argv, permitted login
hosts and a relative credential file. Higgsfield uses this driver: its CLI owns
the localhost callback and token exchange in a scope-isolated HOME, and the
account is confirmed only after a successful CLI exit and a saved credential
file. The CLI keeps its own credential format; MCP tokens use Magician's
encrypted vault. Ordinary agent calls still cannot run Higgsfield `auth`
commands. Native setup still returns `authorization_url` for older Desktop
clients; newer clients open the fixed viewer path against their verified engine
connection.

## Boundary

- `/auth/browser-signin/accounts` and `/launch` require an owner session or a
  verified owner phone. Clients cannot supply URLs, executable paths or argv.
- A launch ticket expires after 60 seconds and is consumed once; claiming it
  rotates it into a separate viewer capability. The original ticket can still
  cancel its own viewer (also during startup) but cannot view or control it.
  Neither credential enters HITL events, model context, Web Storage or ordinary
  tool history; the web viewer removes the one-use launch fragment from browser
  history on opening.
- `/claim`, `/frame`, `/input` and `/close` authenticate that capability, not
  caller scope headers. A new controller revokes the old one for the same
  scoped connection. Input binds to the displayed document revision; a
  duplicate sequence number must name the same operation and is not replayed.
  An uncertain input response closes the viewer rather than risk repeating a
  consent submission; a known document change can refresh safely. Frame
  throttling asks clients to wait without ending the sign-in.
- At most four sessions; control expires after ten minutes or 90 seconds idle.
  Completion, closure and expiry stop the private browser. Abandoned CLI login
  processes are cancelled and reaped; manually opened logins keep their
  ten-minute deadline. Unix cleanup includes child process groups, so npm
  wrappers cannot leave callback listeners behind. Only owned OS-temporary
  profiles are removed.
- Revocation cancels browser startup and in-flight control operations. OAuth
  dismissal, settlement and expiry cancel its viewers. Closing an MCP viewer
  leaves its request open to reopen; closing a CLI viewer cancels its login
  process. Delivered input cannot be rolled back.
- A CLI login reservation is held until process cleanup completes; aliases of
  the same CLI share it, and completion revokes viewer control before another
  login can reuse the callback listener. Saved credentials must be private
  regular files (symlinks and hard links refused on Unix); readiness queries
  never create login directories.
- The web viewer has a restrictive CSP. Native clients keep their paired
  connection headers outside the rendered page and refuse connection changes.
  Sign-in API transports refuse redirects so credentials and input cannot be
  forwarded to another origin; provider navigation stays inside the browser.

Implementation: `magician-api/src/browser_signin/`, `magician_v2::mcp_oauth`,
`magician-components/src/setup_catalog.yaml`, and the shared/native HITL controls.
