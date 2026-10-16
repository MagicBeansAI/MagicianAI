import os
import XCTest
@testable import Magician

/// The cross-process disarm channel, exercised as far as one process can reach.
///
/// What is provable here is the part that decides whether the orb may be taken
/// down: the request record, the identity-matched acknowledgement, and the wait.
/// The rest — a widget process posting to an app process — is verified on device.
///
/// The Darwin notification IS covered, and deliberately so: `notify(3)` delivers
/// to every registered listener including the poster, so a single process still
/// crosses `notifyd`. That is the boundary worth crossing here — a name typed
/// differently on the two sides, a handler that was never stored, an observer
/// registered against the wrong pointer — none of which a double standing in for
/// the notification centre could ever catch.
final class AmbientSignalTests: XCTestCase {

    /// This suite writes to the REAL App Group, for the reason `AmbientArmTests`
    /// gives: that container is the contract between the app and the widget
    /// process, so testing anything else would be testing a different thing.
    /// On hardware that means the developer's own container, hence save/restore
    /// rather than an unconditional wipe.
    ///
    /// The keys are spelled out for the same reason `AmbientArmTests` spells out
    /// `ambient.activeArm`: they are a cross-process contract, and renaming one
    /// side produces no compile error anywhere.
    private let store = UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard
    private var savedPending: String?
    private var savedAcknowledgement: String?
    private var savedExtension: String?

    override func setUp() {
        super.setUp()
        savedPending = store.string(forKey: "ambient.pendingDisarm")
        savedAcknowledgement = store.string(forKey: "ambient.disarmAck")
        savedExtension = store.string(forKey: "ambient.pendingExtension")
        AmbientSignal.clearPendingDisarm()
        AmbientSignal.clearAcknowledgement()
        AmbientSignal.clearPendingExtension()
    }

    override func tearDown() {
        AmbientSignal.stopObservingDisarm()
        AmbientSignal.stopObservingExtension()
        restore(savedPending, forKey: "ambient.pendingDisarm")
        restore(savedAcknowledgement, forKey: "ambient.disarmAck")
        restore(savedExtension, forKey: "ambient.pendingExtension")
        savedPending = nil
        savedAcknowledgement = nil
        savedExtension = nil
        super.tearDown()
    }

    private func restore(_ value: String?, forKey key: String) {
        if let value {
            store.set(value, forKey: key)
        } else {
            store.removeObject(forKey: key)
        }
    }

    // MARK: - the request record

    func testARequestIsConsumedExactlyOnce() {
        let request = AmbientSignal.requestDisarm()

        XCTAssertEqual(AmbientSignal.consumePendingDisarm(), request)
        XCTAssertNil(AmbientSignal.consumePendingDisarm(), "a request is answered once, not once per reader")
    }

    func testConsumingWithNothingPendingReturnsNil() {
        XCTAssertNil(AmbientSignal.consumePendingDisarm())
    }

    func testAnExtensionRequestIsConsumedExactlyOnce() {
        let request = AmbientSignal.requestExtension()

        XCTAssertEqual(AmbientSignal.consumePendingExtension(), request)
        XCTAssertNil(AmbientSignal.consumePendingExtension())
    }

    func testPublishingAnotherExtensionCoalescesAPendingTap() {
        let first = AmbientSignal.requestExtension()
        let second = AmbientSignal.requestExtension()

        XCTAssertNotEqual(first, second)
        XCTAssertEqual(AmbientSignal.consumePendingExtension(), second)
        XCTAssertNil(AmbientSignal.consumePendingExtension())
    }

    /// The intent's evidence check reads the outstanding request to decide whether
    /// anyone picked it up. Consuming it there would destroy the record the app is
    /// still about to read — turning the check into the bug it exists to prevent.
    func testPeekingAtTheRequestDoesNotConsumeIt() {
        let request = AmbientSignal.requestDisarm()

        XCTAssertEqual(AmbientSignal.pendingDisarm(), request)
        XCTAssertEqual(AmbientSignal.pendingDisarm(), request)
        XCTAssertEqual(AmbientSignal.consumePendingDisarm(), request, "the app must still find it")
    }

    // MARK: - the acknowledgement

    func testAnAcknowledgementIsConsumedExactlyOnce() {
        let request = AmbientSignal.requestDisarm()
        AmbientSignal.acknowledgeDisarm(request)

        XCTAssertTrue(AmbientSignal.consumeAcknowledgement(of: request))
        XCTAssertFalse(AmbientSignal.consumeAcknowledgement(of: request))
    }

