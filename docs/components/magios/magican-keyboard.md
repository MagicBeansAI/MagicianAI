# Magican Keyboard (iOS custom keyboard extension)

`magios/MagiosKeyboard/` + shared logic in `magios/Shared/Keyboard*.swift`. A
full-featured iOS system keyboard that types like Apple's, with Magican's agentic
features (Rewrite / Ask / skills / Act) revealed **on demand** — never always-on.

## Interaction model — normal-first, AI on demand

The keyboard's north star: **it feels like a normal iOS keyboard**, and the AI is
summoned intentionally (parity of intent with OpenActi's "Acti Bar", not its
always-present prediction-replacement).

- **Typing / suggestions / autocorrect / prediction** — on-device, no network, works
  with Full Access **off**. Correction uses an in-house **SymSpell** (symmetric-delete)
  engine (`magios/Shared/Correction/`) over a bundled **82k English + 730 Indian-English
  + 673 Hinglish** frequency dictionary, ranked by frequency + keyboard adjacency
  (fat-finger model), with a confidence gate that never "corrects" a valid Indian name,
  Indian-English word, Hinglish token, or a word you've typed before; the one-tap
  "revert" chip undoes an autocorrection. The runtime SymSpell index is capped to the
  top **~30k** English words (all curated Indian/Hinglish + learned words are force-kept)
  and indexes long words at distance 1 only, keeping the extension **well under its
  ~60–70 MB memory budget** so iOS never jetsams it back to the system keyboard — with no
  change to corrections (the long-word cap is provably lossless). **Next-word prediction** fills the after-space
  slot (`magios/Shared/Prediction/`): Apple **Foundation Models** on capable devices
  (iOS 26.1+ / iPhone 15 Pro+), falling back to a universal bundled **n-gram +
  learn-your-bigrams** so every device gets predictions (the predictor falls back to the
  n-gram whenever Foundation Models yields nothing). Both correction and prediction learn
  from your own typing. No *agentic* AI is involved in the typing path.
- **The agentic surface is hidden by default.** It is revealed two ways:
  1. the **✦ Magican brand key** on the trailing edge of the top strip, and
  2. **holding the spacebar still** (~0.5s).
- **Spacebar** is branded "Magican ✦" and carries three movement-disambiguated intents:
  - plain tap → space;
  - **move** the finger → cursor **trackpad** (caret follows the horizontal drag);
  - **hold still** ~0.5s → summon the agentic surface.
  Movement wins over the hold, so dragging the caret never fires the AI, and holding
  still never scrubs the caret. (`KeyView.spaceGesture` in `KeyboardRootView.swift`.)
- **Revealed surface** (`AIActionRow`): explicit **Rewrite**, **Ask**, **Paste**
  (clipboard → personal context, shown only when the clipboard holds text), and the
  user's **skill chips** (Write / Ask / Act lanes). A ✕ closes it.
- **Canvas expansion** — when a Write preview, Ask answer, or Act verification card is
  open, the input view grows (`KeyboardModel.canvasExpanded` → `keyboardHeight()`),
  giving the canvas real room instead of cramping it over the keys.
- **Theme** — the keys, pills, and panels adopt the **Magican app's active theme**
  (Longhand, Jarvis, Mario 8-bit, …), not a generic system look. The app publishes
  its palette to the App Group on every theme change (`Shared/KeyboardPalette.swift`,
  written by `ThemeManager`); `KeyboardTheme` sources every color from it (backdrop =
  background, keys = elevated/surface, text, ✦ accent = the theme accent). The
  **day/night variant follows the SYSTEM appearance** (iOS light/dark), independent of the
  app's own mode, and the first paint is **seeded from the last appearance the keyboard
  observed in-window** (persisted in the App Group as `KeyboardThemeStore.lastKnownDark`)
  so it opens straight into the right variant instead of flashing light→dark — a keyboard
  extension's `traitCollection` isn't reliable that early in `viewDidLoad`. Falls back to
  the app default (Longhand) until the app has been opened once.

### The three agentic lanes

