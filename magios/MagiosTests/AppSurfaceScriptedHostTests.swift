import XCTest
@testable import Magician

final class AppSurfaceScriptedHostTests: XCTestCase {
    private let digest = "blake3:\(String(repeating: "a", count: 64))"
    private let session = "bridge-scripted:install_1:abc"

    func testCanonicalAssetTailsAdmitOnlyDigestAddressedSurfacesMembers() throws {
        XCTAssertTrue(AppSurfaceScriptedPolicy.isCanonicalAssetTail(
            digest: digest,
            path: "surfaces/canvas.html"
        ))
        for hostile in [
            "not-a-digest",
            String(repeating: "a", count: 64),
            "blake3:abc",
            "blake3:\(String(repeating: "A", count: 64))",
            "md5:\(String(repeating: "a", count: 64))",
        ] {
            XCTAssertFalse(
                AppSurfaceScriptedPolicy.isCanonicalAssetTail(digest: hostile, path: "surfaces/canvas.html"),
                hostile
            )
        }
        for hostilePath in [
            "SKILL.md",
            "surfaces/../SKILL.md",
            "surfaces/canvas%2ejs",
            "surfaces\\canvas.html",
        ] {
            XCTAssertFalse(
                AppSurfaceScriptedPolicy.isCanonicalAssetTail(digest: digest, path: hostilePath),
                hostilePath
            )
        }
    }

    func testSchemeOriginsArePerInstallationAndMutuallyCrossOrigin() throws {
        let url = try XCTUnwrap(AppSurfaceScriptedPolicy.schemeURL(
            installationID: "install_1",
            digest: digest,
            path: "surfaces/canvas.html",
            session: session
        ))
        XCTAssertEqual(url.scheme, "magapp-surface")
        XCTAssertEqual(url.host, "install_1")
        XCTAssertEqual(url.path, "/\(session)/\(digest)/surfaces/canvas.html")
        XCTAssertNil(url.query)
        // Path escapes and hostile tails never construct an origin at all,
        // and neither does a session reference a path would rewrite.
        XCTAssertNil(AppSurfaceScriptedPolicy.schemeURL(
            installationID: "install_1",
            digest: "nope",
            path: "surfaces/canvas.html",
            session: session
        ))
        for hostileSession in ["", "bridge script", "bridge&script", "bridge#script", "bridge%20script"] {
            XCTAssertNil(
                AppSurfaceScriptedPolicy.schemeURL(
                    installationID: "install_1",
                    digest: digest,
                    path: "surfaces/canvas.html",
                    session: hostileSession
                ),
                hostileSession
            )
        }
    }

    func testDestinationMappingStaysInsideTheInstallationAssetRoute() throws {
        let origin = try XCTUnwrap(URL(string: "https://home.example"))
        let schemeURL = try XCTUnwrap(AppSurfaceScriptedPolicy.schemeURL(
            installationID: "install_1",
            digest: digest,
            path: "surfaces/canvas.html",
            session: session
        ))
        let destination = try XCTUnwrap(AppSurfaceScriptedPolicy.destinationURL(
            for: schemeURL,
            origin: origin,
            installationID: "install_1"
        ))
        XCTAssertEqual(
            destination.absoluteString,
            "https://home.example/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/\(session)/\(digest)/surfaces/canvas.html"
        )
        // A sibling subresource — exactly the scheme URL a relative
        // `<script src="canvas.js">` resolves to, session path retained — maps
        // through the same installation-scoped asset route.
        let sibling = try XCTUnwrap(URL(
            string: "magapp-surface://install_1/\(session)/\(digest)/surfaces/canvas.js"
        ))
        let siblingDestination = try XCTUnwrap(AppSurfaceScriptedPolicy.destinationURL(
            for: sibling,
            origin: origin,
            installationID: "install_1"
        ))
        XCTAssertEqual(
            siblingDestination.absoluteString,
            "https://home.example/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/\(session)/\(digest)/surfaces/canvas.js"
        )
        // Another installation's frame cannot proxy through this one.
        XCTAssertNil(AppSurfaceScriptedPolicy.destinationURL(
            for: schemeURL,
            origin: origin,
            installationID: "install_2"
        ))
        // Non-canonical tails are refused even when scheme and host match:
        // the proxied destination gets the same `isCanonicalAssetTail`
        // validation the initial entry URL gets.
        for hostile in [
            URL(string: "magapp-surface://install_1/\(session)/not-a-digest/SKILL.md"),
            URL(string: "magapp-surface://install_1/\(session)/\(digest)/SKILL.md"),
            URL(string: "magapp-surface://install_1/\(session)/\(digest)/surfaces/canvas%2ejs"),
        ].compactMap({ $0 }) {
            XCTAssertNil(
                AppSurfaceScriptedPolicy.destinationURL(
                    for: hostile,
                    origin: origin,
                    installationID: "install_1"
                ),
                hostile.absoluteString
            )
        }
        // The session path segment is the frame's only credential and it is
        // mandatory: missing, rewritten, or query-augmented paths are
        // refused here exactly as the backend asset route refuses them.
        for hostile in [
            URL(string: "magapp-surface://install_1/\(digest)/surfaces/canvas.html"),
            URL(string: "magapp-surface://install_1//\(digest)/surfaces/canvas.html"),
            URL(string: "magapp-surface://install_1/other/\(digest)/surfaces/canvas.html?x=1"),
            URL(string: "magapp-surface://install_1/\(session)/\(digest)/surfaces/canvas.html?route=/canvas"),
        ].compactMap({ $0 }) {
            XCTAssertNil(
                AppSurfaceScriptedPolicy.destinationURL(
                    for: hostile,
                    origin: origin,
                    installationID: "install_1"
                ),
                hostile.absoluteString
            )
        }
    }

