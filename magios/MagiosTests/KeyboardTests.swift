import XCTest
@testable import Magician

/// Unit tests for the Magican Keyboard's pure `Shared/` logic (UI-framework-free,
/// app-host testable). The data-integrity core (`KeyboardEditPlanner`) is covered
/// exhaustively — it's what guards against corrupting a user's field.
final class KeyboardTests: XCTestCase {

    // MARK: KeyboardShadowBuffer

    func testShadowAccumulateDeleteReset() {
        var s = KeyboardShadowBuffer()
        s.insert("h"); s.insert("i")
        XCTAssertEqual(s.text, "hi")
        XCTAssertEqual(s.count, 2)
        s.deleteBackward()
        XCTAssertEqual(s.text, "h")
        s.reset()
        XCTAssertTrue(s.isEmpty)
    }

    func testShadowDeleteOnEmptyIsSafe() {
        var s = KeyboardShadowBuffer()
        s.deleteBackward()
        XCTAssertTrue(s.isEmpty)
    }

    // MARK: KeyboardThemeStore — persisted system appearance (seeds first paint)

    /// The last-known system appearance round-trips through the App Group so the
    /// keyboard's first paint on the next cold start matches (no light→dark flash).
    func testLastKnownAppearanceRoundTrips() throws {
        guard UserDefaults(suiteName: MagicianAccess.appGroup) != nil else {
            throw XCTSkip("App Group suite unavailable in this host")
        }
        let saved = KeyboardThemeStore.lastKnownDark          // preserve real value
        defer {
            if let saved { KeyboardThemeStore.lastKnownDark = saved }
        }
        KeyboardThemeStore.lastKnownDark = true
        XCTAssertEqual(KeyboardThemeStore.lastKnownDark, true)
        KeyboardThemeStore.lastKnownDark = false
        XCTAssertEqual(KeyboardThemeStore.lastKnownDark, false)
    }

    // MARK: KeyboardEditPlanner (integrity core)

    func testPlanReplacesWhenLiveTextEndsWithShadow() {
        let plan = KeyboardEditPlanner.plan(shadow: "helo", draft: "hello", documentBefore: "type helo")
        guard case .replace(let del, let ins, let undoDel, let undoIns) = plan else {
            return XCTFail("expected replace")
        }
        XCTAssertEqual(del, 4)
        XCTAssertEqual(ins, "hello")
        XCTAssertEqual(undoDel, 5)
        XCTAssertEqual(undoIns, "helo")
        XCTAssertTrue(plan.isReplace)
    }

    func testPlanInsertOnlyWhenFieldDiverged() {
        // Live text does NOT end with the shadow (autocorrect / smart quotes) →
        // must NOT blind-delete.
        let plan = KeyboardEditPlanner.plan(shadow: "helo", draft: "hello", documentBefore: "unrelated text")
        guard case .insertOnly(let ins) = plan else { return XCTFail("expected insertOnly") }
        XCTAssertEqual(ins, "hello")
        XCTAssertFalse(plan.isReplace)
    }

    func testPlanInsertOnlyWhenShadowEmpty() {
        let plan = KeyboardEditPlanner.plan(shadow: "", draft: "hi", documentBefore: "abc")
        XCTAssertFalse(plan.isReplace)
    }

    func testPlanInsertOnlyWhenNilContext() {
        let plan = KeyboardEditPlanner.plan(shadow: "x", draft: "y", documentBefore: nil)
        XCTAssertFalse(plan.isReplace)
    }

    func testReplaceSucceededPostVerify() {
        XCTAssertTrue(KeyboardEditPlanner.replaceSucceeded(draft: "hello", documentBefore: "say hello"))
        XCTAssertFalse(KeyboardEditPlanner.replaceSucceeded(draft: "hello", documentBefore: "say hell"))
    }

    func testUndoValidityGuard() {
        XCTAssertTrue(KeyboardEditPlanner.undoIsValid(insertedDraft: "hello", documentBefore: "say hello"))
        XCTAssertFalse(KeyboardEditPlanner.undoIsValid(insertedDraft: "", documentBefore: "x"))
        // User typed since → undo would corrupt.
        XCTAssertFalse(KeyboardEditPlanner.undoIsValid(insertedDraft: "hello", documentBefore: "say hello!"))
    }

    // MARK: KeyboardInputState

    func testShiftToggleAndCaps() {
        var s = KeyboardInputState()
        XCTAssertEqual(s.shift, .on)          // sentence-start default
        s.tapShift(); XCTAssertEqual(s.shift, .off)
        s.tapShift(); XCTAssertEqual(s.shift, .on)
        s.engageCapsLock(); XCTAssertEqual(s.shift, .capsLock)
        s.tapShift(); XCTAssertEqual(s.shift, .off)
    }