- **Write** (M2) — contextual rewrite/reply/continue of the keyhole via
  `POST /contextual-writing/actions` (see
  [contextual-writing](../magician/contextual-writing.md)). A generating→preview→undo
  flow with "Try another" (regenerate) and "Try again" on error/timeout. A generic
  staged label ("Reading… → Magican is writing… → Polishing…") runs on a timer during the
  wait; the request carries a client `chatTurnId` so the real turn-events tail can
  replace it later.
- **Ask** (M3) — a streamed answer in the canvas (`KeyboardAskClient`, chat SSE).
- **Act** (M4) — verification-card-gated task execution (`KeyboardActClient` →
  `/executions`). Nothing runs without the Confirm card; on start it stashes a pending
  task so opening Magican surfaces it (the human-in-the-loop handoff).

**Each lane arrives on a DIFFERENT invocation surface** — Write →
`contextual_assist`, Ask → `chat`, Act → `task` — all served by the workspace's
**primary agent** (`get_primary_agent()`, falling back to `personal-assistant`).
Act carries no `agent_id` and no `env_mode`, so it runs with that agent's full
resolved tool set (its allowlist plus matching AgentSkills procedure skills,
plus `task_state` because the run is task-backed).

Two consequences worth knowing before debugging a dead lane:

- If the primary agent ever sets a non-empty `allowed_direct_surfaces`, it must
  list `chat`, `task`, AND `contextual_assist`. That field is an exact
  allowlist, and the three lanes fail independently — one missing entry looks
  like a single broken keyboard button, not a policy change. See
  [agent-definition-reference](../magician/agents/agent-definition-reference.md).
- Skill chips are read ONCE per keyboard launch (`KeyboardModel.skills` is a
  `let`), so edits made in the app do not appear until the keyboard is
  relaunched.

## Secure fields

In secure / OTP / password / number-pad fields the keyboard suppresses **all** agentic
surfaces and captures **no** typed content (the shadow keyhole stays empty). Detected
from the field traits in `KeyboardViewController.isSensitiveField()`.

## App Store Review 4.4.1 (keyboard extensions) compliance

Custom keyboards get extra scrutiny. This keyboard is built to satisfy 4.4.1:

- **Fully functional without network / without Full Access.** All typing,
  suggestions, autocorrect, layers, trackpad, and diacritic callouts work with
  Full Access OFF and offline. Only the agentic lanes (Write/Ask/Act, paste,
  haptics) require Full Access, and the surface clearly says so when it's off.
- **Provides number & decimal input** (number/URL/email content modes) — 4.4.1
  requires the keyboard to enter numbers.
- **A next-keyboard (globe) switcher** is always present when the system needs it
  (`needsInputModeSwitchKey`).
- **`RequestsOpenAccess`** is only used to enable the network-backed agentic
  features; the primary purpose (typing) does not depend on it.
- **No data collection without Full Access**; with Full Access, only the explicit,
  user-initiated action's text leaves the device (to the user's own Magican backend).
- **The clipboard is read only on an explicit "Paste" tap** — never ambiently.
- **PrivacyInfo.xcprivacy** declares no tracking and only app-functionality use of
  user content + `UserDefaults` (App Group). See `MagiosKeyboard/PrivacyInfo.xcprivacy`.
- **Not a keylogger.** Typed content is held only in an in-memory shadow buffer for
  the Write keyhole and is never persisted or transmitted except as the explicit
  action payload.

## Privacy policy copy (keyboard section)

> **Magican Keyboard.** The Magican keyboard works fully offline for typing, suggestions,
> and autocorrection, and does not require Full Access for that. When you explicitly
> invoke an AI action (Rewrite, Ask, a skill, or a task), and only then, the text you
> selected for that action is sent over an encrypted connection to your own Magican
> assistant to produce the result. The keyboard does not log your keystrokes, does not
> read your clipboard unless you tap "Paste", and does not track you or share data with
> third parties. Turning on Full Access enables the AI actions, haptic feedback, and the
> one-tap paste; with Full Access off, the keyboard is a normal keyboard.

## Install & guided setup

The Magican app guides enabling the keyboard (Settings → General → Keyboard → Keyboards →
Add New → Magican, then Allow Full Access) with a live status row that reflects the
installed / Full-Access state via `KeyboardInstall` (App Group). See
`Magios/KeyboardSettingsView.swift`, and the interactive in-keyboard tutorial
(`KeyboardCoach` + `KeyboardPlaygroundView`).