    func testAnUnacknowledgedRequestIsNotSatisfied() {
        let request = AmbientSignal.requestDisarm()

        XCTAssertFalse(AmbientSignal.consumeAcknowledgement(of: request))
    }

    /// The reason a request has an identity at all. An acknowledgement written
    /// just after an earlier request gave up waiting is still in the container
    /// when the next tap arrives; read as a bare flag it would tell the new
    /// intent the microphone is off when nothing has heard the new request.
    func testAnAcknowledgementForAnEarlierRequestDoesNotSatisfyALaterOne() {
        let earlier = AmbientSignal.requestDisarm()
        AmbientSignal.acknowledgeDisarm(earlier)
        let later = AmbientDisarmRequest(id: "a-different-request")

        XCTAssertFalse(
            AmbientSignal.consumeAcknowledgement(of: later),
            "an answer to a different question must not close this one"
        )
        XCTAssertTrue(AmbientSignal.consumeAcknowledgement(of: earlier), "and it must not have been eaten either")
    }

    /// Hygiene rather than the safety property — the identity match above is what
    /// makes a stale answer harmless — but a container that accumulates answers
    /// nobody is waiting for is how the identity match ends up load-bearing.
    func testPublishingARequestDropsAnAcknowledgementLeftByAnEarlierOne() {
        let earlier = AmbientSignal.requestDisarm()
        AmbientSignal.acknowledgeDisarm(earlier)

        let later = AmbientSignal.requestDisarm()

        XCTAssertFalse(AmbientSignal.consumeAcknowledgement(of: earlier))
        XCTAssertEqual(AmbientSignal.pendingDisarm(), later)
    }

    // MARK: - the wait

    func testTheWaitEndsAsSoonAsTheAcknowledgementLands() async {
        let request = AmbientSignal.requestDisarm()
        Task {
            try? await Task.sleep(nanoseconds: 30_000_000)
            AmbientSignal.acknowledgeDisarm(request)
        }

        let outcome = await AmbientSignal.awaitAcknowledgement(of: request, within: 2)

        XCTAssertEqual(outcome, .acknowledged)
    }

    /// The case the whole design turns on: nothing answered. The wait reports
    /// that honestly and consumes nothing, leaving the caller its evidence check
    /// — a wait that guessed here is how an orb outlives the microphone it stands
    /// for.
    func testTheWaitReportsSilenceAndLeavesTheRequestOutstanding() async {
        let request = AmbientSignal.requestDisarm()

        let outcome = await AmbientSignal.awaitAcknowledgement(of: request, within: 0.1)

        XCTAssertEqual(outcome, .silent)
        XCTAssertEqual(
            AmbientSignal.pendingDisarm(),
            request,
            "the outstanding request is the evidence that nobody picked it up; the wait must not spend it"
        )
    }

    /// A cancelled wait is NOT silence, and the difference is the whole reason the
    /// outcome is not a `Bool`.
    ///
    /// Silence means nobody answered in a full budget — which is what makes an
    /// outstanding request mean "nobody is there". Cancellation can arrive one
    /// poll interval in, when the app has not had time to answer anything; read as
    /// silence it sends the caller straight to the destructive branch, and the orb
    /// of a healthy, still-listening app is collected ~20 ms after the tap. No
    /// stall, no dead app, no unusual state — this feature's signature failure,
    /// through the one door that skips the budget entirely.
    func testACancelledWaitIsReportedAsCancellationRatherThanSilence() async {
        let request = AmbientSignal.requestDisarm()
        let waiter = Task { await AmbientSignal.awaitAcknowledgement(of: request, within: 60) }
        try? await Task.sleep(nanoseconds: 50_000_000)

        waiter.cancel()

        let outcome = await waiter.value
        XCTAssertEqual(outcome, .cancelled, "a wait that was cut short has learned nothing about the app")
        XCTAssertEqual(AmbientSignal.pendingDisarm(), request, "and it leaves the request for the app's resume path")
    }

    // MARK: - the Darwin notification

