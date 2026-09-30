import XCTest
@testable import Magician

final class MobileEnrollmentTests: XCTestCase {
    override func tearDown() {
        MockURLProtocol.handler = nil
        super.tearDown()
    }

    func testConnectionLinkAcceptsOnlyNormalizedIOSOneTimeCapabilities() throws {
        let id = "abcdefghijklmnopqrstuvwx"
        let secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789"
        let base = "https%3A%2F%2Fmobile.example"
        let link = try MobileEnrollmentLink.parse(
            "magican://connect?base=\(base)&id=\(id)&secret=\(secret)&kind=ios"
        )
        XCTAssertEqual(link.publicOrigin.absoluteString, "https://mobile.example")
        XCTAssertEqual(link.clientKind, "ios")

        XCTAssertTrue(MagicanAppURL.isScheme("magican"))
        XCTAssertFalse(MagicanAppURL.isScheme("https"))
        XCTAssertFalse(MagicanAppURL.isScheme("magios"))
        XCTAssertFalse(MagicanAppURL.isScheme(nil))

        for invalid in [
            "magican://connect?base=\(base)&id=\(id)&secret=\(secret)&kind=android",
            "magican://connect?base=http%3A%2F%2Fmobile.example&id=\(id)&secret=\(secret)&kind=ios",
            "magican://connect?base=https%3A%2F%2Fmobile.example%2Fapi&id=\(id)&secret=\(secret)&kind=ios",
            "magican://connect?base=\(base)&id=\(id)&id=duplicate&secret=\(secret)&kind=ios",
            "magican://pair?base=\(base)&id=\(id)&secret=\(secret)"
        ] {
            XCTAssertThrowsError(try MobileEnrollmentLink.parse(invalid), invalid)
        }
    }

    func testAndroidConnectionCodeExplainsHowToRecoverOnIOS() throws {
        let id = "abcdefghijklmnopqrstuvwx"
        let secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789"
        let base = "https%3A%2F%2Fmobile.example"

        XCTAssertThrowsError(
            try MobileEnrollmentLink.parse(
                "magican://connect?base=\(base)&id=\(id)&secret=\(secret)&kind=android"
            )
        ) { error in
            XCTAssertEqual(
                error.localizedDescription,
                "This connection code was created for Android. Create an iPhone code instead."
            )
        }
    }

    func testSameWiFiConnectionAcceptsPrivateAddressesButRejectsPublicPlaintext() throws {
        let id = "abcdefghijklmnopqrstuvwx"
        let secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789"
        let local = try MobileEnrollmentLink.parse(
            "magican://connect?base=http%3A%2F%2F192.168.68.62%3A3002&id=\(id)&secret=\(secret)&kind=ios"
        )

        XCTAssertEqual(local.publicOrigin.absoluteString, "http://192.168.68.62:3002")
        XCTAssertTrue(local.usesSameWifi)
        XCTAssertEqual(local.connectionMode, .sameWifi)
        XCTAssertThrowsError(
            try MobileEnrollmentLink.parse(
                "magican://connect?base=http%3A%2F%2F8.8.8.8%3A3002&id=\(id)&secret=\(secret)&kind=ios"
            )
        )
    }

    func testRouteChoiceRejectsAMismatchedCodeWithActionableGuidance() throws {
        let remote = try MobileEnrollmentLink.parse(
            "magican://connect?base=https%3A%2F%2Fconnect.magican.ai&id=abcdefghijklmnopqrstuvwx&secret=abcdefghijklmnopqrstuvwxyzABCDEFGH123456789&kind=ios"
        )

        XCTAssertEqual(remote.connectionMode, .remote)
        let error = MobileConnectionError.routeMismatch(selected: .sameWifi, scanned: remote.connectionMode)
        XCTAssertTrue(error.localizedDescription.contains("Remote code"))
        XCTAssertTrue(error.localizedDescription.contains("Same Wi-Fi code"))
    }

