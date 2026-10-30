import XCTest
@testable import Magician

/// Ports of the web act rules the task detail screen reads:
/// - `actOrder` from `ui/unified-ui/src/lib/magician/tasks/taskCapabilities.test.ts`
///   (`deriveActs` / `defaultOpenAct` are already in `TaskCapabilitiesTests`);
/// - `planSummary` / `runSummary` / `outputSummary` from `actSummaries.test.ts`;
/// - `outputKindOf` from `taskPanelModel.test.ts`.
final class TaskActsModelTests: XCTestCase {
    private let all: [ActId] = [.plan, .run, .output]

    private var subsets: [[ActId]] {
        all.reduce([[]]) { acc, act in acc + acc.map { $0 + [act] } }
    }

    // MARK: - actOrder

    func testPutsTheResultFirstForAFinishedTaskAndKeepsTheRestInLifecycleOrder() {
        XCTAssertEqual(TaskActs.actOrder(state: .finished, acts: all), [.output, .plan, .run])
        XCTAssertEqual(TaskActs.actOrder(state: .archived, acts: all), [.output, .plan, .run])
        XCTAssertEqual(TaskActs.actOrder(state: .finalizing, acts: all), [.output, .plan, .run])
    }

    func testPutsTheLiveRunFirstWhileItRunsAndTheRunFirstOnceItStoppedShort() {
        for state in [VerdictState.running, .stalled, .paused, .failed, .cancelled] {
            XCTAssertEqual(TaskActs.actOrder(state: state, acts: all), [.run, .plan, .output], state.rawValue)
        }
    }

    func testKeepsLifecycleOrderWhileWaitingOnTheReaderOrQueued() {
        XCTAssertEqual(TaskActs.actOrder(state: .waiting, acts: all), [.plan, .run, .output])
        XCTAssertEqual(TaskActs.actOrder(state: .queued, acts: all), [.plan, .run, .output])
    }

    func testLeavesTheOrderAloneWhenTheLeadingActIsAbsent() {
        XCTAssertEqual(TaskActs.actOrder(state: .finished, acts: [.plan, .run]), [.plan, .run])
        XCTAssertEqual(TaskActs.actOrder(state: .running, acts: [.plan, .output]), [.plan, .output])
    }

    func testIsAPermutationOfItsInputForEveryStateAndEverySubset() {
        for state in VerdictState.allCases {
            for acts in subsets {
                let ordered = TaskActs.actOrder(state: state, acts: acts)
                XCTAssertEqual(ordered.map(\.rawValue).sorted(), acts.map(\.rawValue).sorted(),
                               "\(state) · \(acts)")
            }
        }
    }

    func testLeadsWithTheActThatOpensByDefaultWheneverNoAskIsPending() {
        for state in VerdictState.allCases where state != .waiting && state != .queued {
            let opens = TaskActs.defaultOpenAct(state: state, acts: all, attention: nil)
            XCTAssertEqual(TaskActs.actOrder(state: state, acts: all).first, opens, state.rawValue)
        }
    }

    func testDelegatesPresenceToTheCapabilityRule() {
        XCTAssertEqual(
            TaskActs.deriveActs(ActCapabilities(hasPlanAct: false, hasRunAct: true, hasOutputAct: true)),
            [.run, .output]
        )
    }

    // MARK: - planSummary

    /// A real epoch, so an approval behind it is a real date.
    private let now = ISO8601DateFormatter().date(from: "2026-07-29T12:00:00Z")!

    private func plan(
        status: TaskPlanStatus? = .draft,
        approvedAgo: TimeInterval? = nil,
        questions: [String] = []
    ) -> PlanSummaryInput {
        PlanSummaryInput(
            status: status,
            approvedAt: approvedAgo.map { now.addingTimeInterval(-$0) },
            questions: questions,
            now: now
        )
    }

    func testQuotesTheFirstWaitingQuestionInsteadOfCountingThem() {
        XCTAssertEqual(ActSummaries.planSummary(plan(questions: ["Which quarter?", "Include forecasts?"])),
                       "2 questions waiting — \"Which quarter?\"")
    }