    func testOneShotShiftReleasesButCapsHolds() {
        var s = KeyboardInputState()          // .on
        s.didTypeCharacter()
        XCTAssertEqual(s.shift, .off)
        s.engageCapsLock()
        s.didTypeCharacter()
        XCTAssertEqual(s.shift, .capsLock)
    }

    func testShiftDoubleTapDetectionAndImmediateToggle() {
        // Regression for the ~0.5s Shift lag: caps-lock is now decided by a pure
        // timestamp check (no TapGesture(count: 2) disambiguation wait).
        typealias S = KeyboardInputState
        XCTAssertFalse(S.shiftTapEngagesCapsLock(now: 100.0, lastTapAt: -.greatestFiniteMagnitude)) // 1st tap
        XCTAssertTrue(S.shiftTapEngagesCapsLock(now: 10.2, lastTapAt: 10.0))   // quick 2nd tap → caps
        XCTAssertFalse(S.shiftTapEngagesCapsLock(now: 11.0, lastTapAt: 10.2))  // slow → lone toggle

        // The two outcomes drive the (already-tested) state machine:
        var s = KeyboardInputState()          // .on
        s.tapShift(); XCTAssertEqual(s.shift, .off)              // lone tap → immediate toggle
        s.engageCapsLock(); XCTAssertEqual(s.shift, .capsLock)  // quick 2nd tap → caps-lock
    }

    func testLayerSwitch() {
        var s = KeyboardInputState()
        s.switchLayer(.numbers)
        XCTAssertEqual(s.layer, .numbers)
    }

    func testAutocapAtFieldStartAndAfterSentence() {
        var s = KeyboardInputState()
        s.switchLayer(.letters)
        s.refreshAutocapitalize(before: "")
        XCTAssertEqual(s.shift, .on)
        s.refreshAutocapitalize(before: "Hello there")
        XCTAssertEqual(s.shift, .off)
        s.refreshAutocapitalize(before: "Done. ")
        XCTAssertEqual(s.shift, .on)
    }

    // MARK: KeyboardLayoutModel

    func testLetterRowCasing() {
        let lower = KeyboardLayoutModel.rows(layer: .letters, shift: .off, needsGlobe: true)
        let upper = KeyboardLayoutModel.rows(layer: .letters, shift: .on, needsGlobe: true)
        XCTAssertEqual(firstCharacter(lower), "q")
        XCTAssertEqual(firstCharacter(upper), "Q")
    }

    func testNumberAndSymbolLayers() {
        XCTAssertEqual(firstCharacter(KeyboardLayoutModel.rows(layer: .numbers, shift: .off, needsGlobe: true)), "1")
        XCTAssertEqual(firstCharacter(KeyboardLayoutModel.rows(layer: .symbols, shift: .off, needsGlobe: true)), "[")
    }

    func testGlobeOnlyWhenNeeded() {
        XCTAssertTrue(hasGlobe(KeyboardLayoutModel.rows(layer: .letters, shift: .off, needsGlobe: true)))
        XCTAssertFalse(hasGlobe(KeyboardLayoutModel.rows(layer: .letters, shift: .off, needsGlobe: false)))
    }

    func testCalloutsCaseInsensitive() {
        let e = KeyCap(id: "e", kind: .character("e"))
        XCTAssertTrue(e.callouts.contains("é"))
        let upperE = KeyCap(id: "E", kind: .character("E"))
        XCTAssertTrue(upperE.callouts.contains("é"))
        let z = KeyCap(id: "z", kind: .character("z"))   // has alternates
        XCTAssertFalse(z.callouts.isEmpty)
        let shift = KeyCap(id: "s", kind: .shift)
        XCTAssertTrue(shift.callouts.isEmpty)
    }

    // MARK: KeyboardSkill + store

    func testSkillResolvedSubstitution() {
        let s = KeyboardSkill(label: "Sum", symbol: "x", lane: .ask, template: "Summarize: {text}")
        XCTAssertEqual(s.resolved(text: "hi"), "Summarize: hi")
    }

    func testSkillStoreRoundTripAndReset() {
        let original = KeyboardSkillStore.load()
        defer { KeyboardSkillStore.save(original) }   // restore

        let custom = [KeyboardSkill(label: "Only", symbol: "x", lane: .write, template: "t", writeAction: "reply")]
        KeyboardSkillStore.save(custom)
        let loaded = KeyboardSkillStore.load()
        XCTAssertEqual(loaded.count, 1)
        XCTAssertEqual(loaded.first?.label, "Only")
        XCTAssertEqual(loaded.first?.writeAction, "reply")

        KeyboardSkillStore.resetToDefault()
        XCTAssertEqual(KeyboardSkillStore.load().count, KeyboardSkillStore.defaultPack.count)
    }

