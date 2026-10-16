import XCTest
@testable import Magician

final class AmbientArmTests: XCTestCase {

    /// This suite writes to the REAL App Group, because that shared container is
    /// the contract under test — the app and the widget process agree through it
    /// and nothing else. On a device that means the developer's own armed window,
    /// so it is preserved and put back rather than clobbered. Deleting it mid-run
    /// would leave the microphone tap running with the disarm intent reading
    /// `nil`, i.e. a live window with no working disarm control.
    private var preexisting: AmbientArm?

    override func setUp() {
        super.setUp()
        preexisting = AmbientArm.claim()
        AmbientArm.clear()
    }

    override func tearDown() {
        if let preexisting {
            preexisting.save()
        } else {
            AmbientArm.clear()
        }
        preexisting = nil
        super.tearDown()
    }

    func testSaveThenClaimRoundTrips() {
        let armedAt = Date(timeIntervalSince1970: 1_000_000)
        AmbientArm(armedAt: armedAt, capSeconds: 7_200, ownerID: "abc").save()
        let claimed = AmbientArm.claim()
        XCTAssertEqual(claimed?.capSeconds, 7_200)
        XCTAssertEqual(claimed?.ownerID, "abc")
        XCTAssertEqual(claimed?.armedAt, armedAt)
    }

    func testClearRemovesTheRecord() {
        AmbientArm(armedAt: Date(), capSeconds: 60, ownerID: "abc").save()
        AmbientArm.clear()
        XCTAssertNil(AmbientArm.claim())
    }

    func testExpiryIsArmedAtPlusCap() {
        let armedAt = Date(timeIntervalSince1970: 0)
        let arm = AmbientArm(armedAt: armedAt, capSeconds: 1_800, ownerID: "abc")
        XCTAssertEqual(arm.expiresAt, Date(timeIntervalSince1970: 1_800))
        XCTAssertFalse(arm.isExpired(now: Date(timeIntervalSince1970: 1_799)))
        XCTAssertTrue(arm.isExpired(now: Date(timeIntervalSince1970: 1_800)))
    }

    func testExtensionAddsThirtyMinutesWithoutMovingTheWindowIdentity() throws {
        let armedAt = Date(timeIntervalSince1970: 1_000)
        let current = armedAt.addingTimeInterval(1_800)
        let extended = try XCTUnwrap(
            AmbientExtensionPolicy.extendedExpiry(
                armedAt: armedAt,
                currentExpiry: current
            )
        )

        XCTAssertEqual(extended.timeIntervalSince(current), 1_800)
        XCTAssertEqual(armedAt, Date(timeIntervalSince1970: 1_000))
    }

    func testExtensionClampsAtEightHoursAndThenRefuses() throws {
        let armedAt = Date(timeIntervalSince1970: 1_000)
        let almostMax = armedAt.addingTimeInterval(
            AmbientExtensionPolicy.maximumWindowSeconds - 600
        )
        let maximum = try XCTUnwrap(
            AmbientExtensionPolicy.extendedExpiry(
                armedAt: armedAt,
                currentExpiry: almostMax
            )
        )

        XCTAssertEqual(
            maximum,
            armedAt.addingTimeInterval(AmbientExtensionPolicy.maximumWindowSeconds)
        )
        XCTAssertNil(
            AmbientExtensionPolicy.extendedExpiry(
                armedAt: armedAt,
                currentExpiry: maximum
            )
        )
    }

    /// The asymmetry with ObservationArm. Ambient cannot survive termination, so
    /// a record left by a dead process is garbage to collect, never a session to
    /// resume.
    func testARecordFromAnotherProcessIsStaleAndNotAdoptable() throws {
        AmbientArm(armedAt: Date(), capSeconds: 7_200, ownerID: "dead-process").save()
        let claimed = try XCTUnwrap(AmbientArm.claim())
        XCTAssertTrue(claimed.isStale(currentOwnerID: "live-process"))
    }

    func testARecordFromThisProcessIsNotStale() {
        let arm = AmbientArm(armedAt: Date(), capSeconds: 7_200, ownerID: "live-process")
        XCTAssertFalse(arm.isStale(currentOwnerID: "live-process"))
    }

    /// The realistic corruption is schema drift, not garbage: the first commit to
    /// add a non-optional field makes every record written by an earlier build
    /// undecodable. Those bytes must not sit in the container forever — a caller
    /// that reads `nil` has no reason to clear, so the read has to do it.
    ///
    /// The key is spelled out here on purpose. It is a cross-process contract with
    /// the widget's disarm intent, and a silent rename would break that with no
    /// compile error anywhere.
    func testAnUndecodableRecordIsDroppedRatherThanLeftResident() {
        let store = UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard
        store.set(Data(#"{"armedAt":0,"ownerID":"abc"}"#.utf8), forKey: "ambient.activeArm")

        XCTAssertNil(AmbientArm.claim())
        XCTAssertNil(store.data(forKey: "ambient.activeArm"))
    }
}
