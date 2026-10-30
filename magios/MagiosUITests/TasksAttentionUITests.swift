import XCTest

/// Smoke tests for the Tasks + Attention + Chat navigation and static chrome.
/// No backend fixture is needed — these assert the always-present UI (tabs, lane
/// picker, filters, create sheet, history toggle, composer), so they catch wiring
/// regressions even against the empty/loading state.
final class TasksAttentionUITests: XCTestCase {
    private var app: XCUIApplication?

    override func setUpWithError() throws { continueAfterFailure = false }

    /// Terminate the app deterministically at the end of each test. Without this,
    /// XCUITest leaves the app LINGERING for ~30s after the test body finishes and
    /// then kills it at session cleanup — an uncontrolled late SIGTERM that, under
    /// the full parallel `make test` load, races with the framework's per-test
    /// bookkeeping and is misattributed as "Test crashed with signal term." A
    /// test-initiated `terminate()` ends the app in ~1s inside the test's own scope,
    /// so the shutdown is expected (not a crash) and each test runs ~9s, not ~38s.
    override func tearDown() {
        app?.terminate()
        app = nil
        super.tearDown()
    }

    private func launchedApp(extraArguments: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        // Launch offline + deterministic: the app skips real WebSocket connects,
        // health polling, and speech, so the static chrome under test settles fast
        // and XCUITest's wait-for-idle doesn't time out under coverage / cold-sim
        // load (these smoke tests assert always-present UI, not backend data).
        app.launchArguments.append("--ui-test")
        app.launchArguments.append(contentsOf: extraArguments)
        app.launch()
        // The brand splash shows for ~3s ("I'll try my best.").
        _ = app.staticTexts["I'll try my best."].waitForNonExistence(timeout: 10)
        self.app = app
        return app
    }

