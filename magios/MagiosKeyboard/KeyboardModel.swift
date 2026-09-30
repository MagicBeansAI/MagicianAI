import Combine
import UIKit

/// The Write-lane state machine (M2).
enum KeyboardWriteState: Equatable {
    case idle
    case charging
    case generating
    case preview(draft: String, plan: KeyboardEditPlan)
    case undo(insertedDraft: String, restore: String)
    case error(String)
}

/// The Ask-lane state machine (M3): a streamed answer in the canvas.
enum KeyboardAskState: Equatable {
    case idle
    case streaming(answer: String, question: String)
    case done(answer: String, question: String)
    case error(String)
}

/// The Act-lane state machine (M4): verification-card-gated task execution.
enum KeyboardActState: Equatable {
    case idle
    case confirm(skill: KeyboardSkill, goal: String)
    case working(label: String)
    case done(taskId: String, label: String)
    case error(String)
}

/// Bridges the SwiftUI keyboard to the host text field via `UITextDocumentProxy`,
/// and holds the shift/layer state. The pure transition logic lives in
/// `KeyboardInputState` (Shared); this is the side-effecting adapter.
final class KeyboardModel: ObservableObject {
    @Published var input = KeyboardInputState()
    /// Up to three local suggestions for the in-progress word (completions);
    /// no network, works with Full Access OFF.
    @Published var suggestions: [String] = []
    /// When an auto-correction just fired, the original word — shown as a one-tap
    /// "revert" chip until the next keystroke.
    @Published var revertOriginal: String?
    /// The Write lane (agentic rewrite of what you typed).
    @Published var write: KeyboardWriteState = .idle
    /// A generic lifecycle hint shown while the Write lane is `.generating`
    /// (advances on a timer — not fabricated backend events; see K0 doc for the
    /// real turn-events tail that would replace this once session pre-resolution
    /// lands).
    @Published var writeStage: String = ""
    /// The Ask lane (streamed answer in the canvas).
    @Published var ask: KeyboardAskState = .idle
    /// The Act lane (verification-card-gated task execution).
    @Published var act: KeyboardActState = .idle
    /// The agentic surface is hidden by default — the keyboard types like a normal
    /// iOS keyboard. It's revealed on demand via the ✦ Magican brand key on the strip
    /// or a stationary space long-press. This keeps AI intentional, not always-on.
    @Published var aiSurfaceRevealed = false
    /// Bumped when the system appearance flips (day↔night) so SwiftUI re-renders the
    /// keyboard with the freshly-picked theme variant.
    @Published var appearanceTick = 0
    func bumpAppearance() { appearanceTick &+= 1 }
    /// The user's skill chips (App Group; seeded with the default pack).
    let skills = KeyboardSkillStore.load()
    /// Live onboarding step (nil unless the in-app playground armed the coach).
    @Published var coachStep: KeyboardCoachStep?

    weak var proxy: UITextDocumentProxy?
    var needsGlobe = true
    var hasFullAccess = false
    var lexicon: UILexicon?
    /// Secure/OTP/number field — the agentic surfaces are hidden and NO typed
    /// content is captured (the shadow buffer stays empty). Set by the controller
    /// from the field's traits.
    @Published var isSensitive = false

    var advanceInputMode: () -> Void = {}
    var dismissKeyboard: () -> Void = {}

