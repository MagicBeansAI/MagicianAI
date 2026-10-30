import XCTest
@testable import Magician

/// Port of `ui/unified-ui/src/lib/magician/tasks/taskVerdict.test.ts`. The cases
/// that matter most are the precedence pairs — the branch order in
/// `TaskVerdict.derive` is a priority, not a list, and an inversion is silent.
final class TaskVerdictTests: XCTestCase {
    /// The epoch, as the web fixture's `now: 0`. Every instant below is
    /// expressed relative to it so the arithmetic reads the same as the
    /// TypeScript it came from.
    private let epoch = Date(timeIntervalSince1970: 0)

    private func at(_ seconds: TimeInterval) -> Date {
        Date(timeIntervalSince1970: seconds)
    }

    /// The base fixture. `currentStep` (4) and `totalSteps` (7) differ, and
    /// differ from every duration below, so a swapped pair cannot pass.
    private func base(
        status: String = "running",
        queuedFor: QueuedReason? = nil,
        attention: VerdictAttention? = nil,
        error: String? = nil,
        currentStep: Int? = 4,
        totalSteps: Int? = 7,
        currentStepLabel: String? = "Searching memory for \"quarterly plan\"",
        elapsed: TimeInterval? = nil,
        lastProgressAt: Date? = nil,
        synthesisPending: Bool = false,
        now: Date? = nil
    ) -> VerdictInput {
        VerdictInput(
            status: status,
            queuedFor: queuedFor,
            attention: attention,
            error: error,
            currentStep: currentStep,
            totalSteps: totalSteps,
            currentStepLabel: currentStepLabel,
            elapsed: elapsed,
            lastProgressAt: lastProgressAt,
            synthesisPending: synthesisPending,
            now: now ?? epoch
        )
    }

    // MARK: - Running

    func testReportsARunningTaskWithItsStepAndWhatItIsDoing() {
        let v = TaskVerdict.derive(base())
        XCTAssertEqual(v.state, .running)
        XCTAssertEqual(v.headline, "Running · step 4 of 7")
        XCTAssertEqual(v.detail, "Searching memory for \"quarterly plan\"")
    }

    func testDropsTheDenominatorWhenTheRunWasNeverPlanned() {
        XCTAssertEqual(TaskVerdict.derive(base(totalSteps: nil)).headline, "Running · step 4")
    }

    func testDropsTheStepPhraseEntirelyWhenThereIsNoStep() {
        // The empty-string contract of `stepPhrase`: one expression both
        // interpolates it and tests it for presence, so "no step" must not
        // render `Running · `.
        let v = TaskVerdict.derive(base(currentStep: nil, totalSteps: nil, currentStepLabel: nil))
        XCTAssertEqual(v.headline, "Running")
        XCTAssertEqual(v.detail, "Working")
    }

    // MARK: - Precedence

    func testRanksWaitingOnYouAboveFinished() {
        let v = TaskVerdict.derive(base(
            status: "finished",
            attention: VerdictAttention(source: .planApproval, summary: nil, raisedAt: nil)
        ))
        XCTAssertEqual(v.state, .waiting)
        XCTAssertEqual(v.detail, "Approve the plan before it can run")
    }

    func testRanksWaitingOnYouAboveFailedSoThePersonBlockingItIsNotHidden() {
        let v = TaskVerdict.derive(base(
            status: "failed",
            attention: VerdictAttention(source: .escalation, summary: nil, raisedAt: nil),
            error: "boom"
        ))
        XCTAssertEqual(v.state, .waiting)
        XCTAssertEqual(v.detail, "The run got stuck and needs a decision")
    }

    func testRanksWaitingOnYouAboveStalled() {
        let v = TaskVerdict.derive(base(
            attention: VerdictAttention(source: .clarification, summary: nil, raisedAt: nil),
            lastProgressAt: epoch,
            now: at(6 * 60)
        ))
        XCTAssertEqual(v.state, .waiting)
        XCTAssertEqual(v.detail, "Answer a question so planning can finish")
    }

    func testRanksFailureAboveStalled() {
        let v = TaskVerdict.derive(base(
            status: "failed", error: "boom", lastProgressAt: epoch, now: at(6 * 60)
        ))
        XCTAssertEqual(v.state, .failed)
    }

    // MARK: - Attention copy