    func testDefaultPackHasAllLanes() {
        let lanes = Set(KeyboardSkillStore.defaultPack.map { $0.lane })
        XCTAssertTrue(lanes.contains(.write))
        XCTAssertTrue(lanes.contains(.ask))
        XCTAssertTrue(lanes.contains(.act))
        // Reply + Continue write actions present.
        let actions = Set(KeyboardSkillStore.defaultPack.compactMap { $0.writeAction })
        XCTAssertTrue(actions.contains("reply"))
        XCTAssertTrue(actions.contains("continue"))
    }

    // MARK: KeyboardTextOps — word-delete escalation (held delete)

    func testWordDeleteRemovesLastWord() {
        // "hello world" (caret at end) → removes "world" (5), leaving "hello ".
        XCTAssertEqual(KeyboardTextOps.trailingWordDeleteCount(before: "hello world"), 5)
    }

    func testWordDeleteEatsTrailingWhitespaceThenWord() {
        // "hello world " → trailing space + "world" = 6, leaving "hello ".
        XCTAssertEqual(KeyboardTextOps.trailingWordDeleteCount(before: "hello world "), 6)
    }

    func testWordDeleteTrailingSpacesOnly() {
        // "hi   " → 3 trailing spaces + "hi" = 5.
        XCTAssertEqual(KeyboardTextOps.trailingWordDeleteCount(before: "hi   "), 5)
    }

    func testWordDeleteSingleWord() {
        XCTAssertEqual(KeyboardTextOps.trailingWordDeleteCount(before: "word"), 4)
    }

    func testWordDeleteEmptyIsZero() {
        XCTAssertEqual(KeyboardTextOps.trailingWordDeleteCount(before: ""), 0)
    }

    func testWordDeleteNewlineCountsAsWhitespace() {
        // "a b\n" → "\n" + "b" = 2, leaving "a ".
        XCTAssertEqual(KeyboardTextOps.trailingWordDeleteCount(before: "a b\n"), 2)
    }

    // MARK: KeyboardEmoji

    func testEmojiCategoriesPopulated() {
        XCTAssertFalse(KeyboardEmoji.categories.isEmpty)
        XCTAssertTrue(KeyboardEmoji.all.count > 100)
        // Every category has emojis + a distinct id.
        for c in KeyboardEmoji.categories { XCTAssertFalse(c.emojis.isEmpty) }
        let ids = KeyboardEmoji.categories.map { $0.id }
        XCTAssertEqual(ids.count, Set(ids).count)
    }

    func testEmojiKeyPresentOnBottomRow() {
        let rows = KeyboardLayoutModel.rows(layer: .letters, shift: .off, needsGlobe: true)
        let bottom = rows.last ?? []
        let hasEmoji = bottom.contains { if case .layer(.emoji) = $0.kind { return true }; return false }
        XCTAssertTrue(hasEmoji)
    }

    // MARK: KeyboardLanguage

    func testLanguageStoreRoundTripAndDefault() {
        let original = KeyboardLanguageStore.current
        defer { KeyboardLanguageStore.set(original) }

        KeyboardLanguageStore.set(.uk)
        XCTAssertEqual(KeyboardLanguageStore.current, .uk)
        KeyboardLanguageStore.set(.india)
        XCTAssertEqual(KeyboardLanguageStore.current, .india)
    }

    func testLanguageMappings() {
        XCTAssertEqual(KeyboardLanguage.india.primaryLanguage, "en-IN")
        XCTAssertEqual(KeyboardLanguage.uk.primaryLanguage, "en-GB")
        XCTAssertEqual(KeyboardLanguage.us.primaryLanguage, "en-US")
        // Indian English prefers en_IN but degrades through British to US.
        XCTAssertEqual(KeyboardLanguage.india.checkerCandidates.first, "en_IN")
        XCTAssertTrue(KeyboardLanguage.india.checkerCandidates.contains("en_GB"))
        XCTAssertEqual(KeyboardLanguage.india.checkerCandidates.last, "en_US")
    }

    // MARK: helpers

    private func firstCharacter(_ rows: [[KeyCap]]) -> String? {
        guard case .character(let c)? = rows.first?.first?.kind else { return nil }
        return c
    }

    private func hasGlobe(_ rows: [[KeyCap]]) -> Bool {
        rows.flatMap { $0 }.contains { if case .globe = $0.kind { return true }; return false }
    }
}