    /// What the keyboard has typed into this field — the Write lane's keyhole.
    private var shadow = KeyboardShadowBuffer()
    /// The last auto-correction, for one-tap revert.
    private var pendingRevert: (original: String, corrected: String, boundary: String)?
    /// The last Write run's turn/session ids (echoed by the backend) — kept for
    /// correlation and future turn-events tailing.
    private var lastWriteTurnID: String?
    private var lastWriteSessionID: String?
    /// The last Write run's action + guidance, so "Try another" / "Try again" can
    /// re-run the exact same request for a fresh variant.
    private var lastWriteAction: ContextualAssistAction = .rewrite
    private var lastWriteGuidance: String?
    private var stageTicker: Task<Void, Never>?
    private let suggestionEngine = SuggestionEngine()
    /// Debounces the (expensive, `UITextChecker`-backed) per-keystroke suggestion
    /// recompute OFF the keystroke thread — cancelled + rescheduled on each keypress
    /// so the character insert stays instant and the spell-check never blocks it.
    private var suggestionWork: DispatchWorkItem?
    /// Phase-2a: debounces + cancels Foundation-Models next-word predictions so the
    /// after-space slot updates without ever touching the keypress thread. Built
    /// lazily the first time a predictor is available (nil on unsupported OS/devices,
    /// where the after-space slot stays empty — today's behaviour). Never fights the
    /// correction path: it's only consulted when there is NO in-progress word.
    private var debouncedPredictor: DebouncedPredictor?
    /// Monotonic token so a late prediction completion for a stale cursor is dropped.
    private var predictionGeneration = 0
    private let assistClient = ContextualAssistClient()
    private let askClient = KeyboardAskClient()
    private let actClient = KeyboardActClient()
    // iOS keyboard extensions cannot read the host app's bundle id (sandbox
    // privacy boundary), so — unlike the macOS Writing Help which keys a session
    // per host app — the keyboard uses one stable session + a distinct source
    // identity (kept separate from the Share-sheet Action extension's sessions).
    private static let sessionKey = "contextual-writing:app:ios-keyboard"
    private static let sourceKey = "app:ios-keyboard"

    init() {
        // Load the UITextChecker language model off the hot path so the first
        // correction / completion after launch isn't laggy.
        suggestionEngine.prewarm()
    }

    var hasShadowIntent: Bool { !shadow.isEmpty }

    /// The text the agentic lanes operate on: what the keyboard typed this
    /// session, or — if that's empty — the field's existing text before the cursor
    /// (so Write/Ask/skills work on pasted or pre-existing content, like OpenActi).
    private var effectiveKeyhole: String {
        if !shadow.isEmpty { return shadow.text }
        return proxy?.documentContextBeforeInput ?? ""
    }

