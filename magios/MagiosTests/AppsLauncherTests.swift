import XCTest
@testable import Magician

final class AppsLauncherTests: XCTestCase {
    func testDirectoryPageStrictlyDecodesTheBoundedMetadataContract() throws {
        let page = try AppsDirectoryContract.decodePage(try encodedPage())

        XCTAssertEqual(page.entries.count, 1)
        XCTAssertEqual(page.entries[0].installationID, "install-1")
        XCTAssertEqual(page.entries[0].status, .enabled)
        XCTAssertEqual(page.entries[0].icon, AppsDirectoryIcon(kind: "monogram", value: "TS"))
        XCTAssertEqual(page.entries[0].views.map(\.viewID), ["main"])
        XCTAssertEqual(page.entries[0].actions.map(\.actionID), ["refresh"])
        XCTAssertFalse(page.entries[0].actions[0].pinned)
        XCTAssertEqual(page.entries[0].storage.revisionCount, 4)
        XCTAssertEqual(page.entries[0].permissions?.grantedTools, 2)
        XCTAssertEqual(page.nextCursor, "appdir:2:install-1")
        XCTAssertTrue(page.hasMore)
    }

    func testDirectoryContractRejectsUnknownFieldsAndUnknownLifecycleStates() throws {
        var unknown = validPage()
        unknown["unexpected"] = true
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(unknown)))

        var invalidStatus = validPage()
        var entries = try XCTUnwrap(invalidStatus["entries"] as? [[String: Any]])
        entries[0]["status"] = "invented"
        invalidStatus["entries"] = entries
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(invalidStatus)))

        var unknownAction = validPage()
        entries = try XCTUnwrap(unknownAction["entries"] as? [[String: Any]])
        var actions = try XCTUnwrap(entries[0]["actions"] as? [[String: Any]])
        actions[0]["payload"] = ["secret": "must-not-enter-the-directory"]
        entries[0]["actions"] = actions
        unknownAction["entries"] = entries
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(unknownAction)))
    }

    /// The directory is one wire for two clients. The web shell mounts an
    /// installed system package's declared navigation; the phone has nowhere
    /// to put it and ignores it. Without the key named in `CodingKeys` the
    /// strict unknown-field check would throw on the *whole page*, so the
    /// first system package to declare a console would blank the iOS Apps
    /// launcher rather than merely adding nothing to it.
    func testDirectoryEntryToleratesWebOnlyNavigationWithoutRenderingIt() throws {
        var declared = validPage()
        var entries = try XCTUnwrap(declared["entries"] as? [[String: Any]])
        entries[0]["navigation"] = [[
            "id": "meetings_console",
            "title": "Meetings",
            "route": "/meetings-console",
            "placement": ["kind": "section", "section": "observe"],
            "surface": ["kind": "view", "view": "main"]
        ]]
        declared["entries"] = entries
        let page = try AppsDirectoryContract.decodePage(try encode(declared))
        XCTAssertEqual(page.entries.count, 1)
        XCTAssertEqual(page.entries[0].views.map(\.viewID), ["main"])
    }

    func testDirectoryContractRejectsDuplicateAndIncoherentPaginationIdentities() throws {
        var duplicateEntry = validPage()
        let entry = try XCTUnwrap((duplicateEntry["entries"] as? [[String: Any]])?.first)
        duplicateEntry["entries"] = [entry, entry]
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(duplicateEntry)))

        var repeatedView = validPage()
        var entries = try XCTUnwrap(repeatedView["entries"] as? [[String: Any]])
        let view = try XCTUnwrap((entries[0]["views"] as? [[String: Any]])?.first)
        entries[0]["views"] = [view, view]
        repeatedView["entries"] = entries
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(repeatedView)))

        var missingCursor = validPage()
        missingCursor.removeValue(forKey: "next_cursor")
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(missingCursor)))

        var staleCursor = validPage()
        staleCursor["has_more"] = false
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(staleCursor)))
    }

    func testDirectoryContractRejectsCrossInstallationRoutesAndSummaryDrift() throws {
        var foreignRoute = validPage()
        var entries = try XCTUnwrap(foreignRoute["entries"] as? [[String: Any]])
        var views = try XCTUnwrap(entries[0]["views"] as? [[String: Any]])
        views[0]["route"] = "/apps/install-2/main"
        entries[0]["views"] = views
        entries[0]["default_route"] = "/apps/install-2/main"
        foreignRoute["entries"] = entries
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(foreignRoute)))

        var summaryDrift = validPage()
        entries = try XCTUnwrap(summaryDrift["entries"] as? [[String: Any]])
        entries[0]["payload_bytes"] = 999
        summaryDrift["entries"] = entries
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(try encode(summaryDrift)))
    }

    func testDirectoryContractCapsBytesAndJSONDepthBeforeDecoding() throws {
        XCTAssertThrowsError(
            try AppsDirectoryContract.decodePage(Data(repeating: 0x20, count: AppsDirectoryContract.maximumResponseBytes + 1))
        )
        let nested = String(repeating: "[", count: AppsDirectoryContract.maximumJSONDepth + 1)
            + "0"
            + String(repeating: "]", count: AppsDirectoryContract.maximumJSONDepth + 1)
        XCTAssertThrowsError(try AppsDirectoryContract.decodePage(Data(nested.utf8)))
    }

    func testDirectoryEntryDecodesCustomSurfaceHostabilityForOpenAppGating() throws {
        func viewlessPayload(count: Any?) throws -> [String: Any] {
            var payload = validPage()
            var entries = try XCTUnwrap(payload["entries"] as? [[String: Any]])
            entries[0]["views"] = []
            entries[0].removeValue(forKey: "default_route")
            if let count { entries[0]["custom_surface_entry_count"] = count } else {
                entries[0].removeValue(forKey: "custom_surface_entry_count")
            }
            payload["entries"] = entries
            return payload
        }

        // A declared custom-surface package reports its entry-point count.
        let hostable = try AppsDirectoryContract.decodePage(try encode(viewlessPayload(count: 2)))
            .entries[0]
        XCTAssertEqual(hostable.customSurfaceEntryCount, 2)
        XCTAssertTrue(hostable.status.canLaunch && hostable.customSurfaceEntryCount > 0)

        // Older servers omit the field (additive wire): nothing hostable, so
        // the viewless Open-app affordance stays hidden.
        let plain = try AppsDirectoryContract.decodePage(try encode(viewlessPayload(count: nil)))
            .entries[0]
        XCTAssertEqual(plain.customSurfaceEntryCount, 0)
        XCTAssertFalse(plain.status.canLaunch && plain.customSurfaceEntryCount > 0)

        // The manifest's eight-entry-point bound is enforced client-side too.
        XCTAssertThrowsError(
            try AppsDirectoryContract.decodePage(try encode(viewlessPayload(count: 9)))
        )
    }

    func testDirectoryResponseAccumulatorAppendsChunksAndRejectsTheFirstOverflow() throws {
        var accumulator = try BoundedAppsDirectoryResponseAccumulator(
            maximumBytes: 8,
            expectedLength: 6
        )
        try accumulator.append(Data([1, 2, 3]))
        try accumulator.append(Data([4, 5, 6]))
        XCTAssertEqual(accumulator.data, Data([1, 2, 3, 4, 5, 6]))
        XCTAssertThrowsError(try accumulator.append(Data([7, 8, 9])))
        XCTAssertThrowsError(try BoundedAppsDirectoryResponseAccumulator(
            maximumBytes: 8,
            expectedLength: 9
        ))
    }

    func testPaginationDeduplicatesOverlapsAndRejectsARepeatedCursor() throws {
        let first = try AppsDirectoryContract.decodePage(try encodedPage())
        var secondPayload = validPage()
        secondPayload["next_cursor"] = "appdir:3:install-2"
        let second = try AppsDirectoryContract.decodePage(try encode(secondPayload))
        let merged = try AppsDirectoryPagination.merge(
            existing: first.entries,
            page: second,
            requestedCursor: "appdir:2:install-1"
        )
        XCTAssertEqual(merged.entries.count, 1)
        XCTAssertEqual(merged.nextCursor, "appdir:3:install-2")

        XCTAssertThrowsError(try AppsDirectoryPagination.merge(
            existing: first.entries,
            page: first,
            requestedCursor: "appdir:2:install-1"
        ))
    }

    func testRoutesAreCanonicalBoundToTheirInstallationAndPreserveTheEnrolledOrigin() throws {
        let origin = try XCTUnwrap(URL(string: "https://self-hosted.example:8443"))
        let route = "/apps/install-1/dashboard?range=week"
        let url = try XCTUnwrap(
            AppRoutePolicy.routeURL(route, installationID: "install-1", origin: origin)
        )
        XCTAssertEqual(url.absoluteString, "https://self-hosted.example:8443/apps/install-1/dashboard?range=week")

        let scheme = try XCTUnwrap(AppRoutePolicy.schemeURL(for: url, origin: origin))
        XCTAssertEqual(scheme.host, "runtime")
        XCTAssertEqual(scheme.scheme, "magapp")
        XCTAssertEqual(AppRoutePolicy.destinationURL(for: scheme, origin: origin), url)

        XCTAssertFalse(AppRoutePolicy.routeBelongsToInstallation(route, installationID: "install-2"))
        XCTAssertFalse(AppRoutePolicy.isCanonicalRoute("/apps/install-1/%2e%2e/other"))
        XCTAssertFalse(AppRoutePolicy.isCanonicalRoute("/apps/install-1/%252e%252e/other"))
        XCTAssertFalse(AppRoutePolicy.isCanonicalRoute("https://attacker.example/apps/install-1/main"))
        XCTAssertFalse(AppRoutePolicy.sameOrigin(
            try XCTUnwrap(URL(string: "https://self-hosted.example:9443/apps/install-1/main")),
            origin
        ))
    }

    func testRouteProxyAllowsOnlyStaticAssetsAndTheSelectedInstallationsSurfaceAPIs() throws {
        XCTAssertTrue(AppRouteResourceLimits.admitsExpectedContentLength(-1))
        XCTAssertTrue(AppRouteResourceLimits.admitsExpectedContentLength(
            AppRouteResourceLimits.maximumResponseBytes
        ))
        XCTAssertFalse(AppRouteResourceLimits.admitsExpectedContentLength(
            AppRouteResourceLimits.maximumResponseBytes + 1
        ))
        XCTAssertTrue(AppRoutePolicy.permitsNetworkPath(
            "/apps/install-1/main", method: "GET", installationID: "install-1"
        ))
        XCTAssertTrue(AppRoutePolicy.permitsNetworkPath(
            "/_app/immutable/entry/app.js", method: "GET", installationID: "install-1"
        ))
        XCTAssertTrue(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-1/surfaces",
            method: "GET",
            installationID: "install-1"
        ))
        XCTAssertTrue(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-1/surfaces/reports/daily",
            method: "GET",
            installationID: "install-1"
        ))
        XCTAssertTrue(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-1/surfaces/reports/daily",
            method: "HEAD",
            installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-1/surfaces//daily",
            method: "GET",
            installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-1/surfaces/%252e%252e/directory",
            method: "GET",
            installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-1/surfaces/reports/daily",
            method: "POST",
            installationID: "install-1"
        ))
        XCTAssertTrue(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-1/surface-mutations",
            method: "POST",
            installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/installations/install-2/surfaces",
            method: "GET",
            installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/api/magician/v2/apps/directory", method: "GET", installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/_app/immutable/entry/app.js", method: "POST", installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/_app/../api/magician/v2/apps/directory", method: "GET", installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkPath(
            "/_app/%252e%252e/api/magician/v2/apps/directory", method: "GET", installationID: "install-1"
        ))
        XCTAssertTrue(AppRoutePolicy.permitsNetworkURL(
            try XCTUnwrap(URL(string: "https://self-hosted.example/apps/install-1/main?range=week")),
            method: "GET",
            installationID: "install-1"
        ))
        XCTAssertFalse(AppRoutePolicy.permitsNetworkURL(
            try XCTUnwrap(URL(string: "https://self-hosted.example/apps/install-1/main?query="
                + String(repeating: "x", count: 513))),
            method: "GET",
            installationID: "install-1"
        ))
        XCTAssertNil(AppRoutePolicy.destinationURL(
            for: try XCTUnwrap(URL(string: "magapp://user@runtime/apps/install-1/main")),
            origin: try XCTUnwrap(URL(string: "https://self-hosted.example"))
        ))
    }

    func testRequestsUseTheScannedRuntimeProfileAndExactActivityContracts() throws {
        let profile = try MobileConnectionProfile(
            publicOrigin: try XCTUnwrap(URL(string: "http://127.0.0.1:3017")),
            principal: "owner",
            workspace: "personal",
            deviceID: "iphone-1",
            deviceToken: "device-token",
            cloudflareClientID: "outer-id",
            cloudflareClientSecret: "outer-secret"
        )
        let directory = try AppsDirectoryClient.directoryRequest(
            profile: profile,
            section: .needsAttention,
            search: " travel ",
            limit: 25,
            cursor: "appdir:2:install-1"
        )
        XCTAssertEqual(directory.url?.scheme, "http")
        XCTAssertEqual(directory.url?.host, "127.0.0.1")
        XCTAssertEqual(directory.url?.port, 3017)
        XCTAssertEqual(directory.url?.path, "/api/magician/v2/apps/directory")
        let query = try XCTUnwrap(URLComponents(url: try XCTUnwrap(directory.url), resolvingAgainstBaseURL: false))
        XCTAssertEqual(query.queryItems?.first(where: { $0.name == "section" })?.value, "needs_attention")
        XCTAssertEqual(query.queryItems?.first(where: { $0.name == "search" })?.value, "travel")
        XCTAssertNil(directory.value(forHTTPHeaderField: "X-Principal"))
        XCTAssertNil(directory.value(forHTTPHeaderField: "X-Workspace"))
        XCTAssertEqual(directory.value(forHTTPHeaderField: "Authorization"), "Bearer device-token")
        XCTAssertEqual(directory.value(forHTTPHeaderField: "CF-Access-Client-Id"), "outer-id")

        let pinnedViews = try AppsDirectoryClient.directoryRequest(
            profile: profile,
            section: .pinned,
            search: "",
            limit: PinnedAppsTodayProjection.maximumItems,
            cursor: nil,
            pinnedTargetKind: .view
        )
        let pinnedQuery = try XCTUnwrap(
            URLComponents(url: try XCTUnwrap(pinnedViews.url), resolvingAgainstBaseURL: false)
        )
        XCTAssertEqual(
            pinnedQuery.queryItems?.first(where: { $0.name == "pinned_target_kind" })?.value,
            "view"
        )
        XCTAssertThrowsError(try AppsDirectoryClient.directoryRequest(
            profile: profile,
            section: .installed,
            search: "",
            limit: 8,
            cursor: nil,
            pinnedTargetKind: .view
        ))

        let opened = try AppsDirectoryClient.activityRequest(
            profile: profile,
            installationID: "install-1",
            activity: .opened(viewID: "main")
        )
        XCTAssertEqual(opened.httpMethod, "POST")
        XCTAssertEqual(opened.url?.path, "/api/magician/v2/apps/installations/install-1/directory-activity")
        XCTAssertEqual(
            try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(opened.httpBody)) as? [String: String]),
            ["kind": "opened", "view_id": "main"]
        )

        let pin = try AppsDirectoryClient.activityRequest(
            profile: profile,
            installationID: "install-1",
            activity: .pinView(viewID: "main", pinned: true)
        )
        let pinBody = try XCTUnwrap(
            JSONSerialization.jsonObject(with: try XCTUnwrap(pin.httpBody)) as? [String: Any]
        )
        XCTAssertEqual(pinBody["kind"] as? String, "pin")
        XCTAssertEqual(pinBody["target_kind"] as? String, "view")
        XCTAssertEqual(pinBody["target_id"] as? String, "main")
        XCTAssertEqual(pinBody["pinned"] as? Bool, true)
    }

    func testActivityReceiptsAreStrictAndInstallationBound() throws {
        let valid = try encode([
            "installation_id": "install-1",
            "updated_at": "2026-08-17T10:00:00Z"
        ])
        XCTAssertEqual(
            try AppsDirectoryContract.decodeActivityReceipt(valid, installationID: "install-1").installationID,
            "install-1"
        )
        XCTAssertThrowsError(
            try AppsDirectoryContract.decodeActivityReceipt(valid, installationID: "install-2")
        )
        XCTAssertThrowsError(try AppsDirectoryContract.decodeActivityReceipt(
            try encode([
                "installation_id": "install-1",
                "updated_at": "2026-08-17T10:00:00Z",
                "authority": "unexpected"
            ]),
            installationID: "install-1"
        ))
    }

    func testTodayProjectionIncludesOnlyExplicitlyPinnedViewsFromEnabledApps() throws {
        var payload = validPage()
        var enabled = try XCTUnwrap((payload["entries"] as? [[String: Any]])?.first)
        var unpinned = try XCTUnwrap((enabled["views"] as? [[String: Any]])?.first)
        unpinned["view_id"] = "secondary"
        unpinned["label"] = "Secondary"
        unpinned["route"] = "/apps/install-1/secondary"
        unpinned["pinned"] = false
        enabled["views"] = [unpinned] + (enabled["views"] as? [[String: Any]] ?? [])

        var disabled = enabled
        disabled["installation_id"] = "install-2"
        disabled["name"] = "Disabled app"
        disabled["status"] = "disabled"
        disabled["default_route"] = "/apps/install-2/main"
        disabled["views"] = [[
            "view_id": "main",
            "label": "Must not appear",
            "route": "/apps/install-2/main",
            "pinned": true
        ]]
        payload["entries"] = [enabled, disabled]
        payload["has_more"] = false
        payload.removeValue(forKey: "next_cursor")

        let page = try AppsDirectoryContract.decodePage(try encode(payload))
        let projected = PinnedAppsTodayProjection.make(entries: page.entries)

        XCTAssertEqual(projected.map(\.id), ["install-1:main"])
        XCTAssertEqual(projected.map(\.label), ["Itinerary"])
        XCTAssertEqual(projected.map(\.appName), ["Trip planner"])
    }

    func testUnpinningTheLastViewPreservesAnEntryWithAPinnedAction() throws {
        var payload = validPage()
        var entries = try XCTUnwrap(payload["entries"] as? [[String: Any]])
        entries[0]["actions"] = [["action_id": "refresh", "label": "Refresh", "pinned": true]]
        payload["entries"] = entries
        let entry = try XCTUnwrap(
            AppsDirectoryContract.decodePage(try encode(payload)).entries.first
        )

        let updated = entry.settingPin(viewID: "main", pinned: false)

        XCTAssertFalse(updated.views[0].pinned)
        XCTAssertTrue(updated.actions[0].pinned)
        XCTAssertTrue(updated.hasPinnedTarget)
    }

    func testTodayProjectionIsDeterministicBoundedAndHasNoDefaultViewFallback() throws {
        let template = try XCTUnwrap((validPage()["entries"] as? [[String: Any]])?.first)
        var entries: [[String: Any]] = []
        for index in 0..<12 {
            var entry = template
            let installationID = "install-\(index + 1)"
            entry["installation_id"] = installationID
            entry["name"] = "App \(index + 1)"
            entry["default_route"] = "/apps/\(installationID)/main"
            entry["views"] = [[
                "view_id": "main",
                "label": "View \(index + 1)",
                "route": "/apps/\(installationID)/main",
                "pinned": index != 0
            ]]
            entries.append(entry)
        }
        let page = try AppsDirectoryContract.decodePage(try encode([
            "entries": entries,
            "has_more": false
        ]))

        let projected = PinnedAppsTodayProjection.make(entries: page.entries, limit: 99)

        XCTAssertEqual(projected.count, PinnedAppsTodayProjection.maximumItems)
        XCTAssertEqual(projected.first?.id, "install-2:main")
        XCTAssertEqual(projected.last?.id, "install-9:main")

        let onlyDefault = try AppsDirectoryContract.decodePage(try encode([
            "entries": [entries[0]],
            "has_more": false
        ]))
        XCTAssertTrue(PinnedAppsTodayProjection.make(entries: onlyDefault.entries).isEmpty)
    }

    func testNativeSlotIdentityIsInjectivePageQualifiedAndRejectsDynamicPages() throws {
        XCTAssertEqual(
            try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "primary"),
            "page:2f:primary"
        )
        XCTAssertNotEqual(
            try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "primary"),
            try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/observe", region: "primary")
        )
        XCTAssertTrue(AppNativeSurfaceContract.isPageQualifiedSlotID("page:2f:primary"))
        XCTAssertFalse(AppNativeSurfaceContract.isPageQualifiedSlotID("primary"))
        XCTAssertFalse(AppNativeSurfaceContract.isPageQualifiedSlotID("page:2F:primary"))
        XCTAssertFalse(AppNativeSurfaceContract.isPageQualifiedSlotID("page:2f:primary:extra"))
        XCTAssertThrowsError(
            try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/entity/:id", region: "primary")
        )
        XCTAssertThrowsError(
            try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "not valid")
        )
    }

    func testNativeWidgetContractStrictlyDecodesEveryClosedModelFamily() throws {
        let row: [String: Any] = [
            "entity": "plan",
            "record_id": "plan-1",
            "record_revision": 3,
            "fields": ["title": "Ship native widgets", "parent": NSNull(), "at": "2026-09-02T10:00:00Z"]
        ]
        let hints: [String: Any] = [
            "display_field": "title", "parent_field": "parent", "timestamp_field": "at"
        ]
        let modelBodies: [[String: Any]] = [
            ["model": "detail", "row": row, "hints": hints, "actions": []],
            ["model": "list", "rows": [row], "hints": hints, "actions": []],
            ["model": "table", "columns": ["title", "at"], "rows": [row], "hints": hints, "actions": []],
            ["model": "timeline", "rows": [row], "hints": hints, "actions": []],
            ["model": "tree", "rows": [row], "hints": hints, "actions": []],
            ["model": "graph", "rows": [row], "hints": hints, "actions": [
                ["action_id": "refresh", "label": "Refresh"]
            ]]
        ]
        for model in modelBodies {
            let decoded = try AppNativeSurfaceContract.decodeWidgets(
                try encode(nativeWidgetResponse(model: model))
            )
            XCTAssertEqual(decoded.widgets.count, 1)
            XCTAssertEqual(decoded.widgets[0].installationGeneration, 4)
        }

        var invalid = nativeWidgetResponse(model: modelBodies[1])
        var widgets = try XCTUnwrap(invalid["widgets"] as? [[String: Any]])
        widgets[0]["authority"] = "must-not-enter-native-ui"
        invalid["widgets"] = widgets
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeWidgets(try encode(invalid)))

        var missingCadence = nativeWidgetResponse(model: modelBodies[0])
        widgets = try XCTUnwrap(missingCadence["widgets"] as? [[String: Any]])
        widgets[0].removeValue(forKey: "refresh_after")
        missingCadence["widgets"] = widgets
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeWidgets(try encode(missingCadence)))

        var mismatchedCadence = nativeWidgetResponse(model: modelBodies[0])
        mismatchedCadence["refresh_after"] = "2026-09-02T10:00:30Z"
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeWidgets(try encode(mismatchedCadence)))
    }

    func testNativeIndicatorContractBindsMaterializedIdentityAndRejectsUnknownFields() throws {
        let digest = "blake3:" + String(repeating: "a", count: 64)
        let payload: [String: Any] = [
            "schema_version": 1,
            "revision": digest,
            "etag": digest,
            "generated_at": "2026-09-02T10:00:00Z",
            "indicators": [[
                "installation_id": "install-1",
                "installation_generation": 4,
                "indicator_id": "due",
                "title": "Due",
                "revision": digest,
                "evaluated_at": "2026-09-02T10:00:00Z",
                "expires_at": "2099-09-02T10:00:00Z",
                "model": ["kind": "badge", "count": 3]
            ]]
        ]
        let decoded = try AppNativeSurfaceContract.decodeIndicators(try encode(payload))
        XCTAssertEqual(decoded.indicators.first?.model.text, "3")

        var invalid = payload
        var indicators = try XCTUnwrap(invalid["indicators"] as? [[String: Any]])
        indicators[0]["query"] = ["entity": "secret"]
        invalid["indicators"] = indicators
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeIndicators(try encode(invalid)))
    }

    func testNativeRequestsUseETagsAndGovernedActionsHaveExactEmptyInput() throws {
        let profile = try MobileConnectionProfile(
            publicOrigin: try XCTUnwrap(URL(string: "https://self-hosted.example:8443")),
            principal: "owner",
            workspace: "personal",
            deviceID: "iphone-1",
            deviceToken: "device-token"
        )
        let digest = "blake3:" + String(repeating: "a", count: 64)
        let widgets = try AppNativeSurfaceClient.widgetRequest(
            profile: profile,
            targets: [AppNativeWidgetTarget(installationID: "install-1", widgetID: "plans")],
            etag: digest
        )
        XCTAssertEqual(widgets.httpMethod, "POST")
        XCTAssertEqual(widgets.value(forHTTPHeaderField: "If-None-Match"), "\"\(digest)\"")
        XCTAssertEqual(widgets.url?.path, "/api/magician/v2/apps/widgets/render-batch")
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotRequest(profile: profile, slotID: "primary"))

        let action = try AppNativeSurfaceClient.actionRequest(
            profile: profile,
            installationID: "install-1",
            actionID: "refresh",
            idempotencyKey: "ios-widget:one",
            expectedInstallationBinding: AppNativeExpectedInstallationBinding(
                generation: 4,
                packageRevisionRef: "revision:install-1:4"
            )
        )
        XCTAssertEqual(action.url?.path, "/api/magician/v2/apps/installations/install-1/actions/refresh/runs")
        let body = try XCTUnwrap(
            JSONSerialization.jsonObject(with: try XCTUnwrap(action.httpBody)) as? [String: Any]
        )
        XCTAssertEqual(body["idempotency_key"] as? String, "ios-widget:one")
        XCTAssertEqual((body["input"] as? [String: Any])?.count, 0)
        XCTAssertEqual(Set(body.keys), ["idempotency_key", "input", "expected_installation_binding"])
        let binding = try XCTUnwrap(body["expected_installation_binding"] as? [String: Any])
        XCTAssertEqual(binding["generation"] as? Int, 4)
        XCTAssertEqual(binding["package_revision_ref"] as? String, "revision:install-1:4")
        XCTAssertThrowsError(try AppNativeSurfaceClient.actionRequest(
            profile: profile,
            installationID: "install-1",
            actionID: "refresh",
            idempotencyKey: "ios-widget:one",
            expectedInstallationBinding: AppNativeExpectedInstallationBinding(
                generation: 0,
                packageRevisionRef: "revision:install-1:4"
            )
        ))
    }

    func testNativeWidgetActionIdempotencyIsRetryStableUntilProvenSuccess() {
        var ledger = AppNativeActionIdempotencyLedger()
        let authority = "install-1\u{0}4\u{0}revision\u{0}refresh"
        let first = ledger.key(for: authority) { "ios-widget:first" }
        let retry = ledger.key(for: authority) { "ios-widget:must-not-replace" }
        XCTAssertEqual(first, retry)

        ledger.markSucceeded(authority: authority)
        XCTAssertEqual(ledger.key(for: authority) { "ios-widget:next" }, "ios-widget:next")
    }

    func testNativeWidgetNotModifiedRequiresAndReturnsTheNextRefreshDeadline() throws {
        let digest = "blake3:" + String(repeating: "a", count: 64)
        let url = try XCTUnwrap(URL(string: "https://self-hosted.example/widgets"))
        let response = try XCTUnwrap(HTTPURLResponse(
            url: url,
            statusCode: 304,
            httpVersion: "HTTP/1.1",
            headerFields: [
                "ETag": "\"\(digest)\"",
                "X-App-Widget-Refresh-After": "2026-09-02T10:01:00Z"
            ]
        ))
        let outcome: AppNativeHTTPResult<AppNativeWidgetBatchResponse> = try AppNativeSurfaceClient
            .decodeConditional(
                data: Data(),
                response: response,
                requestedETag: digest,
                requireWidgetRefreshAfter: true,
                decode: AppNativeSurfaceContract.decodeWidgets
            )
        guard case .notModified(let etag, let refreshAfter) = outcome else {
            return XCTFail("Expected a validated not-modified representation")
        }
        XCTAssertEqual(etag, digest)
        XCTAssertEqual(refreshAfter, "2026-09-02T10:01:00Z")

        let missingCadence = try XCTUnwrap(HTTPURLResponse(
            url: url,
            statusCode: 304,
            httpVersion: "HTTP/1.1",
            headerFields: ["ETag": "\"\(digest)\""]
        ))
        XCTAssertThrowsError(try AppNativeSurfaceClient.decodeConditional(
            data: Data(),
            response: missingCadence,
            requestedETag: digest,
            requireWidgetRefreshAfter: true,
            decode: AppNativeSurfaceContract.decodeWidgets
        ) as AppNativeHTTPResult<AppNativeWidgetBatchResponse>)
    }

    private func nativeWidgetResponse(model: [String: Any]) -> [String: Any] {
        let digest = "blake3:" + String(repeating: "a", count: 64)
        return [
            "schema_version": 1,
            "revision": digest,
            "etag": digest,
            "rendered_at": "2026-09-02T10:00:00Z",
            "refresh_after": "2026-09-02T10:01:00Z",
            "widgets": [[
                "installation_id": "install-1",
                "widget_id": "plans",
                "title": "Plans",
                "installation_generation": 4,
                "revision": digest,
                "rendered_at": "2026-09-02T10:00:00Z",
                "refresh_after": "2026-09-02T10:01:00Z",
                "state": "ready",
                "model": model
            ]]
        ]
    }


    // MARK: - Page-shared slot batch, picker and contextual fitting

    func testPageSlotBatchIsBoundToItsOwnRequestOrderAndRegions() throws {
        let primary = try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "primary")
        let secondary = try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "secondary")

        let decoded = try AppNativeSurfaceContract.decodeSlotBatch(
            try encode(["assignments": [assignedSlot(slotID: primary), emptySlot(slotID: secondary)]]),
            slotIDs: [primary, secondary]
        )
        XCTAssertEqual(decoded.map(\.slotID), [primary, secondary])
        XCTAssertNotNil(decoded[0].widget)
        XCTAssertNil(decoded[1].widget)

        // A reordered answer would render one region's widget in another.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotBatch(
            try encode(["assignments": [assignedSlot(slotID: secondary), emptySlot(slotID: primary)]]),
            slotIDs: [primary, secondary]
        ))
        // A short answer is not a partial page; it is a different page.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotBatch(
            try encode(["assignments": [assignedSlot(slotID: primary)]]),
            slotIDs: [primary, secondary]
        ))
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotBatch(
            try encode(["assignments": [assignedSlot(slotID: primary), assignedSlot(slotID: primary)]]),
            slotIDs: [primary, primary]
        ))
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotBatch(
            try encode(["assignments": [[String: Any]](), "next": "x"] as [String: Any]),
            slotIDs: []
        ))
    }

    func testSlotSettingsPageBindsItsPickerToInjectiveSuggestionsAndCursorTruth() throws {
        let page = try AppNativeSurfaceContract.decodeSlotSettings(try encode(settingsPage()))
        XCTAssertEqual(page.head.revision, 7)
        XCTAssertEqual(page.head.fence, 3)
        XCTAssertEqual(page.picker.count, 1)
        XCTAssertEqual(page.picker[0].targetKey, "install-1\u{0}plans")
        XCTAssertFalse(page.assignmentsTruncated)

        // A fence of zero can never come from a settings read, so it can never
        // be written against.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotSettings(
            try encode(settingsPage(overrides: ["head": ["revision": 7, "fence": 0]]))
        ))
        // A suggestion must re-derive the slot id it names.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotSettings(
            try encode(settingsPage(overrides: ["picker": [pickerCandidate(overrides: [
                "suggested_slots": [[
                    "page": "/observe",
                    "region": "primary",
                    "slot_id": "page:2f:primary",
                    "system_default": false
                ]]
            ])]]))
        ))
        // System-default provenance is host-derived; an untrusted widget cannot
        // claim a default slot.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotSettings(
            try encode(settingsPage(overrides: ["picker": [pickerCandidate(overrides: [
                "suggested_slots": [[
                    "page": "/",
                    "region": "primary",
                    "slot_id": "page:2f:primary",
                    "system_default": true
                ]],
                "system_class": false
            ])]]))
        ))
        // A truncation flag and its cursor are one fact.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotSettings(
            try encode(settingsPage(overrides: ["picker_truncated": true]))
        ))
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotSettings(
            try encode(settingsPage(overrides: ["next_picker_cursor": "slot:2"]))
        ))
        // Two picker rows for one installation/widget are one row twice.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotSettings(
            try encode(settingsPage(overrides: ["picker": [pickerCandidate(), pickerCandidate()]]))
        ))
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotSettings(
            try encode(settingsPage(overrides: ["authority": "must-not-enter-native-ui"]))
        ))
    }

    func testSlotMutationReceiptMustBeTheReceiptForThisExactWrite() throws {
        let slotID = try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "primary")
        let binding = try JSONDecoder().decode(
            AppNativeWidgetBinding.self,
            from: try encode(slotWidgetBinding())
        )
        let request = AppNativeSlotAssignmentWriteRequest(
            expectedRevision: 7,
            writeFence: 3,
            mutationID: "slot-mutation:abc",
            command: .assign(
                slotID: slotID,
                installationID: "install-1",
                widgetID: "plans",
                expectedCandidate: AppNativeSlotWidgetBindingWire(binding)
            )
        )
        XCTAssertTrue(request.isValid)

        let receipt = try AppNativeSurfaceContract.decodeSlotMutationReceipt(
            try encode(mutationReceipt()),
            request: request
        )
        XCTAssertEqual(receipt.head.revision, 8)

        // Another editor's answer wearing this write's shape.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotMutationReceipt(
            try encode(mutationReceipt(overrides: ["mutation_id": "slot-mutation:other"])),
            request: request
        ))
        // The revision this write was expected to produce, exactly.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotMutationReceipt(
            try encode(mutationReceipt(overrides: ["head": ["revision": 9, "fence": 3]])),
            request: request
        ))
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotMutationReceipt(
            try encode(mutationReceipt(overrides: ["head": ["revision": 8, "fence": 4]])),
            request: request
        ))
        // A receipt that parses proves the write landed somewhere; the applied
        // state has to be the change that was asked for.
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotMutationReceipt(
            try encode(mutationReceipt(overrides: [
                "assignment": assignedSlot(slotID: slotID, generation: 5)
            ])),
            request: request
        ))

        let optOut = AppNativeSlotAssignmentWriteRequest(
            expectedRevision: 7,
            writeFence: 3,
            mutationID: "slot-mutation:abc",
            command: .optOut(slotID: slotID)
        )
        XCTAssertThrowsError(try AppNativeSurfaceContract.decodeSlotMutationReceipt(
            try encode(mutationReceipt()),
            request: optOut
        ))
        XCTAssertNoThrow(try AppNativeSurfaceContract.decodeSlotMutationReceipt(
            try encode(mutationReceipt(overrides: [
                "assignment": [
                    "slot_id": slotID,
                    "pinned_system_default": false,
                    "opted_out": true
                ] as [String: Any]
            ])),
            request: optOut
        ))
    }

    func testSlotBatchSettingsAndMutationRequestsAreBoundedAndCanonical() throws {
        let profile = try MobileConnectionProfile(
            publicOrigin: try XCTUnwrap(URL(string: "https://self-hosted.example:8443")),
            principal: "owner",
            workspace: "personal",
            deviceID: "iphone-1",
            deviceToken: "device-token"
        )
        let primary = try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "primary")
        let secondary = try AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "secondary")

        let batch = try AppNativeSurfaceClient.slotBatchRequest(
            profile: profile,
            slotIDs: [primary, secondary]
        )
        XCTAssertEqual(batch.httpMethod, "POST")
        XCTAssertEqual(batch.url?.path, "/api/magician/v2/apps/slots/resolve-batch")
        let batchBody = try XCTUnwrap(
            JSONSerialization.jsonObject(with: try XCTUnwrap(batch.httpBody)) as? [String: Any]
        )
        XCTAssertEqual(batchBody["slot_ids"] as? [String], [primary, secondary])
        XCTAssertEqual(Set(batchBody.keys), ["slot_ids"])
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotBatchRequest(
            profile: profile,
            slotIDs: [primary, primary]
        ))
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotBatchRequest(
            profile: profile,
            slotIDs: ["primary"]
        ))
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotBatchRequest(profile: profile, slotIDs: []))
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotBatchRequest(
            profile: profile,
            slotIDs: (0...AppNativeSurfaceContract.maximumPageRegions).map {
                (try? AppNativeSurfaceContract.pageQualifiedSlotID(page: "/", region: "r\($0)")) ?? ""
            }
        ))

        let settings = try AppNativeSurfaceClient.slotSettingsRequest(
            profile: profile,
            query: AppNativeSlotSettingsQuery(
                assignmentLimit: 128,
                assignmentCursor: "slot:a",
                pickerLimit: 32,
                pickerCursor: nil
            )
        )
        XCTAssertEqual(settings.url?.path, "/api/magician/v2/apps/slot-assignments")
        let components = try XCTUnwrap(
            URLComponents(url: try XCTUnwrap(settings.url), resolvingAgainstBaseURL: false)
        )
        let items = try XCTUnwrap(components.queryItems)
        XCTAssertEqual(
            Dictionary(uniqueKeysWithValues: items.map { ($0.name, $0.value ?? "") }),
            ["assignment_limit": "128", "picker_limit": "32", "assignment_cursor": "slot:a"]
        )
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotSettingsRequest(
            profile: profile,
            query: AppNativeSlotSettingsQuery(
                assignmentLimit: 129,
                assignmentCursor: nil,
                pickerLimit: 32,
                pickerCursor: nil
            )
        ))
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotSettingsRequest(
            profile: profile,
            query: AppNativeSlotSettingsQuery(
                assignmentLimit: 32,
                assignmentCursor: nil,
                pickerLimit: 0,
                pickerCursor: nil
            )
        ))

        let binding = try JSONDecoder().decode(
            AppNativeWidgetBinding.self,
            from: try encode(slotWidgetBinding())
        )
        let mutation = try AppNativeSurfaceClient.slotMutationRequest(
            profile: profile,
            writeRequest: AppNativeSlotAssignmentWriteRequest(
                expectedRevision: 7,
                writeFence: 3,
                mutationID: "slot-mutation:abc",
                command: .assign(
                    slotID: primary,
                    installationID: "install-1",
                    widgetID: "plans",
                    expectedCandidate: AppNativeSlotWidgetBindingWire(binding)
                )
            )
        )
        let mutationBody = try XCTUnwrap(
            JSONSerialization.jsonObject(with: try XCTUnwrap(mutation.httpBody)) as? [String: Any]
        )
        XCTAssertEqual(
            Set(mutationBody.keys),
            ["expected_revision", "write_fence", "mutation_id", "command"]
        )
        let command = try XCTUnwrap(mutationBody["command"] as? [String: Any])
        XCTAssertEqual(command["command"] as? String, "assign")
        XCTAssertEqual(command["slot_id"] as? String, primary)
        // The write echoes the whole picker row it was composed from, which is
        // what closes the update/reinstall race between the read and the write.
        let candidate = try XCTUnwrap(command["expected_candidate"] as? [String: Any])
        let candidatePackage = try XCTUnwrap(candidate["package"] as? [String: Any])
        XCTAssertEqual(candidate["widget_id"] as? String, "plans")
        XCTAssertEqual(candidatePackage["installation_generation"] as? Int, 4)
        XCTAssertEqual(candidatePackage["package_content_digest"] as? String, packageDigest)

        // A fence a settings read never produced, and a candidate that does not
        // identify the command's own installation/widget, are both refused
        // before anything leaves the device.
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotMutationRequest(
            profile: profile,
            writeRequest: AppNativeSlotAssignmentWriteRequest(
                expectedRevision: 7,
                writeFence: 0,
                mutationID: "slot-mutation:abc",
                command: .optOut(slotID: primary)
            )
        ))
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotMutationRequest(
            profile: profile,
            writeRequest: AppNativeSlotAssignmentWriteRequest(
                expectedRevision: 7,
                writeFence: 3,
                mutationID: "slot-mutation:abc",
                command: .assign(
                    slotID: primary,
                    installationID: "install-2",
                    widgetID: "plans",
                    expectedCandidate: AppNativeSlotWidgetBindingWire(binding)
                )
            )
        ))
        XCTAssertThrowsError(try AppNativeSurfaceClient.slotMutationRequest(
            profile: profile,
            writeRequest: AppNativeSlotAssignmentWriteRequest(
                expectedRevision: 7,
                writeFence: 3,
                mutationID: "not-a-slot-mutation",
                command: .optOut(slotID: primary)
            )
        ))
    }

    func testContextualSlotPagesAreInjectivePerEntityRouteAndNeverFallBackToTheRoot() throws {
        let root = try XCTUnwrap(AppNativeSurfaceContract.appSurfaceSlotPage(
            installationID: "install-1",
            surfacePath: ""
        ))
        let entity = try XCTUnwrap(AppNativeSurfaceContract.appSurfaceSlotPage(
            installationID: "install-1",
            surfacePath: "plans/plan-1"
        ))
        XCTAssertEqual(root, "/apps/install-1")
        XCTAssertEqual(entity, "/apps/install-1/plans/plan-1")
        XCTAssertNotEqual(
            try AppNativeSurfaceContract.pageQualifiedSlotID(page: root, region: "contextual"),
            try AppNativeSurfaceContract.pageQualifiedSlotID(page: entity, region: "contextual")
        )
        // A tail that cannot form a canonical static page has NO contextual
        // slot: falling back to the installation root would make two different
        // entity pages share one assignment.
        XCTAssertNil(AppNativeSurfaceContract.appSurfaceSlotPage(
            installationID: "install-1",
            surfacePath: "plans/:id"
        ))
        XCTAssertNil(AppNativeSurfaceContract.appSurfaceSlotPage(
            installationID: "install-1",
            surfacePath: "plans/../../etc"
        ))
        XCTAssertNil(AppNativeSurfaceContract.appSurfaceSlotPage(
            installationID: "install 1",
            surfacePath: ""
        ))
    }

    private var packageDigest: String { "blake3:" + String(repeating: "b", count: 64) }

    private func slotPackage(generation: Int = 4) -> [String: Any] {
        [
            "installation_id": "install-1",
            "package_id": "app:planner",
            "package_revision_ref": "app-package-revision:one",
            "package_content_digest": packageDigest,
            "installation_generation": generation
        ]
    }

    private func slotWidgetBinding(generation: Int = 4) -> [String: Any] {
        ["package": slotPackage(generation: generation), "widget_id": "plans"]
    }

    private func assignedSlot(slotID: String, generation: Int = 4) -> [String: Any] {
        [
            "slot_id": slotID,
            "source": "user",
            "pinned_system_default": false,
            "opted_out": false,
            "widget": [
                "pinned": slotWidgetBinding(generation: generation),
                "current": slotWidgetBinding(generation: generation),
                "restored_across_generation": false,
                "assignment_compatibility": "exact_digest_only"
            ]
        ]
    }

    private func emptySlot(slotID: String) -> [String: Any] {
        ["slot_id": slotID, "pinned_system_default": false, "opted_out": false]
    }

    private func pickerCandidate(overrides: [String: Any] = [:]) -> [String: Any] {
        var candidate: [String: Any] = [
            "widget": slotWidgetBinding(),
            "title": "Plans",
            "suggested_slots": [[
                "page": "/",
                "region": "primary",
                "slot_id": "page:2f:primary",
                "system_default": false
            ]],
            "system_class": false
        ]
        candidate.merge(overrides) { _, new in new }
        return candidate
    }

    private func settingsPage(overrides: [String: Any] = [:]) -> [String: Any] {
        var page: [String: Any] = [
            "head": ["revision": 7, "fence": 3],
            "inventory_revision": "blake3:" + String(repeating: "c", count: 64),
            "assignments": [assignedSlot(slotID: "page:2f:primary")],
            "assignments_truncated": false,
            "picker": [pickerCandidate()],
            "picker_truncated": false
        ]
        page.merge(overrides) { _, new in new }
        return page
    }

    private func mutationReceipt(overrides: [String: Any] = [:]) -> [String: Any] {
        var receipt: [String: Any] = [
            "mutation_id": "slot-mutation:abc",
            "head": ["revision": 8, "fence": 3],
            "assignment": assignedSlot(slotID: "page:2f:primary")
        ]
        receipt.merge(overrides) { _, new in new }
        return receipt
    }

    private func encodedPage() throws -> Data { try encode(validPage()) }

    private func validPage() -> [String: Any] {
        [
            "entries": [[
                "installation_id": "install-1",
                "name": "Trip planner",
                "description": "Keeps a private itinerary.",
                "icon": ["kind": "monogram", "value": "TS"],
                "package_version": "1.0.0",
                "package_revision_ref": "app-package-revision:one",
                "installation_generation": 1,
                "status": "enabled",
                "default_route": "/apps/install-1/main",
                "views": [[
                    "view_id": "main",
                    "label": "Itinerary",
                    "route": "/apps/install-1/main",
                    "pinned": true
                ]],
                "actions": [["action_id": "refresh", "label": "Refresh", "pinned": false]],
                "last_opened_at": "2026-08-17T10:00:00Z",
                "attention_reason": NSNull(),
                "permissions": [
                    "granted_tools": 2,
                    "granted_context_reads": 1,
                    "granted_personal_data_projections": 0,
                    "background_execution": false,
                    "network_access": true
                ],
                "storage": [
                    "record_count": 3,
                    "revision_count": 4,
                    "payload_bytes": 512,
                    "attachment_bytes": 128
                ],
                "record_count": 3,
                "payload_bytes": 512
            ]],
            "next_cursor": "appdir:2:install-1",
            "has_more": true
        ]
    }

    private func encode(_ value: Any) throws -> Data {
        try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    }
}