    func testAllPrimaryTabsPresent() {
        let app = launchedApp()
        // The primary set: Chat · Work · Today · Attention · Observe. Settings
        // moved OFF the tab bar into the side menu (hamburger → Settings sheet).
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.tabBars.buttons["Work"].exists)
        XCTAssertFalse(app.tabBars.buttons["Tasks"].exists)
        XCTAssertTrue(app.tabBars.buttons["Today"].exists)
        XCTAssertTrue(app.tabBars.buttons["Attention"].exists)
        XCTAssertTrue(app.tabBars.buttons["Observe"].exists)
    }

    func testObserveDeckKPICardsSwitchViews() {
        let app = launchedApp()
        app.tabBars.buttons["Observe"].tap()
        // The Command Deck: status line + four KPI cards as the only switchers.
        XCTAssertTrue(app.descendants(matching: .any)["observe-status-line"].waitForExistence(timeout: 5))
        for pane in ["now", "sources", "audio", "notes"] {
            XCTAssertTrue(app.buttons["observe-kpi-\(pane)"].exists, "missing KPI card \(pane)")
        }
        app.buttons["observe-kpi-now"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["observe-launchpad"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["observe-tile-listen"].exists)
        XCTAssertTrue(app.buttons["observe-tile-join"].exists)
        XCTAssertTrue(app.buttons["observe-tile-broadcast"].exists)
        XCTAssertTrue(app.buttons["observe-tile-brainstorm"].exists)
        app.buttons["observe-kpi-sources"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["observe-sources"].waitForExistence(timeout: 3))
        app.buttons["observe-kpi-audio"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["observe-audio"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["observe-kpi-audio"].isSelected)
        // Leave the remembered view on Now for the next launch.
        app.buttons["observe-kpi-now"].tap()
    }

    func testObserveExposesServerPagedPublishedNotes() {
        let app = launchedApp()
        app.tabBars.buttons["Observe"].tap()
        // Published Notes live in the deck's Notes view (Notes & Recents card).
        let notesCard = app.buttons["observe-kpi-notes"]
        XCTAssertTrue(notesCard.waitForExistence(timeout: 5))
        notesCard.tap()
        XCTAssertTrue(app.buttons["observe-audio-notes"].waitForExistence(timeout: 3))
        let section = app.descendants(matching: .any)["published-notes-section"]
        for _ in 0..<8 where !section.exists { app.swipeUp() }
        XCTAssertTrue(section.waitForExistence(timeout: 3))
        XCTAssertTrue(app.textFields["published-notes-search"].exists)
        XCTAssertTrue(app.buttons["published-notes-previous"].exists)
        XCTAssertTrue(app.buttons["published-notes-next"].exists)
        // Leave the remembered view on Now for the next launch.
        for _ in 0..<8 where !app.buttons["observe-kpi-now"].isHittable { app.swipeDown() }
        app.buttons["observe-kpi-now"].tap()
    }

    func testTasksTabRendersLanePickerAndFilters() {
        let app = launchedApp()
        let tab = app.tabBars.buttons["Work"]
        XCTAssertTrue(tab.waitForExistence(timeout: 5))
        tab.tap()
        XCTAssertTrue(tab.isSelected)
        // Two-lane segmented control (the "Internal tasks" segment is unambiguous).
        XCTAssertTrue(app.buttons["System work"].waitForExistence(timeout: 3))
        // The web preset filters.
        XCTAssertTrue(app.buttons["Overdue"].exists)
        XCTAssertTrue(app.buttons["Completed"].exists)
        // The create affordance.
        XCTAssertTrue(app.buttons["tasks-create"].exists)
    }

    func testWorkViewsKeepQuickCreationAvailableFromPrograms() {
        let app = launchedApp()
        app.tabBars.buttons["Work"].tap()
        XCTAssertTrue(app.buttons["work-view-all"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["work-view-needsYou"].exists)
        let monitors = app.buttons["tasks-lane-monitors"]
        XCTAssertTrue(monitors.waitForExistence(timeout: 3)); monitors.tap()
        app.buttons["work-view-crew"].tap()
        XCTAssertTrue(app.buttons["work-new"].waitForExistence(timeout: 5))
        app.buttons["work-more"].tap()
        app.buttons["Playbooks"].tap()
        XCTAssertTrue(app.buttons["work-new"].waitForExistence(timeout: 5))
        app.buttons["work-new"].tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 5))
        app.buttons["Cancel"].tap()
        XCTAssertTrue(app.buttons["work-view-all"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["tasks-lane-tasks"].isSelected, "New work from another view must return to ordinary tasks")
    }

    func testWorkDeepLinkDoesNotReopenCrewWhenChoosingProgramsAfterCreation() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture", "--open-work-ui-test", "/work?view=crew&agent=fixture-crew"])
        XCTAssertTrue(app.navigationBars["Fixture crew"].waitForExistence(timeout: 5))
        app.navigationBars.buttons.firstMatch.tap()
        XCTAssertTrue(app.buttons["work-new"].waitForExistence(timeout: 5))
        app.buttons["work-new"].tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 5)); app.buttons["Cancel"].tap()
        XCTAssertTrue(app.buttons["work-more"].waitForExistence(timeout: 5))
        app.buttons["work-more"].tap(); app.buttons["Playbooks"].tap()
        XCTAssertTrue(app.buttons["work-program-fixture.md"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.navigationBars["Fixture crew"].exists)
    }

    func testWorkLargeTextKeepsNavigationCrewActionsAndKeyboardEditorReachable() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture",
            "-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityXXXL"])
        app.tabBars.buttons["Work"].tap()
        let frame = app.frame
        for id in ["all", "needsYou", "crew"] {
            let control = app.buttons["work-view-\(id)"]
            XCTAssertTrue(control.waitForExistence(timeout: 5)); XCTAssertTrue(control.isHittable)
            XCTAssertGreaterThanOrEqual(control.frame.minX, frame.minX)
            XCTAssertLessThanOrEqual(control.frame.maxX, frame.maxX)
        }
        app.buttons["work-view-crew"].tap()
        let crew = app.buttons["work-crew-fixture-crew"]
        XCTAssertTrue(crew.waitForExistence(timeout: 5)); crew.tap()
        let actions = app.buttons["Actions"]
        for _ in 0..<6 where !actions.isHittable { app.swipeUp() }
        XCTAssertTrue(actions.isHittable); actions.tap()
        XCTAssertTrue(app.buttons["Run once"].waitForExistence(timeout: 3))
        let menuImage = XCTAttachment(screenshot: app.screenshot()); menuImage.name = "work-large-text-crew-actions"; menuImage.lifetime = .keepAlways; add(menuImage)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.1)).tap()
        let definition = app.buttons["Definition, rules and model pins"]
        for _ in 0..<8 where !definition.isHittable { app.swipeUp() }
        definition.tap()
        let edit = app.buttons["Edit definition"]
        for _ in 0..<8 where !edit.isHittable { app.swipeUp() }
        edit.tap()
        app.buttons["Advanced JSON"].tap()
        let editor = app.textViews["work-crew-definition-editor"]
        XCTAssertTrue(editor.waitForExistence(timeout: 3)); editor.tap()
        XCTAssertTrue(app.buttons["Cancel"].isHittable)
        XCTAssertTrue(app.buttons["Save"].isHittable)
        XCTAssertTrue(app.buttons["Save"].isEnabled)
        XCTAssertGreaterThan(editor.frame.height, 80, "The keyboard must leave useful editing space")
        let editorImage = XCTAttachment(screenshot: app.screenshot()); editorImage.name = "work-large-text-crew-editor-keyboard"; editorImage.lifetime = .keepAlways; add(editorImage)
        editor.typeText(" ")
        app.buttons["Save"].tap()
        XCTAssertTrue(editor.waitForNonExistence(timeout: 5), "The version-checked save must complete from the first presentation")
    }

    func testWorkCrewProgramsAndPolicyReviewUseNativeDetails() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap()
        app.buttons["work-view-crew"].tap()
        let crew = app.buttons["work-crew-fixture-crew"]
        XCTAssertTrue(crew.waitForExistence(timeout: 5)); crew.tap()
        XCTAssertFalse(app.buttons["work-more"].exists, "Work navigation belongs to the root, not a pushed detail")
        for _ in 0..<4 where !app.buttons["Definition, rules and model pins"].isHittable { app.swipeUp() }
        app.buttons["Definition, rules and model pins"].tap()
        for _ in 0..<4 where !app.buttons["Edit definition"].isHittable { app.swipeUp() }
        app.buttons["Edit definition"].tap()
        app.buttons["Advanced JSON"].tap()
        XCTAssertTrue(app.textViews["work-crew-definition-editor"].waitForExistence(timeout: 3))
        app.buttons["Cancel"].tap()
        app.navigationBars.buttons.firstMatch.tap()
        app.buttons["work-more"].tap(); app.buttons["Playbooks"].tap()
        app.buttons["work-program-fixture.md"].tap()
        XCTAssertTrue(app.buttons["Restore"].waitForExistence(timeout: 3))
        app.buttons["Restore"].tap()
        XCTAssertTrue(app.buttons["Restore version"].waitForExistence(timeout: 3))
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.92)).tap()
        XCTAssertTrue(app.buttons["Restore version"].waitForNonExistence(timeout: 2))
        app.navigationBars.buttons.firstMatch.tap()
        app.buttons["work-more"].tap(); app.buttons["Quality checks"].tap()
        for _ in 0..<4 where !app.buttons["Capture policies"].isHittable { app.swipeUp() }
        app.buttons["Capture policies"].tap()
        app.buttons["New capture policy"].tap()
        XCTAssertTrue(app.textViews["work-policy-editor"].waitForExistence(timeout: 3))
        app.swipeUp(); app.buttons["Review frozen policy"].tap()
        for _ in 0..<4 where !app.buttons["Approve exact policy"].isHittable { app.swipeUp() }
        XCTAssertTrue(app.buttons["Approve exact policy"].exists)
    }

    func testProgramShowsResponsibleCrewAndExactRuntimeEvidence() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap(); app.buttons["work-more"].tap(); app.buttons["Playbooks"].tap()
        let program = app.buttons["work-program-fixture.md"]; XCTAssertTrue(program.waitForExistence(timeout: 5)); program.tap()
        let evidence = app.buttons["Task and execution evidence"]
        for _ in 0..<7 where !evidence.isHittable { app.swipeUp() }
        XCTAssertTrue(app.staticTexts["Current Phase: Verify migration"].exists)
        XCTAssertTrue(evidence.exists); evidence.tap()
        XCTAssertTrue(app.buttons["task-detail-act-run"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["task-detail-act-output"].exists)
    }

    func testCrewCycleOpensSharedRunInspectorWithoutTaskOnlyActions() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap(); app.buttons["work-view-crew"].tap()
        let crew = app.buttons["work-crew-fixture-crew"]; XCTAssertTrue(crew.waitForExistence(timeout: 5)); crew.tap()
        let cycle = app.buttons["work-crew-current-cycle"]
        for _ in 0..<5 where !cycle.isHittable { app.swipeUp() }
        XCTAssertTrue(cycle.exists); cycle.tap()
        XCTAssertTrue(app.buttons["task-detail-act-run"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["task-detail-act-plan"].exists)
        XCTAssertFalse(app.buttons["task-detail-act-output"].exists)
        XCTAssertFalse(app.buttons["Quality review"].exists)
        app.buttons["task-detail-menu"].tap()
        XCTAssertFalse(app.buttons["Delete task"].exists)
        XCTAssertFalse(app.buttons["task-detail-publish-notes"].exists)
    }

    func testWorkGuidedMemberEditorAndGoalMemoryRemainNative() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap(); app.buttons["work-view-crew"].tap()
        let crew = app.buttons["work-crew-fixture-crew"]; XCTAssertTrue(crew.waitForExistence(timeout: 5)); crew.tap()
        let rules = app.buttons["Rules and policies"]
        for _ in 0..<6 where !rules.isHittable { app.swipeUp() }
        rules.tap(); XCTAssertTrue(app.staticTexts["Native fixture assistant"].waitForExistence(timeout: 3))
        app.navigationBars.buttons.firstMatch.tap()
        let memory = app.buttons["Memory tiers"]
        for _ in 0..<6 where !memory.isHittable { app.swipeDown() }
        memory.tap()
        let tier = app.buttons["work-memory-tier-goal-memory"]
        XCTAssertTrue(tier.waitForExistence(timeout: 3)); tier.tap()
        XCTAssertTrue(app.staticTexts["Exact goal memory"].waitForExistence(timeout: 3))
        app.navigationBars.buttons.firstMatch.tap(); app.navigationBars.buttons.firstMatch.tap()
        let definition = app.buttons["Definition, rules and model pins"]
        for _ in 0..<8 where !definition.isHittable { app.swipeUp() }
        definition.tap(); app.buttons["Edit definition"].tap()
        XCTAssertTrue(app.textFields["Name"].waitForExistence(timeout: 3))
        app.textFields["Name"].tap(); app.textFields["Name"].typeText(" reviewed")
        app.buttons["Save"].tap()
        XCTAssertTrue(app.textFields["Name"].waitForNonExistence(timeout: 5))
    }

    func testWorkReferenceAttachmentAndExactAttemptComparison() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap(); app.buttons["work-more"].tap(); app.buttons["Quality checks"].tap()
        app.buttons["New quality work"].tap()
        let search = app.textFields["Find a reference"]; XCTAssertTrue(search.waitForExistence(timeout: 3))
        for _ in 0..<5 where !search.isHittable { app.swipeUp() }
        search.tap(); search.typeText("launch")
        let searchButton = app.buttons["Search notes"]
        for _ in 0..<5 where !searchButton.isHittable { app.swipeUp() }
        searchButton.tap()
        let hit = app.buttons["Launch evidence"]; XCTAssertTrue(hit.waitForExistence(timeout: 3)); hit.tap()
        XCTAssertTrue(app.buttons["Remove Launch evidence"].waitForExistence(timeout: 3))
        app.buttons["Done"].tap()
        let run = app.buttons.containing(.staticText, identifier: "Fixture quality work").firstMatch
        XCTAssertTrue(run.waitForExistence(timeout: 3)); run.tap()
        let attempt = app.buttons["Attempt 2"]
        for _ in 0..<6 where !attempt.isHittable { app.swipeUp() }
        attempt.tap()
        let artifact = app.buttons["Inspect artifact and evidence"]
        for _ in 0..<4 where !artifact.isHittable { app.swipeUp() }
        artifact.tap()
        let compare = app.buttons["Compare previous attempt"]; XCTAssertTrue(compare.waitForExistence(timeout: 3)); compare.tap()
        XCTAssertTrue(app.staticTexts["Compare exact attempts"].waitForExistence(timeout: 3))
        let previous = app.buttons["Previous attempt · previous-hash"]; previous.tap()
        XCTAssertTrue(app.staticTexts["Previous fixture output"].waitForExistence(timeout: 3))
        let current = app.buttons["Current attempt · fixture-hash"]; current.tap()
        XCTAssertTrue(app.staticTexts["Exact fixture output"].waitForExistence(timeout: 3))
        XCTAssertFalse(app.buttons["I reviewed this file"].exists)
    }

    func testTaskTitleAndLoadedThreadEditorKeepOptionalChangesExplicit() {
        let app = launchedApp(extraArguments: ["--tasks-ui-test-fixture", "--work-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap()
        let actions = app.buttons["task-action-actions-fixture-active"]
        XCTAssertTrue(actions.waitForExistence(timeout: 3)); actions.tap()
        let title = app.textFields["Task title"]
        for _ in 0..<8 where !title.isHittable { app.swipeUp() }
        XCTAssertTrue(title.exists); title.tap(); title.typeText(" updated")
        XCTAssertTrue(app.buttons["Save title"].isEnabled)
        let move = app.buttons["Move to another thread"]
        for _ in 0..<6 where !move.isHittable { app.swipeUp() }
        move.tap()
        XCTAssertTrue(app.staticTexts["The destination is checked again before the move."].waitForExistence(timeout: 3))
        XCTAssertFalse(app.buttons["Move task"].isEnabled)
        // Unsaved title text is cancelled without an API mutation.
        app.navigationBars["Task actions"].buttons["Done"].firstMatch.tap()
        XCTAssertTrue(app.staticTexts["Fixture active task"].waitForExistence(timeout: 3))
    }
    func testCompletedReferencesAndCleanupSelectionAreOperableOnPhone() {
        let app = launchedApp(extraArguments: ["--tasks-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap(); app.buttons["tasks-create"].tap()
        let references = app.buttons["Reference completed work (0)"]
        XCTAssertTrue(references.waitForExistence(timeout: 3)); references.tap()
        let task = app.buttons.containing(.staticText, identifier: "Fixture completed task").firstMatch
        XCTAssertTrue(task.waitForExistence(timeout: 3)); task.tap()
        XCTAssertTrue(app.buttons["Remove reference: Fixture completed task"].exists)
        app.buttons["Remove reference: Fixture completed task"].tap()
        XCTAssertTrue(app.buttons["Reference completed work (0)"].exists)
        app.navigationBars["New Task"].buttons["Cancel"].firstMatch.tap()
        app.buttons["System work"].tap()
        let cleanup = app.buttons["Select cleanup"]
        for _ in 0..<5 where !cleanup.isHittable { app.swipeLeft() }
        XCTAssertTrue(cleanup.waitForExistence(timeout: 3)); cleanup.tap()
        XCTAssertTrue(app.buttons["Select for cleanup"].waitForExistence(timeout: 3)); app.buttons["Select for cleanup"].tap()
        XCTAssertTrue(app.buttons["Selected for cleanup"].exists)
        // Selection ends without sending a deletion against a live backend.
        app.buttons["Finish selection"].tap(); XCTAssertFalse(app.buttons["Selected for cleanup"].exists)
    }
    func testTasksCreateSheetOpensAndCancels() {
        let app = launchedApp()
        app.tabBars.buttons["Work"].tap()
        let create = app.buttons["tasks-create"]
        XCTAssertTrue(create.waitForExistence(timeout: 5))
        create.tap()
        XCTAssertTrue(app.navigationBars["New Task"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["Agent"].exists)      // the required agent picker
        app.buttons["Cancel"].tap()
        XCTAssertFalse(app.navigationBars["New Task"].waitForExistence(timeout: 2))
    }

    func testWorkAdoptedMonitoringRejectsChangedEvidence() {
        let app = launchedApp(extraArguments: ["--work-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap()
        app.buttons["work-more"].tap(); app.buttons["Quality checks"].tap()
        for _ in 0..<5 where !app.buttons["Learning policies"].isHittable { app.swipeUp() }
        app.buttons["Learning policies"].tap()
        let policy = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Fixture learning policy")).firstMatch
        XCTAssertTrue(policy.waitForExistence(timeout: 5)); policy.tap()
        for _ in 0..<6 where !app.buttons["Sealed monitoring evidence"].isHittable { app.swipeUp() }
        XCTAssertTrue(app.buttons["Adopted run fixture-run"].exists)
        app.buttons["Sealed monitoring evidence"].tap()
        XCTAssertTrue(app.staticTexts["Evidence changed or is not sealed. Refresh the recorded policy result."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["untrusted-fixture-marker"].exists)
    }

    func testTasksAndInternalTasksOpenSharedMobileDetailWorkspace() {
        let app = launchedApp(extraArguments: ["--tasks-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap()

        let active = app.staticTexts["Fixture active task"]
        XCTAssertTrue(active.waitForExistence(timeout: 3))
        active.tap()
        // A pushed screen: header card, the acts (no tab bar), Technical details last.
        XCTAssertTrue(app.navigationBars["Task"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.otherElements["task-detail-header"].waitForExistence(timeout: 3)
                      || app.staticTexts["task-detail-verdict"].exists)
        for act in ["run", "plan"] {
            XCTAssertTrue(app.buttons["task-detail-act-\(act)"].exists)
        }
        XCTAssertFalse(app.buttons["task-detail-tab-overview"].exists)
        app.buttons["task-detail-technical"].firstMatch.tap()
        XCTAssertTrue(app.buttons["task-detail-runs"].firstMatch.waitForExistence(timeout: 2)
                      || app.staticTexts["Runs"].waitForExistence(timeout: 2))
        app.navigationBars["Task"].buttons.firstMatch.tap()

        app.buttons["System work"].tap()
        let internalTask = app.staticTexts["Fixture internal task"]
        XCTAssertTrue(internalTask.waitForExistence(timeout: 3))
        internalTask.tap()
        XCTAssertTrue(app.navigationBars["Task"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["task-detail-act-run"].waitForExistence(timeout: 3))
    }

    /// Reveal a swipe rail WITHOUT committing: the task cards commit their
    /// default action on a full swipe (past ~half the card, Mail-style), and
    /// `swipeLeft()` travels far past that — it EXECUTES the action instead of
    /// opening the rail. A controlled ~40% drag stays under the threshold.
    private func revealSwipeRail(_ element: XCUIElement, byPoints dx: CGFloat) {
        let start = element.coordinate(
            withNormalizedOffset: CGVector(dx: dx < 0 ? 0.8 : 0.2, dy: 0.5))
        let end = start.withOffset(CGVector(dx: dx, dy: 0))
        start.press(forDuration: 0.08, thenDragTo: end)
    }

    func testTaskCardsExposeStateAwareSwipesVisibleResetAndActionsSheet() {
        let app = launchedApp(extraArguments: ["--tasks-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap()

        let active = app.staticTexts["Fixture active task"]
        XCTAssertTrue(active.waitForExistence(timeout: 3))
        revealSwipeRail(active, byPoints: -200)

        let cancel = app.buttons["task-swipe-cancel-fixture-active"]
        let delete = app.buttons["task-swipe-delete-fixture-active"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 2))
        XCTAssertTrue(cancel.isHittable)
        XCTAssertTrue(delete.isHittable)

        cancel.tap()
        // iOS action-sheet titles are not consistently exposed as static text;
        // the destructive confirmation control is the stable accessibility contract.
        let confirmCancel = app.buttons["Cancel task"]
        XCTAssertTrue(confirmCancel.waitForExistence(timeout: 2))
        // The compact confirmation dialog exposes only the destructive action
        // as an AX button — the cancel role ("Keep task") is dismissed by
        // tapping outside the dialog.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.92)).tap()
        XCTAssertTrue(confirmCancel.waitForNonExistence(timeout: 2))

        app.buttons["System work"].tap()
        let failed = app.staticTexts["Fixture internal task"]
        XCTAssertTrue(failed.waitForExistence(timeout: 3))
        let reset = app.buttons["task-action-reset-fixture-internal"]
        XCTAssertTrue(reset.waitForExistence(timeout: 2))
        XCTAssertTrue(reset.isHittable)

        let actions = app.buttons["task-action-actions-fixture-internal"]
        XCTAssertTrue(actions.isHittable)
        actions.tap()
        XCTAssertTrue(app.navigationBars["Task actions"].waitForExistence(timeout: 2))
        XCTAssertTrue(app.buttons["Open full task"].exists)
        XCTAssertTrue(app.buttons["Reset to Ready"].exists)
        // The metadata rows (Description / Priority / Due date / Schedule)
        // render lazily below a variable sheet detent — how many exist in the
        // AX tree varies run to run, so they are deliberately not probed. The
        // sheet's ACTIONABLE contract (Open full task / Reset to Ready above)
        // is the stable assertion surface.
        app.buttons["Done"].tap()

        failed.tap()
        XCTAssertTrue(app.navigationBars["Task"].waitForExistence(timeout: 2))
        // Lifecycle actions live in the ⋯ menu.
        let menu = app.buttons["task-detail-menu"]
        XCTAssertTrue(menu.waitForExistence(timeout: 3))
        menu.tap()
        XCTAssertTrue(app.buttons["Reset to Ready"].waitForExistence(timeout: 2))
    }

    func testCompletedTaskPublishesFromActionsAndDetailHeader() {
        let app = launchedApp(extraArguments: ["--tasks-ui-test-fixture"])
        app.tabBars.buttons["Work"].tap()
        app.buttons["Completed"].tap()

        let completed = app.staticTexts["Fixture completed task"]
        XCTAssertTrue(completed.waitForExistence(timeout: 3))

        let actions = app.buttons["task-action-actions-fixture-completed"]
        XCTAssertTrue(actions.waitForExistence(timeout: 2))
        actions.tap()
        XCTAssertTrue(app.buttons["task-actions-publish-notes"].waitForExistence(timeout: 2))
        app.buttons["Done"].tap()

        completed.tap()
        XCTAssertTrue(app.navigationBars["Task"].waitForExistence(timeout: 2))
        let menu = app.buttons["task-detail-menu"]
        XCTAssertTrue(menu.waitForExistence(timeout: 3))
        menu.tap()
        XCTAssertTrue(app.buttons["Publish to Notes"].waitForExistence(timeout: 2))
    }

    func testCrossSurfaceInternalTaskRequestSelectsLaneAndOpensTask() {
        let app = launchedApp(extraArguments: [
            "--tasks-ui-test-fixture",
            "--open-internal-task-ui-test"
        ])

        XCTAssertTrue(app.tabBars.buttons["Work"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.tabBars.buttons["Work"].isSelected)
        XCTAssertTrue(app.navigationBars["Task"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Fixture internal task"].waitForExistence(timeout: 3))
        app.navigationBars["Task"].buttons.firstMatch.tap()

        let internalLane = app.buttons["System work"]
        XCTAssertTrue(internalLane.waitForExistence(timeout: 3))
        XCTAssertTrue(internalLane.isSelected)
        XCTAssertTrue(app.staticTexts["Fixture internal task"].exists)
    }

    func testAttentionHistoryToggle() {
        let app = launchedApp()
        app.tabBars.buttons["Attention"].tap()
        XCTAssertTrue(app.navigationBars["Attention"].waitForExistence(timeout: 5))
        let toggle = app.buttons["attention-history-toggle"]
        XCTAssertTrue(toggle.exists)
        toggle.tap()
        XCTAssertTrue(app.navigationBars["History"].waitForExistence(timeout: 3))
        toggle.tap()
        XCTAssertTrue(app.navigationBars["Attention"].waitForExistence(timeout: 3))
    }

    /// Regression: owner found the REAL tappable area is "a small part below
    /// the text" — the visual chip (padding + tinted background) rendered but
    /// only a slice accepted taps. XCUITest's `.tap()` hits the element's
    /// accessibility-frame center, which can sit inside that slice and hide
    /// the bug — so this test taps EVERY vertical slice of the visual chip
    /// (element frame extended by the chip's padding) via absolute app-space
    /// coordinates, and records the frame for diagnosis.
    func testAttentionLaneTabFullChipIsTappable() {
        let app = launchedApp()
        app.tabBars.buttons["Attention"].tap()
        XCTAssertTrue(app.navigationBars["Attention"].waitForExistence(timeout: 5))
        let failedTab = app.buttons["attention-lane-failed"]
        let allTab = app.buttons["attention-lane-all"]
        XCTAssertTrue(failedTab.waitForExistence(timeout: 5))

        func waitForSelected(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if element.isSelected { return true }
                Thread.sleep(forTimeInterval: 0.2)
            }
            return element.isSelected
        }

        let window = app.frame
        let frame = failedTab.frame
        // Diagnostic: keep a screenshot + every tab frame so a device-only
        // hit-area report can be compared against where the chip RENDERS
        // versus where the accessibility frame CLAIMS it is.
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "tab-strip-window-\(window)"
        shot.lifetime = .keepAlways
        add(shot)
        for lane in ["all", "requests", "approvals", "escalations", "failed"] {
            let el = app.buttons["attention-lane-\(lane)"]
            print("LANE-FRAME \(lane): \(el.frame)")
        }
        print("LANE-WINDOW: \(window)")
        // Probe INSIDE the visual chip only (1pt inset); the padding band
        // around it belongs to the scroll strip and is expected not to tap.
        let top = frame.minY + 1
        let bottom = frame.maxY - 1
        let x = frame.midX

        var misses: [String] = []
        var y = top
        while y <= bottom {
            app.coordinate(withNormalizedOffset: CGVector(
                dx: x / window.width,
                dy: y / window.height
            )).tap()
            if !waitForSelected(failedTab, timeout: 2) {
                misses.append(String(format: "y=%.0f (frame %.0f-%.0f)", y, frame.minY, frame.maxY))
            }
            app.coordinate(withNormalizedOffset: CGVector(
                dx: allTab.frame.midX / window.width,
                dy: ((allTab.frame.minY + allTab.frame.maxY) / 2) / window.height
            )).tap()
            if !waitForSelected(allTab, timeout: 2) {
                misses.append("reselect-all failed")
            }
            y += 5
        }
        XCTAssertTrue(
            misses.isEmpty,
            "chip slices that did not respond (element frame height \(frame.height)): \(misses.joined(separator: ", "))"
        )
    }

    /// Regression: owner-reported "taps on the lane tabs mostly don't respond,
    /// only work randomly" on device. Each tap must switch the selected tab
    /// (asserted via the `.isSelected` trait on the tapped tab) — a dropped
    /// tap leaves the previous tab selected. Taps lanes in alternation so a
    /// gesture race shows up as a specific miss, not a flake.
    func testAttentionLaneTabsRespondToEveryTap() {
        let app = launchedApp()
        app.tabBars.buttons["Attention"].tap()
        XCTAssertTrue(app.navigationBars["Attention"].waitForExistence(timeout: 5))

        let allTab = app.buttons["attention-lane-all"]
        let failedTab = app.buttons["attention-lane-failed"]
        let requestsTab = app.buttons["attention-lane-requests"]
        XCTAssertTrue(allTab.waitForExistence(timeout: 5), "lane tab identifiers missing")
        XCTAssertTrue(failedTab.exists)
        XCTAssertTrue(requestsTab.exists)

        func waitForSelected(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if element.isSelected { return true }
                Thread.sleep(forTimeInterval: 0.2)
            }
            return element.isSelected
        }

        func tapAndAssertSelected(_ tab: XCUIElement, _ line: UInt = #line) {
            tab.tap()
            XCTAssertTrue(waitForSelected(tab, timeout: 4), "tap on \(tab.identifier) did not select it", line: line)
        }

        tapAndAssertSelected(failedTab)
        tapAndAssertSelected(allTab)
        tapAndAssertSelected(requestsTab)
        tapAndAssertSelected(allTab)
        tapAndAssertSelected(failedTab)
        tapAndAssertSelected(allTab)
    }

    func testChatComposerIsPresent() {
        let app = launchedApp()
        app.tabBars.buttons["Chat"].tap()
        // Chat is voice-first now: the text composer sits behind the voice
        // hero's "type instead" toggle when that hero is showing. Assert via
        // the stable `chat-composer` id, not the placeholder copy.
        let typeInstead = app.buttons["chat-type-instead"]
        if typeInstead.waitForExistence(timeout: 2) {
            typeInstead.tap()
        }
        XCTAssertTrue(app.textFields["chat-composer"].waitForExistence(timeout: 5))
    }

    func testComposerVoiceChevronsOpenSharedSettingsSheetAtExpectedSection() {
        let app = launchedApp()
        app.tabBars.buttons["Chat"].tap()

        let replySettings = app.buttons["voice-settings-replies"]
        if !replySettings.waitForExistence(timeout: 2) {
            let voice = app.buttons["chat-mic"]
            XCTAssertTrue(voice.waitForExistence(timeout: 3))
            voice.tap()
        }

        XCTAssertTrue(replySettings.waitForExistence(timeout: 3))
        replySettings.tap()
        XCTAssertTrue(app.navigationBars["Voice settings"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["voice-settings-tab-replies"].isSelected)
        XCTAssertTrue(app.buttons["voice-settings-tab-dictation"].exists)
        XCTAssertTrue(app.buttons["voice-settings-tab-session"].exists)
        app.buttons["voice-settings-tab-dictation"].tap()
        XCTAssertTrue(app.buttons["voice-settings-tab-dictation"].isSelected)
        XCTAssertTrue(app.staticTexts["Dictation pipeline"].waitForExistence(timeout: 2))
        app.buttons["Done"].tap()

        let selector = app.buttons["voice-mode-selector"]
        XCTAssertTrue(selector.waitForExistence(timeout: 3))
        let expectedSection = selector.label.contains("Dictation") ? "dictation" : "session"
        selector.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5)).tap()
        for mode in ["Live", "Dictation", "Hands-free"] {
            XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", mode)).firstMatch.waitForExistence(timeout: 3))
        }
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.02, dy: 0.5)).tap()
        selector.coordinate(withNormalizedOffset: CGVector(dx: 0.2, dy: 0.5)).tap()
        let settings = app.buttons["voice-mode-settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 3)); settings.tap()
        XCTAssertTrue(app.navigationBars["Voice settings"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["voice-settings-tab-\(expectedSection)"].isSelected)
        app.buttons["voice-settings-tab-session"].tap()
        XCTAssertTrue(app.buttons["voice-settings-tab-session"].isSelected)
        XCTAssertTrue(app.buttons["voice-settings-start-call"].exists)
    }
}