    /// True when there's anything to act on (typed OR in the field). Drives pill
    /// visibility beyond just freshly-typed text.
    var hasContext: Bool {
        !effectiveKeyhole.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// True when the clipboard holds text we can paste in as personal context.
    /// `hasStrings` is a non-consuming check (no paste banner); the actual read in
    /// `pasteClipboard()` is the one explicit, user-tapped paste. Gated on Full
    /// Access (no pasteboard access otherwise) and never in a secure field.
    var canPaste: Bool {
        hasFullAccess && !isSensitive && UIPasteboard.general.hasStrings
    }

    // MARK: agentic surface reveal (on-demand)

    /// Reveal the agentic action surface (Rewrite / Ask / skills / paste). No-op in
    /// a secure field.
    func revealAISurface() {
        guard !isSensitive else { return }
        aiSurfaceRevealed = true
        advanceCoach(to: .tapToAsk)
    }

    func hideAISurface() { aiSurfaceRevealed = false }

    func toggleAISurface() {
        if aiSurfaceRevealed { aiSurfaceRevealed = false } else { revealAISurface() }
    }

    /// Paste the clipboard's text at the caret (personal context in one tap). The
    /// only pasteboard read is this explicit, user-tapped action.
    func pasteClipboard() {
        guard canPaste, let text = UIPasteboard.general.string, !text.isEmpty else { return }
        proxy?.insertText(text)
        if !isSensitive { shadow.insert(text) }
        input.didTypeCharacter()
        refresh()
    }

    /// Adapts the letters bottom row for email / URL fields.
    @Published var contentMode: KeyboardContentMode = .normal

    var rows: [[KeyCap]] {
        KeyboardLayoutModel.rows(layer: input.layer, shift: input.shift, needsGlobe: needsGlobe, contentMode: contentMode)
    }

    // MARK: field-type adaptation

    /// Set the initial layer + content mode from the field's keyboard type
    /// (number fields open on the number layer; email/URL add @ · . · /).
    func configure(for type: UIKeyboardType) {
        updateContentMode(for: type)
        if Self.isNumberType(type), input.layer == .letters {
            input.switchLayer(.numbers)
        }
    }

    /// Lighter update on field/selection change — content mode only (don't fight a
    /// user who switched layers).
    func updateContentMode(for type: UIKeyboardType) {
        switch type {
        case .emailAddress: contentMode = .email
        case .URL, .webSearch: contentMode = .url
        default: contentMode = .normal
        }
    }

    private static func isNumberType(_ t: UIKeyboardType) -> Bool {
        [.numberPad, .phonePad, .decimalPad, .asciiCapableNumberPad].contains(t)
    }

    // MARK: key handling

    func press(_ key: KeyCap) {
        // In a secure/OTP field we never capture typed content.
        let capture = !isSensitive
        switch key.kind {
        case .character(let c):
            if Self.isBoundary(c) {
                maybeAutocorrect(boundary: c)
            } else {
                clearRevert()
            }
            proxy?.insertText(c)
            if capture { shadow.insert(c) }
            input.didTypeCharacter()
            refresh()
        case .space:
            maybeAutocorrect(boundary: " ")
            proxy?.insertText(" ")
            if capture { shadow.insert(" ") }
            refresh()
        case .ret:
            maybeAutocorrect(boundary: "\n")
            proxy?.insertText("\n")
            if capture { shadow.insert("\n") }
            refresh()
        case .delete:
            proxy?.deleteBackward()
            if capture { shadow.deleteBackward() }
            refresh()
        case .shift:
            input.tapShift()
        case .layer(let target):
            input.switchLayer(target)
        case .globe:
            advanceInputMode()
        case .dismiss:
            dismissKeyboard()
        }
    }

    func doubleTapShift() {
        input.engageCapsLock()
    }

    /// Monotonic timestamp (seconds) of the last Shift tap, for manual double-tap
    /// detection. Starts far in the past so the first tap is never a "double".
    private var lastShiftTapAt: TimeInterval = -.greatestFiniteMagnitude

    /// Handle a Shift tap with ZERO disambiguation lag: toggle immediately, and
    /// upgrade to caps-lock only when a second tap lands within the double-tap
    /// window (see `KeyboardInputState.shiftTapEngagesCapsLock`). A dedicated
    /// `TapGesture(count: 2)` for caps-lock instead forced the single tap to wait
    /// ~0.35–0.5s for the double-tap to fail — the Shift lag.
    func shiftTapped(now: TimeInterval = ProcessInfo.processInfo.systemUptime) {
        if KeyboardInputState.shiftTapEngagesCapsLock(now: now, lastTapAt: lastShiftTapAt) {
            input.engageCapsLock()
        } else {
            input.tapShift()
        }
        lastShiftTapAt = now
    }

    // MARK: space-drag trackpad

    /// Move the caret by a signed character offset (space-bar trackpad).
    func moveCursor(by offset: Int) {
        guard offset != 0 else { return }
        proxy?.adjustTextPosition(byCharacterOffset: offset)
    }

    /// The trackpad drag ended: the caret moved, so the typed-intent context is
    /// broken — reset the shadow (Write/Ask fall back to the field text) and
    /// recompute suggestions/autocap for the new position.
    func endTrackpad() {
        shadow.reset()
        refresh()
    }

    /// Insert an alternate glyph chosen from a long-press callout.
    func insertCallout(_ glyph: String) {
        proxy?.insertText(glyph)
        input.didTypeCharacter()
        refresh()
    }

    /// Insert an emoji from the emoji layer (stays on the emoji layer for rapid entry).
    func insertEmoji(_ emoji: String) {
        proxy?.insertText(emoji)
        if !isSensitive { shadow.insert(emoji) }
        refresh()
    }

    func deleteBackward() {
        clearRevert()
        proxy?.deleteBackward()
        if !isSensitive { shadow.deleteBackward() }
        refresh()
    }

    /// Delete the previous word (trailing whitespace + the word before it) — the
    /// standard escalation when the delete key is held down past a few characters.
    func deleteWordBackward() {
        clearRevert()
        let before = proxy?.documentContextBeforeInput ?? ""
        let count = KeyboardTextOps.trailingWordDeleteCount(before: before)
        guard count > 0 else { return }
        for _ in 0..<count {
            proxy?.deleteBackward()
            if !isSensitive { shadow.deleteBackward() }
        }
        refresh()
    }

    /// Replace the in-progress word with a tapped suggestion. The shadow buffer
    /// would diverge from the field, so reset it (a fresh Write keyhole starts).
    func applySuggestion(_ suggestion: String) {
        let word = Self.currentWord(before: proxy?.documentContextBeforeInput)
        // Soft-learn the pick: choosing a suggestion is a light "I want this word"
        // signal, so record it toward the normal learn threshold (an accidental tap
        // won't stick; a word you keep choosing will). Resolve the bigram antecedent
        // from the current context BEFORE we delete the in-progress word.
        let previous = Self.wordBefore(word: word, in: proxy?.documentContextBeforeInput)
        recordLearnedWord(suggestion, previous: previous)
        for _ in 0..<word.count { proxy?.deleteBackward() }
        // Append a trailing space so you can keep typing the next word without it
        // running into the picked one (standard suggestion-bar behavior).
        proxy?.insertText(suggestion + " ")
        shadow.reset()
        input.didTypeCharacter()
        refresh()
    }

    // MARK: auto-correction

    /// Characters that complete a word and trigger a conservative auto-correction
    /// of the word before them.
    static func isBoundary(_ c: String) -> Bool {
        [".", ",", "!", "?", ";", ":"].contains(c)
    }

    /// Auto-replace the just-completed word if it's a clear typo. Called BEFORE the
    /// boundary char is inserted (the cursor is right after the word).
    private func maybeAutocorrect(boundary: String) {
        clearRevert()
        guard !isSensitive else { return }
        let word = Self.currentWord(before: proxy?.documentContextBeforeInput)
        // Learn-your-words: record the just-completed word off the keypress thread
        // so the correction engine adapts to the user's own vocabulary. Only the
        // final (accepted) word matters, so this fires on the word boundary.
        learnCommittedWord(word)
        guard let correction = suggestionEngine.autocorrection(for: word, lexicon: lexicon) else { return }
        for _ in 0..<word.count { proxy?.deleteBackward() }
        proxy?.insertText(correction)
        // Keep the shadow keyhole aligned with the replaced tail.
        for _ in 0..<min(word.count, shadow.count) { shadow.deleteBackward() }
        shadow.insert(correction)
        pendingRevert = (original: word, corrected: correction, boundary: boundary)
        revertOriginal = word
        KeyboardHaptics.keyTap()
    }

    /// Record a committed word into the learn-your-words store, off the keypress
    /// thread. Skips empties and very short tokens (nothing to learn). A word the
    /// user types repeatedly stops being autocorrected and starts being
    /// suggested. No-op in a secure field (caller already gated, kept defensive).
    ///
    /// Also records the ordered **bigram** `(previousWord → word)` into the learned-
    /// bigrams store (Phase 2b) so the universal n-gram predictor adapts to the user's
    /// own phrasing (incl. Indian-English + Hinglish). The previous word is read from
    /// the document context before the just-committed word; if there's none, the
    /// bigram is skipped (a lone word still learns as a unigram above).
    private func learnCommittedWord(_ word: String) {
        // Previous word = the word-token immediately preceding `word` in the field.
        // Read on the main thread (proxy is main-only); record off-thread below.
        let previous = Self.wordBefore(word: word, in: proxy?.documentContextBeforeInput)
        recordLearnedWord(word, previous: previous)
    }

    /// Soft-learn a word toward the learn threshold plus its `(previous → word)`
    /// bigram, off the keypress thread. Shared by the typed-word boundary hook and
    /// the suggestion-strip pick. Skips sensitive fields and tiny tokens; a word
    /// only becomes "learned" after `learnThreshold` such signals (so one stray
    /// tap won't stick, but a word you keep choosing will). `previous` is the
    /// bigram antecedent, resolved by the caller before it mutates the field.
    private func recordLearnedWord(_ word: String, previous: String?) {
        guard !isSensitive else { return }
        let trimmed = word.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count >= 3 else { return }
        DispatchQueue.global(qos: .utility).async {
            CorrectionStore.shared.learned.record(trimmed)
            if let previous, !previous.isEmpty {
                PredictorStore.shared.learnedBigrams.record(prev: previous, next: trimmed)
            }
        }
    }

    /// The word-token immediately before `word` in `context` (the text before the
    /// cursor, which ends in `word`). Strips the trailing `word` run, then returns the
    /// preceding trailing word-token (letters + apostrophe). `nil` when there's no
    /// earlier word (start of field / only one word typed).
    static func wordBefore(word: String, in context: String?) -> String? {
        guard let context, !context.isEmpty else { return nil }
        // Drop the just-committed word's trailing run, then any non-word separators,
        // then read the previous word run.
        let allowed = CharacterSet.letters.union(CharacterSet(charactersIn: "'’"))
        func isWordChar(_ ch: Character) -> Bool {
            ch.unicodeScalars.allSatisfy { allowed.contains($0) }
        }
        let chars = Array(context)
        var i = chars.count - 1
        // Skip the trailing `word` run (only up to word.count chars, defensively).
        var toDrop = word.count
        while i >= 0, toDrop > 0, isWordChar(chars[i]) { i -= 1; toDrop -= 1 }
        // Skip separators (spaces/punctuation) between the two words.
        while i >= 0, !isWordChar(chars[i]) { i -= 1 }
        // Collect the previous word run.
        var prev: [Character] = []
        while i >= 0, isWordChar(chars[i]) { prev.append(chars[i]); i -= 1 }
        guard !prev.isEmpty else { return nil }
        return String(prev.reversed())
    }

    /// Undo the last auto-correction (the revert chip).
    func revertAutocorrect() {
        guard let r = pendingRevert else { return }
        let toDelete = r.corrected.count + r.boundary.count
        for _ in 0..<toDelete { proxy?.deleteBackward() }
        let restored = r.original + r.boundary
        proxy?.insertText(restored)
        for _ in 0..<min(toDelete, shadow.count) { shadow.deleteBackward() }
        shadow.insert(restored)
        // Reject feedback: the user just reverted our correction back to the word
        // they actually typed, so trust it immediately — stop autocorrecting it
        // from now on (one revert is enough; no need to wait for 3 repeats).
        let reverted = r.original
        DispatchQueue.global(qos: .utility).async {
            CorrectionStore.shared.learned.trust(reverted)
        }
        clearRevert()
        refresh()
    }

    private func clearRevert() {
        pendingRevert = nil
        if revertOriginal != nil { revertOriginal = nil }
    }

    // MARK: Write lane (M2)

    func beginCharge() {
        if case .idle = write { write = .charging }
    }

    func cancelCharge() {
        if case .charging = write { write = .idle }
    }

    /// Commit the pill hold: rewrite what the keyboard typed, in place.
    @MainActor
    func commitWrite() async {
        guard case .charging = write else { return }
        await runWrite(guidance: nil)
    }

    /// Run a contextual-writing action on the keyhole (freshly-typed text, or the
    /// field's existing text). `.rewrite` replaces in place; `.reply` /
    /// `.continueWriting` produce new text that is appended (never destructive).
    @MainActor
    func runWrite(action: ContextualAssistAction = .rewrite, guidance: String?) async {
        guard hasFullAccess else { write = .error("Turn on Full Access for Magican to write."); return }
        let source = effectiveKeyhole
        let fromField = shadow.isEmpty
        guard !source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { write = .idle; return }
        lastWriteAction = action
        lastWriteGuidance = guidance
        write = .generating
        startWriteStages()
        defer { stopWriteStages() }
        // Client-supplied turn id (parity with chat's chat_turn_id): the backend
        // honors + echoes it, so the id is stable and correlatable across the run.
        let turnID = UUID().uuidString
        let request = ContextualAssistRequest.make(
            action: action,
            text: source,
            guidance: guidance,
            personality: "active",
            sessionKey: Self.sessionKey,
            sourceKey: Self.sourceKey,
            chatTurnID: turnID
        )
        do {
            let response = try await assistClient.run(request)
            lastWriteTurnID = response.chatTurnID ?? turnID
            lastWriteSessionID = response.sessionID
            guard let draft = response.draftText?.trimmingCharacters(in: .whitespacesAndNewlines), !draft.isEmpty else {
                write = .error("Magican didn't return a draft.")
                return
            }
            let plan: KeyboardEditPlan
            if action != .rewrite {
                // Reply / Continue produce new text — append, never replace.
                plan = .insertOnly(draft)
            } else if fromField, source.count > 600 {
                // The field context can be truncated for long documents — never
                // blind-replace; append instead.
                plan = .insertOnly(draft)
            } else {
                plan = KeyboardEditPlanner.plan(
                    shadow: source,
                    draft: draft,
                    documentBefore: proxy?.documentContextBeforeInput
                )
            }
            write = .preview(draft: draft, plan: plan)
            advanceCoach(to: .skills)
        } catch {
            write = .error((error as? LocalizedError)?.errorDescription ?? "Couldn't write. Try again.")
        }
    }

    /// Advance a generic "what's happening" label while the Write lane waits. These
    /// are lifecycle hints on a timer — honest about progress without claiming to
    /// mirror backend stages (that needs the turn-events tail; see K0 doc).
    @MainActor
    private func startWriteStages() {
        writeStage = "Reading your text…"
        stageTicker?.cancel()
        stageTicker = Task { @MainActor [weak self] in
            let steps: [(UInt64, String)] = [
                (1_500_000_000, "Magican is writing…"),
                (3_500_000_000, "Polishing…"),
                (7_000_000_000, "Still working…"),
            ]
            for (delay, label) in steps {
                try? await Task.sleep(nanoseconds: delay)
                guard let self, !Task.isCancelled, case .generating = self.write else { return }
                self.writeStage = label
            }
        }
    }

    @MainActor
    private func stopWriteStages() {
        stageTicker?.cancel()
        stageTicker = nil
        writeStage = ""
    }

    /// Re-run the last Write request for a fresh variant ("Try another" on the
    /// preview, "Try again" on an error/timeout).
    @MainActor
    func regenerateWrite() async {
        await runWrite(action: lastWriteAction, guidance: lastWriteGuidance)
    }

    private func writeAction(for id: String?) -> ContextualAssistAction {
        switch id {
        case "reply": return .reply
        case "continue": return .continueWriting
        default: return .rewrite
        }
    }

    /// Accept the previewed draft — replace-in-place (or insert-only if the field
    /// diverged), then offer Undo.
    func acceptWrite(draft: String, plan: KeyboardEditPlan) {
        switch plan {
        case .replace(let deleteCount, let insert, _, let undoInsert):
            for _ in 0..<deleteCount { proxy?.deleteBackward() }
            proxy?.insertText(insert)
            if KeyboardEditPlanner.replaceSucceeded(draft: insert, documentBefore: proxy?.documentContextBeforeInput) {
                write = .undo(insertedDraft: insert, restore: undoInsert)
            } else {
                // Restore what we deleted; leave the field as the user had it.
                proxy?.insertText(undoInsert)
                write = .error("Couldn't replace safely — left your text unchanged.")
            }
        case .insertOnly(let insert):
            proxy?.insertText(insert)
            write = .undo(insertedDraft: insert, restore: "")
        }
        shadow.reset()
        refresh()
    }

    func undoWrite() {
        guard case .undo(let inserted, let restore) = write else { return }
        guard KeyboardEditPlanner.undoIsValid(insertedDraft: inserted, documentBefore: proxy?.documentContextBeforeInput) else {
            write = .idle
            return
        }
        for _ in 0..<inserted.count { proxy?.deleteBackward() }
        if !restore.isEmpty { proxy?.insertText(restore) }
        write = .idle
        refresh()
    }

    func dismissWrite() {
        write = .idle
    }

    // MARK: Ask lane (M3)

    /// Quick-tap the pill: answer what you typed as a question, streaming into the
    /// canvas. Insert appends the answer (no delete).
    @MainActor
    func beginAsk() async {
        await runAsk(question: effectiveKeyhole.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    @MainActor
    func runAsk(question: String) async {
        guard hasFullAccess else { ask = .error("Turn on Full Access for Magican to ask."); return }
        guard !question.isEmpty else { ask = .error("Type a question first, then tap."); return }
        ask = .streaming(answer: "", question: question)
        advanceCoach(to: .skills)
        do {
            let sessionId = try await askClient.resolveSession()
            try await askClient.ask(question, sessionId: sessionId) { [weak self] text in
                Task { @MainActor in
                    guard let self, case .streaming(_, let q) = self.ask else { return }
                    self.ask = .streaming(answer: text, question: q)
                }
            }
            if case .streaming(let answer, let q) = ask {
                ask = answer.isEmpty ? .error("Magican didn't answer. Try again.") : .done(answer: answer, question: q)
            }
        } catch {
            ask = .error("Couldn't reach Magican. Check your connection.")
        }
    }

    func insertAnswer(_ text: String) {
        proxy?.insertText(text)
        shadow.reset()
        ask = .idle
        refresh()
    }

    func dismissAsk() {
        ask = .idle
    }

    // MARK: Skill chips + Act lane (M4)

    /// Route a skill chip to its lane.
    func dispatch(_ skill: KeyboardSkill) {
        let text = effectiveKeyhole
        switch skill.lane {
        case .write:
            let guidance = skill.template.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : skill.template
            Task { await runWrite(action: writeAction(for: skill.writeAction), guidance: guidance) }
        case .ask:
            Task { await runAsk(question: skill.resolved(text: text)) }
        case .act:
            proposeAct(skill, goal: skill.resolved(text: text))
        }
        if coachStep == .skills { advanceCoach(to: .done) }
    }

    /// Act NEVER runs without the verification card.
    func proposeAct(_ skill: KeyboardSkill, goal: String) {
        guard hasFullAccess else { act = .error("Turn on Full Access for Magican to run tasks."); return }
        let trimmed = goal.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { act = .error("Type something first, then pick a skill."); return }
        act = .confirm(skill: skill, goal: trimmed)
    }

    @MainActor
    func confirmAct() async {
        guard case .confirm(let skill, let goal) = act else { return }
        act = .working(label: skill.label)
        do {
            let taskId = try await actClient.run(title: skill.label, goal: goal)
            SharedActions.setPendingTask(taskId)   // the app opens it on next foreground
            act = .done(taskId: taskId, label: skill.label)
        } catch {
            act = .error("Couldn't start the task. Try again.")
        }
    }

    func dismissAct() {
        act = .idle
    }

    func refresh() {
        refreshAutocapitalize()
        refreshSuggestions()
        refreshCoach()
    }

    // MARK: live onboarding coach

    func refreshCoach() {
        guard KeyboardCoach.isActive else { coachStep = nil; return }
        coachStep = KeyboardCoach.step
        // Advance the first step once there's something typed.
        if coachStep == .typeSomething, hasContext {
            advanceCoach(to: .holdToRewrite)
        }
    }

    private func advanceCoach(to step: KeyboardCoachStep) {
        guard KeyboardCoach.isActive, KeyboardCoach.step.rawValue < step.rawValue else { return }
        KeyboardCoach.step = step
        coachStep = step
    }

    func finishCoach() {
        KeyboardCoach.finish()
        coachStep = nil
    }

    func refreshAutocapitalize() {
        input.refreshAutocapitalize(before: proxy?.documentContextBeforeInput)
    }

    func refreshSuggestions() {
        // Cancel any in-flight recompute — only the latest keystroke matters.
        suggestionWork?.cancel()
        // Any strip change invalidates a pending prediction completion.
        predictionGeneration &+= 1
        guard !isSensitive, input.layer == .letters else {
            cancelPredictions()
            suggestions = []
            return
        }
        // Read the field context on the main thread (UITextDocumentProxy is main-only),
        // but run the expensive UITextChecker recompute on a background queue after a
        // short debounce so the keypress that scheduled it returns immediately.
        let before = proxy?.documentContextBeforeInput
        let word = Self.currentWord(before: before)

        // Phase-2a: with NO in-progress word (cursor right after a space / start of
        // field), the after-space slot shows Foundation-Models next-word predictions.
        // With an in-progress word, Phase-1 corrections own the strip (below), and any
        // pending prediction is cancelled so the two never fight over the slot.
        switch StripSlotDecider.slot(inProgressWord: word, contextBeforeCursor: before ?? "") {
        case .predictions:
            requestPredictions(context: before ?? "")
            return
        case .corrections:
            cancelPredictions()
        }
        guard !word.isEmpty else { suggestions = []; return }
        let lexicon = self.lexicon
        let engine = self.suggestionEngine
        let work = DispatchWorkItem { [weak self] in
            let computed = engine.suggestions(forWord: word, lexicon: lexicon)
            DispatchQueue.main.async {
                guard let self, !self.isSensitive, self.input.layer == .letters else { return }
                // Only publish if the current word still matches what we computed for
                // (the user may have typed more / deleted since we scheduled).
                let current = Self.currentWord(before: self.proxy?.documentContextBeforeInput)
                guard current == word else { return }
                self.suggestions = computed
            }
        }
        suggestionWork = work
        DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + 0.05, execute: work)
    }

    /// Request debounced Foundation-Models next-word predictions for the after-space
    /// slot. No-op (empties the slot) when no predictor is available. The FM call runs
    /// off the keypress thread inside `DebouncedPredictor`; the result is published on
    /// the main thread only if the cursor hasn't moved on since we asked.
    private func requestPredictions(context: String) {
        // Lazily build the debounced wrapper the first time a predictor exists. Until
        // the background prewarm publishes one, this is a no-op and the slot is empty.
        if debouncedPredictor == nil, let predictor = suggestionEngine.nextWordPredictor {
            debouncedPredictor = DebouncedPredictor(predictor: predictor, debounceMillis: 200, limit: 3)
        }
        guard let debouncedPredictor else { suggestions = []; return }

        let generation = predictionGeneration
        Task {
            await debouncedPredictor.request(context: context) { [weak self] words in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    // Drop stale results: only publish if no newer refresh has happened
                    // and we're still in a state that wants predictions.
                    guard self.predictionGeneration == generation,
                          !self.isSensitive, self.input.layer == .letters else { return }
                    self.suggestions = words
                }
            }
        }
    }

    /// Cancel any pending/in-flight prediction (entering the corrections slot, a secure
    /// field, or a non-letters layer).
    private func cancelPredictions() {
        guard let debouncedPredictor else { return }
        Task { await debouncedPredictor.cancel() }
    }

    /// The trailing run of word characters before the cursor (letters + apostrophe).
    static func currentWord(before context: String?) -> String {
        guard let context, !context.isEmpty else { return "" }
        let allowed = CharacterSet.letters.union(CharacterSet(charactersIn: "'’"))
        var chars: [Character] = []
        for ch in context.reversed() {
            if ch.unicodeScalars.allSatisfy({ allowed.contains($0) }) { chars.append(ch) }
            else { break }
        }
        return String(chars.reversed())
    }
}
