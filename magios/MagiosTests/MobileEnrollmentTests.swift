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
            "magican://connect?base=https%3A%2F%2Fconnect.example.com&id=abcdefghijklmnopqrstuvwx&secret=abcdefghijklmnopqrstuvwxyzABCDEFGH123456789&kind=ios"
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
                        ],
                        "machine_name": "macbookpro",
                        "machine_origins": ["https://mobile.example", "http://192.168.68.57:3002"]
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

        let grant = try await MobileEnrollmentClient.exchange(link, session: makeMockSession())
        let profile = grant.profile
        XCTAssertEqual(profile.publicOrigin.absoluteString, "https://mobile.example")
        XCTAssertEqual(profile.machineName, "macbookpro")
        XCTAssertEqual(grant.machineOrigins, ["https://mobile.example", "http://192.168.68.57:3002"])
        XCTAssertEqual(profile.deviceID, issuedDeviceID)
        XCTAssertEqual(profile.cloudflareClientSecret, "outer-secret")
        XCTAssertEqual(calls, 2)
    }

    func testExchangeToleratesAHostWithoutMachineFields() async throws {
        let link = try MobileEnrollmentLink.parse(
            "magican://connect?base=https%3A%2F%2Fmobile.example&id=abcdefghijklmnopqrstuvwx&secret=abcdefghijklmnopqrstuvwxyzABCDEFGH123456789&kind=ios"
        )
        let lock = NSLock()
        var issuedDeviceID = ""
        MockURLProtocol.handler = { request in
            lock.lock()
            defer { lock.unlock() }
            if request.url?.path == "/api/magician/v2/devices/enrollment/exchange" {
                let body = try XCTUnwrap(requestBody(request))
                let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
                issuedDeviceID = try XCTUnwrap(object["device_id"] as? String)
                return (response(for: request), jsonData([
                    "token": "device-token",
                    "principal": "owner",
                    "workspace": "default",
                    "public_origin": "https://mobile.example",
                    "client_kind": "ios",
                    "capabilities": ["mobile_client"],
                    "machine_name": NSNull()
                ]))
            }
            return (response(for: request), jsonData([
                "device_id": issuedDeviceID, "principal": "owner", "workspace": "default"
            ]))
        }

        let grant = try await MobileEnrollmentClient.exchange(link, session: makeMockSession())
        XCTAssertNil(grant.profile.machineName)
        XCTAssertEqual(grant.machineOrigins, [])
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

/// Multi-machine profiles: one paired profile per Magician origin, one active.
/// Hosted tests run in a per-process Keychain namespace (`MagicianAccess`
/// storage suffix), so these never touch the phone's real enrollment.
final class MobileConnectionProfilesTests: XCTestCase {
    override func setUp() {
        super.setUp()
        MagicianAccess.testingResetStores()
    }

    override func tearDown() {
        MockURLProtocol.handler = nil
        MagicianAccess.testingResetStores()
        super.tearDown()
    }

    private func profile(
        _ origin: String,
        token: String = "token",
        machine: String? = nil
    ) throws -> MobileConnectionProfile {
        try MobileConnectionProfile(
            publicOrigin: XCTUnwrap(URL(string: origin)),
            principal: "owner",
            workspace: "default",
            deviceID: "magios-test-device",
            deviceToken: token,
            cloudflareClientID: "cf-\(token)",
            cloudflareClientSecret: "cf-secret-\(token)",
            machineName: machine
        )
    }

    func testV1ProfileMigratesIntoV2AsTheOnlyActiveProfileAndV1IsKept() throws {
        let legacy = try profile("https://macbookpro.example.com", token: "legacy")
        try MagicianAccess.testingWriteLegacyV1Profile(legacy)
        XCTAssertFalse(MagicianAccess.testingHasV2Store)

        XCTAssertEqual(MagicianAccess.connectionProfile, legacy)
        XCTAssertEqual(MagicianAccess.allProfiles, [legacy])
        XCTAssertEqual(MagicianAccess.activeOrigin, "https://macbookpro.example.com")
        XCTAssertEqual(MagicianAccess.baseURL.absoluteString, "https://macbookpro.example.com")
        XCTAssertTrue(MagicianAccess.testingHasV2Store, "migration persists V2 once")
        XCTAssertEqual(MagicianAccess.testingLegacyV1Profile, legacy, "V1 stays for downgrade")

        // Once V2 exists it is authoritative: V1 no longer changes the answer.
        try MagicianAccess.testingWriteLegacyV1Profile(profile("https://other.example.com", token: "stale"))
        XCTAssertEqual(MagicianAccess.connectionProfile, legacy)
        XCTAssertEqual(MagicianAccess.allProfiles.count, 1)
    }

    func testResolveKeepsV2AuthoritativeAndNeverResurrectsV1FromACorruptBlob() throws {
        let legacy = try profile("https://macbookpro.example.com")
        let v1 = String(decoding: try JSONEncoder().encode(legacy), as: UTF8.self)

        let migrated = MobileConnectionProfiles.resolve(v2Raw: nil, v1Raw: v1)
        XCTAssertTrue(migrated.needsWrite)
        XCTAssertEqual(migrated.state.active, legacy)

        let empty = String(decoding: try JSONEncoder().encode(MobileConnectionProfiles.empty), as: UTF8.self)
        let authoritative = MobileConnectionProfiles.resolve(v2Raw: empty, v1Raw: v1)
        XCTAssertFalse(authoritative.needsWrite)
        XCTAssertNil(authoritative.state.active)

        let corrupt = MobileConnectionProfiles.resolve(v2Raw: "{not json", v1Raw: v1)
        XCTAssertNil(corrupt.state.active)
        XCTAssertFalse(corrupt.needsWrite)
    }

    func testInstallKeepsOtherMachinesAndMakesTheNewOneActive() throws {
        let pro = try profile("https://macbookpro.example.com", token: "pro")
        let studio = try profile("https://macstudio.example.com", token: "studio")
        try MagicianAccess.install(pro)
        try MagicianAccess.install(studio)

        XCTAssertEqual(MagicianAccess.connectionProfile, studio)
        XCTAssertEqual(MagicianAccess.allProfiles, [studio, pro], "active first")

        // Re-pairing the same origin replaces only that machine's credential.
        let rotated = try profile("https://MacBookPro.example.com/", token: "pro-2")
        try MagicianAccess.install(rotated)
        XCTAssertEqual(MagicianAccess.allProfiles.count, 2)
        XCTAssertEqual(MagicianAccess.connectionProfile?.deviceToken, "pro-2")
        XCTAssertEqual(MagicianAccess.profile(forOrigin: "https://macstudio.example.com"), studio)
    }

    func testInstallReplacesTheSameMachinesLanProfileListedInMachineOrigins() throws {
        let lan = try profile("http://192.168.68.57:3002", token: "lan")
        let studio = try profile("https://macstudio.example.com", token: "studio")
        try MagicianAccess.install(lan)
        try MagicianAccess.install(studio)

        let remote = try profile("https://macbookpro.example.com", token: "remote")
        try MagicianAccess.install(remote, sameMachineOrigins: [
            "https://MacBookPro.example.com/", "http://192.168.68.57:3002/"
        ])

        XCTAssertNil(MagicianAccess.profile(forOrigin: "http://192.168.68.57:3002"),
                     "the revoked same-Wi-Fi profile is removed")
        XCTAssertEqual(MagicianAccess.allProfiles, [remote, studio], "another machine is kept")
        XCTAssertEqual(MagicianAccess.connectionProfile, remote)
    }

    func testInstallReplacesAProfileWithTheSameMachineNameAndKeepsOtherMachines() throws {
        var state = MobileConnectionProfiles.empty
        state.install(try profile("https://macbookpro.example.com", token: "remote", machine: "macbookpro"))
        state.install(try profile("https://macstudio.example.com", token: "studio", machine: "macstudio"))
        state.install(try profile("https://unnamed.example.com", token: "unnamed"))

        let lan = try profile("http://192.168.68.57:3002", token: "lan", machine: " MacBookPro ")
        let replaced = state.install(lan)

        XCTAssertEqual(replaced.map(\.deviceToken), ["remote"])
        XCTAssertNil(state.profile(forOrigin: "https://macbookpro.example.com"))
        XCTAssertEqual(state.profile(forOrigin: "https://macstudio.example.com")?.deviceToken, "studio")
        XCTAssertEqual(state.profile(forOrigin: "https://unnamed.example.com")?.deviceToken, "unnamed",
                       "a profile without a machine name never matches by name")
        XCTAssertEqual(state.active, lan)
        XCTAssertEqual(state.profiles.count, 3)
    }

    func testStoredProfilesWithoutMachineNameStillDecode() throws {
        let raw = """
        {"active_origin":"https://macbookpro.example.com","profiles":{"https://macbookpro.example.com":{\
        "publicOrigin":"https://macbookpro.example.com","principal":"owner","workspace":"default",\
        "deviceID":"magios-test-device","deviceToken":"pro","cloudflareClientID":"",\
        "cloudflareClientSecret":""}}}
        """
        let resolved = MobileConnectionProfiles.resolve(v2Raw: raw, v1Raw: nil)
        XCTAssertEqual(resolved.state.active?.deviceToken, "pro")
        XCTAssertNil(resolved.state.active?.machineName)

        // A named profile round-trips its name.
        var state = MobileConnectionProfiles.empty
        state.install(try profile("https://macstudio.example.com", machine: "macstudio"))
        let encoded = String(decoding: try JSONEncoder().encode(state), as: UTF8.self)
        let decoded = MobileConnectionProfiles.resolve(v2Raw: encoded, v1Raw: nil).state
        XCTAssertEqual(decoded.active?.machineName, "macstudio")
    }

    func testActivateSwitchesBaseURLAndCredentialsWithoutRepairing() throws {
        let pro = try profile("https://macbookpro.example.com", token: "pro")
        let studio = try profile("https://macstudio.example.com", token: "studio")
        try MagicianAccess.install(pro)
        try MagicianAccess.install(studio)

        let changed = expectation(forNotification: .magicianMobileConnectionDidChange, object: nil)
        XCTAssertTrue(MagicianAccess.activate(origin: "https://macbookpro.example.com"))
        wait(for: [changed], timeout: 1)

        XCTAssertEqual(MagicianAccess.baseURL.absoluteString, "https://macbookpro.example.com")
        XCTAssertEqual(MagicianAccess.webSocketBaseURL.absoluteString, "wss://macbookpro.example.com")
        XCTAssertEqual(MagicianAccess.headers()["Authorization"], "Bearer pro")
        XCTAssertEqual(MagicianAccess.headers()["CF-Access-Client-Id"], "cf-pro")
        XCTAssertTrue(MagicianAccess.isMagicianRuntimeURL(
            URL(string: "https://macbookpro.example.com/api/x"),
            profile: MagicianAccess.connectionProfile
        ))
        XCTAssertFalse(MagicianAccess.isMagicianRuntimeURL(
            URL(string: "https://macstudio.example.com/api/x"),
            profile: MagicianAccess.connectionProfile
        ))

        // Switching never creates a credential for an unpaired origin.
        XCTAssertFalse(MagicianAccess.activate(origin: "https://unpaired.example.com"))
        XCTAssertEqual(MagicianAccess.baseURL.absoluteString, "https://macbookpro.example.com")
    }

    func testForgetRemovesOnlyThatMachine() throws {
        let pro = try profile("https://macbookpro.example.com", token: "pro")
        let studio = try profile("https://macstudio.example.com", token: "studio")
        try MagicianAccess.install(pro)
        try MagicianAccess.install(studio)

        XCTAssertEqual(MagicianAccess.forget(origin: "https://macbookpro.example.com"), pro)
        XCTAssertEqual(MagicianAccess.allProfiles, [studio])
        XCTAssertEqual(MagicianAccess.connectionProfile, studio, "forgetting an inactive machine keeps the active one")
        XCTAssertNil(MagicianAccess.forget(origin: "https://macbookpro.example.com"))
    }

    func testForgettingTheActiveMachineFallsBackToAnotherThenToNone() throws {
        let legacy = try profile("https://macbookpro.example.com", token: "pro")
        try MagicianAccess.testingWriteLegacyV1Profile(legacy)
        let studio = try profile("https://macstudio.example.com", token: "studio")
        try MagicianAccess.install(studio)
        XCTAssertEqual(MagicianAccess.allProfiles.count, 2, "migrated V1 plus the new machine")

        MagicianAccess.clearConnection()
        XCTAssertEqual(MagicianAccess.connectionProfile, legacy, "the other paired machine becomes active")

        MagicianAccess.forget(origin: "https://macbookpro.example.com")
        XCTAssertNil(MagicianAccess.connectionProfile)
        XCTAssertFalse(MagicianAccess.isConfigured)
        XCTAssertEqual(MagicianAccess.baseURL.absoluteString, "https://unconfigured.invalid")
        XCTAssertNil(MagicianAccess.testingLegacyV1Profile, "a forgotten credential leaves the V1 copy too")
    }

    func testMachinePickerNamesPairedMachinesFromTheRegistryAndListsUnpaired() throws {
        var state = MobileConnectionProfiles.empty
        state.install(try profile("https://macstudio.example.com"))
        state.install(try profile("http://192.168.1.20:3002"))
        state.install(try profile("https://macbookpro.example.com"))
        let registry = [
            RemoteMachine(name: "macbookpro", apiOrigin: "https://macbookpro.example.com",
                          uiOrigin: "https://ui-macbookpro.example.com", isSelf: true),
            RemoteMachine(name: "macstudio", apiOrigin: "https://MacStudio.example.com",
                          uiOrigin: "https://ui-macstudio.example.com", isSelf: false),
            RemoteMachine(name: "mini", apiOrigin: "https://mini.example.com",
                          uiOrigin: "https://ui-mini.example.com", isSelf: false)
        ]

        let rows = MachinePickerRows(profiles: state, registry: registry)
        XCTAssertEqual(rows.paired.map(\.name), ["macbookpro", "192.168.1.20", "macstudio"])
        XCTAssertEqual(rows.paired.map(\.isActive), [true, false, false])
        XCTAssertEqual(rows.unpaired.map(\.name), ["mini"])
    }

    func testMachinePickerPrefersTheStoredMachineNameAndTreatsItsRegistryEntryAsPaired() throws {
        var state = MobileConnectionProfiles.empty
        state.install(try profile("http://192.168.68.57:3002", machine: "macbookpro"))
        let registry = [
            RemoteMachine(name: "macbookpro", apiOrigin: "https://macbookpro.example.com",
                          uiOrigin: "https://ui-macbookpro.example.com", isSelf: true),
            RemoteMachine(name: "mini", apiOrigin: "https://mini.example.com",
                          uiOrigin: "https://ui-mini.example.com", isSelf: false)
        ]

        let rows = MachinePickerRows(profiles: state, registry: registry)
        XCTAssertEqual(rows.paired.map(\.name), ["macbookpro"])
        XCTAssertEqual(rows.paired.map(\.host), ["192.168.68.57"])
        XCTAssertEqual(rows.unpaired.map(\.name), ["mini"])
    }

    func testRemoteMachinesClientUsesTheProfilesOwnCredential() async throws {
        let studio = try profile("https://macstudio.example.com", token: "studio")
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.absoluteString, "https://macstudio.example.com/api/magician/v2/remote-machines")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer studio")
            XCTAssertEqual(request.value(forHTTPHeaderField: "CF-Access-Client-Id"), "cf-studio")
            return (response(for: request), jsonData([
                "self": "macstudio",
                "machines": [
                    ["name": "macstudio", "api_origin": "https://macstudio.example.com",
                     "ui_origin": "https://ui-macstudio.example.com", "is_self": true]
                ]
            ]))
        }

        let result = try await RemoteMachinesClient.fetch(profile: studio, session: makeMockSession())
        XCTAssertEqual(result.selfName, "macstudio")
        XCTAssertEqual(result.machines.first?.uiOrigin, "https://ui-macstudio.example.com")
        XCTAssertEqual(result.machines.first?.isSelf, true)
    }
}
