import XCTest
@testable import Magician

/// Port of `ui/unified-ui/src/lib/magician/tasks/taskCapabilities.test.ts`.
final class TaskCapabilitiesTests: XCTestCase {
    /// Every act, and every subset of them. Both are derived rather than
    /// listed, so a fourth act cannot leave the sweeps below quietly covering
    /// three.
    private let all = TaskCapabilities.deriveActs(
        ActCapabilities(hasPlanAct: true, hasRunAct: true, hasOutputAct: true)
    )

    private var subsets: [[ActId]] {
        all.reduce([[]]) { acc, act in acc + acc.map { $0 + [act] } }
    }

    /// This array is not the exhaustiveness guard — a new `VerdictState`
    /// would not fail here, it would fail to compile `preferredAct`, whose
    /// `switch` has no `default`. What it covers is the rendering rule: every
    /// state opens an act the task actually has.
    private let states = VerdictState.allCases

    // MARK: - deriveActs

    func testGivesAPlannedTaskAllThreeActsInFixedOrder() {
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: true, hasRunAct: true, hasOutputAct: true)
            ),
            [.plan, .run, .output]
        )
    }

    func testOmitsAnActTheTaskDoesNotHaveRatherThanDisablingIt() {
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: false, hasRunAct: true, hasOutputAct: true)
            ),
            [.run, .output]
        )
    }

    func testKeepsOrderFixedRegardlessOfWhichActsExist() {
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: true, hasRunAct: false, hasOutputAct: true)
            ),
            [.plan, .output]
        )
    }

    func testReturnsNothingForATaskWithNoActsYet() {
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: false, hasRunAct: false, hasOutputAct: false)
            ),
            []
        )
    }

    func testMapsTheFourCombinationsTheCasesAboveLeaveOut() {
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: true, hasRunAct: false, hasOutputAct: false)
            ),
            [.plan]
        )
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: false, hasRunAct: true, hasOutputAct: false)
            ),
            [.run]
        )
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: false, hasRunAct: false, hasOutputAct: true)
            ),
            [.output]
        )
        XCTAssertEqual(
            TaskCapabilities.deriveActs(
                ActCapabilities(hasPlanAct: true, hasRunAct: true, hasOutputAct: false)
            ),
            [.plan, .run]
        )
    }

    func testEachCapabilityDrivesItsOwnActAndNoOther() {
        // Three flags and three acts is exactly the shape where a copy-paste in
        // the `switch` binds two acts to one flag and every all-true / all-false
        // fixture still passes. Each flag is therefore flipped alone.
        for act in ActId.allCases {
            let caps = ActCapabilities(
                hasPlanAct: act == .plan, hasRunAct: act == .run, hasOutputAct: act == .output
            )
            XCTAssertEqual(TaskCapabilities.deriveActs(caps), [act], "\(act)")
        }
    }

    func testActIdOrderIsTheLifecycleOrder() {
        // `allCases` replaces the web's separate `ORDER` array, so the order the
        // fallback below walks is pinned here rather than assumed.
        XCTAssertEqual(ActId.allCases, [.plan, .run, .output])
    }

    // MARK: - Titles

    func testNamesEveryActPinnedAgainstLiterals() {
        // Completeness is the compiler's job — the `title` switch has no
        // `default`, so an act joining the enum unnamed fails to build. What
        // that cannot catch is a title that is empty or attached to the wrong
        // act, so the words are pinned here against literals.
        XCTAssertEqual(ActId.plan.title, "Plan")
        XCTAssertEqual(ActId.run.title, "Run")
        XCTAssertEqual(ActId.output.title, "Output")
    }

    // MARK: - defaultOpenAct, by state

    func testOpensTheActEachVerdictStateIsAbout() {
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .waiting, acts: all, attention: nil), .plan)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .queued, acts: all, attention: nil), .plan)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .running, acts: all, attention: nil), .run)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .stalled, acts: all, attention: nil), .run)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .paused, acts: all, attention: nil), .run)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .failed, acts: all, attention: nil), .run)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .cancelled, acts: all, attention: nil), .run)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .archived, acts: all, attention: nil), .output)
        XCTAssertEqual(TaskCapabilities.defaultOpenAct(state: .finished, acts: all, attention: nil), .output)
    }

    func testFallsBackToAnEarlierActWhenTheOneTheStateIsAboutIsAbsent() {
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .finished, acts: [.plan, .run], attention: nil), .run
        )
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .finished, acts: [.plan], attention: nil), .plan
        )
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .running, acts: [.plan], attention: nil), .plan
        )
    }

    func testFallsForwardOnlyWhenTheTaskHasReachedNothingEarlier() {
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .waiting, acts: [.run, .output], attention: nil), .run
        )
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .queued, acts: [.output], attention: nil), .output
        )
    }

    func testPrefersTheEarlierActWhenTheMissingOneSitsBetweenTwoTheTaskHas() {
        // The only shape where the direction preference is observable: a Run act
        // that failed to load leaves Plan and Output around the gap (design §6).
        // A "nearest" rule would answer `.output` for both of these.
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .running, acts: [.plan, .output], attention: nil), .plan
        )
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .failed, acts: [.plan, .output], attention: nil), .plan
        )
    }

    func testOpensNothingWhenTheTaskHasNoActsAtAll() {
        for state in states {
            XCTAssertNil(TaskCapabilities.defaultOpenAct(state: state, acts: [], attention: nil), "\(state)")
            XCTAssertNil(
                TaskCapabilities.defaultOpenAct(state: state, acts: [], attention: .diffApproval),
                "\(state)"
            )
        }
    }

    func testNeverOpensAnActTheTaskDoesNotHaveForAnyState() {
        for state in states {
            for acts in subsets where !acts.isEmpty {
                let opened = TaskCapabilities.defaultOpenAct(state: state, acts: acts, attention: nil)
                XCTAssertNotNil(opened, "\(state) · \(acts)")
                XCTAssertTrue(acts.contains(opened!), "\(state) · \(acts) opened \(opened!)")
            }
        }
    }

    // MARK: - defaultOpenAct, routing by the ask

    /// Every HITL source, and the act its ask is answered in. Literals rather
    /// than the module's own mapping, which would assert only that it equals
    /// itself. Six of the eight are run-time asks — that imbalance is the whole
    /// reason the source is a parameter, so a lookup that ignored it would fail
    /// six ways.
    private let actForSource: [HitlSource: ActId] = [
        .planApproval: .plan,
        .clarification: .plan,
        .agentic: .run,
        .userRequest: .run,
        .approval: .run,
        .escalation: .run,
        .diffApproval: .run,
        .serviceHealth: .run,
        .botAuth: .run
    ]

    func testOpensTheActEachHitlSourceIsAnsweredIn() {
        // A literal table can go stale silently where a `Record` cannot, so it
        // is checked for completeness against the enum first.
        XCTAssertEqual(Set(actForSource.keys), Set(HitlSource.allCases))
        for source in HitlSource.allCases {
            // The state is `waiting` for all eight, which is exactly why the
            // state cannot decide this.
            XCTAssertEqual(
                TaskCapabilities.defaultOpenAct(state: .waiting, acts: all, attention: source),
                actForSource[source],
                "\(source)"
            )
        }
    }

    func testLetsTheAskOutrankTheLifecycleAsTheVerdictItselfDoes() {
        // `TaskVerdict.derive` cannot produce these pairings — an ask always
        // reports `waiting` — but the rule is stated rather than left to that
        // coincidence: the act holding the ask beats the act holding the state.
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .finished, acts: all, attention: .diffApproval), .run
        )
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(state: .running, acts: all, attention: .planApproval), .plan
        )
    }

    func testFallsBackFromTheActAnAskLivesInByTheSameRuleAsFromTheState() {
        // A mid-run ask on a task whose Run act failed to load: nearest earlier.
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(
                state: .waiting, acts: [.plan, .output], attention: .escalation
            ),
            .plan
        )
        // A plan-time ask on a task that was never planned: nothing earlier, so
        // the earliest later act opens.
        XCTAssertEqual(
            TaskCapabilities.defaultOpenAct(
                state: .waiting, acts: [.run, .output], attention: .planApproval
            ),
            .run
        )
    }

    func testNeverOpensAnActTheTaskDoesNotHaveForAnySource() {
        for source in HitlSource.allCases {
            for acts in subsets where !acts.isEmpty {
                let opened = TaskCapabilities.defaultOpenAct(
                    state: .waiting, acts: acts, attention: source
                )
                XCTAssertNotNil(opened, "\(source) · \(acts)")
                XCTAssertTrue(acts.contains(opened!), "\(source) · \(acts) opened \(opened!)")
            }
        }
    }

    func testTheSubsetSweepActuallyCoversEverySubset() {
        // The sweeps above are only as strong as this: eight subsets of three
        // acts, all distinct. A `reduce` that dropped the accumulator would
        // leave them asserting almost nothing, silently.
        XCTAssertEqual(subsets.count, 8)
        XCTAssertEqual(Set(subsets.map { $0.map(\.rawValue).joined(separator: ",") }).count, 8)
    }
}