    func testHasCopyForEveryHitlSource() {
        // Pin the reviewed wire vocabulary, including service health.
        // `attentionCopy` also has an exhaustive switch with no default.
        // The sweep checks that every source renders copy without leaking
        // its wire enum value.
        XCTAssertEqual(HitlSource.allCases.count, 10)
        for source in HitlSource.allCases {
            let v = TaskVerdict.derive(base(
                attention: VerdictAttention(source: source, summary: nil, raisedAt: nil)
            ))
            XCTAssertEqual(v.state, .waiting, "\(source)")
            XCTAssertFalse(v.detail.isEmpty, "\(source)")
            XCTAssertFalse(
                v.detail.lowercased().contains(source.rawValue),
                "\(source) leaked its enum value into the detail line"
            )
        }
    }

    func testNormalizesWireSourcesLikeTheWebAdapters() {
        // Mirrors `normalizeHitlSource` in `ui/unified-ui/src/lib/hitl/adapters.ts`.
        XCTAssertEqual(HitlSource.normalized("mcp_oauth"), .mcpOauth)
        XCTAssertEqual(HitlSource.normalized("service_health"), .serviceHealth)
        XCTAssertEqual(HitlSource.normalized("  plan_approval "), .planApproval)
        XCTAssertEqual(HitlSource.normalized("primitive"), .agentic)
        XCTAssertEqual(HitlSource.normalized("inner_loop"), .agentic)
        XCTAssertNil(HitlSource.normalized("telepathy"))
        XCTAssertNil(HitlSource.normalized("   "))
        XCTAssertNil(HitlSource.normalized(nil))
        for source in HitlSource.allCases {
            XCTAssertEqual(HitlSource.normalized(source.rawValue), source)
        }
    }

    func testPrefersTheAttentionSummaryOverTheGenericPerSourceCopy() {
        let v = TaskVerdict.derive(base(
            attention: VerdictAttention(
                source: .clarification, summary: "Which quarter?", raisedAt: nil
            )
        ))
        XCTAssertEqual(v.detail, "Which quarter?")
    }

    // MARK: - Durations

    func testSaysHowLongYouHaveBeenBlockedBecause4mAnd3hMeanDifferentThings() {
        let v = TaskVerdict.derive(base(
            attention: VerdictAttention(source: .planApproval, summary: nil, raisedAt: epoch),
            now: at(4 * 60)
        ))
        XCTAssertEqual(v.headline, "Waiting on you · 4m")
    }

    func testOmitsTheDurationRatherThanInventingOneWhenTheAskHasNoTimestamp() {
        let v = TaskVerdict.derive(base(
            attention: VerdictAttention(source: .planApproval, summary: nil, raisedAt: nil)
        ))
        XCTAssertEqual(v.headline, "Waiting on you")
    }

    func testOmitsTheDurationRatherThanRenderingANegativeOneWhenTheClocksDisagree() {
        // `raisedAt` is the server's clock and `now` is the device's, so skew is
        // routine. Every negative value passes the `< 60s` test, so an
        // unguarded version prints `-5s` and `-5400s` — a number worse than no
        // number. This is the same answer as a missing timestamp: we do not know.
        func skewed(_ raisedAt: TimeInterval) -> String {
            TaskVerdict.derive(base(
                attention: VerdictAttention(
                    source: .planApproval, summary: nil, raisedAt: at(raisedAt)
                ),
                now: epoch
            )).headline
        }

        XCTAssertEqual(skewed(5), "Waiting on you")
        XCTAssertEqual(skewed(90 * 60), "Waiting on you")
        // The boundary the guard must not swallow: no skew at all is 0s, a real
        // reading, not an unknown one.
        XCTAssertEqual(skewed(0), "Waiting on you · 0s")
    }

    func testDropsANegativeElapsedTimeOnFinishedAndCancelledTooForOneRuleNotTwo() {
        XCTAssertEqual(
            TaskVerdict.derive(base(status: "finished", elapsed: -1)).headline, "Finished"
        )
        XCTAssertEqual(
            TaskVerdict.derive(base(status: "cancelled", elapsed: -1)).headline, "Cancelled"
        )
    }