    func testStartingAndStoppingObservationTracksRegistration() {
        XCTAssertFalse(AmbientSignal.isObservingDisarm)

        AmbientSignal.startObservingDisarm {}
        XCTAssertTrue(AmbientSignal.isObservingDisarm)

        AmbientSignal.stopObservingDisarm()
        XCTAssertFalse(AmbientSignal.isObservingDisarm, "an armed window that ended must not leave a listener behind")
        AmbientSignal.stopObservingDisarm()
        XCTAssertFalse(AmbientSignal.isObservingDisarm, "stopping twice is what a disarm after an unwind does")
    }

    func testStartingAndStoppingExtensionObservationTracksRegistrationIndependently() {
        XCTAssertFalse(AmbientSignal.isObservingExtension)
        AmbientSignal.startObservingDisarm {}

        AmbientSignal.startObservingExtension {}
        XCTAssertTrue(AmbientSignal.isObservingDisarm)
        XCTAssertTrue(AmbientSignal.isObservingExtension)

        AmbientSignal.stopObservingExtension()
        XCTAssertTrue(AmbientSignal.isObservingDisarm)
        XCTAssertFalse(AmbientSignal.isObservingExtension)
    }

    func testPostingExtensionReachesARegisteredObserver() async {
        let delivered = OSAllocatedUnfairLock(initialState: false)
        let heard = expectation(description: "the extension signal was delivered")
        AmbientSignal.startObservingExtension {
            let isFirst = delivered.withLock { delivered -> Bool in
                defer { delivered = true }
                return !delivered
            }
            if isFirst { heard.fulfill() }
        }

        AmbientSignal.postExtension()

        await fulfillment(of: [heard], timeout: 5)
    }

    /// The real round trip through `notifyd`. Single-process, because a Darwin
    /// notification is delivered to the poster as well — which is enough to catch
    /// every way the two halves can fail to agree, and is the one thing no double
    /// could stand in for.
    ///
    /// Fulfilled once and only once by construction. A registration leaked by an
    /// earlier case would otherwise deliver here a second time and fail *this*
    /// test with `API violation - multiple calls made to fulfill`, pointing at the
    /// round trip when the fault is in the removal. Leak detection belongs to the
    /// test below, which reports it as a count.
    func testPostingTheSignalReachesARegisteredObserver() async {
        let delivered = OSAllocatedUnfairLock(initialState: false)
        let heard = expectation(description: "the disarm signal was delivered")
        AmbientSignal.startObservingDisarm {
            let isFirst = delivered.withLock { delivered -> Bool in
                defer { delivered = true }
                return !delivered
            }
            if isFirst { heard.fulfill() }
        }

        AmbientSignal.postDisarm()

        await fulfillment(of: [heard], timeout: 5)
    }

    /// A window that ended leaves no registration behind — *gone*, not merely
    /// silenced.
    ///
    /// The shape matters. `stopObservingDisarm` nils the handler before it removes
    /// the observer, so a removal that silently failed would still fire
    /// `deliverDisarm()` into a `nil` handler and do nothing: an inverted
    /// expectation after a stop passes whether or not the registration leaked, and
    /// would be a test that cannot fail on the thing it is named for. Registering
    /// again and posting once is what makes a leak visible — two registrations
    /// deliver twice into the *live* handler, and over-fulfilment catches it.
    /// The count is kept here rather than left to `XCTestExpectation`'s
    /// over-fulfilment trap, which throws `API violation - multiple calls made to
    /// fulfill` — a message that reads as a broken test rather than as a leaked
    /// registration, and which the next person to see it will debug in the wrong
    /// place. The expectation is fulfilled once, on the first delivery; a second
    /// one lands during the settle below and fails as a plain count mismatch.
    func testAStoppedObserverIsRemovedRatherThanMerelySilenced() async {
        let deliveries = OSAllocatedUnfairLock(initialState: 0)
        let heard = expectation(description: "the disarm signal was delivered")
        let record: @Sendable () -> Void = {
            let count = deliveries.withLock { count -> Int in
                count += 1
                return count
            }
            if count == 1 { heard.fulfill() }
        }
        AmbientSignal.startObservingDisarm(record)
        AmbientSignal.stopObservingDisarm()
        AmbientSignal.startObservingDisarm(record)

        AmbientSignal.postDisarm()

        await fulfillment(of: [heard], timeout: 5)
        // A leaked registration's second delivery rides the same post and lands
        // within a runloop turn or two, so give it time to arrive before counting.
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(
            deliveries.withLock { $0 },
            1,
            "two deliveries means the first registration outlived stopObservingDisarm — nilling the handler hid it, "
                + "and an inverted expectation after a stop would have passed"
        )
    }
}
