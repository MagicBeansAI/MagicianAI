# Magican Keyboard (iOS custom keyboard extension)

`magios/MagiosKeyboard/` + shared logic in `magios/Shared/Keyboard*.swift`. A
full iOS system keyboard that types like Apple's, with Magican's agentic
features (Rewrite / Ask / skills / Act) revealed **on demand**, never
always-on.

## Interaction model — normal-first, AI on demand

The keyboard should **feel like a normal iOS keyboard**; the AI is summoned on
purpose.

- **Typing, suggestions, autocorrect, prediction** run on-device with no
  network and work with Full Access **off**.
  - Correction is an in-house **SymSpell** (symmetric-delete) engine
    (`magios/Shared/Correction/`) over a bundled **82k English + 730
    Indian-English + 673 Hinglish** frequency dictionary, ranked by frequency
    plus keyboard adjacency. A confidence gate never "corrects" a valid Indian
    name, Indian-English word, Hinglish token, or a word you typed before; a
    one-tap "revert" chip undoes an autocorrection.
  - The runtime index keeps the top **~30k** English words (all curated
    Indian/Hinglish and learned words are kept) and indexes long words at
    distance 1 only, so the extension stays **well under its ~60–70 MB memory
    budget** and iOS never jetsams it. The long-word cap is lossless.
  - **Next-word prediction** fills the after-space slot
    (`magios/Shared/Prediction/`): Apple **Foundation Models** on capable
    devices (iOS 26.1+ / iPhone 15 Pro+), else (or when it yields nothing) a
    bundled **n-gram + learn-your-bigrams** model.
  - Correction and prediction learn from your typing. No agentic AI is in the
    typing path.
- **The agentic surface is hidden by default**, revealed by the **✦ Magican
  brand key** at the trailing edge of the top strip, or by **holding the
  spacebar still** (0.45 s).
- **Spacebar** ("Magican ✦"): tap → space; **move** → cursor **trackpad**;
  **hold still** 0.45 s → agentic surface. Movement wins over the hold
  (`KeyView.spaceGesture` in `KeyboardRootView.swift`).
- **Revealed surface** (`AIActionRow`): **Rewrite**, **Ask**, **Paste**
  (clipboard → personal context, shown only when the clipboard holds text), and
  the user's **skill chips** (Write / Ask / Act lanes). ✕ closes it.
- **Canvas expansion**: an open Write preview, Ask answer or Act card grows the
  input view (`KeyboardModel.canvasExpanded` → `keyboardHeight()`).
- **Theme**: keys, pills and panels follow the **Magican app's active theme**
  (Longhand, Jarvis, Mario 8-bit, …). `ThemeManager` publishes the palette to
  the App Group on every change (`Shared/KeyboardPalette.swift`), and
  `KeyboardTheme` reads every color from it. The **day/night variant follows
  the system appearance**, not the app's mode; the first paint uses the last
  appearance seen in-window (`KeyboardThemeStore.lastKnownDark` in the App
  Group), because `traitCollection` is unreliable in `viewDidLoad`. Until the
  app is opened once, the app default (Longhand) applies.

### The three agentic lanes

- **Write**: contextual rewrite/reply/continue of the keyhole via
  `POST /contextual-writing/actions` (see
  [contextual-writing](../magician/contextual-writing.md)). Generating →
  preview → undo, with "Try another" (regenerate) and "Try again" on error or
  timeout. A timed staged label ("Reading… → Magican is writing… →
  Polishing…") covers the wait; the request carries a client `chatTurnId` so
  real turn events can replace it.
- **Ask**: a streamed answer in the canvas (`KeyboardAskClient`, chat SSE).
- **Act**: task execution gated by a verification card (`KeyboardActClient` →
  `/executions`). Nothing runs without Confirm; on start it stashes a pending
  task so opening Magican shows it.

Each lane uses a **different invocation surface** (Write →
`contextual_assist`, Ask → `chat`, Act → `task`) and agent:

- Write sends `agentId: "writing-assistant"` (`ContextualAssistRequest.make`
  in `magios/Shared/ContextualAssist.swift`).
- Ask opens one `POST /chat/new` session with an empty body, so the server
  picks the agent (`preferred_agent_id_for_thread`, default
  `personal-assistant`).
- Act posts `/executions` with no `agent_id`, so the orchestrator takes the
  tool filters of the primary enabled agent
  (`get_primary_enabled_agent()` in `v2_orchestrator.rs`; no primary means no
  filters).

Before debugging a dead lane:

- If the serving agent sets a non-empty `allowed_direct_surfaces`, it must list
  that lane's surface. The field is an exact allowlist, and a missing entry
  breaks one button only. See
  [agent-definition-reference](../magician/agents/agent-definition-reference.md).
- Skill chips are read once per keyboard launch (`KeyboardModel.skills` is a
  `let`); app edits appear after a relaunch.

## Secure fields

In secure / OTP / password / number-pad fields the keyboard suppresses **all** agentic
surfaces and captures **no** typed content (the shadow keyhole stays empty). Detected
from the field traits in `KeyboardViewController.isSensitiveField()`.

## App Store Review 4.4.1 (keyboard extensions) compliance

- **Works without network or Full Access.** Typing, suggestions, autocorrect,
  layers, trackpad and diacritic callouts work offline with Full Access off.
  Only the agentic lanes (Write/Ask/Act, paste, haptics) need Full Access, and
  the surface says so when it is off.
- **Number and decimal input** (number/URL/email content modes), as 4.4.1
  requires.
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