    func testTreatsANonFiniteElapsedTimeAsUnknownRatherThanRenderingNaNAtTheReader() {
        // In the web these reach the tier tests and print `Finished · NaNh
        // NaNm`. In Swift `Int(nan)` and `Int(infinity)` TRAP, so the same guard
        // is the difference between a wrong line and a crash.
        for elapsed in [TimeInterval.nan, .infinity, .greatestFiniteMagnitude] {
            XCTAssertEqual(
                TaskVerdict.derive(base(status: "finished", elapsed: elapsed)).headline,
                "Finished", "\(elapsed)"
            )
            XCTAssertEqual(
                TaskVerdict.derive(base(status: "cancelled", elapsed: elapsed)).headline,
                "Cancelled", "\(elapsed)"
            )
        }

        func blocked(_ raisedAt: TimeInterval) -> String {
            TaskVerdict.derive(base(
                attention: VerdictAttention(
                    source: .planApproval, summary: nil, raisedAt: at(raisedAt)
                ),
                now: epoch
            )).headline
        }

        XCTAssertEqual(blocked(.nan), "Waiting on you")
        // `0 - -infinity` is `+infinity`, which clears the negative guard and
        // needs the range check of its own.
        XCTAssertEqual(blocked(-.infinity), "Waiting on you")
    }

    func testScalesTheDurationUnitSoALongBlockReadsAsAbandonedRatherThanAs180m() {
        func blocked(_ seconds: TimeInterval) -> String {
            TaskVerdict.derive(base(
                attention: VerdictAttention(source: .planApproval, summary: nil, raisedAt: epoch),
                now: at(seconds)
            )).headline
        }

        XCTAssertEqual(blocked(45), "Waiting on you · 45s")
        XCTAssertEqual(blocked(59), "Waiting on you · 59s")
        // Each bucket boundary, so a separator or ordering slip cannot hide.
        XCTAssertEqual(blocked(60), "Waiting on you · 1m")
        XCTAssertEqual(blocked(61), "Waiting on you · 1m 1s")
        XCTAssertEqual(blocked(192), "Waiting on you · 3m 12s")
        XCTAssertEqual(blocked(59 * 60 + 59), "Waiting on you · 59m 59s")
        XCTAssertEqual(blocked(60 * 60), "Waiting on you · 1h")
        XCTAssertEqual(blocked(61 * 60), "Waiting on you · 1h 1m")
        XCTAssertEqual(blocked(80 * 60), "Waiting on you · 1h 20m")
        XCTAssertEqual(blocked(3 * 60 * 60), "Waiting on you · 3h")
        // Every other hour-scale case above divides evenly into minutes, so all
        // of them pass against an implementation that appends a non-zero seconds
        // component. This one does not: the hour bucket drops seconds always,
        // and no duration is ever three units.
        XCTAssertEqual(blocked(80 * 60 + 5), "Waiting on you · 1h 20m")
    }

    func testDurationIfKnownIsTheTotalFunctionAndAnswersNilForEveryUnknown() {
        // Called directly, because the exported/private split is the contract:
        // the Run act's summary will render these strings too.
        XCTAssertNil(TaskVerdict.durationIfKnown(nil))
        XCTAssertNil(TaskVerdict.durationIfKnown(-0.001))
        XCTAssertNil(TaskVerdict.durationIfKnown(.nan))
        XCTAssertNil(TaskVerdict.durationIfKnown(.infinity))
        XCTAssertEqual(TaskVerdict.durationIfKnown(0), "0s")
        XCTAssertEqual(TaskVerdict.durationIfKnown(192), "3m 12s")
    }

    // MARK: - Stalled

    func testReportsStalledWhenProgressHasStoppedButStatusIsStillRunning() {
        let v = TaskVerdict.derive(base(lastProgressAt: epoch, now: at(6 * 60)))
        XCTAssertEqual(v.state, .stalled)
        XCTAssertEqual(v.headline, "Stalled · no progress for 6m")
        // No denominator: `step 4 of 7` frames the step as progress, which is
        // the opposite of what the headline just said.
        XCTAssertEqual(v.detail, "Still on step 4: Searching memory for \"quarterly plan\"")
    }

    func testNamesTheStalledStepItCannotNumberRatherThanPrintingANilOne() {
        // A live step title with no index is a real shape — an unplanned run has
        // one — and the two fields are independently nullable.
        let v = TaskVerdict.derive(base(
            currentStep: nil, totalSteps: nil, lastProgressAt: epoch, now: at(6 * 60)
        ))
        XCTAssertEqual(v.state, .stalled)
        XCTAssertEqual(v.detail, "Still on this step: Searching memory for \"quarterly plan\"")
        XCTAssertFalse(v.detail.contains("nil"))
    }