    func testExchangeVerifiesTheIssuedProfileBeforeReturningIt() async throws {
        let id = "abcdefghijklmnopqrstuvwx"
        let secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789"
        let link = try MobileEnrollmentLink.parse(
            "magican://connect?base=https%3A%2F%2Fmobile.example&id=\(id)&secret=\(secret)&kind=ios"
        )
        let lock = NSLock()
        var issuedDeviceID = ""
        var calls = 0
        MockURLProtocol.handler = { request in
            lock.lock()
            defer { lock.unlock() }
            calls += 1
            if request.url?.path == "/api/magician/v2/devices/enrollment/exchange" {
                XCTAssertNil(request.value(forHTTPHeaderField: "CF-Access-Client-Id"))
                let body = try XCTUnwrap(requestBody(request))
                let object = try XCTUnwrap(
                    JSONSerialization.jsonObject(with: body) as? [String: Any]
                )
                issuedDeviceID = try XCTUnwrap(object["device_id"] as? String)
                return (
                    response(for: request),
                    jsonData([
                        "token": "device-token",
                        "principal": "owner",
                        "workspace": "default",
                        "public_origin": "https://mobile.example",
                        "client_kind": "ios",
                        "capabilities": ["mobile_client"],
                        "cloudflare_access": [
                            "client_id": "outer-id",
                            "client_secret": "outer-secret"
                        ]
                    ])
                )
            }
            XCTAssertEqual(request.url?.path, "/api/magician/v2/devices/me")
            XCTAssertEqual(request.value(forHTTPHeaderField: "X-Magician-Device-Id"), issuedDeviceID)
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer device-token")
            XCTAssertEqual(request.value(forHTTPHeaderField: "CF-Access-Client-Id"), "outer-id")
            return (
                response(for: request),
                jsonData([
                    "device_id": issuedDeviceID,
                    "principal": "owner",
                    "workspace": "default"
                ])
            )
        }

        let profile = try await MobileEnrollmentClient.exchange(link, session: makeMockSession())
        XCTAssertEqual(profile.publicOrigin.absoluteString, "https://mobile.example")
        XCTAssertEqual(profile.deviceID, issuedDeviceID)
        XCTAssertEqual(profile.cloudflareClientSecret, "outer-secret")
        XCTAssertEqual(calls, 2)
    }

    func testIOSRejectsAnAutomationGrantWithoutCommittingOrProbing() async throws {
        let id = "abcdefghijklmnopqrstuvwx"
        let secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789"
        let link = try MobileEnrollmentLink.parse(
            "magican://connect?base=https%3A%2F%2Fmobile.example&id=\(id)&secret=\(secret)&kind=ios"
        )
        var calls = 0
        MockURLProtocol.handler = { request in
            calls += 1
            return (
                response(for: request),
                jsonData([
                    "token": "device-token",
                    "principal": "owner",
                    "workspace": "default",
                    "public_origin": "https://mobile.example",
                    "client_kind": "ios",
                    "capabilities": ["mobile_client", "device_automation"]
                ])
            )
        }

        do {
            _ = try await MobileEnrollmentClient.exchange(link, session: makeMockSession())
            XCTFail("an iPhone must not accept device automation authority")
        } catch {
            XCTAssertEqual(calls, 1)
        }
    }

    func testExchangeDistinguishesMissingRouteFromUnavailablePairingStorage() async throws {
        let id = "abcdefghijklmnopqrstuvwx"
        let secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789"
        let link = try MobileEnrollmentLink.parse(
            "magican://connect?base=https%3A%2F%2Fmobile.example&id=\(id)&secret=\(secret)&kind=ios"
        )

        for (status, expected) in [
            (404, "does not expose this enrollment route"),
            (503, "Device pairing storage is unavailable")
        ] {
            MockURLProtocol.handler = { request in
                (response(for: request, status: status), Data())
            }
            do {
                _ = try await MobileEnrollmentClient.exchange(link, session: makeMockSession())
                XCTFail("HTTP \(status) must not create a mobile profile")
            } catch {
                XCTAssertTrue(error.localizedDescription.contains(expected), error.localizedDescription)
            }
        }
    }
}