    func testNavigationLocksAdmitOnlyTheInitialEntryLoad() throws {
        let initial = try XCTUnwrap(AppSurfaceScriptedPolicy.schemeURL(
            installationID: "install_1",
            digest: digest,
            path: "surfaces/canvas.html",
            session: session
        ))
        XCTAssertTrue(AppSurfaceScriptedPolicy.permits(
            navigationURL: initial,
            initialURL: initial,
            isInitialLoad: true
        ))
        // Every later navigation, anywhere, is denied — including a second
        // load of the same document (that is a reload against the budget).
        XCTAssertFalse(AppSurfaceScriptedPolicy.permits(
            navigationURL: initial,
            initialURL: initial,
            isInitialLoad: false
        ))
        let elsewhere = try XCTUnwrap(URL(string: "https://evil.example/canvas.html"))
        XCTAssertFalse(AppSurfaceScriptedPolicy.permits(
            navigationURL: elsewhere,
            initialURL: initial,
            isInitialLoad: true
        ))
        // Redirects are rejected unconditionally.
        XCTAssertFalse(AppSurfaceScriptedPolicy.admitsRedirect())
    }

    func testTheBridgeMethodSetIsExactlyTheEightSupportedPublicOperations() throws {
        for method in [
            "query_data",
            "mutate_data",
            "launch_action",
            "get_action_run",
            "compose_action_run",
            "cancel_action_run",
            "read_entity_changes",
            "contract_capabilities",
        ] {
            XCTAssertTrue(AppSurfaceScriptedPolicy.bridgeMethodIsAdmitted(method), method)
        }
        for hostile in [
            "subscribe",
            "wait_action_run",
            "query_data_v2",
            "eval",
            "",
        ] {
            XCTAssertFalse(AppSurfaceScriptedPolicy.bridgeMethodIsAdmitted(hostile), hostile)
        }
        XCTAssertEqual(AppSurfaceScriptedPolicy.bridgeMethods.count, 8)
    }

    func testBridgeHTTPSubmissionFIFOStartsOnlyOneSequenceAtATime() {
        let inspected = expectation(description: "FIFO inspected on its main-thread owner")
        DispatchQueue.main.async {
            var started: [Int] = []
            var completions: [() -> Void] = []
            let fifo = AppSurfaceScriptedBridgeSubmissionFIFO<Int> { sequence, completion in
                started.append(sequence)
                completions.append(completion)
            }

            fifo.enqueue(1)
            fifo.enqueue(2)
            fifo.enqueue(3)
            XCTAssertEqual(
                started,
                [1],
                "later sequence numbers must not race the first HTTP request"
            )

            completions.removeFirst()()
            XCTAssertEqual(started, [1, 2])
            completions.removeFirst()()
            XCTAssertEqual(started, [1, 2, 3])
            completions.removeFirst()()
            XCTAssertEqual(started, [1, 2, 3])
            inspected.fulfill()
        }
        wait(for: [inspected], timeout: 1)
    }