    func testFallsBackToSayingNothingAdvancedWhenThereIsNoStepLabelAtAll() {
        let v = TaskVerdict.derive(base(
            currentStepLabel: nil, lastProgressAt: epoch, now: at(6 * 60)
        ))
        XCTAssertEqual(v.detail, "The run has not advanced")
    }

    func testReportsAStallExactlyAtTheThresholdSoCopyAndThresholdCannotDrift() {
        let v = TaskVerdict.derive(base(lastProgressAt: epoch, now: at(TaskVerdict.stallAfter)))
        XCTAssertEqual(v.state, .stalled)
        XCTAssertEqual(v.headline, "Stalled · no progress for 5m")
    }

    func testDoesNotReportAStallOneSecondUnderTheThreshold() {
        let v = TaskVerdict.derive(base(lastProgressAt: epoch, now: at(TaskVerdict.stallAfter - 1)))
        XCTAssertEqual(v.state, .running)
    }

    func testDoesNotReportAStallForANonRunningStatusHoweverOldTheProgressIs() {
        // The stall branch is gated on `running`; a completed task's stale
        // progress timestamp is not a wedged run.
        let v = TaskVerdict.derive(base(status: "finished", lastProgressAt: epoch, now: at(60 * 60)))
        XCTAssertEqual(v.state, .finished)
    }

    func testTreatsAnUnmeasurableSilenceAsRunningRatherThanTrapping() {
        // `+infinity` clears any threshold. The web renders `Infinityh
        // Infinitym`; Swift's `Int(_:)` would trap, so the measurable guard is
        // load-bearing here in a way it is not in the source language.
        let v = TaskVerdict.derive(base(
            lastProgressAt: Date(timeIntervalSince1970: -.infinity), now: epoch
        ))
        XCTAssertEqual(v.state, .running)
    }

    // MARK: - Terminal states

    func testCarriesTheErrorTextOnFailureRatherThanPointingElsewhere() {
        let v = TaskVerdict.derive(base(
            status: "failed", error: "Couldn't read revenue.csv — file not found"
        ))
        XCTAssertEqual(v.headline, "Failed · at step 4 of 7")
        XCTAssertEqual(v.detail, "Couldn't read revenue.csv — file not found")
    }

    func testSaysSoWhenAFailureRecordedNoErrorMessage() {
        XCTAssertEqual(
            TaskVerdict.derive(base(status: "failed", error: nil)).detail,
            "No error message was recorded"
        )
    }

    func testDistinguishesAnInstantFinishFromOneWithNoTimingRecorded() {
        XCTAssertEqual(
            TaskVerdict.derive(base(status: "finished", elapsed: 0)).headline, "Finished · 0s"
        )
        XCTAssertEqual(
            TaskVerdict.derive(base(status: "finished", elapsed: nil)).headline, "Finished"
        )
    }

    func testReportsACancelledRunWithHowLongItRanAndWhereYouStoppedIt() {
        let v = TaskVerdict.derive(base(status: "cancelled", elapsed: 100))
        XCTAssertEqual(v.state, .cancelled)
        XCTAssertEqual(v.headline, "Cancelled · after 1m 40s")
        XCTAssertEqual(v.detail, "You stopped this at step 4 of 7")

        XCTAssertEqual(
            TaskVerdict.derive(base(status: "cancelled", elapsed: nil)).headline, "Cancelled"
        )
        XCTAssertEqual(
            TaskVerdict.derive(base(status: "cancelled", currentStep: nil, totalSteps: nil)).detail,
            "You stopped this"
        )
    }

    func testReportsAnUnstartedTaskAsNotStartedRatherThanAsStarted() {
        let v = TaskVerdict.derive(base(status: "queued", queuedFor: .start))
        XCTAssertEqual(v.state, .queued)
        XCTAssertEqual(v.headline, "Queued")
        XCTAssertEqual(v.detail, "Not started yet")
    }

    /// One state, four statuses, four sentences. `pending` and `ready` are not
    /// waiting for a runner either: V3 queues nothing, both wait to be started.
    func testGivesEachReasonAQueuedTaskHasNotStartedItsOwnSecondLine() {
        func detail(_ reason: QueuedReason?) -> String {
            TaskVerdict.derive(base(status: "queued", queuedFor: reason)).detail
        }
        XCTAssertEqual(detail(.start), "Not started yet")
        XCTAssertEqual(detail(.approved), "Plan approved, not started yet")
        XCTAssertEqual(detail(.plan), "Working out a plan before it starts")
        XCTAssertEqual(detail(.schedule), "Scheduled to start later")

        // Distinct, not merely present.
        let lines = Set(QueuedReason.allCases.map { detail($0) })
        XCTAssertEqual(lines.count, 4)
        XCTAssertEqual(QueuedReason.allCases.count, 4)
        for line in lines { XCTAssertFalse(line.isEmpty) }
    }