    func testUsesTheSingularNounWhenThereIsOneQuestion() {
        XCTAssertEqual(ActSummaries.planSummary(plan(questions: ["Which quarter?"])),
                       "1 question waiting — \"Which quarter?\"")
    }

    func testTrimsTheQuotedQuestion() {
        XCTAssertEqual(ActSummaries.planSummary(plan(questions: ["  Which quarter?\n"])),
                       "1 question waiting — \"Which quarter?\"")
    }

    func testFallsBackToTheBareCountWhenTheQuestionCarriesNoContent() {
        XCTAssertEqual(ActSummaries.planSummary(plan(questions: ["   ", "Include forecasts?"])), "2 questions waiting")
        XCTAssertEqual(ActSummaries.planSummary(plan(questions: [""])), "1 question waiting")
    }

    func testSaysADraftIsUnapprovedWhenNothingIsBeingAsked() {
        XCTAssertEqual(ActSummaries.planSummary(plan()), "draft, not yet approved")
    }

    func testSaysWhenThePlanWasApprovedRelativeToNow() {
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .approved, approvedAgo: 6 * 60)), "approved 6m ago")
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .approved, approvedAgo: 3 * 3_600)), "approved 3h ago")
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .approved, approvedAgo: 2 * 86_400)), "approved 2d ago")
    }

    func testSaysJustNowRatherThanNowAgoForAFreshApproval() {
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .approved, approvedAgo: 5)), "approved just now")
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .approved, approvedAgo: -60)), "approved just now")
    }

    func testNamesTheDateOnceTheApprovalIsOverAWeekOld() {
        let old = ActSummaries.planSummary(plan(status: .approved, approvedAgo: 30 * 86_400))
        XCTAssertTrue(old.hasPrefix("approved on "), old)
        XCTAssertFalse(old.hasSuffix(" ago"), old)
        XCTAssertGreaterThan(old.count, "approved on ".count)
    }

    func testOmitsTheApprovalTimeWhenItCannotBeRead() {
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .approved, approvedAgo: nil)), "approved")
    }

    func testRanksAWaitingQuestionAboveTheApproval() {
        let summary = ActSummaries.planSummary(plan(status: .approved, approvedAgo: 6 * 60, questions: ["Which quarter?"]))
        XCTAssertEqual(summary, "1 question waiting — \"Which quarter?\"")
        XCTAssertFalse(summary.contains("approved"))
    }

    func testReportsTheCurrentStatusEvenWhenThePlanWasApprovedEarlier() {
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .rejected, approvedAgo: 6 * 60)),
                       "rejected, so it will not run")
        XCTAssertEqual(ActSummaries.planSummary(plan(status: .draft, approvedAgo: 6 * 60)),
                       "draft, not yet approved")
    }

    func testRendersEveryStatusInTheReadersOwnWords() {
        let lines: [(TaskPlanStatus, String)] = [
            (.planning, "being planned"),
            (.eliciting, "gathering what it needs before planning"),
            (.draft, "draft, not yet approved"),
            (.approved, "approved"),
            (.rejected, "rejected, so it will not run"),
            (.failed, "planning failed")
        ]
        XCTAssertEqual(lines.count, TaskPlanStatus.allCases.count)
        for (status, expected) in lines {
            XCTAssertEqual(ActSummaries.planSummary(plan(status: status)), expected)
        }
        XCTAssertEqual(TaskPlanStatus(wire: " Approved "), .approved)
        XCTAssertNil(TaskPlanStatus(wire: "teleported"))
    }

    func testRendersNothingWhenTheStatusIsUnknown() {
        XCTAssertEqual(ActSummaries.planSummary(plan(status: nil)), "")
        XCTAssertEqual(ActSummaries.planSummary(plan(status: nil, questions: ["Which quarter?"])),
                       "1 question waiting — \"Which quarter?\"")
    }

    // MARK: - runSummary

    private func run(
        steps: Int = 14, retries: Int = 0, elapsed: TimeInterval? = nil,
        currentStep: Int? = nil, totalSteps: Int? = nil, events: Int = 0
    ) -> RunSummaryInput {
        RunSummaryInput(steps: steps, retries: retries, elapsed: elapsed,
                        currentStep: currentStep, totalSteps: totalSteps, events: events)
    }

    func testSummarisesAFinishedRunAndFoldsRetriesIntoTheirStep() {
        XCTAssertEqual(ActSummaries.runSummary(run(retries: 2, elapsed: 192)), "14 steps · 2 retries · 3m 12s")
        XCTAssertEqual(ActSummaries.runSummary(run(retries: 2, elapsed: 80 * 60)), "14 steps · 2 retries · 1h 20m")
    }

    func testOmitsRetriesWhenThereWereNone() {
        XCTAssertEqual(ActSummaries.runSummary(run(elapsed: 192)), "14 steps · 3m 12s")
    }

    func testUsesTheSingularForOneStepAndOneRetry() {
        XCTAssertEqual(ActSummaries.runSummary(run(steps: 1, retries: 1, elapsed: 192)), "1 step · 1 retry · 3m 12s")
    }

    func testShowsPositionInsteadOfTotalsWhileLive() {
        let live = ActSummaries.runSummary(run(currentStep: 4, totalSteps: 7))
        XCTAssertEqual(live, "step 4 of 7")
        XCTAssertFalse(live.contains("14"))
    }

    func testDropsTheDenominatorWhileLiveWhenTheRunWasNeverPlanned() {
        XCTAssertEqual(ActSummaries.runSummary(run(currentStep: 4, totalSteps: nil)), "step 4")
    }

    func testKeepsRetriesAndElapsedVisibleWhileLive() {
        XCTAssertEqual(ActSummaries.runSummary(run(retries: 2, elapsed: 100, currentStep: 4, totalSteps: 7)),
                       "step 4 of 7 · 2 retries · 1m 40s")
    }

    func testDropsTheElapsedSegmentRatherThanItsSeparatorWhenTheTimeCannotBeRead() {
        for elapsed in [nil, -1, .nan] as [TimeInterval?] {
            XCTAssertEqual(ActSummaries.runSummary(run(retries: 2, elapsed: elapsed)), "14 steps · 2 retries")
        }
    }

    func testKeepsAnInstantRunDistinctFromOneWithNoTimingRecorded() {
        XCTAssertEqual(ActSummaries.runSummary(run(elapsed: 0)), "14 steps · 0s")
        XCTAssertEqual(ActSummaries.runSummary(run(elapsed: nil)), "14 steps")
    }

    func testSaysARunHasNotStartedRatherThanRenderingAnEmptyLine() {
        XCTAssertEqual(ActSummaries.runSummary(run(steps: 0)), "not started")
    }

    func testCountsEventsUnderTheirOwnNounBesideTheSteps() {
        XCTAssertEqual(ActSummaries.runSummary(run(retries: 2, elapsed: 192, events: 217)),
                       "14 steps · 2 retries · 217 events · 3m 12s")
        XCTAssertEqual(ActSummaries.runSummary(run(currentStep: 4, totalSteps: 7, events: 217)),
                       "step 4 of 7 · 217 events")
    }

    func testUsesTheSingularForOneEvent() {
        XCTAssertEqual(ActSummaries.runSummary(run(events: 1)), "14 steps · 1 event")
    }

    func testDropsTheEventSegmentRatherThanRenderingZeroEvents() {
        let line = ActSummaries.runSummary(run(elapsed: 192, events: 0))
        XCTAssertEqual(line, "14 steps · 3m 12s")
        XCTAssertFalse(line.contains("event"))
    }

    // MARK: - outputSummary

    private func file(_ name: String, _ kind: OutputKind) -> ActOutputFile { ActOutputFile(name: name, kind: kind) }

    func testNamesOneFileAndCountsTheRestByKind() {
        XCTAssertEqual(ActSummaries.outputSummary([
            file("report.md", .document), file("revenue.png", .image), file("costs.png", .image)
        ]), "report.md and 2 images")
    }

    func testCountsTheWholeRemainder() {
        XCTAssertEqual(ActSummaries.outputSummary([
            file("report.md", .document), file("revenue.png", .image),
            file("costs.png", .image), file("margin.png", .image)
        ]), "report.md and 3 images")
    }

    func testNamesBothFilesWhenThereAreExactlyTwo() {
        XCTAssertEqual(ActSummaries.outputSummary([file("report.md", .document), file("notes.md", .document)]),
                       "report.md and notes.md")
    }

    func testNamesALoneFileOnItsOwn() {
        XCTAssertEqual(ActSummaries.outputSummary([file("report.md", .document)]), "report.md")
    }

    func testIsExplicitAboutHavingProducedNothing() {
        XCTAssertEqual(ActSummaries.outputSummary([]), "no output")
    }

    func testCountsASameKindRemainderByItsOwnWord() {
        XCTAssertEqual(ActSummaries.outputSummary([
            file("revenue.png", .image), file("report.md", .document), file("notes.md", .document)
        ]), "revenue.png and 2 documents")
    }

    func testFallsBackToANeutralPluralWhenTheRestDoNotShareAKind() {
        XCTAssertEqual(ActSummaries.outputSummary([
            file("report.md", .document), file("revenue.png", .image), file("raw.bin", .other)
        ]), "report.md and 2 other files")
    }

    func testGivesUnclassifiableFilesTheSameNeutralPlural() {
        XCTAssertEqual(ActSummaries.outputSummary([
            file("report.md", .document), file("raw.bin", .other), file("dump.bin", .other)
        ]), "report.md and 2 other files")
    }

    func testNamesTheFirstFileSoTheHeaderMatchesTheActsOrder() {
        let files = [file("revenue.png", .image), file("report.md", .document), file("costs.png", .image)]
        XCTAssertEqual(ActSummaries.outputSummary(files), "revenue.png and 2 other files")
        XCTAssertEqual(ActSummaries.outputSummary(files.reversed()), "costs.png and 2 other files")
    }

    // MARK: - outputKindOf

    func testClassifiesFromTheMediaTypeFirstAndTheExtensionOnlyWhenThereIsNone() {
        XCTAssertEqual(ActSummaries.outputKind(mediaType: "image/png", path: "chart.bin"), .image)
        XCTAssertEqual(ActSummaries.outputKind(mediaType: "application/pdf", path: "notes"), .document)
        XCTAssertEqual(ActSummaries.outputKind(mediaType: "application/json", path: "data.md"), .other)
        XCTAssertEqual(ActSummaries.outputKind(mediaType: nil, path: "revenue.PNG"), .image)
        XCTAssertEqual(ActSummaries.outputKind(mediaType: nil, path: "report.md"), .document)
        XCTAssertEqual(ActSummaries.outputKind(mediaType: "", path: "archive.tar.gz"), .other)
    }

    func testSummarisesTaskDeliverablesByTheirBasenameAndKind() {
        let artifacts = [
            TaskDetailArtifact(id: "1", relativePath: "outputs/report.md", mediaType: "text/markdown",
                               role: nil, audience: nil, sizeBytes: nil, bodySnippet: nil, sourceExecutionId: nil),
            TaskDetailArtifact(id: "2", relativePath: "outputs/a.png", mediaType: nil,
                               role: nil, audience: nil, sizeBytes: nil, bodySnippet: nil, sourceExecutionId: nil),
            TaskDetailArtifact(id: "3", relativePath: "outputs/b.png", mediaType: "image/png",
                               role: nil, audience: nil, sizeBytes: nil, bodySnippet: nil, sourceExecutionId: nil)
        ]
        XCTAssertEqual(ActSummaries.outputSummary(artifacts: artifacts), "report.md and 2 images")
    }
}