    // MARK: - Reply delivery and reload/crash budget (1.6 review fixes)

    func testJavaScriptSafeJSONEscapesTheLineTerminatorsJSONMayEmit() throws {
        // JSONSerialization legally emits raw U+2028/U+2029 inside string
        // literals; both are JavaScript line terminators, so the
        // interpolated evaluateJavaScript program would throw and the
        // reply would be lost. The escape is JSON-equivalent and inert to
        // the JS parser.
        let payload = "{\"result\":\"line1\u{2028}line2\u{2029}line3\"}"
        let safe = AppSurfaceScriptedPolicy.javaScriptSafeJSON(payload)
        XCTAssertFalse(safe.contains("\u{2028}"))
        XCTAssertFalse(safe.contains("\u{2029}"))
        XCTAssertEqual(safe, "{\"result\":\"line1\\u2028line2\\u2029line3\"}")
        // Payloads without the terminators are byte-identical.
        XCTAssertEqual(
            AppSurfaceScriptedPolicy.javaScriptSafeJSON("{\"result\":1}"),
            "{\"result\":1}"
        )
        // The escaped payload still decodes to the same JSON string.
        let decoded = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(safe.utf8)) as? [String: String])
        XCTAssertEqual(decoded["result"], "line1\u{2028}line2\u{2029}line3")
    }

    func testReloadNoteURITargetsTheKernelReloadBudgetRoute() throws {
        let origin = try XCTUnwrap(URL(string: "https://home.example"))
        let url = try XCTUnwrap(AppSurfaceScriptedPolicy.reloadNoteURL(
            origin: origin,
            installationID: "install_1",
            session: session
        ))
        XCTAssertEqual(
            url.absoluteString,
            "https://home.example/api/magician/v2/apps/installations/install_1/custom-surface-v1/sessions/\(session)/reload-note"
        )
        // A session reference a query or path could rewrite is refused —
        // the route is authenticated, so a hostile reference buys nothing.
        XCTAssertNil(AppSurfaceScriptedPolicy.reloadNoteURL(
            origin: origin,
            installationID: "install_1",
            session: "bridge script"
        ))
    }

    // MARK: - Launcher wiring (1.6 completion)

    func testOnlyTheInstallationRootIsProbedForScriptedHosting() throws {
        XCTAssertTrue(AppSurfaceScriptedPolicy.probesScriptedHostAtRouteRoot("/apps/install_1"))
        XCTAssertFalse(
            AppSurfaceScriptedPolicy.probesScriptedHostAtRouteRoot("/apps/install_1/canvas"),
            "a declared view route hydrates and never falls through to the scripted host"
        )
        XCTAssertFalse(AppSurfaceScriptedPolicy.probesScriptedHostAtRouteRoot("/apps/install_1/canvas/nested"))
        for hostile in [
            "",
            "/",
            "/apps",
            "/apps/../install_1",
            "apps/install_1",
            "https://evil.example/apps/install_1",
        ] {
            XCTAssertFalse(
                AppSurfaceScriptedPolicy.probesScriptedHostAtRouteRoot(hostile),
                hostile
            )
        }
    }

    func testHostProbeRequestMirrorsTheWebHostFlow() throws {
        let profile = try MobileConnectionProfile(
            publicOrigin: try XCTUnwrap(URL(string: "http://127.0.0.1:3017")),
            principal: "owner",
            workspace: "personal",
            deviceID: "iphone-1",
            deviceToken: "device-token",
            cloudflareClientID: "outer-id",
            cloudflareClientSecret: "outer-secret"
        )
        let root = try AppSurfaceScriptedHostClient.hostRequest(
            profile: profile,
            installationID: "install_1"
        )
        XCTAssertEqual(root.httpMethod, "GET")
        XCTAssertEqual(
            root.url?.path,
            "/api/magician/v2/apps/installations/install_1/custom-surface-v1/host"
        )
        // The root probe omits `?route=`: the kernel answers with the
        // package's first declared entry point, exactly like the web host.
        XCTAssertNil(root.url?.query)
        XCTAssertEqual(root.value(forHTTPHeaderField: "Accept"), "application/json")
        // Authentication is attached here, never in the frame.
        XCTAssertNil(root.value(forHTTPHeaderField: "X-Principal"))
        XCTAssertNil(root.value(forHTTPHeaderField: "X-Workspace"))
        XCTAssertEqual(root.value(forHTTPHeaderField: "Authorization"), "Bearer device-token")
        XCTAssertEqual(root.value(forHTTPHeaderField: "CF-Access-Client-Id"), "outer-id")
        XCTAssertThrowsError(try AppSurfaceScriptedHostClient.hostRequest(
            profile: profile,
            installationID: "not a valid id"
        ))
    }

    func testPlanDecodingIsInstallationBoundAndCanonical() throws {
        func planJSON(installationID: String, document: String, digest: String) throws -> Data {
            try JSONSerialization.data(withJSONObject: [
                "installation_id": installationID,
                "session_ref": "bridge-scripted:\(installationID):abc",
                "nonce": "nonce:abc",
                "package_revision_ref": "pkg:1",
                "surface_revision": 3,
                "grant_revision": 2,
                "entry_document_digest": digest,
                "entry_document": document
            ])
        }
        let plan = try AppSurfaceScriptedHostClient.decodePlan(
            planJSON(installationID: "install_1", document: "surfaces/canvas.html", digest: digest),
            installationID: "install_1"
        )
        XCTAssertEqual(plan.installationID, "install_1")
        XCTAssertEqual(plan.entryDocument, "surfaces/canvas.html")
        // A plan minted for another installation is refused — the plan is
        // authority the frame never supplies.
        XCTAssertThrowsError(try AppSurfaceScriptedHostClient.decodePlan(
            planJSON(installationID: "install_2", document: "surfaces/canvas.html", digest: digest),
            installationID: "install_1"
        ))
        // So is any non-HTML or non-`surfaces/` entry document, any
        // non-digest entry identity, and any missing binding field.
        for hostile in try [
            planJSON(installationID: "install_1", document: "SKILL.md", digest: digest),
            planJSON(installationID: "install_1", document: "surfaces/canvas.js", digest: digest),
            planJSON(installationID: "install_1", document: "surfaces/canvas.html", digest: "nope"),
        ] {
            XCTAssertThrowsError(
                try AppSurfaceScriptedHostClient.decodePlan(hostile, installationID: "install_1")
            )
        }
        XCTAssertThrowsError(try AppSurfaceScriptedHostClient.decodePlan(
            try JSONSerialization.data(withJSONObject: ["session_ref": "bridge-scripted:x:y"]),
            installationID: "install_1"
        ))
        // An empty package_revision_ref fails the same guard the web host
        // enforces (the wiring-review parity fix).
        var noPkgRev = try planJSON(installationID: "install_1", document: "surfaces/canvas.html", digest: "blake3:" + String(repeating: "a", count: 64))
        let planText = String(data: noPkgRev, encoding: .utf8)!
        let emptied = planText.replacingOccurrences(
            of: "\"package_revision_ref\":\"pkg:1\"",
            with: "\"package_revision_ref\":\"\""
        )
        noPkgRev = Data(emptied.utf8)
        XCTAssertThrowsError(
            try AppSurfaceScriptedHostClient.decodePlan(noPkgRev, installationID: "install_1")
        )
    }

    func testAMintedPlanMountsItsPerInstallationSchemeOrigin() throws {
        let plan = try AppSurfaceScriptedHostClient.decodePlan(
            try JSONSerialization.data(withJSONObject: [
                "installation_id": "install_1",
                "session_ref": "bridge-scripted:install_1:abc",
                "nonce": "nonce:abc",
                "package_revision_ref": "pkg:1",
                "surface_revision": 3,
                "grant_revision": 2,
                "entry_document_digest": digest,
                "entry_document": "surfaces/canvas.html"
            ]),
            installationID: "install_1"
        )
        // Entry routing mirrors the web host: the frame's initial URL is
        // derived from the plan's own digest-keyed entry document, never
        // from the launcher page URL, and carries the plan's session
        // reference as its inherited path segment.
        let initial = try XCTUnwrap(AppSurfaceScriptedPolicy.schemeURL(
            installationID: plan.installationID,
            digest: plan.entryDocumentDigest,
            path: plan.entryDocument,
            session: plan.sessionRef
        ))
        XCTAssertEqual(initial.scheme, "magapp-surface")
        XCTAssertEqual(initial.host, "install_1")
        XCTAssertEqual(initial.path, "/bridge-scripted:install_1:abc/\(digest)/surfaces/canvas.html")
        XCTAssertNil(initial.query)
    }

    // MARK: - Page-bounded mini-frame host (gate S4)

    func testMiniFrameEntryPointsAdmitOnlyDeclaredParameterFreeRoutes() {
        XCTAssertTrue(AppMiniFramePolicy.isDeclaredEntryPoint("/canvas"))
        XCTAssertTrue(AppMiniFramePolicy.isDeclaredEntryPoint("/canvas/board.v2"))
        // V1 has no input mapping for a widget, so a parameterised route names
        // a document this client cannot bind.
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("/canvas/:id"))
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("/canvas/../../etc"))
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("/canvas?x=1"))
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("/canvas#frag"))
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("/canvas%2e"))
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("canvas"))
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint(""))
        // Web parity: `/` splits to one EMPTY segment and is not an entry point.
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("/"))
        XCTAssertFalse(AppMiniFramePolicy.isDeclaredEntryPoint("/" + String(repeating: "a", count: 600)))

        XCTAssertNotNil(AppMiniFrameDeclaration(entryPoint: "/canvas", maxHeightPx: 240))
        // Widget-sized, not page-sized.
        XCTAssertNil(AppMiniFrameDeclaration(
            entryPoint: "/canvas",
            maxHeightPx: AppMiniFramePolicy.maximumFrameHeightPx + 1
        ))
        XCTAssertNil(AppMiniFrameDeclaration(entryPoint: "/canvas", maxHeightPx: 0))
        XCTAssertNil(AppMiniFrameDeclaration(entryPoint: "/canvas/:id", maxHeightPx: 240))
    }

    @MainActor
    func testMiniFrameBudgetsBoundThePageTheSessionAndTheVisibleFrames() {
        let ledger = AppMiniFrameSessionLedger()
        let now = Date(timeIntervalSince1970: 1_000_000)
        let page = ledger.openPage()

        XCTAssertEqual(page.admit(key: "a", now: now), .admitted)
        XCTAssertEqual(page.admit(key: "b", now: now), .admitted)
        // Two per page is the page budget; a third is refused, not queued.
        XCTAssertEqual(page.admit(key: "c", now: now), .refused(.pageBudget))
        // Re-admitting a held key is the same frame and costs nothing.
        XCTAssertEqual(page.admit(key: "a", now: now), .admitted)
        XCTAssertEqual(ledger.sessionSpentForTest, 2)

        // Two mounted anywhere is the app-wide visible limit, so a SECOND page
        // is refused on the visible budget before its own page budget applies.
        let other = ledger.openPage()
        XCTAssertEqual(other.admit(key: "d", now: now), .refused(.visibleBudget))

        // Releasing frees the visible slot but never refunds the page budget:
        // the escalation was reviewed and taken.
        page.release(key: "a")
        page.release(key: "b")
        XCTAssertEqual(page.admit(key: "c", now: now), .refused(.pageBudget))
        XCTAssertEqual(other.admit(key: "d", now: now), .admitted)

        // The session budget is the outer bound: fresh pages keep spending it.
        var spent = ledger.sessionSpentForTest
        var index = 0
        while spent < AppMiniFramePolicy.maximumFramesPerSession {
            let lease = ledger.openPage()
            XCTAssertEqual(lease.admit(key: "s\(index)", now: now), .admitted)
            lease.release(key: "s\(index)")
            spent += 1
            index += 1
        }
        XCTAssertEqual(
            ledger.openPage().admit(key: "last", now: now),
            .refused(.sessionBudget)
        )

        // Closing a page returns every slot it still held.
        other.close()
        XCTAssertFalse(ledger.isMounted(key: "d"))
        XCTAssertEqual(ledger.mountedCountForTest, 0)
        // A closed lease admits nothing further.
        XCTAssertEqual(other.admit(key: "d", now: now), .refused(.pageBudget))
    }

    @MainActor
    func testMiniFrameUnmountTTLReleasesFramesThatStayOutOfView() {
        let ledger = AppMiniFrameSessionLedger()
        let now = Date(timeIntervalSince1970: 2_000_000)
        let page = ledger.openPage()
        XCTAssertEqual(page.admit(key: "a", now: now), .admitted)

        // A frame starts hidden and its clock starts at admission: one admitted
        // below the fold must expire on its own.
        XCTAssertTrue(page.sweepExpired(now: now.addingTimeInterval(1)).isEmpty)
        XCTAssertEqual(
            page.sweepExpired(now: now.addingTimeInterval(AppMiniFramePolicy.unmountTTLSeconds)),
            ["a"]
        )
        XCTAssertFalse(ledger.isMounted(key: "a"))

        let second = ledger.openPage()
        XCTAssertEqual(second.admit(key: "b", now: now), .admitted)
        second.noteVisibility(key: "b", visible: true, now: now)
        XCTAssertTrue(second.sweepExpired(now: now.addingTimeInterval(600)).isEmpty)
        second.noteVisibility(key: "b", visible: false, now: now.addingTimeInterval(600))
        // A repeated hidden report must not keep pushing the deadline forward.
        second.noteVisibility(key: "b", visible: false, now: now.addingTimeInterval(620))
        XCTAssertEqual(
            second.sweepExpired(
                now: now.addingTimeInterval(600 + AppMiniFramePolicy.unmountTTLSeconds)
            ),
            ["b"]
        )
        // A key another lease's sweep already reclaimed is forgotten here too,
        // so closing this page cannot release a slot handed to someone else.
        second.close()
        XCTAssertEqual(ledger.mountedCountForTest, 0)
    }

    func testMiniFrameHostPlanIsAdmittedOnlyForItsExactTarget() throws {
        let target = try XCTUnwrap(AppMiniFrameTarget(item: try framedWidget()))
        XCTAssertEqual(target.ledgerKey, "install_1\u{0}plans\u{0}4")

        let plan = try AppMiniFrameHostClient.decodePlan(try encodeJSON(miniFramePlan()), target: target)
        XCTAssertEqual(plan.maxHeightPx, 240)
        // The proxied frame origin maps back onto exactly the minted entry URL,
        // so the address the frame loads and the address the host authorized
        // cannot drift apart.
        let schemeURL = try XCTUnwrap(AppSurfaceScriptedPolicy.schemeURL(
            installationID: plan.installationID,
            digest: plan.entryDocumentDigest,
            path: plan.entryDocument,
            session: plan.sessionRef
        ))
        let destination = try XCTUnwrap(AppSurfaceScriptedPolicy.destinationURL(
            for: schemeURL,
            origin: try XCTUnwrap(URL(string: "https://self-hosted.example:8443")),
            installationID: plan.installationID,
            assetRoute: AppMiniFramePolicy.assetRoute
        ))
        XCTAssertEqual(destination.path, plan.entryURL)

        // A plan for another installation, widget or generation is a stale or
        // substituted binding: mounting it would give one app's declaration
        // another app's frame.
        for override in [
            ["installation_id": "install_2"],
            ["widget_id": "other"],
            ["installation_generation": 5],
            ["entry_point": "/other"],
            ["max_height_px": 241],
            ["schema_version": 2],
            ["sandbox": "allow-scripts allow-same-origin"],
            ["csp": "default-src 'none'; script-src 'self'; frame-ancestors 'self'"],
            ["entry_document": "assets/canvas.html"],
            ["entry_url": "/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/x"],
            ["session_ref": "mini frame"],
            ["nonce": ""]
        ] as [[String: Any]] {
            XCTAssertThrowsError(
                try AppMiniFrameHostClient.decodePlan(
                    try encodeJSON(miniFramePlan(overrides: override)),
                    target: target
                ),
                "override \(override) must be refused"
            )
        }
    }

    func testMiniFrameHostRequestNamesTheDeclaredWidgetAndCarriesNoFrameCredential() throws {
        let profile = try MobileConnectionProfile(
            publicOrigin: try XCTUnwrap(URL(string: "https://self-hosted.example:8443")),
            principal: "owner",
            workspace: "personal",
            deviceID: "iphone-1",
            deviceToken: "device-token"
        )
        let target = try XCTUnwrap(AppMiniFrameTarget(item: try framedWidget()))
        let request = try AppMiniFrameHostClient.hostRequest(profile: profile, target: target)
        XCTAssertEqual(
            request.url?.path,
            "/api/magician/v2/apps/installations/install_1/mini-frame-v1/host"
        )
        XCTAssertEqual(request.url?.query, "widget=plans")
        XCTAssertNil(request.httpBody)
    }

    /// The pin for the closed door. `compile_native_manifest_widgets` strips a
    /// mini-frame declaration's entry point and its `mini_frame_v1` capability,
    /// so nothing the runtime renders today names a frame and this client
    /// mounts none. The rest asserts the client half stays fail-closed for the
    /// day that changes.
    func testTodaysRenderedWidgetsNameNoMiniFrameAndAMalformedOneIsRefused() throws {
        let item = try renderedWidget(extra: [:])
        XCTAssertNil(item.miniFrame)
        XCTAssertNil(AppMiniFrameTarget(item: item))

        let framed = try framedWidget()
        XCTAssertEqual(framed.miniFrame?.entryPoint, "/canvas")
        XCTAssertEqual(framed.miniFrame?.maxHeightPx, 240)

        // A declaration this client cannot admit fails the WHOLE item rather
        // than degrading to a frameless render of unreviewed shape.
        XCTAssertThrowsError(try renderedWidget(extra: [
            "mini_frame": ["entry_point": "/canvas/:id", "max_height_px": 240] as [String: Any]
        ]))
        XCTAssertThrowsError(try renderedWidget(extra: [
            "mini_frame": ["entry_point": "/canvas", "max_height_px": 4_000] as [String: Any]
        ]))
        // The member set is exact in both directions.
        XCTAssertThrowsError(try renderedWidget(extra: [
            "mini_frame": ["entry_point": "/canvas"] as [String: Any]
        ]))
        XCTAssertThrowsError(try renderedWidget(extra: [
            "mini_frame": [
                "entry_point": "/canvas",
                "max_height_px": 240,
                "sandbox": "allow-same-origin"
            ] as [String: Any]
        ]))
        // A non-ready widget may not claim a frame: it would be asking this
        // client to run app code in place of content the host could not make.
        XCTAssertThrowsError(try renderedWidget(
            state: "unavailable",
            extra: ["mini_frame": ["entry_point": "/canvas", "max_height_px": 240] as [String: Any]]
        ))
    }

    private func framedWidget() throws -> AppNativeWidgetItem {
        try renderedWidget(
            installationID: "install_1",
            extra: ["mini_frame": ["entry_point": "/canvas", "max_height_px": 240] as [String: Any]]
        )
    }

    private func miniFramePlan(overrides: [String: Any] = [:]) -> [String: Any] {
        let session = "mini-frame:install_1:abc"
        var plan: [String: Any] = [
            "schema_version": 1,
            "sandbox": "allow-scripts",
            "csp": "default-src 'none'; connect-src 'none'; script-src 'self'; frame-ancestors 'self'",
            "session_ref": session,
            "nonce": "nonce:abc",
            "installation_id": "install_1",
            "installation_generation": 4,
            "package_revision_ref": "app-package-revision:one",
            "widget_id": "plans",
            "entry_point": "/canvas",
            "entry_document": "surfaces/canvas.html",
            "entry_document_digest": digest,
            "entry_url": AppMiniFrameHostPlan.expectedEntryURL(
                installationID: "install_1",
                sessionRef: session,
                digest: digest,
                document: "surfaces/canvas.html"
            ),
            "max_height_px": 240
        ]
        plan.merge(overrides) { _, new in new }
        return plan
    }

    private func renderedWidget(
        installationID: String = "install_1",
        state: String = "ready",
        extra: [String: Any]
    ) throws -> AppNativeWidgetItem {
        var widget: [String: Any] = [
            "installation_id": installationID,
            "widget_id": "plans",
            "title": "Plans",
            "installation_generation": 4,
            "revision": digest,
            "rendered_at": "2026-09-04T10:00:00Z",
            "refresh_after": "2026-09-04T10:01:00Z",
            "state": state
        ]
        if state == "ready" {
            widget["model"] = [
                "model": "list",
                "rows": [[String: Any]](),
                "hints": ["display_field": "title"],
                "actions": [[String: Any]]()
            ] as [String: Any]
        }
        widget.merge(extra) { _, new in new }
        let batch = try AppNativeSurfaceContract.decodeWidgets(
            try encodeJSON([
                "schema_version": 1,
                "revision": digest,
                "etag": digest,
                "rendered_at": "2026-09-04T10:00:00Z",
                "refresh_after": "2026-09-04T10:01:00Z",
                "widgets": [widget]
            ])
        )
        return try XCTUnwrap(batch.widgets.first)
    }

    private func encodeJSON(_ value: Any) throws -> Data {
        try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    }
}