    func testSaysLessRatherThanSomethingFalseWhenNothingNamedTheReason() {
        let v = TaskVerdict.derive(base(status: "queued", queuedFor: nil))
        XCTAssertEqual(v.state, .queued)
        XCTAssertEqual(v.detail, "Waiting to start")
        XCTAssertNotEqual(v.detail, "Not started yet")
    }

    /// Web `VERDICT_STATUS` (taskPanelModel.ts) over `normalizeV3TaskStatus`:
    /// what each wire status reads as, and why a queued one has not started.
    func testMapsEachTaskStatusToItsVerdictWordAndQueuedReason() {
        let table: [(String, String, QueuedReason?)] = [
            ("running", "running", nil), ("failed", "failed", nil), ("cancelled", "cancelled", nil),
            ("completed", "finished", nil), ("pending", "queued", .start), ("planning", "queued", .plan),
            ("ready", "queued", .approved), ("deferred", "queued", .schedule), ("sleeping", "queued", .schedule),
            ("paused", "paused", nil), ("waiting_for_user", "paused", nil), ("waiting_for_children", "running", nil),
            ("archived", "archived", nil),
            // An unknown status is `pending` (web): queued, never `Finished`.
            ("teleported", "queued", .start)
        ]
        for (wire, word, reason) in table {
            let mapped = TaskVerdict.verdictStatus(forTaskStatus: wire)
            XCTAssertEqual(mapped.word, word, wire)
            XCTAssertEqual(mapped.queuedFor, reason, wire)
        }
    }

    /// No queued shape claims capacity, and only `ready` says its plan is
    /// approved (web corpus eval).
    func testNeverClaimsCapacityAndOnlyAnApprovedPlanSaysItIsApproved() {
        for wire in ["pending", "planning", "ready", "deferred", "sleeping"] {
            let mapped = TaskVerdict.verdictStatus(forTaskStatus: wire)
            let v = TaskVerdict.derive(base(status: mapped.word, queuedFor: mapped.queuedFor))
            XCTAssertEqual(v.state, .queued, wire)
            XCTAssertNotEqual(v.detail, "Waiting for a free slot", wire)
            XCTAssertEqual(v.detail == "Plan approved, not started yet", wire == "ready", wire)
        }
    }

    func testReportsADeliberatePauseWithoutPretendingItIsQueued() {
        let v = TaskVerdict.derive(base(status: "paused"))
        XCTAssertEqual(v.state, .paused)
        XCTAssertEqual(v.headline, "Paused")
        XCTAssertEqual(v.detail, "Ready to resume when you are")
    }

    func testReportsArchivedWorkAsInactiveHistory() {
        let v = TaskVerdict.derive(base(status: "archived"))
        XCTAssertEqual(v.state, .archived)
        XCTAssertEqual(v.headline, "Archived")
        XCTAssertEqual(v.detail, "No longer active")
    }

    func testLeavesTheFinishedSecondLineEmptyForTheOutputSummaryToFill() {
        let v = TaskVerdict.derive(base(status: "finished", elapsed: 192))
        XCTAssertEqual(v.state, .finished)
        XCTAssertEqual(v.headline, "Finished · 3m 12s")
        // Pinned in both directions so filling it in has to be a deliberate
        // test change.
        XCTAssertEqual(v.detail, "")
    }

    func testFallsThroughToFinishedForAStatusItDoesNotModel() {
        // Every other case here names a status the chain checks, so all of them
        // would still pass if the last branch grew an `if` — silently deleting a
        // documented decision.
        let v = TaskVerdict.derive(base(status: "future_status", elapsed: 192))
        XCTAssertEqual(v.state, .finished)
        XCTAssertEqual(v.headline, "Finished · 3m 12s")
    }

    // MARK: - Finalizing (synthesis pending)

    func testReportsFinalizingWhenTheWorkIsDoneButTheAnswerIsStillBeingWritten() {
        let v = TaskVerdict.derive(base(status: "finished", elapsed: 192, synthesisPending: true))
        XCTAssertEqual(v.state, .finalizing)
        XCTAssertEqual(v.headline, "Preparing final result")
        XCTAssertFalse(v.detail.isEmpty)
    }

    func testStillFinishesWhenSynthesisIsNotPending() {
        let v = TaskVerdict.derive(base(status: "finished", elapsed: 192, synthesisPending: false))
        XCTAssertEqual(v.state, .finished)
        XCTAssertEqual(v.headline, "Finished · 3m 12s")
    }

    func testLetsFailureOutrankFinalizing() {
        let v = TaskVerdict.derive(base(status: "failed", error: "boom", synthesisPending: true))
        XCTAssertEqual(v.state, .failed)
    }

    func testLetsCancellationOutrankFinalizing() {
        let v = TaskVerdict.derive(base(status: "cancelled", synthesisPending: true))
        XCTAssertEqual(v.state, .cancelled)
    }

    func testLetsAWaitingAskOutrankFinalizing() {
        let ask = VerdictAttention(source: .approval, summary: nil, raisedAt: nil)
        let v = TaskVerdict.derive(base(status: "finished", attention: ask, synthesisPending: true))
        XCTAssertEqual(v.state, .waiting)
    }

    func testKeepsARunningTaskRunningEvenIfSynthesisIsFlagged() {
        let v = TaskVerdict.derive(base(status: "running", synthesisPending: true))
        XCTAssertEqual(v.state, .running)
    }

    func testStopsSpinningOnceTheFlagHasBeenSetForMoreThanTenMinutes() {
        let start = Date(timeIntervalSince1970: 1_000)
        XCTAssertFalse(SynthesisWait.isStale(since: start, now: start))
        XCTAssertFalse(SynthesisWait.isStale(since: start, now: start.addingTimeInterval(600)))
        XCTAssertTrue(SynthesisWait.isStale(since: start, now: start.addingTimeInterval(601)))
        // A device clock behind the server's instant is not stale.
        XCTAssertFalse(SynthesisWait.isStale(since: start, now: start.addingTimeInterval(-30)))
        XCTAssertEqual(SynthesisWait.elapsed(since: start, now: start.addingTimeInterval(-30)), 0)
    }

    // MARK: - Legibility

    func testEveryStateHasItsOwnMarkerGlyphSoGreyscaleStillDistinguishesThem() {
        // Every state has its own glyph. The severity bands are deliberately
        // shared, so if the glyphs collapse too the states become
        // indistinguishable without colour.
        XCTAssertEqual(VerdictState.allCases.count, 10)
        let markers = VerdictState.allCases.map(\.marker)
        XCTAssertEqual(Set(markers).count, markers.count, "two states share a marker glyph")
        XCTAssertFalse(markers.contains(where: \.isEmpty))
    }

    func testSeverityGroupsTheStatesTheDesignSaysAreAlike() {
        XCTAssertEqual(VerdictState.waiting.severity, .attention)
        XCTAssertEqual(VerdictState.stalled.severity, .attention)
        XCTAssertEqual(VerdictState.failed.severity, .failure)
        XCTAssertEqual(VerdictState.running.severity, .progress)
        XCTAssertEqual(VerdictState.paused.severity, .neutral)
        XCTAssertEqual(VerdictState.queued.severity, .neutral)
        // Not `.failure`: a task you stopped on purpose is okay. This is a known
        // and deliberate disagreement with the task-list status chip.
        XCTAssertEqual(VerdictState.cancelled.severity, .neutral)
        XCTAssertEqual(VerdictState.archived.severity, .neutral)
        XCTAssertEqual(VerdictState.finalizing.severity, .progress)
        XCTAssertEqual(VerdictState.finished.severity, .success)
    }

    func testHitlSourceRawValuesAreTheWireStringsSoDecodingIsTheKnownSourceCheck() {
        // `HitlSource(rawValue:)` replaces the web's `KNOWN_SOURCES` set. If a
        // raw value drifts from the wire spelling, an ask silently stops being
        // recognised and the verdict falls back to the task's status.
        XCTAssertEqual(
            Set(HitlSource.allCases.map(\.rawValue)),
            ["agentic", "user_request", "approval", "plan_approval",
             "clarification", "escalation", "diff_approval", "bot_auth", "service_health",
             "mcp_oauth"]
        )
        XCTAssertNil(HitlSource(rawValue: "planApproval"))
        XCTAssertNil(HitlSource(rawValue: "task_failed"))
    }
}
