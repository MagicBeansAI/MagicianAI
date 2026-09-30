import Foundation
import SwiftUI
import WebKit

private extension Notification.Name {
    static let magicianAppsDirectoryPinsDidChange = Notification.Name(
        "ai.magicbeans.magican.appsDirectoryPinsDidChange"
    )
}

enum AppsDirectorySection: String, CaseIterable, Identifiable {
    case installed
    case pinned
    case recent
    case needsAttention = "needs_attention"
    case disabled
    case recovery

    var id: String { rawValue }

    var label: String {
        switch self {
        case .installed: return "Installed"
        case .pinned: return "Pinned"
        case .recent: return "Recent"
        case .needsAttention: return "Needs attention"
        case .disabled: return "Disabled"
        case .recovery: return "Recovery"
        }
    }
}

enum AppsDirectoryPinnedTargetKind: String {
    case view
    case action
}

enum AppsInstallationStatus: String, Decodable, Equatable {
    case readyForReview = "ready_for_review"
    case enabled
    case disabled
    case updatePending = "update_pending"
    case quarantined
    case uninstalledRetained = "uninstalled_retained"
    case purged

    var label: String { rawValue.replacingOccurrences(of: "_", with: " ") }
    var canLaunch: Bool { self == .enabled }
}

struct AppsDirectoryIcon: Decodable, Equatable {
    let kind: String
    let value: String

    enum CodingKeys: String, CodingKey, CaseIterable { case kind, value }

    init(kind: String, value: String) {
        self.kind = kind
        self.value = value
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decode(String.self, forKey: .kind)
        value = try container.decode(String.self, forKey: .value)
        guard kind == "monogram",
              AppsDirectoryContract.isSafeText(value, maximum: 8),
              !value.isEmpty else {
            throw AppsDirectoryContractError.invalidField("icon")
        }
    }
}

struct AppsDirectoryView: Decodable, Identifiable, Equatable {
    let viewID: String
    let label: String
    let route: String
    let pinned: Bool

    var id: String { viewID }

    enum CodingKeys: String, CodingKey, CaseIterable {
        case viewID = "view_id"
        case label
        case route
        case pinned
    }

    init(viewID: String, label: String, route: String, pinned: Bool) {
        self.viewID = viewID
        self.label = label
        self.route = route
        self.pinned = pinned
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        viewID = try container.decode(String.self, forKey: .viewID)
        label = try container.decode(String.self, forKey: .label)
        route = try container.decode(String.self, forKey: .route)
        pinned = try container.decode(Bool.self, forKey: .pinned)
        guard AppsDirectoryContract.isName(viewID),
              AppsDirectoryContract.isSafeText(label, maximum: 160),
              !label.isEmpty,
              AppRoutePolicy.isCanonicalRoute(route) else {
            throw AppsDirectoryContractError.invalidField("view")
        }
    }
}

struct AppsDirectoryAction: Decodable, Identifiable, Equatable {
    let actionID: String
    let label: String
    let pinned: Bool

    var id: String { actionID }

    enum CodingKeys: String, CodingKey, CaseIterable {
        case actionID = "action_id"
        case label
        case pinned
    }

    init(actionID: String, label: String, pinned: Bool) {
        self.actionID = actionID
        self.label = label
        self.pinned = pinned
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        actionID = try container.decode(String.self, forKey: .actionID)
        label = try container.decode(String.self, forKey: .label)
        pinned = try container.decode(Bool.self, forKey: .pinned)
        guard AppsDirectoryContract.isName(actionID),
              AppsDirectoryContract.isSafeText(label, maximum: 160),
              !label.isEmpty else {
            throw AppsDirectoryContractError.invalidField("action")
        }
    }
}

struct AppsDirectoryPermissionSummary: Decodable, Equatable {
    let grantedTools: UInt64
    let grantedContextReads: UInt64
    let grantedPersonalDataProjections: UInt64
    let backgroundExecution: Bool
    let networkAccess: Bool

    enum CodingKeys: String, CodingKey, CaseIterable {
        case grantedTools = "granted_tools"
        case grantedContextReads = "granted_context_reads"
        case grantedPersonalDataProjections = "granted_personal_data_projections"
        case backgroundExecution = "background_execution"
        case networkAccess = "network_access"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        grantedTools = try container.decode(UInt64.self, forKey: .grantedTools)
        grantedContextReads = try container.decode(UInt64.self, forKey: .grantedContextReads)
        grantedPersonalDataProjections = try container.decode(UInt64.self, forKey: .grantedPersonalDataProjections)
        backgroundExecution = try container.decode(Bool.self, forKey: .backgroundExecution)
        networkAccess = try container.decode(Bool.self, forKey: .networkAccess)
    }
}

struct AppsDirectoryStorageSummary: Decodable, Equatable {
    let recordCount: UInt64
    let revisionCount: UInt64
    let payloadBytes: UInt64
    let attachmentBytes: UInt64

    enum CodingKeys: String, CodingKey, CaseIterable {
        case recordCount = "record_count"
        case revisionCount = "revision_count"
        case payloadBytes = "payload_bytes"
        case attachmentBytes = "attachment_bytes"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        recordCount = try container.decode(UInt64.self, forKey: .recordCount)
        revisionCount = try container.decode(UInt64.self, forKey: .revisionCount)
        payloadBytes = try container.decode(UInt64.self, forKey: .payloadBytes)
        attachmentBytes = try container.decode(UInt64.self, forKey: .attachmentBytes)
    }
}

struct AppsDirectoryEntry: Decodable, Identifiable, Equatable {
    let installationID: String
    let name: String
    let description: String
    let icon: AppsDirectoryIcon
    let packageVersion: String
    let packageRevisionRef: String
    let installationGeneration: UInt64
    let status: AppsInstallationStatus
    let defaultRoute: String?
    let views: [AppsDirectoryView]
    let actions: [AppsDirectoryAction]
    let customSurfaceEntryCount: Int
    let lastOpenedAt: String?
    let attentionReason: String?
    let permissions: AppsDirectoryPermissionSummary?
    let storage: AppsDirectoryStorageSummary
    let recordCount: UInt64
    let payloadBytes: UInt64

    var id: String { installationID }

    enum CodingKeys: String, CodingKey, CaseIterable {
        case installationID = "installation_id"
        case name
        case description
        case icon
        case packageVersion = "package_version"
        case packageRevisionRef = "package_revision_ref"
        case installationGeneration = "installation_generation"
        case status
        case defaultRoute = "default_route"
        case views
        case actions
        case customSurfaceEntryCount = "custom_surface_entry_count"
        // Accepted, never decoded. First-party navigation an installed
        // system package declares is web-shell chrome; the phone has no
        // first-party chrome to mount it into, and inventing one from a
        // manifest declaration is a product decision, not a decode. Naming
        // the key here is what keeps the strict unknown-key check from
        // rejecting the whole page once a package declares one.
        case navigation
        case lastOpenedAt = "last_opened_at"
        case attentionReason = "attention_reason"
        case permissions
        case storage
        case recordCount = "record_count"
        case payloadBytes = "payload_bytes"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        installationID = try container.decode(String.self, forKey: .installationID)
        name = try container.decode(String.self, forKey: .name)
        description = try container.decode(String.self, forKey: .description)
        icon = try container.decode(AppsDirectoryIcon.self, forKey: .icon)
        packageVersion = try container.decode(String.self, forKey: .packageVersion)
        packageRevisionRef = try container.decode(String.self, forKey: .packageRevisionRef)
        installationGeneration = try container.decode(UInt64.self, forKey: .installationGeneration)
        status = try container.decode(AppsInstallationStatus.self, forKey: .status)
        defaultRoute = try container.decodeIfPresent(String.self, forKey: .defaultRoute)
        views = try container.decode([AppsDirectoryView].self, forKey: .views)
        actions = try container.decode([AppsDirectoryAction].self, forKey: .actions)
        // Absent on older servers (additive wire): nothing hostable.
        customSurfaceEntryCount = try container.decodeIfPresent(Int.self, forKey: .customSurfaceEntryCount) ?? 0
        lastOpenedAt = try container.decodeIfPresent(String.self, forKey: .lastOpenedAt)
        attentionReason = try container.decodeIfPresent(String.self, forKey: .attentionReason)
        permissions = try container.decodeIfPresent(AppsDirectoryPermissionSummary.self, forKey: .permissions)
        storage = try container.decode(AppsDirectoryStorageSummary.self, forKey: .storage)
        recordCount = try container.decode(UInt64.self, forKey: .recordCount)
        payloadBytes = try container.decode(UInt64.self, forKey: .payloadBytes)

        guard AppsDirectoryContract.isOpaqueID(installationID),
              AppsDirectoryContract.isSafeText(name, maximum: 160), !name.isEmpty,
              AppsDirectoryContract.isSafeText(description, maximum: 2_048),
              AppsDirectoryContract.isSafeText(packageVersion, maximum: 128), !packageVersion.isEmpty,
              AppsDirectoryContract.isReference(packageRevisionRef),
              installationGeneration > 0,
              views.count <= AppsDirectoryContract.maximumViews,
              actions.count <= AppsDirectoryContract.maximumActions,
              customSurfaceEntryCount <= AppsDirectoryContract.maximumCustomSurfaceEntryPoints,
              AppsDirectoryContract.hasUniqueValues(views.map(\.viewID)),
              AppsDirectoryContract.hasUniqueValues(views.map(\.route)),
              AppsDirectoryContract.hasUniqueValues(actions.map(\.actionID)),
              views.allSatisfy({ AppRoutePolicy.routeBelongsToInstallation($0.route, installationID: installationID) }),
              defaultRoute.map({ route in views.contains(where: { $0.route == route }) }) ?? true,
              lastOpenedAt.map({ AppsDirectoryContract.isSafeText($0, maximum: 64) }) ?? true,
              attentionReason.map({ AppsDirectoryContract.isSafeText($0, maximum: 1_024) }) ?? true,
              recordCount == storage.recordCount,
              payloadBytes == storage.payloadBytes else {
            throw AppsDirectoryContractError.invalidField("entry")
        }
    }

    func settingPin(viewID: String, pinned: Bool) -> AppsDirectoryEntry {
        AppsDirectoryEntry(
            installationID: installationID,
            name: name,
            description: description,
            icon: icon,
            packageVersion: packageVersion,
            packageRevisionRef: packageRevisionRef,
            installationGeneration: installationGeneration,
            status: status,
            defaultRoute: defaultRoute,
            views: views.map {
                AppsDirectoryView(viewID: $0.viewID, label: $0.label, route: $0.route,
                                  pinned: $0.viewID == viewID ? pinned : $0.pinned)
            },
            actions: actions,
            customSurfaceEntryCount: customSurfaceEntryCount,
            lastOpenedAt: lastOpenedAt,
            attentionReason: attentionReason,
            permissions: permissions,
            storage: storage,
            recordCount: recordCount,
            payloadBytes: payloadBytes
        )
    }

    var hasPinnedTarget: Bool {
        views.contains(where: \.pinned) || actions.contains(where: \.pinned)
    }

    private init(
        installationID: String,
        name: String,
        description: String,
        icon: AppsDirectoryIcon,
        packageVersion: String,
        packageRevisionRef: String,
        installationGeneration: UInt64,
        status: AppsInstallationStatus,
        defaultRoute: String?,
        views: [AppsDirectoryView],
        actions: [AppsDirectoryAction],
        customSurfaceEntryCount: Int,
        lastOpenedAt: String?,
        attentionReason: String?,
        permissions: AppsDirectoryPermissionSummary?,
        storage: AppsDirectoryStorageSummary,
        recordCount: UInt64,
        payloadBytes: UInt64
    ) {
        self.installationID = installationID
        self.name = name
        self.description = description
        self.icon = icon
        self.packageVersion = packageVersion
        self.packageRevisionRef = packageRevisionRef
        self.installationGeneration = installationGeneration
        self.status = status
        self.defaultRoute = defaultRoute
        self.views = views
        self.actions = actions
        self.customSurfaceEntryCount = customSurfaceEntryCount
        self.lastOpenedAt = lastOpenedAt
        self.attentionReason = attentionReason
        self.permissions = permissions
        self.storage = storage
        self.recordCount = recordCount
        self.payloadBytes = payloadBytes
    }
}

struct AppsDirectoryPage: Decodable, Equatable {
    let entries: [AppsDirectoryEntry]
    let nextCursor: String?
    let hasMore: Bool

    enum CodingKeys: String, CodingKey, CaseIterable {
        case entries
        case nextCursor = "next_cursor"
        case hasMore = "has_more"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        entries = try container.decode([AppsDirectoryEntry].self, forKey: .entries)
        nextCursor = try container.decodeIfPresent(String.self, forKey: .nextCursor)
        hasMore = try container.decode(Bool.self, forKey: .hasMore)
        guard entries.count <= AppsDirectoryContract.maximumEntries,
              AppsDirectoryContract.hasUniqueValues(entries.map(\.installationID)),
              nextCursor.map(AppsDirectoryContract.isCursor) ?? true,
              hasMore == (nextCursor != nil) else {
            throw AppsDirectoryContractError.invalidField("page")
        }
    }
}

struct AppsDirectoryActivityReceipt: Decodable, Equatable {
    let installationID: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey, CaseIterable {
        case installationID = "installation_id"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        installationID = try container.decode(String.self, forKey: .installationID)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
        guard AppsDirectoryContract.isOpaqueID(installationID),
              AppsDirectoryContract.isSafeText(updatedAt, maximum: 64),
              !updatedAt.isEmpty else {
            throw AppsDirectoryContractError.invalidField("activity receipt")
        }
    }
}

enum AppsDirectoryContractError: LocalizedError, Equatable {
    case oversizedResponse
    case excessiveNesting
    case invalidField(String)

    var errorDescription: String? {
        "The Apps directory returned an invalid response."
    }
}

enum AppsDirectoryContract {
    static let maximumResponseBytes = 4 * 1_048_576
    static let maximumEntries = 100
    static let maximumViews = 128
    static let maximumActions = 256
    static let maximumCustomSurfaceEntryPoints = 8
    static let maximumJSONDepth = 32

    static func decodePage(_ data: Data) throws -> AppsDirectoryPage {
        try preflight(data)
        return try JSONDecoder().decode(AppsDirectoryPage.self, from: data)
    }

    static func decodeActivityReceipt(_ data: Data, installationID: String) throws -> AppsDirectoryActivityReceipt {
        try preflight(data)
        let receipt = try JSONDecoder().decode(AppsDirectoryActivityReceipt.self, from: data)
        guard receipt.installationID == installationID else {
            throw AppsDirectoryContractError.invalidField("activity receipt identity")
        }
        return receipt
    }

    static func isSafeText(_ value: String, maximum: Int) -> Bool {
        value.utf8.count <= maximum && !value.unicodeScalars.contains(where: {
            CharacterSet.controlCharacters.contains($0)
        })
    }

    static func isOpaqueID(_ value: String) -> Bool {
        isASCIIToken(value, maximum: 128, additional: "_-.")
    }

    static func isName(_ value: String) -> Bool {
        isASCIIToken(value, maximum: 64, additional: "_-")
    }

    static func isReference(_ value: String) -> Bool {
        isASCIIToken(value, maximum: 192, additional: "_-.:/@#")
    }

    static func isCursor(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 512 && isSafeText(value, maximum: 512)
    }

    static func hasUniqueValues(_ values: [String]) -> Bool {
        Set(values).count == values.count
    }

    private static func isASCIIToken(_ value: String, maximum: Int, additional: String) -> Bool {
        guard !value.isEmpty, value.utf8.count <= maximum,
              let first = value.utf8.first, isASCIIAlphaNumeric(first) else { return false }
        let allowed = Set(additional.utf8)
        return value.utf8.dropFirst().allSatisfy { isASCIIAlphaNumeric($0) || allowed.contains($0) }
    }

    private static func isASCIIAlphaNumeric(_ byte: UInt8) -> Bool {
        (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122)
    }

    private static func preflight(_ data: Data) throws {
        guard !data.isEmpty, data.count <= maximumResponseBytes else {
            throw AppsDirectoryContractError.oversizedResponse
        }
        try validateJSONDepth(data)
    }

    private static func validateJSONDepth(_ data: Data) throws {
        var depth = 0
        var inString = false
        var escaped = false
        for byte in data {
            if inString {
                if escaped { escaped = false }
                else if byte == 0x5C { escaped = true }
                else if byte == 0x22 { inString = false }
                continue
            }
            if byte == 0x22 {
                inString = true
            } else if byte == 0x7B || byte == 0x5B {
                depth += 1
                if depth > maximumJSONDepth { throw AppsDirectoryContractError.excessiveNesting }
            } else if byte == 0x7D || byte == 0x5D {
                depth -= 1
                if depth < 0 { throw AppsDirectoryContractError.excessiveNesting }
            }
        }
        guard depth == 0, !inString else { throw AppsDirectoryContractError.excessiveNesting }
    }
}

struct AppsDirectoryMergeResult: Equatable {
    let entries: [AppsDirectoryEntry]
    let nextCursor: String?
    let hasMore: Bool
}

enum AppsDirectoryPagination {
    static func merge(
        existing: [AppsDirectoryEntry],
        page: AppsDirectoryPage,
        requestedCursor: String
    ) throws -> AppsDirectoryMergeResult {
        guard !page.hasMore || page.nextCursor != requestedCursor else {
            throw AppsDirectoryContractError.invalidField("pagination did not advance")
        }
        var known = Set(existing.map(\.installationID))
        let additions = page.entries.filter { known.insert($0.installationID).inserted }
        return AppsDirectoryMergeResult(
            entries: existing + additions,
            nextCursor: page.nextCursor,
            hasMore: page.hasMore
        )
    }
}

private struct AnyAppsCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}

private func rejectUnknownKeys(_ decoder: Decoder, allowed: [String]) throws {
    let keys = try decoder.container(keyedBy: AnyAppsCodingKey.self).allKeys.map(\.stringValue)
    guard Set(keys).isSubset(of: Set(allowed)) else {
        throw AppsDirectoryContractError.invalidField("unknown")
    }
}

enum AppRoutePolicy {
    static let scheme = "magapp"

    static func isCanonicalRoute(_ route: String) -> Bool {
        guard route.utf8.count <= 1_024,
              !route.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              let components = URLComponents(string: route),
              components.scheme == nil, components.host == nil,
              components.user == nil, components.password == nil,
              components.fragment == nil,
              components.percentEncodedPath.hasPrefix("/apps/"),
              let decodedPath = fullyDecodedPath(components.percentEncodedPath),
              components.percentEncodedQuery.map(isSafeEncodedQuery) ?? true,
              !decodedPath.contains("\\") else { return false }
        let segments = decodedPath.split(separator: "/", omittingEmptySubsequences: false)
        guard segments.count >= 3, segments[0].isEmpty, segments[1] == "apps" else { return false }
        return segments.dropFirst(2).allSatisfy { !$0.isEmpty && $0 != "." && $0 != ".." }
    }

    static func routeBelongsToInstallation(_ route: String, installationID: String) -> Bool {
        guard isCanonicalRoute(route),
              let encodedPath = URLComponents(string: route)?.percentEncodedPath,
              let path = fullyDecodedPath(encodedPath) else {
            return false
        }
        let segments = path.split(separator: "/", omittingEmptySubsequences: false)
        return segments.count >= 3 && String(segments[2]) == installationID
    }

    static func routeURL(_ route: String, installationID: String, origin: URL) -> URL? {
        guard routeBelongsToInstallation(route, installationID: installationID),
              var originComponents = URLComponents(url: origin, resolvingAgainstBaseURL: false),
              let routeComponents = URLComponents(string: route) else { return nil }
        originComponents.path = routeComponents.path
        originComponents.percentEncodedQuery = routeComponents.percentEncodedQuery
        originComponents.fragment = nil
        return originComponents.url
    }

    static func schemeURL(for routeURL: URL, origin: URL) -> URL? {
        guard sameOrigin(routeURL, origin),
              var components = URLComponents(url: routeURL, resolvingAgainstBaseURL: false) else {
            return nil
        }
        components.scheme = scheme
        components.host = "runtime"
        components.port = nil
        components.user = nil
        components.password = nil
        return components.url
    }

    static func destinationURL(for schemeURL: URL, origin: URL) -> URL? {
        guard schemeURL.scheme == scheme, schemeURL.host == "runtime",
              var destination = URLComponents(url: origin, resolvingAgainstBaseURL: false),
              let source = URLComponents(url: schemeURL, resolvingAgainstBaseURL: false),
              source.user == nil, source.password == nil, source.port == nil,
              source.fragment == nil else {
            return nil
        }
        destination.path = source.path
        destination.percentEncodedQuery = source.percentEncodedQuery
        destination.fragment = nil
        return destination.url
    }

    static func permitsNetworkURL(_ url: URL, method: String, installationID: String) -> Bool {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.user == nil, components.password == nil,
              components.fragment == nil,
              components.percentEncodedQuery.map(isSafeEncodedQuery) ?? true else { return false }
        return permitsNetworkPath(
            components.percentEncodedPath,
            method: method,
            installationID: installationID
        )
    }

    static func permitsNetworkPath(_ path: String, method: String, installationID: String) -> Bool {
        guard isCanonicalAbsolutePath(path) else { return false }
        let normalizedMethod = method.uppercased()
        if path.hasPrefix("/_app/") {
            return normalizedMethod == "GET" || normalizedMethod == "HEAD"
        }
        if routeBelongsToInstallation(path, installationID: installationID) {
            return normalizedMethod == "GET" || normalizedMethod == "HEAD"
        }
        let prefix = "/api/magician/v2/apps/installations/\(installationID)/"
        guard path.hasPrefix(prefix) else { return false }
        let suffix = String(path.dropFirst(prefix.count))
        if suffix == "surfaces" || suffix.hasPrefix("surfaces/") {
            return normalizedMethod == "GET" || normalizedMethod == "HEAD"
        }
        switch suffix {
        case "entity-changes":
            return normalizedMethod == "GET" || normalizedMethod == "HEAD"
        case "surface-mutations":
            return normalizedMethod == "POST"
        default:
            return false
        }
    }

    static func sameOrigin(_ lhs: URL, _ rhs: URL) -> Bool {
        lhs.scheme?.lowercased() == rhs.scheme?.lowercased()
            && lhs.host?.lowercased() == rhs.host?.lowercased()
            && effectivePort(lhs) == effectivePort(rhs)
    }

    private static func effectivePort(_ url: URL) -> Int? {
        url.port ?? (url.scheme?.lowercased() == "https" ? 443 : (url.scheme?.lowercased() == "http" ? 80 : nil))
    }

    private static func isCanonicalAbsolutePath(_ path: String) -> Bool {
        guard path.hasPrefix("/"), path.utf8.count <= 2_048,
              let decoded = fullyDecodedPath(path), !decoded.contains("\\") else { return false }
        let segments = decoded.split(separator: "/", omittingEmptySubsequences: false)
        guard segments.first?.isEmpty == true else { return false }
        return segments.dropFirst().allSatisfy { !$0.isEmpty && $0 != "." && $0 != ".." }
    }

    private static func fullyDecodedPath(_ path: String) -> String? {
        var decoded = path
        for _ in 0..<4 {
            guard let next = decoded.removingPercentEncoding else { return nil }
            if next == decoded { return decoded }
            decoded = next
        }
        // More decoding layers are not a canonical route and can be interpreted
        // differently by successive proxy/server boundaries.
        return decoded.removingPercentEncoding == decoded ? decoded : nil
    }

    private static func isSafeEncodedQuery(_ query: String) -> Bool {
        guard query.utf8.count <= 512, let decoded = fullyDecodedPath(query) else { return false }
        return !decoded.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
    }
}

struct AppRouteContext: Equatable {
    let profile: MobileConnectionProfile
    let installationID: String

    var origin: URL { profile.publicOrigin }

    func authorize(_ request: inout URLRequest) {
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
    }

    func permits(_ url: URL, method: String) -> Bool {
        AppRoutePolicy.sameOrigin(url, origin)
            && AppRoutePolicy.permitsNetworkURL(url, method: method, installationID: installationID)
    }
}

enum AppRouteResourceLimits {
    static let maximumResponseBytes: Int64 = 32 * 1_024 * 1_024

    static func admitsExpectedContentLength(_ length: Int64) -> Bool {
        length < 0 || length <= maximumResponseBytes
    }
}

/// Wiring-level client (1.6 completion) for the scripted custom-surface
/// host endpoint: the native mirror of the web host's plan fetch
/// (`fetchScriptedSurfaceHost` in unified-ui). The request is authorized
/// here — never in the frame — and the decoded plan is refused unless it
/// is bound to the installation that was asked for and names a canonical
/// digest-keyed entry document. Every non-2xx answer (operator switch off,
/// permission absent, entry point not granted, watchdog refusal) is simply
/// "no scripted surface here", leaving the pre-1.6 launcher WebView flow
/// unchanged.
enum AppSurfaceScriptedHostClient {
    static let maximumResponseBytes = 64 * 1_024

    static func hostRequest(
        profile: MobileConnectionProfile,
        installationID: String,
        route: String? = nil
    ) throws -> URLRequest {
        guard AppsDirectoryContract.isOpaqueID(installationID),
              var components = URLComponents(
                url: profile.publicOrigin.appendingPathComponent(
                    "api/magician/v2/apps/installations/\(installationID)/custom-surface-v1/host"
                ),
                resolvingAgainstBaseURL: false
              ) else { throw URLError(.badURL) }
        if let route, !route.isEmpty {
            components.queryItems = [URLQueryItem(name: "route", value: route)]
        }
        guard let url = components.url else { throw URLError(.badURL) }
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        return request
    }

    /// Fail-closed parse of one minted host plan: the installation identity
    /// must match the installation that was probed (the plan is authority
    /// the frame never supplies), the session binding must be non-empty,
    /// and the entry document must be a canonical `surfaces/`-relative HTML
    /// member under its blake3 digest — the same admission the initial
    /// scheme URL construction applies.
    static func decodePlan(_ data: Data, installationID: String) throws -> AppSurfaceScriptedPlan {
        guard !data.isEmpty, data.count <= maximumResponseBytes else {
            throw URLError(.badServerResponse)
        }
        let plan = try JSONDecoder().decode(AppSurfaceScriptedPlan.self, from: data)
        guard plan.installationID == installationID,
              !plan.sessionRef.isEmpty,
              !plan.nonce.isEmpty,
              !plan.packageRevisionRef.isEmpty,
              plan.surfaceRevision > 0,
              plan.grantRevision > 0,
              plan.entryDocument.hasSuffix(".html"),
              AppSurfaceScriptedPolicy.isCanonicalAssetTail(
                digest: plan.entryDocumentDigest,
                path: plan.entryDocument
              )
        else { throw URLError(.badServerResponse) }
        return plan
    }

    static func fetchPlan(
        profile: MobileConnectionProfile,
        installationID: String,
        route: String? = nil
    ) async throws -> AppSurfaceScriptedPlan {
        let request = try hostRequest(profile: profile, installationID: installationID, route: route)
        let (data, response) = try await BoundedAppsDirectoryDataLoader(
            maximumBytes: maximumResponseBytes
        ).load(request)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        return try decodePlan(data, installationID: installationID)
    }
}

struct BoundedAppsDirectoryResponseAccumulator {
    let maximumBytes: Int
    private(set) var data: Data

    init(maximumBytes: Int, expectedLength: Int64 = -1) throws {
        guard maximumBytes > 0,
              expectedLength < 0 || expectedLength <= Int64(maximumBytes) else {
            throw AppsDirectoryContractError.oversizedResponse
        }
        self.maximumBytes = maximumBytes
        data = Data()
        if expectedLength > 0 {
            data.reserveCapacity(min(maximumBytes, Int(expectedLength)))
        }
    }

    mutating func append(_ chunk: Data) throws {
        guard chunk.count <= maximumBytes - data.count else {
            throw AppsDirectoryContractError.oversizedResponse
        }
        data.append(chunk)
    }
}

/// Streaming-bounded loader: the byte ceiling is enforced as chunks arrive, so
/// an oversized body is cancelled rather than buffered and then rejected. Used
/// by every apps client in this module, including the mini-frame host's plan
/// fetch.
final class BoundedAppsDirectoryDataLoader: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let maximumBytes: Int
    private let lock = NSLock()
    private var accumulator: BoundedAppsDirectoryResponseAccumulator?
    private var response: URLResponse?
    private var continuation: CheckedContinuation<(Data, URLResponse), Error>?
    private var session: URLSession?
    private var completed = false

    init(maximumBytes: Int) {
        self.maximumBytes = maximumBytes
    }

    func load(_ request: URLRequest) async throws -> (Data, URLResponse) {
        try Task.checkCancellation()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                let configuration = URLSessionConfiguration.ephemeral
                configuration.timeoutIntervalForRequest = 30
                configuration.timeoutIntervalForResource = 45
                configuration.httpCookieAcceptPolicy = .never
                let delegateQueue = OperationQueue()
                delegateQueue.maxConcurrentOperationCount = 1
                delegateQueue.qualityOfService = .userInitiated
                let session = URLSession(
                    configuration: configuration,
                    delegate: self,
                    delegateQueue: delegateQueue
                )
                let task = session.dataTask(with: request)

                lock.lock()
                if completed {
                    lock.unlock()
                    session.invalidateAndCancel()
                    continuation.resume(throwing: CancellationError())
                    return
                }
                self.continuation = continuation
                self.session = session
                lock.unlock()
                task.resume()
            }
        } onCancel: { [weak self] in
            self?.finish(.failure(CancellationError()))
        }
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        do {
            let accumulator = try BoundedAppsDirectoryResponseAccumulator(
                maximumBytes: maximumBytes,
                expectedLength: response.expectedContentLength
            )
            lock.lock()
            guard !completed else {
                lock.unlock()
                completionHandler(.cancel)
                return
            }
            self.accumulator = accumulator
            self.response = response
            lock.unlock()
            completionHandler(.allow)
        } catch {
            completionHandler(.cancel)
            finish(.failure(error))
        }
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive data: Data
    ) {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        guard var accumulator else {
            lock.unlock()
            dataTask.cancel()
            finish(.failure(URLError(.badServerResponse)))
            return
        }
        do {
            try accumulator.append(data)
            self.accumulator = accumulator
            lock.unlock()
        } catch {
            lock.unlock()
            dataTask.cancel()
            finish(.failure(error))
        }
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        if let error {
            finish(.failure(error))
            return
        }
        lock.lock()
        let result = response.flatMap { response in
            accumulator.map { (data: $0.data, response: response) }
        }
        lock.unlock()
        guard let result else {
            finish(.failure(URLError(.badServerResponse)))
            return
        }
        finish(.success((result.data, result.response)))
    }

    private func finish(_ result: Result<(Data, URLResponse), Error>) {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        let continuation = continuation
        let session = session
        self.continuation = nil
        self.session = nil
        lock.unlock()
        session?.invalidateAndCancel()
        continuation?.resume(with: result)
    }
}

struct AppsDirectoryClient {
    func fetch(
        profile: MobileConnectionProfile,
        section: AppsDirectorySection,
        search: String,
        limit: Int,
        cursor: String?,
        pinnedTargetKind: AppsDirectoryPinnedTargetKind? = nil
    ) async throws -> AppsDirectoryPage {
        let request = try Self.directoryRequest(
            profile: profile,
            section: section,
            search: search,
            limit: limit,
            cursor: cursor,
            pinnedTargetKind: pinnedTargetKind
        )
        let (data, response) = try await Self.boundedData(for: request)
        try Self.requireSuccess(response, data: data)
        return try AppsDirectoryContract.decodePage(data)
    }

    func recordLaunch(profile: MobileConnectionProfile, installationID: String, viewID: String) async throws {
        let request = try Self.activityRequest(
            profile: profile,
            installationID: installationID,
            activity: .opened(viewID: viewID)
        )
        let (data, response) = try await Self.boundedData(for: request)
        try Self.requireSuccess(response, data: data)
        _ = try AppsDirectoryContract.decodeActivityReceipt(data, installationID: installationID)
    }

    func setPin(
        profile: MobileConnectionProfile,
        installationID: String,
        viewID: String,
        pinned: Bool
    ) async throws {
        let request = try Self.activityRequest(
            profile: profile,
            installationID: installationID,
            activity: .pinView(viewID: viewID, pinned: pinned)
        )
        let (data, response) = try await Self.boundedData(for: request)
        try Self.requireSuccess(response, data: data)
        _ = try AppsDirectoryContract.decodeActivityReceipt(data, installationID: installationID)
    }

    static func directoryRequest(
        profile: MobileConnectionProfile,
        section: AppsDirectorySection,
        search: String,
        limit: Int,
        cursor: String?,
        pinnedTargetKind: AppsDirectoryPinnedTargetKind? = nil
    ) throws -> URLRequest {
        guard (1...100).contains(limit),
              search.utf8.count <= 128,
              !search.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              cursor.map(AppsDirectoryContract.isCursor) ?? true,
              pinnedTargetKind == nil || section == .pinned,
              var components = URLComponents(
                url: profile.publicOrigin.appendingPathComponent("api/magician/v2/apps/directory"),
                resolvingAgainstBaseURL: false
              ) else { throw URLError(.badURL) }
        var items = [
            URLQueryItem(name: "section", value: section.rawValue),
            URLQueryItem(name: "limit", value: String(limit))
        ]
        let trimmedSearch = search.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedSearch.isEmpty { items.append(URLQueryItem(name: "search", value: trimmedSearch)) }
        if let cursor { items.append(URLQueryItem(name: "cursor", value: cursor)) }
        if let pinnedTargetKind {
            items.append(URLQueryItem(name: "pinned_target_kind", value: pinnedTargetKind.rawValue))
        }
        components.queryItems = items
        guard let url = components.url else { throw URLError(.badURL) }
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        return request
    }

    static func activityRequest(
        profile: MobileConnectionProfile,
        installationID: String,
        activity: AppsDirectoryActivity
    ) throws -> URLRequest {
        guard AppsDirectoryContract.isOpaqueID(installationID), activity.isValid else {
            throw URLError(.badURL)
        }
        let url = profile.publicOrigin
            .appendingPathComponent("api/magician/v2/apps/installations")
            .appendingPathComponent(installationID)
            .appendingPathComponent("directory-activity")
        var request = URLRequest(url: url, timeoutInterval: 20)
        request.httpMethod = "POST"
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        request.httpBody = try encoder.encode(activity)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        return request
    }

    private static func requireSuccess(_ response: URLResponse, data: Data) throws {
        guard data.count <= AppsDirectoryContract.maximumResponseBytes,
              let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
    }

    private static func boundedData(for request: URLRequest) async throws -> (Data, URLResponse) {
        try await BoundedAppsDirectoryDataLoader(
            maximumBytes: AppsDirectoryContract.maximumResponseBytes
        ).load(request)
    }
}

enum AppsDirectoryActivity: Encodable, Equatable {
    case opened(viewID: String)
    case pinView(viewID: String, pinned: Bool)

    enum CodingKeys: String, CodingKey {
        case kind
        case viewID = "view_id"
        case targetKind = "target_kind"
        case targetID = "target_id"
        case pinned
    }

    var isValid: Bool {
        switch self {
        case .opened(let viewID), .pinView(let viewID, _):
            return AppsDirectoryContract.isName(viewID)
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .opened(let viewID):
            try container.encode("opened", forKey: .kind)
            try container.encode(viewID, forKey: .viewID)
        case .pinView(let viewID, let pinned):
            try container.encode("pin", forKey: .kind)
            try container.encode("view", forKey: .targetKind)
            try container.encode(viewID, forKey: .targetID)
            try container.encode(pinned, forKey: .pinned)
        }
    }
}

// MARK: - Native app widgets, slots, and materialized indicators

enum AppNativeSurfaceContractError: LocalizedError, Equatable {
    case oversizedResponse
    case excessiveNesting
    case invalidField(String)
    case missingCachedRepresentation

    var errorDescription: String? {
        "The app widget returned an invalid native response."
    }
}

enum AppNativeSurfaceContract {
    static let schemaVersion: UInt16 = 1
    static let maximumWidgetResponseBytes = 1 * 1_048_576
    static let maximumIndicatorResponseBytes = 256 * 1_024
    static let maximumSlotResponseBytes = 128 * 1_024
    /// `APP_SLOT_RESOLUTION_BATCH_MAX_RESPONSE_BYTES`. The settings read is a
    /// different shape — up to 128 assignments plus 100 picker rows — so it
    /// carries the larger ceiling the web client uses on the same route.
    static let maximumSlotBatchResponseBytes = 128 * 1_024
    static let maximumSlotSettingsResponseBytes = 2 * 1_048_576
    static let maximumActionResponseBytes = 256 * 1_024
    static let maximumRequestBytes = 32 * 1_024
    static let maximumWidgets = 12
    static let maximumRows = 32
    static let maximumFields = 32
    static let maximumIndicators = 32
    static let maximumActions = 8
    /// One page resolves at most twelve regions in a single batch
    /// (`APP_SLOT_RESOLUTION_BATCH_MAX_ITEMS`), the same ceiling the render
    /// batch applies to widget targets.
    static let maximumPageRegions = 12
    static let maximumSlotAssignments = 128
    static let maximumSlotPickerItems = 100
    static let maximumSlotCursorBytes = 640
    static let maximumSuggestedSlotsPerWidget = 16
    /// Picker rows accumulated across cursor pages. The server bounds one
    /// page; this bounds a walk, so a churning inventory cannot grow the
    /// client's snapshot without limit.
    static let maximumAccumulatedPickerItems = 512
    static let maximumJSONDepth = 32
    static let maximumJSONNodes = 4_096
    static let minimumRefreshSeconds: TimeInterval = 5
    static let maximumRefreshSeconds: TimeInterval = 24 * 60 * 60

    static func decodeSlot(_ data: Data) throws -> AppNativeResolvedSlot {
        try preflight(data, maximumBytes: maximumSlotResponseBytes)
        return try JSONDecoder().decode(AppNativeResolvedSlot.self, from: data)
    }

    static func decodeWidgets(_ data: Data) throws -> AppNativeWidgetBatchResponse {
        try preflight(data, maximumBytes: maximumWidgetResponseBytes)
        let response = try JSONDecoder().decode(AppNativeWidgetBatchResponse.self, from: data)
        var budget = maximumJSONNodes
        try response.validateJSON(budget: &budget)
        return response
    }

    /// One page's batched slot resolution. The host answers in request order;
    /// rebinding each row to the slot that was asked for is what stops a
    /// reordered or substituted row from rendering one region's widget in
    /// another region.
    static func decodeSlotBatch(_ data: Data, slotIDs: [String]) throws -> [AppNativeResolvedSlot] {
        try preflight(data, maximumBytes: maximumSlotBatchResponseBytes)
        let response = try JSONDecoder().decode(AppNativeSlotResolutionBatchResponse.self, from: data)
        guard response.assignments.count == slotIDs.count,
              zip(response.assignments, slotIDs).allSatisfy({ $0.slotID == $1 }) else {
            throw AppNativeSurfaceContractError.invalidField("slot batch binding")
        }
        return response.assignments
    }

    static func decodeSlotSettings(_ data: Data) throws -> AppNativeSlotSettingsPage {
        try preflight(data, maximumBytes: maximumSlotSettingsResponseBytes)
        return try JSONDecoder().decode(AppNativeSlotSettingsPage.self, from: data)
    }

    /// A mutation receipt is only accepted when it is the receipt for THIS
    /// request: the same mutation id, the same slot, the revision this write
    /// was expected to produce, and the exact fence it presented. Anything
    /// else is another editor's write wearing this one's answer.
    static func decodeSlotMutationReceipt(
        _ data: Data,
        request: AppNativeSlotAssignmentWriteRequest
    ) throws -> AppNativeSlotAssignmentMutationReceipt {
        try preflight(data, maximumBytes: maximumSlotResponseBytes)
        let receipt = try JSONDecoder().decode(
            AppNativeSlotAssignmentMutationReceipt.self,
            from: data
        )
        guard receipt.mutationID == request.mutationID,
              receipt.assignment.slotID == request.command.slotID,
              request.expectedRevision < UInt64.max,
              receipt.head.revision == request.expectedRevision + 1,
              receipt.head.fence == request.writeFence,
              receipt.matchesCommand(request.command) else {
            throw AppNativeSurfaceContractError.invalidField("slot mutation receipt")
        }
        return receipt
    }

    static func isSlotCursor(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= maximumSlotCursorBytes
            && isSafeSlotText(value)
    }

    private static func isSafeSlotText(_ value: String) -> Bool {
        !value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
    }

    /// The contextual fitting's page for one app-surface route: the same
    /// injective `page:<hex>:contextual` identity the web client derives, so
    /// an entity page's slot is the same slot on both clients. A surface path
    /// that cannot form a canonical static page has no contextual slot at all
    /// rather than falling back to the installation root, which would let two
    /// different entity pages share one assignment.
    static func appSurfaceSlotPage(installationID: String, surfacePath: String) -> String? {
        guard AppsDirectoryContract.isOpaqueID(installationID) else { return nil }
        let page = surfacePath.isEmpty
            ? "/apps/\(installationID)"
            : "/apps/\(installationID)/\(surfacePath)"
        return (try? pageQualifiedSlotID(page: page, region: "contextual")) == nil ? nil : page
    }

    static func decodeIndicators(_ data: Data) throws -> AppNativeIndicatorListResponse {
        try preflight(data, maximumBytes: maximumIndicatorResponseBytes)
        return try JSONDecoder().decode(AppNativeIndicatorListResponse.self, from: data)
    }

    static func decodeActionLaunch(
        _ data: Data,
        installationID: String,
        actionID: String
    ) throws -> AppNativeActionLaunchResponse {
        try preflight(data, maximumBytes: maximumActionResponseBytes)
        let response = try JSONDecoder().decode(AppNativeActionLaunchResponse.self, from: data)
        guard response.runHandle.installationID == installationID,
              response.runHandle.actionID == actionID else {
            throw AppNativeSurfaceContractError.invalidField("action launch binding")
        }
        var budget = maximumJSONNodes
        try response.result?.validate(budget: &budget)
        return response
    }

    static func isDigest(_ value: String) -> Bool {
        guard value.hasPrefix("blake3:"), value.utf8.count == 71 else { return false }
        return value.utf8.dropFirst(7).allSatisfy {
            ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102)
        }
    }

    static func isFieldPath(_ value: String) -> Bool {
        guard !value.isEmpty, value.utf8.count <= 256 else { return false }
        let parts = value.split(separator: ".", omittingEmptySubsequences: false)
        return parts.count <= 16 && parts.allSatisfy { AppsDirectoryContract.isName(String($0)) }
    }

    static func isTimestamp(_ value: String) -> Bool {
        guard AppsDirectoryContract.isSafeText(value, maximum: 64) else { return false }
        return date(value) != nil
    }

    static func isBoundedRefresh(renderedAt: String, refreshAfter: String) -> Bool {
        guard let rendered = date(renderedAt), let refresh = date(refreshAfter) else { return false }
        let interval = refresh.timeIntervalSince(rendered)
        return interval >= minimumRefreshSeconds && interval <= maximumRefreshSeconds
    }

    static func date(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: value) ?? ISO8601DateFormatter().date(from: value)
    }

    static func normalizedETag(_ value: String?) -> String? {
        guard var value = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.contains(",") else { return nil }
        if value.hasPrefix("W/") { value.removeFirst(2) }
        if value.hasPrefix("\"") && value.hasSuffix("\"") && value.count >= 2 {
            value.removeFirst()
            value.removeLast()
        }
        return isDigest(value) ? value : nil
    }

    static func pageQualifiedSlotID(page: String, region: String) throws -> String {
        guard isCanonicalStaticPage(page), AppsDirectoryContract.isName(region) else {
            throw AppNativeSurfaceContractError.invalidField("slot location")
        }
        let routeHex = page.utf8.map { String(format: "%02x", $0) }.joined()
        return "page:\(routeHex):\(region)"
    }

    static func isPageQualifiedSlotID(_ value: String) -> Bool {
        guard value.hasPrefix("page:"), value.utf8.count <= 600,
              let separator = value.lastIndex(of: ":") else { return false }
        let routeHex = value[value.index(value.startIndex, offsetBy: 5)..<separator]
        let region = String(value[value.index(after: separator)...])
        guard !routeHex.isEmpty, routeHex.count.isMultiple(of: 2),
              AppsDirectoryContract.isName(region) else { return false }
        var routeBytes: [UInt8] = []
        routeBytes.reserveCapacity(routeHex.count / 2)
        var cursor = routeHex.startIndex
        while cursor < routeHex.endIndex {
            let next = routeHex.index(cursor, offsetBy: 2)
            guard let byte = UInt8(routeHex[cursor..<next], radix: 16) else { return false }
            routeBytes.append(byte)
            cursor = next
        }
        guard let page = String(bytes: routeBytes, encoding: .utf8) else { return false }
        return (try? pageQualifiedSlotID(page: page, region: region)) == value
    }

    private static func isCanonicalStaticPage(_ page: String) -> Bool {
        guard !page.isEmpty, page.utf8.count <= 256, page.hasPrefix("/"),
              page.canBeConverted(to: .ascii),
              !page.contains("\\"), !page.contains("?"), !page.contains("#"),
              !page.contains("%"), !page.contains(":"),
              !page.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
        else { return false }
        if page == "/" { return true }
        let segments = page.dropFirst().split(separator: "/", omittingEmptySubsequences: false)
        guard !segments.isEmpty, segments.count <= 16 else { return false }
        return segments.allSatisfy { segment in
            !segment.isEmpty && segment != "." && segment != ".." && segment.utf8.allSatisfy {
                ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90) ||
                    ($0 >= 97 && $0 <= 122) || $0 == 95 || $0 == 45 || $0 == 46
            }
        }
    }

    private static func preflight(_ data: Data, maximumBytes: Int) throws {
        guard !data.isEmpty, data.count <= maximumBytes else {
            throw AppNativeSurfaceContractError.oversizedResponse
        }
        var depth = 0
        var inString = false
        var escaped = false
        for byte in data {
            if inString {
                if escaped { escaped = false }
                else if byte == 0x5C { escaped = true }
                else if byte == 0x22 { inString = false }
                continue
            }
            if byte == 0x22 { inString = true }
            else if byte == 0x7B || byte == 0x5B {
                depth += 1
                if depth > maximumJSONDepth { throw AppNativeSurfaceContractError.excessiveNesting }
            } else if byte == 0x7D || byte == 0x5D {
                depth -= 1
                if depth < 0 { throw AppNativeSurfaceContractError.excessiveNesting }
            }
        }
        guard depth == 0, !inString else {
            throw AppNativeSurfaceContractError.excessiveNesting
        }
    }
}

indirect enum AppNativeJSONValue: Decodable, Equatable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([AppNativeJSONValue])
    case object([String: AppNativeJSONValue])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .bool(value) }
        else if let value = try? container.decode(Double.self), value.isFinite { self = .number(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode([AppNativeJSONValue].self) { self = .array(value) }
        else if let value = try? container.decode([String: AppNativeJSONValue].self) { self = .object(value) }
        else { throw AppNativeSurfaceContractError.invalidField("json value") }
    }

    func validate(budget: inout Int) throws {
        guard budget > 0 else { throw AppNativeSurfaceContractError.invalidField("json node budget") }
        budget -= 1
        switch self {
        case .null, .bool, .number: return
        case .string(let value):
            guard AppsDirectoryContract.isSafeText(value, maximum: 4_096) else {
                throw AppNativeSurfaceContractError.invalidField("json text")
            }
        case .array(let values):
            guard values.count <= 128 else {
                throw AppNativeSurfaceContractError.invalidField("json array")
            }
            for value in values { try value.validate(budget: &budget) }
        case .object(let values):
            guard values.count <= AppNativeSurfaceContract.maximumFields,
                  values.keys.allSatisfy({ AppsDirectoryContract.isSafeText($0, maximum: 256) }) else {
                throw AppNativeSurfaceContractError.invalidField("json object")
            }
            for value in values.values { try value.validate(budget: &budget) }
        }
    }

    var displayText: String {
        switch self {
        case .null: return "—"
        case .bool(let value): return value ? "Yes" : "No"
        case .number(let value):
            if value.rounded() == value,
               value >= Double(Int64.min), value <= Double(Int64.max) {
                return String(Int64(value))
            }
            return String(value)
        case .string(let value): return Self.preview(value)
        case .array(let values):
            let visible = values.prefix(8).map(\.displayText).joined(separator: ", ")
            return values.count > 8 ? "\(visible), …" : visible
        case .object: return "Details"
        }
    }

    private static func preview(_ value: String) -> String {
        guard value.utf8.count > 512 else { return value }
        var result = ""
        for character in value {
            let next = String(character)
            guard result.utf8.count + next.utf8.count <= 509 else { break }
            result.append(contentsOf: next)
        }
        return result + "…"
    }

    var referenceText: String? {
        if case .string(let value) = self { return value }
        return nil
    }
}

struct AppNativePackageBinding: Decodable, Equatable {
    let installationID: String
    let packageID: String
    let packageRevisionRef: String
    let packageContentDigest: String
    let installationGeneration: UInt64

    enum CodingKeys: String, CodingKey, CaseIterable {
        case installationID = "installation_id"
        case packageID = "package_id"
        case packageRevisionRef = "package_revision_ref"
        case packageContentDigest = "package_content_digest"
        case installationGeneration = "installation_generation"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        installationID = try values.decode(String.self, forKey: .installationID)
        packageID = try values.decode(String.self, forKey: .packageID)
        packageRevisionRef = try values.decode(String.self, forKey: .packageRevisionRef)
        packageContentDigest = try values.decode(String.self, forKey: .packageContentDigest)
        installationGeneration = try values.decode(UInt64.self, forKey: .installationGeneration)
        guard AppsDirectoryContract.isOpaqueID(installationID),
              AppsDirectoryContract.isReference(packageID),
              AppsDirectoryContract.isReference(packageRevisionRef),
              AppNativeSurfaceContract.isDigest(packageContentDigest),
              installationGeneration > 0 else {
            throw AppNativeSurfaceContractError.invalidField("package binding")
        }
    }
}

struct AppNativeWidgetBinding: Decodable, Equatable {
    let package: AppNativePackageBinding
    let widgetID: String

    enum CodingKeys: String, CodingKey, CaseIterable { case package; case widgetID = "widget_id" }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        package = try values.decode(AppNativePackageBinding.self, forKey: .package)
        widgetID = try values.decode(String.self, forKey: .widgetID)
        guard AppsDirectoryContract.isName(widgetID) else {
            throw AppNativeSurfaceContractError.invalidField("widget binding")
        }
    }
}

struct AppNativeEffectiveWidget: Decodable, Equatable {
    let pinned: AppNativeWidgetBinding
    let current: AppNativeWidgetBinding
    let restoredAcrossGeneration: Bool
    let assignmentCompatibility: String

    enum CodingKeys: String, CodingKey, CaseIterable {
        case pinned, current
        case restoredAcrossGeneration = "restored_across_generation"
        case assignmentCompatibility = "assignment_compatibility"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        pinned = try values.decode(AppNativeWidgetBinding.self, forKey: .pinned)
        current = try values.decode(AppNativeWidgetBinding.self, forKey: .current)
        restoredAcrossGeneration = try values.decode(Bool.self, forKey: .restoredAcrossGeneration)
        assignmentCompatibility = try values.decode(String.self, forKey: .assignmentCompatibility)
        guard assignmentCompatibility == "exact_digest_only",
              pinned.widgetID == current.widgetID,
              pinned.package.installationID == current.package.installationID,
              pinned.package.packageID == current.package.packageID,
              pinned.package.packageContentDigest == current.package.packageContentDigest,
              current.package.installationGeneration >= pinned.package.installationGeneration,
              restoredAcrossGeneration == (
                current.package.installationGeneration != pinned.package.installationGeneration
              ) else {
            throw AppNativeSurfaceContractError.invalidField("effective widget")
        }
    }
}

struct AppNativeResolvedSlot: Decodable, Equatable {
    let slotID: String
    let source: String?
    let pinnedSystemDefault: Bool
    let optedOut: Bool
    let widget: AppNativeEffectiveWidget?
    let hiddenReason: String?

    enum CodingKeys: String, CodingKey, CaseIterable {
        case slotID = "slot_id"
        case source
        case pinnedSystemDefault = "pinned_system_default"
        case optedOut = "opted_out"
        case widget
        case hiddenReason = "hidden_reason"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        slotID = try values.decode(String.self, forKey: .slotID)
        source = try values.decodeIfPresent(String.self, forKey: .source)
        pinnedSystemDefault = try values.decode(Bool.self, forKey: .pinnedSystemDefault)
        optedOut = try values.decode(Bool.self, forKey: .optedOut)
        widget = try values.decodeIfPresent(AppNativeEffectiveWidget.self, forKey: .widget)
        hiddenReason = try values.decodeIfPresent(String.self, forKey: .hiddenReason)
        let allowedHidden = [
            "package_unavailable", "disabled", "quarantined", "update_pending",
            "package_identity_changed", "package_digest_changed", "generation_rollback",
            "widget_no_longer_declared"
        ]
        guard AppNativeSurfaceContract.isPageQualifiedSlotID(slotID),
              source.map({ $0 == "user" || $0 == "workspace_default" }) ?? true,
              hiddenReason.map(allowedHidden.contains) ?? true,
              !(widget != nil && hiddenReason != nil),
              !(optedOut && (source != nil || widget != nil || hiddenReason != nil)),
              widget == nil || source != nil,
              hiddenReason == nil || source != nil else {
            throw AppNativeSurfaceContractError.invalidField("resolved slot")
        }
    }
}

/// One page's batched slot resolution (`POST /apps/slots/resolve-batch`).
struct AppNativeSlotResolutionBatchResponse: Decodable, Equatable {
    let assignments: [AppNativeResolvedSlot]

    enum CodingKeys: String, CodingKey, CaseIterable { case assignments }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        assignments = try values.decode([AppNativeResolvedSlot].self, forKey: .assignments)
        guard !assignments.isEmpty,
              assignments.count <= AppNativeSurfaceContract.maximumPageRegions,
              AppsDirectoryContract.hasUniqueValues(assignments.map(\.slotID)) else {
            throw AppNativeSurfaceContractError.invalidField("slot batch")
        }
    }
}

/// The settings read's write head. `revision` is the layout revision; `fence`
/// is the monotonic editor fence — opening a newer editor supersedes older
/// editors WITHOUT changing the layout, so a mutation has to present both.
struct AppNativeSlotWriteHead: Decodable, Equatable {
    let revision: UInt64
    let fence: UInt64

    enum CodingKeys: String, CodingKey, CaseIterable { case revision, fence }

    init(revision: UInt64, fence: UInt64) {
        self.revision = revision
        self.fence = fence
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        revision = try values.decode(UInt64.self, forKey: .revision)
        fence = try values.decode(UInt64.self, forKey: .fence)
        // A settings read always advances the fence, so `INITIAL` can never be
        // a head a client may write against.
        guard fence > 0 else {
            throw AppNativeSurfaceContractError.invalidField("slot write head")
        }
    }
}

/// A widget's own suggestion for where it belongs. The declared page/region
/// must re-derive the declared slot id exactly: the id is the only injective
/// identity, and a suggestion that does not re-derive it is describing a
/// different slot than the one it names.
struct AppNativeSlotSuggestion: Decodable, Equatable {
    let page: String
    let region: String
    let slotID: String
    let systemDefault: Bool

    enum CodingKeys: String, CodingKey, CaseIterable {
        case page, region
        case slotID = "slot_id"
        case systemDefault = "system_default"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        page = try values.decode(String.self, forKey: .page)
        region = try values.decode(String.self, forKey: .region)
        slotID = try values.decode(String.self, forKey: .slotID)
        systemDefault = try values.decode(Bool.self, forKey: .systemDefault)
        guard AppsDirectoryContract.isName(region),
              AppsDirectoryContract.isSafeText(page, maximum: 256),
              (try? AppNativeSurfaceContract.pageQualifiedSlotID(
                page: page,
                region: region
              )) == slotID else {
            throw AppNativeSurfaceContractError.invalidField("slot suggestion")
        }
    }
}

/// One bounded picker row. `systemClass` is host-derived provenance, never a
/// manifest claim, so an untrusted widget claiming a system-default slot is
/// refused here rather than offered as a default.
struct AppNativeSlotPickerCandidate: Decodable, Equatable, Identifiable {
    let widget: AppNativeWidgetBinding
    let title: String
    let suggestedSlots: [AppNativeSlotSuggestion]
    let systemClass: Bool

    /// The full binding identity. Two rows for the same installation/widget at
    /// different revisions are different offers, and assigning must carry the
    /// exact one the person saw.
    var id: String {
        [
            widget.package.installationID,
            widget.widgetID,
            widget.package.packageRevisionRef,
            widget.package.packageContentDigest,
            String(widget.package.installationGeneration)
        ].joined(separator: "\u{0}")
    }

    /// The identity a single page of picker rows is unique on.
    var targetKey: String { "\(widget.package.installationID)\u{0}\(widget.widgetID)" }

    enum CodingKeys: String, CodingKey, CaseIterable {
        case widget, title
        case suggestedSlots = "suggested_slots"
        case systemClass = "system_class"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        widget = try values.decode(AppNativeWidgetBinding.self, forKey: .widget)
        title = try values.decode(String.self, forKey: .title)
        suggestedSlots = try values.decode([AppNativeSlotSuggestion].self, forKey: .suggestedSlots)
        systemClass = try values.decode(Bool.self, forKey: .systemClass)
        guard !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              AppsDirectoryContract.isSafeText(title, maximum: 256),
              suggestedSlots.count <= AppNativeSurfaceContract.maximumSuggestedSlotsPerWidget,
              AppsDirectoryContract.hasUniqueValues(suggestedSlots.map(\.slotID)),
              suggestedSlots.allSatisfy({ !$0.systemDefault || systemClass }) else {
            throw AppNativeSurfaceContractError.invalidField("slot picker candidate")
        }
    }
}

/// The bounded settings/picker read. `inventoryRevision` is the exact digest
/// of the package/widget inventory this page was composed from: pages that
/// disagree on it were composed from different inventories and must never be
/// merged into one snapshot.
struct AppNativeSlotSettingsPage: Decodable, Equatable {
    let head: AppNativeSlotWriteHead
    let inventoryRevision: String
    let assignments: [AppNativeResolvedSlot]
    let nextAssignmentCursor: String?
    let assignmentsTruncated: Bool
    let picker: [AppNativeSlotPickerCandidate]
    let nextPickerCursor: String?
    let pickerTruncated: Bool

    enum CodingKeys: String, CodingKey, CaseIterable {
        case head
        case inventoryRevision = "inventory_revision"
        case assignments
        case nextAssignmentCursor = "next_assignment_cursor"
        case assignmentsTruncated = "assignments_truncated"
        case picker
        case nextPickerCursor = "next_picker_cursor"
        case pickerTruncated = "picker_truncated"
    }

    init(
        head: AppNativeSlotWriteHead,
        inventoryRevision: String,
        assignments: [AppNativeResolvedSlot],
        nextAssignmentCursor: String?,
        assignmentsTruncated: Bool,
        picker: [AppNativeSlotPickerCandidate],
        nextPickerCursor: String?,
        pickerTruncated: Bool
    ) {
        self.head = head
        self.inventoryRevision = inventoryRevision
        self.assignments = assignments
        self.nextAssignmentCursor = nextAssignmentCursor
        self.assignmentsTruncated = assignmentsTruncated
        self.picker = picker
        self.nextPickerCursor = nextPickerCursor
        self.pickerTruncated = pickerTruncated
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        head = try values.decode(AppNativeSlotWriteHead.self, forKey: .head)
        inventoryRevision = try values.decode(String.self, forKey: .inventoryRevision)
        assignments = try values.decode([AppNativeResolvedSlot].self, forKey: .assignments)
        nextAssignmentCursor = try values.decodeIfPresent(String.self, forKey: .nextAssignmentCursor)
        assignmentsTruncated = try values.decode(Bool.self, forKey: .assignmentsTruncated)
        picker = try values.decode([AppNativeSlotPickerCandidate].self, forKey: .picker)
        nextPickerCursor = try values.decodeIfPresent(String.self, forKey: .nextPickerCursor)
        pickerTruncated = try values.decode(Bool.self, forKey: .pickerTruncated)
        guard AppNativeSurfaceContract.isDigest(inventoryRevision),
              assignments.count <= AppNativeSurfaceContract.maximumSlotAssignments,
              AppsDirectoryContract.hasUniqueValues(assignments.map(\.slotID)),
              picker.count <= AppNativeSurfaceContract.maximumSlotPickerItems,
              AppsDirectoryContract.hasUniqueValues(picker.map(\.targetKey)),
              nextAssignmentCursor.map(AppNativeSurfaceContract.isSlotCursor) ?? true,
              nextPickerCursor.map(AppNativeSurfaceContract.isSlotCursor) ?? true,
              // A truncation flag and its cursor are one fact. Accepting either
              // alone would let a page claim more rows exist with no way to
              // read them, or hand back a cursor for a complete page.
              assignmentsTruncated == (nextAssignmentCursor != nil),
              pickerTruncated == (nextPickerCursor != nil) else {
            throw AppNativeSurfaceContractError.invalidField("slot settings page")
        }
    }
}

/// The encodable half of a package binding. The decoded contract types are
/// read-only by design; an assignment has to echo the exact picker row it was
/// composed from, so the write carries its own explicit mirror.
struct AppNativeSlotPackageBindingWire: Encodable, Equatable {
    let installationID: String
    let packageID: String
    let packageRevisionRef: String
    let packageContentDigest: String
    let installationGeneration: UInt64

    enum CodingKeys: String, CodingKey {
        case installationID = "installation_id"
        case packageID = "package_id"
        case packageRevisionRef = "package_revision_ref"
        case packageContentDigest = "package_content_digest"
        case installationGeneration = "installation_generation"
    }

    init(_ binding: AppNativePackageBinding) {
        installationID = binding.installationID
        packageID = binding.packageID
        packageRevisionRef = binding.packageRevisionRef
        packageContentDigest = binding.packageContentDigest
        installationGeneration = binding.installationGeneration
    }
}

struct AppNativeSlotWidgetBindingWire: Encodable, Equatable {
    let package: AppNativeSlotPackageBindingWire
    let widgetID: String

    enum CodingKeys: String, CodingKey { case package; case widgetID = "widget_id" }

    init(_ binding: AppNativeWidgetBinding) {
        package = AppNativeSlotPackageBindingWire(binding.package)
        widgetID = binding.widgetID
    }
}

/// The closed slot command set. Removing a user assignment records an opt-out
/// rather than deleting history, so a pinned workspace default survives and
/// can be restored explicitly.
enum AppNativeSlotAssignmentCommand: Encodable, Equatable {
    case assign(
        slotID: String,
        installationID: String,
        widgetID: String,
        expectedCandidate: AppNativeSlotWidgetBindingWire
    )
    case optOut(slotID: String)
    case restoreWorkspaceDefault(slotID: String)

    var slotID: String {
        switch self {
        case .assign(let slotID, _, _, _), .optOut(let slotID),
             .restoreWorkspaceDefault(let slotID):
            return slotID
        }
    }

    var name: String {
        switch self {
        case .assign: return "assign"
        case .optOut: return "opt_out"
        case .restoreWorkspaceDefault: return "restore_workspace_default"
        }
    }

    var isValid: Bool {
        guard AppNativeSurfaceContract.isPageQualifiedSlotID(slotID) else { return false }
        guard case .assign(_, let installationID, let widgetID, let candidate) = self else {
            return true
        }
        return AppsDirectoryContract.isOpaqueID(installationID)
            && AppsDirectoryContract.isName(widgetID)
            && candidate.package.installationID == installationID
            && candidate.widgetID == widgetID
            && candidate.package.installationGeneration > 0
    }

    enum CodingKeys: String, CodingKey {
        case command
        case slotID = "slot_id"
        case installationID = "installation_id"
        case widgetID = "widget_id"
        case expectedCandidate = "expected_candidate"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(name, forKey: .command)
        try container.encode(slotID, forKey: .slotID)
        guard case .assign(_, let installationID, let widgetID, let candidate) = self else { return }
        try container.encode(installationID, forKey: .installationID)
        try container.encode(widgetID, forKey: .widgetID)
        try container.encode(candidate, forKey: .expectedCandidate)
    }
}

struct AppNativeSlotAssignmentWriteRequest: Encodable, Equatable {
    let expectedRevision: UInt64
    let writeFence: UInt64
    let mutationID: String
    let command: AppNativeSlotAssignmentCommand

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case writeFence = "write_fence"
        case mutationID = "mutation_id"
        case command
    }

    static func newMutationID() -> String {
        "slot-mutation:\(UUID().uuidString.lowercased())"
    }

    var isValid: Bool {
        writeFence > 0
            && mutationID.hasPrefix("slot-mutation:")
            && AppsDirectoryContract.isReference(mutationID)
            && command.isValid
    }
}

struct AppNativeSlotAssignmentMutationReceipt: Decodable, Equatable {
    let mutationID: String
    let head: AppNativeSlotWriteHead
    let assignment: AppNativeResolvedSlot

    enum CodingKeys: String, CodingKey, CaseIterable {
        case mutationID = "mutation_id"
        case head, assignment
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        mutationID = try values.decode(String.self, forKey: .mutationID)
        head = try values.decode(AppNativeSlotWriteHead.self, forKey: .head)
        assignment = try values.decode(AppNativeResolvedSlot.self, forKey: .assignment)
    }

    /// The applied state must be the state the command asked for. A receipt
    /// that merely parses proves the write landed somewhere; this proves it
    /// landed as the exact change the person made.
    func matchesCommand(_ command: AppNativeSlotAssignmentCommand) -> Bool {
        switch command {
        case .assign(_, let installationID, let widgetID, let candidate):
            guard let widget = assignment.widget, assignment.source == "user",
                  !assignment.optedOut, !widget.restoredAcrossGeneration else { return false }
            return [widget.current, widget.pinned].allSatisfy { binding in
                binding.package.installationID == installationID
                    && binding.widgetID == widgetID
                    && binding.package.packageID == candidate.package.packageID
                    && binding.package.packageRevisionRef == candidate.package.packageRevisionRef
                    && binding.package.packageContentDigest == candidate.package.packageContentDigest
                    && binding.package.installationGeneration == candidate.package.installationGeneration
            }
        case .optOut:
            return assignment.optedOut && assignment.widget == nil && assignment.source == nil
        case .restoreWorkspaceDefault:
            return !assignment.optedOut && assignment.source != "user"
        }
    }
}

struct AppNativeSlotSettingsQuery: Equatable {
    var assignmentLimit: Int
    var assignmentCursor: String?
    var pickerLimit: Int
    var pickerCursor: String?

    var isValid: Bool {
        (1...AppNativeSurfaceContract.maximumSlotAssignments).contains(assignmentLimit)
            && (1...AppNativeSurfaceContract.maximumSlotPickerItems).contains(pickerLimit)
            && assignmentCursor.map(AppNativeSurfaceContract.isSlotCursor) ?? true
            && pickerCursor.map(AppNativeSurfaceContract.isSlotCursor) ?? true
    }
}

enum AppNativeWidgetCapability: String, Encodable, CaseIterable {
    case detailV1 = "detail_v1"
    case listV1 = "list_v1"
    case tableV1 = "table_v1"
    case timelineV1 = "timeline_v1"
    case treeV1 = "tree_v1"
    case graphV1 = "graph_v1"
    case governedActionsV1 = "governed_actions_v1"
}

struct AppNativeWidgetTarget: Encodable, Equatable {
    let installationID: String
    let widgetID: String

    enum CodingKeys: String, CodingKey {
        case installationID = "installation_id"
        case widgetID = "widget_id"
    }
}

struct AppNativeWidgetBatchRequest: Encodable, Equatable {
    let schemaVersion = AppNativeSurfaceContract.schemaVersion
    let clientCapabilities = AppNativeWidgetCapability.allCases
    let widgets: [AppNativeWidgetTarget]

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case clientCapabilities = "client_capabilities"
        case widgets
    }
}

struct AppNativeGovernedAction: Decodable, Equatable, Identifiable {
    let actionID: String
    let label: String
    var id: String { actionID }

    enum CodingKeys: String, CodingKey, CaseIterable { case actionID = "action_id"; case label }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        actionID = try values.decode(String.self, forKey: .actionID)
        label = try values.decode(String.self, forKey: .label)
        guard AppsDirectoryContract.isName(actionID), !label.isEmpty,
              AppsDirectoryContract.isSafeText(label, maximum: 64) else {
            throw AppNativeSurfaceContractError.invalidField("governed action")
        }
    }
}

struct AppNativeWidgetHints: Decodable, Equatable {
    let displayField: String?
    let partitionField: String?
    let parentField: String?
    let orderField: String?
    let statusField: String?
    let timestampField: String?
    let actionField: String?
    let actorField: String?
    let typeField: String?
    let targetField: String?

    enum CodingKeys: String, CodingKey, CaseIterable {
        case displayField = "display_field"
        case partitionField = "partition_field"
        case parentField = "parent_field"
        case orderField = "order_field"
        case statusField = "status_field"
        case timestampField = "timestamp_field"
        case actionField = "action_field"
        case actorField = "actor_field"
        case typeField = "type_field"
        case targetField = "target_field"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        displayField = try values.decodeIfPresent(String.self, forKey: .displayField)
        partitionField = try values.decodeIfPresent(String.self, forKey: .partitionField)
        parentField = try values.decodeIfPresent(String.self, forKey: .parentField)
        orderField = try values.decodeIfPresent(String.self, forKey: .orderField)
        statusField = try values.decodeIfPresent(String.self, forKey: .statusField)
        timestampField = try values.decodeIfPresent(String.self, forKey: .timestampField)
        actionField = try values.decodeIfPresent(String.self, forKey: .actionField)
        actorField = try values.decodeIfPresent(String.self, forKey: .actorField)
        typeField = try values.decodeIfPresent(String.self, forKey: .typeField)
        targetField = try values.decodeIfPresent(String.self, forKey: .targetField)
        let fields = [displayField, partitionField, parentField, orderField, statusField,
                      timestampField, actionField, actorField, typeField, targetField].compactMap { $0 }
        guard fields.allSatisfy(AppNativeSurfaceContract.isFieldPath) else {
            throw AppNativeSurfaceContractError.invalidField("render hints")
        }
    }
}

struct AppNativeWidgetRow: Decodable, Equatable, Identifiable {
    let entity: String
    let recordID: String
    let recordRevision: UInt64
    let fields: [String: AppNativeJSONValue]
    var id: String { "\(entity):\(recordID)" }

    enum CodingKeys: String, CodingKey, CaseIterable {
        case entity
        case recordID = "record_id"
        case recordRevision = "record_revision"
        case fields
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        entity = try values.decode(String.self, forKey: .entity)
        recordID = try values.decode(String.self, forKey: .recordID)
        recordRevision = try values.decode(UInt64.self, forKey: .recordRevision)
        fields = try values.decode([String: AppNativeJSONValue].self, forKey: .fields)
        guard AppsDirectoryContract.isName(entity), AppsDirectoryContract.isOpaqueID(recordID),
              fields.count <= AppNativeSurfaceContract.maximumFields,
              fields.keys.allSatisfy(AppNativeSurfaceContract.isFieldPath) else {
            throw AppNativeSurfaceContractError.invalidField("render row")
        }
    }

    func validateJSON(budget: inout Int) throws {
        for value in fields.values { try value.validate(budget: &budget) }
    }
}

enum AppNativeWidgetModel: Decodable, Equatable {
    case detail(row: AppNativeWidgetRow?, hints: AppNativeWidgetHints, actions: [AppNativeGovernedAction])
    case list(rows: [AppNativeWidgetRow], hints: AppNativeWidgetHints, actions: [AppNativeGovernedAction])
    case table(columns: [String], rows: [AppNativeWidgetRow], hints: AppNativeWidgetHints, actions: [AppNativeGovernedAction])
    case timeline(rows: [AppNativeWidgetRow], hints: AppNativeWidgetHints, actions: [AppNativeGovernedAction])
    case tree(rows: [AppNativeWidgetRow], hints: AppNativeWidgetHints, actions: [AppNativeGovernedAction])
    case graph(rows: [AppNativeWidgetRow], hints: AppNativeWidgetHints, actions: [AppNativeGovernedAction])

    enum CodingKeys: String, CodingKey, CaseIterable { case model, row, rows, columns, hints, actions }

    init(from decoder: Decoder) throws {
        let discriminator = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try discriminator.decode(String.self, forKey: .model)
        let allowed: [CodingKeys]
        switch kind {
        case "detail": allowed = [.model, .row, .hints, .actions]
        case "list", "timeline", "tree", "graph": allowed = [.model, .rows, .hints, .actions]
        case "table": allowed = [.model, .columns, .rows, .hints, .actions]
        default: throw AppNativeSurfaceContractError.invalidField("render model")
        }
        try rejectUnknownKeys(decoder, allowed: allowed.map(\.stringValue))
        let hints = try discriminator.decode(AppNativeWidgetHints.self, forKey: .hints)
        let actions = try discriminator.decode([AppNativeGovernedAction].self, forKey: .actions)
        guard actions.count <= AppNativeSurfaceContract.maximumActions,
              Set(actions.map(\.actionID)).count == actions.count else {
            throw AppNativeSurfaceContractError.invalidField("render actions")
        }
        if kind == "detail" {
            self = .detail(
                row: try discriminator.decodeIfPresent(AppNativeWidgetRow.self, forKey: .row),
                hints: hints,
                actions: actions
            )
            return
        }
        let rows = try discriminator.decode([AppNativeWidgetRow].self, forKey: .rows)
        guard rows.count <= AppNativeSurfaceContract.maximumRows,
              Set(rows.map(\.id)).count == rows.count else {
            throw AppNativeSurfaceContractError.invalidField("render rows")
        }
        switch kind {
        case "list": self = .list(rows: rows, hints: hints, actions: actions)
        case "timeline": self = .timeline(rows: rows, hints: hints, actions: actions)
        case "tree": self = .tree(rows: rows, hints: hints, actions: actions)
        case "graph": self = .graph(rows: rows, hints: hints, actions: actions)
        case "table":
            let columns = try discriminator.decode([String].self, forKey: .columns)
            guard columns.count <= AppNativeSurfaceContract.maximumFields,
                  Set(columns).count == columns.count,
                  columns.allSatisfy(AppNativeSurfaceContract.isFieldPath) else {
                throw AppNativeSurfaceContractError.invalidField("table columns")
            }
            self = .table(columns: columns, rows: rows, hints: hints, actions: actions)
        default: throw AppNativeSurfaceContractError.invalidField("render model")
        }
    }

    var actions: [AppNativeGovernedAction] {
        switch self {
        case .detail(_, _, let actions), .list(_, _, let actions), .table(_, _, _, let actions),
             .timeline(_, _, let actions), .tree(_, _, let actions), .graph(_, _, let actions):
            return actions
        }
    }

    func validateJSON(budget: inout Int) throws {
        let rows: [AppNativeWidgetRow]
        switch self {
        case .detail(let row, _, _): rows = row.map { [$0] } ?? []
        case .list(let value, _, _), .timeline(let value, _, _),
             .tree(let value, _, _), .graph(let value, _, _): rows = value
        case .table(_, let value, _, _): rows = value
        }
        for row in rows { try row.validateJSON(budget: &budget) }
    }
}

enum AppNativeWidgetFallback: Decodable, Equatable {
    case hide
    case message(title: String, body: String)

    enum CodingKeys: String, CodingKey, CaseIterable { case kind, title, body }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try values.decode(String.self, forKey: .kind)
        switch kind {
        case "hide":
            try rejectUnknownKeys(decoder, allowed: [CodingKeys.kind.stringValue])
            self = .hide
        case "message":
            try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
            let title = try values.decode(String.self, forKey: .title)
            let body = try values.decode(String.self, forKey: .body)
            guard !title.isEmpty, AppsDirectoryContract.isSafeText(title, maximum: 256),
                  !body.isEmpty, AppsDirectoryContract.isSafeText(body, maximum: 256) else {
                throw AppNativeSurfaceContractError.invalidField("widget fallback")
            }
            self = .message(title: title, body: body)
        default: throw AppNativeSurfaceContractError.invalidField("widget fallback")
        }
    }
}

enum AppNativeWidgetRenderState: Equatable {
    case ready(AppNativeWidgetModel)
    case unsupported(AppNativeWidgetFallback)
    case unavailable
}

struct AppNativeWidgetItem: Decodable, Equatable, Identifiable {
    let installationID: String
    let widgetID: String
    let title: String?
    let installationGeneration: UInt64?
    let revision: String
    let renderedAt: String
    let refreshAfter: String
    let state: AppNativeWidgetRenderState
    /// A widget's declared escalation to a sandboxed mini frame, mirroring the
    /// web client's `mini_frame` member. It rides BESIDE a complete native
    /// model, never instead of one, and only a `ready` item may carry it: an
    /// unsupported or unavailable widget claiming a frame would be asking the
    /// client to run app code in place of content the host could not produce.
    ///
    /// Absent is the ordinary case for every widget and every server that mints
    /// no mini frames — `compile_native_manifest_widgets` strips a mini-frame
    /// declaration's entry point today, so nothing the runtime renders names
    /// one yet.
    let miniFrame: AppMiniFrameDeclaration?
    var id: String { "\(installationID):\(widgetID)" }

    enum CodingKeys: String, CodingKey, CaseIterable {
        case installationID = "installation_id"
        case widgetID = "widget_id"
        case title
        case installationGeneration = "installation_generation"
        case revision
        case renderedAt = "rendered_at"
        case refreshAfter = "refresh_after"
        case state, model, fallback
        case miniFrame = "mini_frame"
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        installationID = try values.decode(String.self, forKey: .installationID)
        widgetID = try values.decode(String.self, forKey: .widgetID)
        title = try values.decodeIfPresent(String.self, forKey: .title)
        installationGeneration = try values.decodeIfPresent(UInt64.self, forKey: .installationGeneration)
        revision = try values.decode(String.self, forKey: .revision)
        renderedAt = try values.decode(String.self, forKey: .renderedAt)
        refreshAfter = try values.decode(String.self, forKey: .refreshAfter)
        let kind = try values.decode(String.self, forKey: .state)
        switch kind {
        case "ready":
            try rejectUnknownKeys(
                decoder,
                allowed: ["installation_id", "widget_id", "title", "installation_generation",
                          "revision", "rendered_at", "refresh_after", "state", "model", "mini_frame"]
            )
            state = .ready(try values.decode(AppNativeWidgetModel.self, forKey: .model))
            miniFrame = try values.decodeIfPresent(
                AppMiniFrameDeclaration.self,
                forKey: .miniFrame
            )
        case "unsupported":
            try rejectUnknownKeys(
                decoder,
                allowed: ["installation_id", "widget_id", "title", "installation_generation",
                          "revision", "rendered_at", "refresh_after", "state", "fallback"]
            )
            state = .unsupported(try values.decode(AppNativeWidgetFallback.self, forKey: .fallback))
            miniFrame = nil
        case "unavailable":
            try rejectUnknownKeys(
                decoder,
                allowed: ["installation_id", "widget_id", "title", "installation_generation",
                          "revision", "rendered_at", "refresh_after", "state"]
            )
            state = .unavailable
            miniFrame = nil
        default: throw AppNativeSurfaceContractError.invalidField("widget state")
        }
        guard AppsDirectoryContract.isOpaqueID(installationID),
              AppsDirectoryContract.isName(widgetID),
              title.map({ !$0.isEmpty && AppsDirectoryContract.isSafeText($0, maximum: 256) }) ?? true,
              installationGeneration.map({ $0 > 0 }) ?? true,
              AppNativeSurfaceContract.isDigest(revision),
              AppNativeSurfaceContract.isTimestamp(renderedAt),
              AppNativeSurfaceContract.isBoundedRefresh(
                renderedAt: renderedAt,
                refreshAfter: refreshAfter
              ),
              !(kind == "ready" && installationGeneration == nil) else {
            throw AppNativeSurfaceContractError.invalidField("widget item")
        }
    }

    func validateJSON(budget: inout Int) throws {
        if case .ready(let model) = state { try model.validateJSON(budget: &budget) }
    }
}

struct AppNativeWidgetBatchResponse: Decodable, Equatable {
    let schemaVersion: UInt16
    let revision: String
    let etag: String
    let renderedAt: String
    let refreshAfter: String
    let widgets: [AppNativeWidgetItem]

    enum CodingKeys: String, CodingKey, CaseIterable {
        case schemaVersion = "schema_version"
        case revision, etag
        case renderedAt = "rendered_at"
        case refreshAfter = "refresh_after"
        case widgets
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try values.decode(UInt16.self, forKey: .schemaVersion)
        revision = try values.decode(String.self, forKey: .revision)
        etag = try values.decode(String.self, forKey: .etag)
        renderedAt = try values.decode(String.self, forKey: .renderedAt)
        refreshAfter = try values.decode(String.self, forKey: .refreshAfter)
        widgets = try values.decode([AppNativeWidgetItem].self, forKey: .widgets)
        guard schemaVersion == AppNativeSurfaceContract.schemaVersion,
              AppNativeSurfaceContract.isDigest(revision),
              AppNativeSurfaceContract.isDigest(etag),
              revision == etag,
              AppNativeSurfaceContract.isTimestamp(renderedAt),
              AppNativeSurfaceContract.isTimestamp(refreshAfter),
              !widgets.isEmpty, widgets.count <= AppNativeSurfaceContract.maximumWidgets,
              Set(widgets.map(\.id)).count == widgets.count else {
            throw AppNativeSurfaceContractError.invalidField("widget batch")
        }
        let earliestItemRefresh = widgets.compactMap {
            AppNativeSurfaceContract.date($0.refreshAfter)
        }.min()
        guard earliestItemRefresh == AppNativeSurfaceContract.date(refreshAfter) else {
            throw AppNativeSurfaceContractError.invalidField("widget batch refresh")
        }
    }

    func validateJSON(budget: inout Int) throws {
        for widget in widgets { try widget.validateJSON(budget: &budget) }
    }
}

enum AppNativeIndicatorModel: Decodable, Equatable {
    case chip(String)
    case badge(UInt16)
    case state(String)

    enum CodingKeys: String, CodingKey, CaseIterable { case kind, text, count, label }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(String.self, forKey: .kind) {
        case "chip":
            try rejectUnknownKeys(decoder, allowed: ["kind", "text"])
            let text = try values.decode(String.self, forKey: .text)
            guard !text.isEmpty, AppsDirectoryContract.isSafeText(text, maximum: 256) else {
                throw AppNativeSurfaceContractError.invalidField("indicator chip")
            }
            self = .chip(text)
        case "badge":
            try rejectUnknownKeys(decoder, allowed: ["kind", "count"])
            let count = try values.decode(UInt16.self, forKey: .count)
            guard (1...9_999).contains(count) else {
                throw AppNativeSurfaceContractError.invalidField("indicator badge")
            }
            self = .badge(count)
        case "state":
            try rejectUnknownKeys(decoder, allowed: ["kind", "label"])
            let label = try values.decode(String.self, forKey: .label)
            guard !label.isEmpty, AppsDirectoryContract.isSafeText(label, maximum: 256) else {
                throw AppNativeSurfaceContractError.invalidField("indicator state")
            }
            self = .state(label)
        default: throw AppNativeSurfaceContractError.invalidField("indicator model")
        }
    }

    var text: String {
        switch self {
        case .chip(let text), .state(let text): return text
        case .badge(let count): return String(count)
        }
    }
}

struct AppNativeIndicator: Decodable, Equatable, Identifiable {
    let installationID: String
    let installationGeneration: UInt64
    let indicatorID: String
    let title: String
    let revision: String
    let evaluatedAt: String
    let expiresAt: String
    let model: AppNativeIndicatorModel
    var id: String { "\(installationID):\(indicatorID)" }

    enum CodingKeys: String, CodingKey, CaseIterable {
        case installationID = "installation_id"
        case installationGeneration = "installation_generation"
        case indicatorID = "indicator_id"
        case title, revision
        case evaluatedAt = "evaluated_at"
        case expiresAt = "expires_at"
        case model
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        installationID = try values.decode(String.self, forKey: .installationID)
        installationGeneration = try values.decode(UInt64.self, forKey: .installationGeneration)
        indicatorID = try values.decode(String.self, forKey: .indicatorID)
        title = try values.decode(String.self, forKey: .title)
        revision = try values.decode(String.self, forKey: .revision)
        evaluatedAt = try values.decode(String.self, forKey: .evaluatedAt)
        expiresAt = try values.decode(String.self, forKey: .expiresAt)
        model = try values.decode(AppNativeIndicatorModel.self, forKey: .model)
        guard AppsDirectoryContract.isOpaqueID(installationID), installationGeneration > 0,
              AppsDirectoryContract.isName(indicatorID), !title.isEmpty,
              AppsDirectoryContract.isSafeText(title, maximum: 256),
              AppNativeSurfaceContract.isDigest(revision),
              let evaluated = AppNativeSurfaceContract.date(evaluatedAt),
              let expiry = AppNativeSurfaceContract.date(expiresAt),
              expiry > evaluated else {
            throw AppNativeSurfaceContractError.invalidField("indicator")
        }
    }

    var isUnexpired: Bool {
        AppNativeSurfaceContract.date(expiresAt).map { $0 > Date() } ?? false
    }
}

struct AppNativeIndicatorListResponse: Decodable, Equatable {
    let schemaVersion: UInt16
    let revision: String
    let etag: String
    let generatedAt: String
    let indicators: [AppNativeIndicator]

    enum CodingKeys: String, CodingKey, CaseIterable {
        case schemaVersion = "schema_version"
        case revision, etag
        case generatedAt = "generated_at"
        case indicators
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try values.decode(UInt16.self, forKey: .schemaVersion)
        revision = try values.decode(String.self, forKey: .revision)
        etag = try values.decode(String.self, forKey: .etag)
        generatedAt = try values.decode(String.self, forKey: .generatedAt)
        indicators = try values.decode([AppNativeIndicator].self, forKey: .indicators)
        guard schemaVersion == AppNativeSurfaceContract.schemaVersion,
              AppNativeSurfaceContract.isDigest(revision),
              AppNativeSurfaceContract.isDigest(etag),
              revision == etag,
              AppNativeSurfaceContract.isTimestamp(generatedAt),
              indicators.count <= AppNativeSurfaceContract.maximumIndicators,
              Set(indicators.map(\.id)).count == indicators.count else {
            throw AppNativeSurfaceContractError.invalidField("indicator list")
        }
    }
}

struct AppNativeRunHandle: Decodable, Equatable {
    let protocolVersion: String
    let runRef: String
    let installationID: String
    let actionID: String

    enum CodingKeys: String, CodingKey, CaseIterable {
        case protocolVersion = "protocol_version"
        case runRef = "run_ref"
        case installationID = "installation_id"
        case actionID = "action_id"
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try values.decode(String.self, forKey: .protocolVersion)
        runRef = try values.decode(String.self, forKey: .runRef)
        installationID = try values.decode(String.self, forKey: .installationID)
        actionID = try values.decode(String.self, forKey: .actionID)
        guard protocolVersion == "1", AppsDirectoryContract.isReference(runRef),
              AppsDirectoryContract.isOpaqueID(installationID), AppsDirectoryContract.isName(actionID) else {
            throw AppNativeSurfaceContractError.invalidField("action run handle")
        }
    }
}

struct AppNativeActionLaunchResponse: Decodable, Equatable {
    let runHandle: AppNativeRunHandle
    let executionID: String?
    let result: AppNativeJSONValue?

    enum CodingKeys: String, CodingKey, CaseIterable {
        case runHandle = "run_handle"
        case executionID = "execution_id"
        case result
    }

    init(from decoder: Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let values = try decoder.container(keyedBy: CodingKeys.self)
        runHandle = try values.decode(AppNativeRunHandle.self, forKey: .runHandle)
        executionID = try values.decodeIfPresent(String.self, forKey: .executionID)
        result = try values.decodeIfPresent(AppNativeJSONValue.self, forKey: .result)
        guard executionID.map({ AppsDirectoryContract.isSafeText($0, maximum: 192) }) ?? true else {
            throw AppNativeSurfaceContractError.invalidField("action execution")
        }
    }
}

struct AppNativeExpectedInstallationBinding: Encodable, Equatable {
    let generation: UInt64
    let packageRevisionRef: String

    enum CodingKeys: String, CodingKey {
        case generation
        case packageRevisionRef = "package_revision_ref"
    }
}

private struct AppNativeEmptyActionRequest: Encodable {
    let idempotencyKey: String
    let input: [String: String]
    let expectedInstallationBinding: AppNativeExpectedInstallationBinding

    enum CodingKeys: String, CodingKey {
        case idempotencyKey = "idempotency_key"
        case input
        case expectedInstallationBinding = "expected_installation_binding"
    }
}

enum AppNativeHTTPResult<Value> {
    case modified(Value, etag: String)
    case notModified(etag: String, refreshAfter: String?)
}

private struct AppNativeHTTPStatusError: Error {
    let statusCode: Int
}

struct AppNativeActionIdempotencyLedger {
    private var keys: [String: String] = [:]
    private let maximumEntries = 32

    mutating func key(
        for authority: String,
        make: () -> String = { "ios-widget:\(UUID().uuidString.lowercased())" }
    ) -> String {
        if let existing = keys[authority] { return existing }
        if keys.count >= maximumEntries, let oldest = keys.keys.first {
            keys.removeValue(forKey: oldest)
        }
        let created = make()
        keys[authority] = created
        return created
    }

    mutating func markSucceeded(authority: String) { keys.removeValue(forKey: authority) }
    mutating func reset() { keys.removeAll(keepingCapacity: false) }
}

struct AppNativeSurfaceClient {
    func resolveSlot(profile: MobileConnectionProfile, slotID: String) async throws -> AppNativeResolvedSlot {
        let request = try Self.slotRequest(profile: profile, slotID: slotID)
        let (data, response) = try await Self.boundedData(
            for: request,
            maximumBytes: AppNativeSurfaceContract.maximumSlotResponseBytes
        )
        try Self.requireStatus(response, expected: 200)
        let slot = try AppNativeSurfaceContract.decodeSlot(data)
        guard slot.slotID == slotID else {
            throw AppNativeSurfaceContractError.invalidField("slot response binding")
        }
        return slot
    }

    /// Resolve every region of one page in a single call. This is the shared
    /// per-page batch: one current-inventory snapshot and one slot-state load
    /// on the host, instead of one request per region.
    func resolveSlots(
        profile: MobileConnectionProfile,
        slotIDs: [String]
    ) async throws -> [AppNativeResolvedSlot] {
        let request = try Self.slotBatchRequest(profile: profile, slotIDs: slotIDs)
        let (data, response) = try await Self.boundedData(
            for: request,
            maximumBytes: AppNativeSurfaceContract.maximumSlotBatchResponseBytes
        )
        try Self.requireStatus(response, expected: 200)
        return try AppNativeSurfaceContract.decodeSlotBatch(data, slotIDs: slotIDs)
    }

    /// The bounded settings/picker read. It is also write-fence acquisition:
    /// the returned head is what the next mutation must present, and opening a
    /// newer editor supersedes this one.
    func slotSettings(
        profile: MobileConnectionProfile,
        query: AppNativeSlotSettingsQuery
    ) async throws -> AppNativeSlotSettingsPage {
        let request = try Self.slotSettingsRequest(profile: profile, query: query)
        let (data, response) = try await Self.boundedData(
            for: request,
            maximumBytes: AppNativeSurfaceContract.maximumSlotSettingsResponseBytes
        )
        try Self.requireStatus(response, expected: 200)
        let page = try AppNativeSurfaceContract.decodeSlotSettings(data)
        guard page.assignments.count <= query.assignmentLimit,
              page.picker.count <= query.pickerLimit else {
            throw AppNativeSurfaceContractError.invalidField("slot settings page bounds")
        }
        return page
    }

    func mutateSlotAssignment(
        profile: MobileConnectionProfile,
        request writeRequest: AppNativeSlotAssignmentWriteRequest
    ) async throws -> AppNativeSlotAssignmentMutationReceipt {
        let request = try Self.slotMutationRequest(profile: profile, writeRequest: writeRequest)
        let (data, response) = try await Self.boundedData(
            for: request,
            maximumBytes: AppNativeSurfaceContract.maximumSlotResponseBytes
        )
        try Self.requireStatus(response, expected: 200)
        return try AppNativeSurfaceContract.decodeSlotMutationReceipt(data, request: writeRequest)
    }

    func render(
        profile: MobileConnectionProfile,
        targets: [AppNativeWidgetTarget],
        etag: String?
    ) async throws -> AppNativeHTTPResult<AppNativeWidgetBatchResponse> {
        let request = try Self.widgetRequest(profile: profile, targets: targets, etag: etag)
        let (data, response) = try await Self.boundedData(
            for: request,
            maximumBytes: AppNativeSurfaceContract.maximumWidgetResponseBytes
        )
        return try Self.decodeConditional(
            data: data,
            response: response,
            requestedETag: etag,
            requireWidgetRefreshAfter: true,
            decode: AppNativeSurfaceContract.decodeWidgets
        )
    }

    func indicators(
        profile: MobileConnectionProfile,
        limit: Int,
        etag: String?
    ) async throws -> AppNativeHTTPResult<AppNativeIndicatorListResponse> {
        let request = try Self.indicatorRequest(profile: profile, limit: limit, etag: etag)
        let (data, response) = try await Self.boundedData(
            for: request,
            maximumBytes: AppNativeSurfaceContract.maximumIndicatorResponseBytes
        )
        return try Self.decodeConditional(
            data: data,
            response: response,
            requestedETag: etag,
            requireWidgetRefreshAfter: false,
            decode: AppNativeSurfaceContract.decodeIndicators
        )
    }

    func launchEmptyAction(
        profile: MobileConnectionProfile,
        installationID: String,
        actionID: String,
        idempotencyKey: String,
        expectedInstallationBinding: AppNativeExpectedInstallationBinding
    ) async throws -> AppNativeActionLaunchResponse {
        let request = try Self.actionRequest(
            profile: profile,
            installationID: installationID,
            actionID: actionID,
            idempotencyKey: idempotencyKey,
            expectedInstallationBinding: expectedInstallationBinding
        )
        let (data, response) = try await Self.boundedData(
            for: request,
            maximumBytes: AppNativeSurfaceContract.maximumActionResponseBytes
        )
        try Self.requireStatus(response, expected: 202)
        return try AppNativeSurfaceContract.decodeActionLaunch(
            data,
            installationID: installationID,
            actionID: actionID
        )
    }

    static func slotRequest(profile: MobileConnectionProfile, slotID: String) throws -> URLRequest {
        guard AppNativeSurfaceContract.isPageQualifiedSlotID(slotID) else { throw URLError(.badURL) }
        let url = profile.publicOrigin
            .appendingPathComponent("api/magician/v2/apps/slots")
            .appendingPathComponent(slotID)
        return authorizedRequest(url: url, profile: profile)
    }

    static func slotBatchRequest(
        profile: MobileConnectionProfile,
        slotIDs: [String]
    ) throws -> URLRequest {
        guard !slotIDs.isEmpty,
              slotIDs.count <= AppNativeSurfaceContract.maximumPageRegions,
              AppsDirectoryContract.hasUniqueValues(slotIDs),
              slotIDs.allSatisfy(AppNativeSurfaceContract.isPageQualifiedSlotID) else {
            throw URLError(.badURL)
        }
        let url = profile.publicOrigin
            .appendingPathComponent("api/magician/v2/apps/slots/resolve-batch")
        var request = authorizedRequest(url: url, profile: profile)
        request.httpMethod = "POST"
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        request.httpBody = try encoder.encode(["slot_ids": slotIDs])
        guard request.httpBody?.count ?? 0 <= AppNativeSurfaceContract.maximumRequestBytes else {
            throw AppNativeSurfaceContractError.invalidField("slot batch request size")
        }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        return request
    }

    static func slotSettingsRequest(
        profile: MobileConnectionProfile,
        query: AppNativeSlotSettingsQuery
    ) throws -> URLRequest {
        guard query.isValid,
              var components = URLComponents(
                url: profile.publicOrigin.appendingPathComponent(
                    "api/magician/v2/apps/slot-assignments"
                ),
                resolvingAgainstBaseURL: false
              ) else { throw URLError(.badURL) }
        var items = [
            URLQueryItem(name: "assignment_limit", value: String(query.assignmentLimit)),
            URLQueryItem(name: "picker_limit", value: String(query.pickerLimit))
        ]
        if let cursor = query.assignmentCursor {
            items.append(URLQueryItem(name: "assignment_cursor", value: cursor))
        }
        if let cursor = query.pickerCursor {
            items.append(URLQueryItem(name: "picker_cursor", value: cursor))
        }
        components.queryItems = items
        guard let url = components.url else { throw URLError(.badURL) }
        return authorizedRequest(url: url, profile: profile)
    }

    static func slotMutationRequest(
        profile: MobileConnectionProfile,
        writeRequest: AppNativeSlotAssignmentWriteRequest
    ) throws -> URLRequest {
        guard writeRequest.isValid else { throw URLError(.badURL) }
        let url = profile.publicOrigin
            .appendingPathComponent("api/magician/v2/apps/slot-assignments")
        var request = authorizedRequest(url: url, profile: profile)
        request.httpMethod = "POST"
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        request.httpBody = try encoder.encode(writeRequest)
        guard request.httpBody?.count ?? 0 <= AppNativeSurfaceContract.maximumRequestBytes else {
            throw AppNativeSurfaceContractError.invalidField("slot mutation request size")
        }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        return request
    }

    static func widgetRequest(
        profile: MobileConnectionProfile,
        targets: [AppNativeWidgetTarget],
        etag: String?
    ) throws -> URLRequest {
        guard !targets.isEmpty, targets.count <= AppNativeSurfaceContract.maximumWidgets,
              Set(targets.map { "\($0.installationID):\($0.widgetID)" }).count == targets.count,
              targets.allSatisfy({
                AppsDirectoryContract.isOpaqueID($0.installationID) && AppsDirectoryContract.isName($0.widgetID)
              }) else { throw URLError(.badURL) }
        let url = profile.publicOrigin.appendingPathComponent("api/magician/v2/apps/widgets/render-batch")
        var request = authorizedRequest(url: url, profile: profile)
        request.httpMethod = "POST"
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        request.httpBody = try encoder.encode(AppNativeWidgetBatchRequest(widgets: targets))
        guard request.httpBody?.count ?? 0 <= AppNativeSurfaceContract.maximumRequestBytes else {
            throw AppNativeSurfaceContractError.invalidField("widget request size")
        }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let etag {
            guard let normalized = AppNativeSurfaceContract.normalizedETag(etag) else { throw URLError(.badURL) }
            request.setValue("\"\(normalized)\"", forHTTPHeaderField: "If-None-Match")
        }
        return request
    }

    static func indicatorRequest(
        profile: MobileConnectionProfile,
        limit: Int,
        etag: String?
    ) throws -> URLRequest {
        guard (1...AppNativeSurfaceContract.maximumIndicators).contains(limit),
              var components = URLComponents(
                url: profile.publicOrigin.appendingPathComponent("api/magician/v2/apps/indicators"),
                resolvingAgainstBaseURL: false
              ) else { throw URLError(.badURL) }
        components.queryItems = [URLQueryItem(name: "limit", value: String(limit))]
        guard let url = components.url else { throw URLError(.badURL) }
        var request = authorizedRequest(url: url, profile: profile)
        if let etag {
            guard let normalized = AppNativeSurfaceContract.normalizedETag(etag) else { throw URLError(.badURL) }
            request.setValue("\"\(normalized)\"", forHTTPHeaderField: "If-None-Match")
        }
        return request
    }

    static func actionRequest(
        profile: MobileConnectionProfile,
        installationID: String,
        actionID: String,
        idempotencyKey: String,
        expectedInstallationBinding: AppNativeExpectedInstallationBinding
    ) throws -> URLRequest {
        guard AppsDirectoryContract.isOpaqueID(installationID), AppsDirectoryContract.isName(actionID),
              AppsDirectoryContract.isReference(idempotencyKey),
              expectedInstallationBinding.generation > 0,
              AppsDirectoryContract.isReference(expectedInstallationBinding.packageRevisionRef) else {
            throw URLError(.badURL)
        }
        let url = profile.publicOrigin
            .appendingPathComponent("api/magician/v2/apps/installations")
            .appendingPathComponent(installationID)
            .appendingPathComponent("actions")
            .appendingPathComponent(actionID)
            .appendingPathComponent("runs")
        var request = authorizedRequest(url: url, profile: profile)
        request.httpMethod = "POST"
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        request.httpBody = try encoder.encode(AppNativeEmptyActionRequest(
            idempotencyKey: idempotencyKey,
            input: [:],
            expectedInstallationBinding: expectedInstallationBinding
        ))
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        return request
    }

    private static func authorizedRequest(url: URL, profile: MobileConnectionProfile) -> URLRequest {
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        return request
    }

    static func decodeConditional<Value>(
        data: Data,
        response: URLResponse,
        requestedETag: String?,
        requireWidgetRefreshAfter: Bool,
        decode: (Data) throws -> Value
    ) throws -> AppNativeHTTPResult<Value> {
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        let responseETag = AppNativeSurfaceContract.normalizedETag(
            http.value(forHTTPHeaderField: "ETag")
        )
        let widgetRefreshAfter = http.value(forHTTPHeaderField: "X-App-Widget-Refresh-After")?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if http.statusCode == 304 {
            guard data.isEmpty, let requested = AppNativeSurfaceContract.normalizedETag(requestedETag),
                  let responseETag, requested == responseETag,
                  !requireWidgetRefreshAfter ||
                    widgetRefreshAfter.map(AppNativeSurfaceContract.isTimestamp) == true else {
                throw AppNativeSurfaceContractError.missingCachedRepresentation
            }
            return .notModified(etag: responseETag, refreshAfter: widgetRefreshAfter)
        }
        guard http.statusCode == 200, let responseETag else { throw URLError(.badServerResponse) }
        let value = try decode(data)
        let bodyETag: String
        if let widgets = value as? AppNativeWidgetBatchResponse {
            bodyETag = widgets.etag
            if requireWidgetRefreshAfter {
                guard let header = widgetRefreshAfter.flatMap(AppNativeSurfaceContract.date),
                      header == AppNativeSurfaceContract.date(widgets.refreshAfter) else {
                    throw AppNativeSurfaceContractError.invalidField("refresh header binding")
                }
            }
        }
        else if let indicators = value as? AppNativeIndicatorListResponse { bodyETag = indicators.etag }
        else { throw URLError(.cannotParseResponse) }
        guard responseETag == bodyETag else {
            throw AppNativeSurfaceContractError.invalidField("etag binding")
        }
        return .modified(value, etag: responseETag)
    }

    private static func requireStatus(_ response: URLResponse, expected: Int) throws {
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard http.statusCode == expected else { throw AppNativeHTTPStatusError(statusCode: http.statusCode) }
    }

    private static func boundedData(
        for request: URLRequest,
        maximumBytes: Int
    ) async throws -> (Data, URLResponse) {
        try await BoundedAppsDirectoryDataLoader(maximumBytes: maximumBytes).load(request)
    }
}

/// One region of a page after resolution, carrying whatever the page's shared
/// render batch produced for it.
struct AppNativeSlotPlacement: Identifiable, Equatable {
    let region: String
    let assignment: AppNativeResolvedSlot
    let item: AppNativeWidgetItem?
    let actionBinding: AppNativeExpectedInstallationBinding?
    /// The exact authority this card was rendered under. A mini frame re-mints
    /// whenever it changes, so a frame can never outlive the scope, package
    /// revision, generation or content revision it was admitted for.
    let authorityKey: String
    var id: String { region }

    /// A slot that is assigned but currently renders nothing still occupies
    /// the layout. Erasing it would read as the assignment having been lost,
    /// and would leave nothing to remove — except a hidden workspace default,
    /// which reads as an empty slot.
    var presentation: AppNativeSlotPresentation {
        AppNativeSlotRefreshPolicy.presentation(assignment: assignment, hasItem: item != nil)
    }
}

/// What one slot region draws. Decided purely from the resolved assignment and
/// whether a presentable render exists, so the rules are unit-testable apart
/// from SwiftUI.
enum AppNativeSlotPresentation: Equatable {
    /// A rendered widget card with its Remove control.
    case card
    /// "Temporarily unavailable" plus Remove: an assignment the user made, or
    /// one whose package is disabled or mid-update, that comes back when the
    /// package does and must stay removable meanwhile.
    case unavailable
    /// The empty-slot add affordance.
    case add
}

/// The pure refresh, staleness and presentation rules for a page's widget
/// slots. The view model applies them; tests exercise them directly.
enum AppNativeSlotRefreshPolicy {
    /// How long the last render the server vouched for (by a 200 or a 304)
    /// stays on screen through refreshes and transient failures when the
    /// payload names no bound of its own.
    static let defaultMaximumStalenessSeconds: TimeInterval = 5 * 60

    /// A render-batch deadline to schedule against. The batch deadline is the
    /// minimum over items and a shared server cache can hand back one that has
    /// already passed by the time it arrives; a missing, unparseable or
    /// near-now deadline is accepted and pushed out to the refresh floor
    /// rather than treated as a broken response.
    static func acceptedDeadline(_ raw: String?, now: Date) -> Date {
        let floor = now.addingTimeInterval(AppNativeSurfaceContract.minimumRefreshSeconds)
        guard let raw, let deadline = AppNativeSurfaceContract.date(raw) else { return floor }
        return max(deadline, floor)
    }

    /// A 304 means every cached item is still current until the new deadline,
    /// so every item's own deadline moves with the batch's — not only the
    /// batch header.
    static func renewing(
        _ batch: AppNativeWidgetBatchResponse,
        until deadline: Date
    ) -> AppNativeWidgetBatchResponse {
        let stamp = timestamp(deadline)
        return AppNativeWidgetBatchResponse(
            renewing: batch,
            refreshAfter: stamp,
            widgets: batch.widgets.map { AppNativeWidgetItem(renewing: $0, refreshAfter: stamp) }
        )
    }

    /// Whether the last good render may still be shown: the server vouched for
    /// it within the staleness bound.
    static func retainsLastGood(
        confirmedAt: Date?,
        now: Date,
        maximumStaleness: TimeInterval = defaultMaximumStalenessSeconds
    ) -> Bool {
        guard let confirmedAt else { return false }
        return now.timeIntervalSince(confirmedAt) <= maximumStaleness
    }

    /// An item is shown while its own deadline is ahead, or — stale while
    /// revalidating — while the batch it came in is inside the staleness bound.
    static func isItemLive(
        refreshAfter: String,
        confirmedAt: Date?,
        now: Date,
        maximumStaleness: TimeInterval = defaultMaximumStalenessSeconds
    ) -> Bool {
        if AppNativeSurfaceContract.date(refreshAfter).map({ $0 > now }) == true { return true }
        return retainsLastGood(confirmedAt: confirmedAt, now: now, maximumStaleness: maximumStaleness)
    }

    /// A workspace default the server has hidden for a reason other than the
    /// package being disabled or mid-update is not the user's assignment: it
    /// reads as an empty slot, not as a broken widget the user must remove.
    static func presentation(
        assignment: AppNativeResolvedSlot,
        hasItem: Bool
    ) -> AppNativeSlotPresentation {
        if hasItem { return .card }
        let retained = assignment.widget != nil || assignment.source != nil
            || assignment.hiddenReason != nil
        guard retained else { return .add }
        if assignment.source == "workspace_default", assignment.widget == nil,
           let reason = assignment.hiddenReason,
           reason != "disabled", reason != "update_pending" {
            return .add
        }
        return .unavailable
    }

    static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

extension AppNativeWidgetItem {
    /// The same validated item with a server-renewed deadline.
    init(renewing item: AppNativeWidgetItem, refreshAfter: String) {
        installationID = item.installationID
        widgetID = item.widgetID
        title = item.title
        installationGeneration = item.installationGeneration
        revision = item.revision
        renderedAt = item.renderedAt
        self.refreshAfter = refreshAfter
        state = item.state
        miniFrame = item.miniFrame
    }
}

extension AppNativeWidgetBatchResponse {
    /// The same validated batch with server-renewed deadlines.
    init(
        renewing batch: AppNativeWidgetBatchResponse,
        refreshAfter: String,
        widgets: [AppNativeWidgetItem]
    ) {
        schemaVersion = batch.schemaVersion
        revision = batch.revision
        etag = batch.etag
        renderedAt = batch.renderedAt
        self.refreshAfter = refreshAfter
        self.widgets = widgets
    }
}

/// One page's slot owner: ONE `slots/resolve-batch` call for every region on
/// the page and ONE `widgets/render-batch` call for every widget those regions
/// resolve to, plus the bounded picker/removal flow over the same head.
///
/// It is per page rather than per region because the host bounds its work per
/// request, not per client: a page of N regions used to cost N inventory
/// snapshots and N single-target renders.
@MainActor
final class AppNativeSlotPageViewModel: ObservableObject {
    @Published private(set) var placements: [AppNativeSlotPlacement] = []
    @Published private(set) var actionNotice: String?
    @Published private(set) var launchingAction = false
    @Published private(set) var pickerRegion: String?
    @Published private(set) var settings: AppNativeSlotSettingsPage?
    @Published private(set) var settingsLoading = false
    @Published private(set) var applyingMutation = false
    @Published private(set) var slotError: String?
    @Published private(set) var hasPendingMutation = false

    let page: String
    private let regions: [String]
    private let slotIDs: [String]
    private let client: AppNativeSurfaceClient
    private var assignments: [String: AppNativeResolvedSlot] = [:]
    private var cachedBatch: AppNativeWidgetBatchResponse?
    /// When the server last vouched for `cachedBatch` (a 200 or a validated
    /// 304). Bounds how long the last good render survives refreshes and
    /// transient failures.
    private var confirmedAt: Date?
    private var etag: String?
    private var targetKey: String?
    private var appliedProfile: MobileConnectionProfile?
    private var generation: UInt64 = 0
    private var foregroundActive = false
    private var refreshAfter: Date?
    private var cadenceTask: Task<Void, Never>?
    private var actionIdempotency = AppNativeActionIdempotencyLedger()
    private var pendingMutation: PendingSlotMutation?
    private var pickerCursors: Set<String> = []

    /// A mutation whose outcome was never observed. Its exact fence and
    /// mutation id are retained so the retry is the SAME write, not a second
    /// one composed against a head that has since moved.
    private struct PendingSlotMutation {
        let profile: MobileConnectionProfile
        let region: String
        let request: AppNativeSlotAssignmentWriteRequest
    }

    /// A page whose regions are not a bounded, unique, canonical set resolves
    /// nothing at all. Refusing here keeps a malformed fitting from silently
    /// rendering a partial page.
    init(
        page: String,
        regions: [String],
        client: AppNativeSurfaceClient = AppNativeSurfaceClient()
    ) {
        self.page = page
        self.client = client
        let ids = regions.compactMap {
            try? AppNativeSurfaceContract.pageQualifiedSlotID(page: page, region: $0)
        }
        if regions.isEmpty || regions.count > AppNativeSurfaceContract.maximumPageRegions
            || ids.count != regions.count || !AppsDirectoryContract.hasUniqueValues(regions) {
            self.regions = []
            self.slotIDs = []
        } else {
            self.regions = regions
            self.slotIDs = ids
        }
    }

    func invalidate() {
        generation &+= 1
        cadenceTask?.cancel()
        cadenceTask = nil
        placements = []
        assignments = [:]
        cachedBatch = nil
        confirmedAt = nil
        etag = nil
        targetKey = nil
        appliedProfile = nil
        refreshAfter = nil
        actionNotice = nil
        launchingAction = false
        actionIdempotency.reset()
        resetSlotControls()
    }

    /// The picker, its accumulated cursors, and any pending write belong to one
    /// scope and one page binding. None of them survives a change to either.
    private func resetSlotControls() {
        pickerRegion = nil
        settings = nil
        settingsLoading = false
        applyingMutation = false
        slotError = nil
        pendingMutation = nil
        hasPendingMutation = false
        pickerCursors = []
    }

    func setForegroundActive(_ active: Bool) {
        guard foregroundActive != active else { return }
        foregroundActive = active
        if active { scheduleCadence() }
        else {
            generation &+= 1
            cadenceTask?.cancel()
            cadenceTask = nil
            // A background transition removes the presentation immediately,
            // while retaining the conditional body for a validated 304 when the
            // scene becomes active again.
            placements = []
        }
    }

    func reload() async {
        generation &+= 1
        cadenceTask?.cancel()
        cadenceTask = nil
        let requestGeneration = generation
        // Stale while revalidating: the last good cards stay up through the
        // refresh unless the server has not vouched for them within the
        // staleness bound.
        if !AppNativeSlotRefreshPolicy.retainsLastGood(confirmedAt: confirmedAt, now: Date()) {
            placements = []
        }
        guard !slotIDs.isEmpty, let profile = MagicianAccess.connectionProfile else {
            invalidate()
            return
        }
        if appliedProfile != nil && appliedProfile != profile {
            // An ETag and a picker head are scoped response authority, never a
            // global content key. Neither may cross into a replacement profile.
            hideProfileBoundState()
        }
        do {
            let resolved = try await client.resolveSlots(profile: profile, slotIDs: slotIDs)
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            assignments = Dictionary(
                uniqueKeysWithValues: zip(regions, resolved).map { ($0, $1) }
            )
            var targets: [AppNativeWidgetTarget] = []
            var seenTargets: Set<String> = []
            for region in regions {
                guard let current = assignments[region]?.widget?.current else { continue }
                let key = "\(current.package.installationID)\u{0}\(current.widgetID)"
                guard seenTargets.insert(key).inserted else { continue }
                targets.append(AppNativeWidgetTarget(
                    installationID: current.package.installationID,
                    widgetID: current.widgetID
                ))
            }
            let nextTargetKey = Self.targetKey(profile: profile, regions: regions, assignments: assignments)
            if targets.isEmpty {
                cachedBatch = nil
                confirmedAt = nil
                etag = nil
                targetKey = nextTargetKey
                appliedProfile = profile
                refreshAfter = nil
                publish(batch: nil)
                // Empty and retained-hidden slots still have to discover a
                // remote assignment, enablement, or default change without a
                // relaunch, so an empty page keeps a slow cadence.
                scheduleCadence(fallbackSeconds: 30)
                return
            }
            let requestETag = nextTargetKey == targetKey ? etag : nil
            if nextTargetKey != targetKey {
                // The old cards are no longer authorized by this exact scope,
                // package revision/digest, and generation. Hide them before the
                // replacement render can suspend or be cancelled.
                placements = []
                cachedBatch = nil
                confirmedAt = nil
                actionNotice = nil
                actionIdempotency.reset()
            }
            let result = try await client.render(
                profile: profile,
                targets: targets,
                etag: requestETag
            )
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            switch result {
            case .notModified(let responseETag, let nextRefreshAfter):
                guard requestETag != nil, let cachedBatch else {
                    throw AppNativeSurfaceContractError.missingCachedRepresentation
                }
                // A 304 vouches for every cached item until the new deadline.
                // A deadline already at or behind local now (or absent) is
                // accepted and retried after the refresh floor.
                let now = Date()
                let deadline = AppNativeSlotRefreshPolicy.acceptedDeadline(nextRefreshAfter, now: now)
                let renewed = AppNativeSlotRefreshPolicy.renewing(cachedBatch, until: deadline)
                self.cachedBatch = renewed
                confirmedAt = now
                etag = responseETag
                targetKey = nextTargetKey
                appliedProfile = profile
                refreshAfter = deadline
                publish(batch: renewed)
                scheduleCadence()
            case .modified(let response, let responseETag):
                let requested = Set(targets.map { "\($0.installationID)\u{0}\($0.widgetID)" })
                guard response.widgets.count == targets.count,
                      response.widgets.allSatisfy({
                        requested.contains("\($0.installationID)\u{0}\($0.widgetID)")
                      }) else {
                    throw AppNativeSurfaceContractError.invalidField("widget response binding")
                }
                let now = Date()
                cachedBatch = response
                confirmedAt = now
                etag = responseETag
                targetKey = nextTargetKey
                appliedProfile = profile
                refreshAfter = AppNativeSlotRefreshPolicy.acceptedDeadline(response.refreshAfter, now: now)
                scheduleCadence()
                publish(batch: response)
            }
        } catch {
            guard !Task.isCancelled, generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            // A transient failure keeps the last good cards, within the
            // staleness bound, instead of blanking every slot. The ETag is
            // dropped so the next attempt fetches a full body.
            etag = nil
            refreshAfter = nil
            if AppNativeSlotRefreshPolicy.retainsLastGood(confirmedAt: confirmedAt, now: Date()) {
                publish(batch: cachedBatch)
            } else {
                placements = []
                assignments = [:]
                cachedBatch = nil
                confirmedAt = nil
                targetKey = nil
                appliedProfile = nil
            }
            scheduleCadence(fallbackSeconds: 30)
        }
    }

    /// The exact authority every rendered card and every widget action on this
    /// page is bound to: the scope, and each region's current package identity,
    /// revision, digest and generation.
    private static func targetKey(
        profile: MobileConnectionProfile,
        regions: [String],
        assignments: [String: AppNativeResolvedSlot]
    ) -> String {
        var parts = [
            profile.publicOrigin.absoluteString,
            profile.principal,
            profile.workspace,
            profile.deviceID
        ]
        for region in regions {
            let current = assignments[region]?.widget?.current
            parts.append(contentsOf: [
                region,
                current?.package.installationID ?? "",
                current?.widgetID ?? "",
                current?.package.packageRevisionRef ?? "",
                current?.package.packageContentDigest ?? "",
                current.map { String($0.package.installationGeneration) } ?? ""
            ])
        }
        return parts.joined(separator: "\u{0}")
    }

    /// Bind each region to its rendered item. A rendered widget whose
    /// generation is not the generation the slot resolved to is a different
    /// installation state and is dropped rather than displayed.
    private func publish(batch: AppNativeWidgetBatchResponse?) {
        let now = Date()
        var rendered: [String: AppNativeWidgetItem] = [:]
        for item in batch?.widgets ?? [] {
            rendered["\(item.installationID)\u{0}\(item.widgetID)"] = item
        }
        placements = regions.compactMap { region -> AppNativeSlotPlacement? in
            guard let assignment = assignments[region] else { return nil }
            guard let current = assignment.widget?.current,
                  let item = rendered["\(current.package.installationID)\u{0}\(current.widgetID)"],
                  item.installationGeneration == current.package.installationGeneration,
                  AppNativeSlotRefreshPolicy.isItemLive(
                    refreshAfter: item.refreshAfter,
                    confirmedAt: confirmedAt,
                    now: now
                  ),
                  Self.isPresentable(item) else {
                return AppNativeSlotPlacement(
                    region: region,
                    assignment: assignment,
                    item: nil,
                    actionBinding: nil,
                    authorityKey: ""
                )
            }
            return AppNativeSlotPlacement(
                region: region,
                assignment: assignment,
                item: item,
                actionBinding: AppNativeExpectedInstallationBinding(
                    generation: current.package.installationGeneration,
                    packageRevisionRef: current.package.packageRevisionRef
                ),
                authorityKey: [
                    targetKey ?? "",
                    region,
                    current.package.packageRevisionRef,
                    String(current.package.installationGeneration),
                    item.revision
                ].joined(separator: "\u{0}")
            )
        }
    }

    private static func isPresentable(_ item: AppNativeWidgetItem) -> Bool {
        switch item.state {
        case .ready, .unsupported(.message(_, _)): return true
        case .unsupported(.hide), .unavailable: return false
        }
    }

    private func scheduleCadence(fallbackSeconds: TimeInterval? = nil) {
        cadenceTask?.cancel()
        cadenceTask = nil
        guard foregroundActive else { return }
        let delay: TimeInterval
        if let refreshAfter {
            delay = min(
                max(refreshAfter.timeIntervalSinceNow, AppNativeSurfaceContract.minimumRefreshSeconds),
                AppNativeSurfaceContract.maximumRefreshSeconds
            )
        } else if let fallbackSeconds {
            delay = fallbackSeconds
        } else {
            return
        }
        cadenceTask = Task { [weak self] in
            do {
                try await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            } catch {
                return
            }
            guard !Task.isCancelled, let self, self.foregroundActive else { return }
            await self.reload()
        }
    }

    func launch(_ action: AppNativeGovernedAction, in region: String) async {
        guard !launchingAction, let profile = MagicianAccess.connectionProfile,
              appliedProfile == profile, let targetKey,
              let placement = placements.first(where: { $0.region == region }),
              let item = placement.item, let binding = placement.actionBinding,
              case .ready(let model) = item.state,
              item.installationGeneration == binding.generation,
              model.actions.contains(where: { $0.actionID == action.actionID }) else { return }
        launchingAction = true
        actionNotice = nil
        let requestGeneration = generation
        let actionAuthority = [
            targetKey,
            region,
            item.installationID,
            String(item.installationGeneration ?? 0),
            item.revision,
            action.actionID
        ].joined(separator: "\u{0}")
        let idempotencyKey = actionIdempotency.key(for: actionAuthority)
        defer { launchingAction = false }
        do {
            let receipt = try await client.launchEmptyAction(
                profile: profile,
                installationID: item.installationID,
                actionID: action.actionID,
                idempotencyKey: idempotencyKey,
                expectedInstallationBinding: binding
            )
            actionIdempotency.markSucceeded(authority: actionAuthority)
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            actionNotice = "\(action.label) started · \(receipt.runHandle.runRef)"
            await reload()
        } catch {
            guard !Task.isCancelled, generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            if (error as? AppNativeHTTPStatusError)?.statusCode == 409 {
                actionNotice = "The widget changed. Refreshing before another action."
                await reload()
                return
            }
            actionNotice = "\(action.label) could not be started."
        }
    }

    // MARK: - Picker and removal

    /// Read the settings head, following exactly one assignment cursor when the
    /// first page did not carry this slot. Both pages must come from one
    /// layout revision and one inventory revision with a strictly newer fence,
    /// or they are two different snapshots and cannot be merged.
    private func settingsHead(
        for slotID: String,
        pickerLimit: Int,
        profile: MobileConnectionProfile
    ) async throws -> AppNativeSlotSettingsPage {
        let first = try await client.slotSettings(
            profile: profile,
            query: AppNativeSlotSettingsQuery(
                assignmentLimit: AppNativeSurfaceContract.maximumSlotAssignments,
                assignmentCursor: nil,
                pickerLimit: pickerLimit,
                pickerCursor: nil
            )
        )
        if first.assignments.contains(where: { $0.slotID == slotID }) || !first.assignmentsTruncated {
            return first
        }
        guard let cursor = first.nextAssignmentCursor else {
            throw AppNativeSurfaceContractError.invalidField("slot settings pagination")
        }
        let second = try await client.slotSettings(
            profile: profile,
            query: AppNativeSlotSettingsQuery(
                assignmentLimit: AppNativeSurfaceContract.maximumSlotAssignments,
                assignmentCursor: cursor,
                pickerLimit: pickerLimit,
                pickerCursor: nil
            )
        )
        let merged = first.assignments + second.assignments
        guard second.head.revision == first.head.revision,
              second.head.fence > first.head.fence,
              second.inventoryRevision == first.inventoryRevision,
              !second.assignmentsTruncated, second.nextAssignmentCursor == nil,
              merged.count <= 2 * AppNativeSurfaceContract.maximumSlotAssignments,
              AppsDirectoryContract.hasUniqueValues(merged.map(\.slotID)) else {
            throw AppNativeSurfaceContractError.invalidField("slot settings pagination")
        }
        return AppNativeSlotSettingsPage(
            head: second.head,
            inventoryRevision: second.inventoryRevision,
            assignments: merged,
            nextAssignmentCursor: nil,
            assignmentsTruncated: false,
            picker: second.picker,
            nextPickerCursor: second.nextPickerCursor,
            pickerTruncated: second.pickerTruncated
        )
    }

    /// The picker's snapshot must describe the same slot the page rendered.
    /// Comparing the whole resolved authority — not just the widget id —
    /// catches a reinstall or default change between the two reads.
    private func settingsMatch(_ page: AppNativeSlotSettingsPage, _ resolved: AppNativeResolvedSlot) -> Bool {
        guard let row = page.assignments.first(where: { $0.slotID == resolved.slotID }) else {
            // The settings read carries only slots that have state, so an
            // absent row IS the empty slot — and matches a resolution that
            // likewise carries none. Treating absence as a mismatch would make
            // every first assignment to an empty slot look like a race.
            return resolved.source == nil && resolved.widget == nil
                && resolved.hiddenReason == nil && !resolved.optedOut
                && !resolved.pinnedSystemDefault
        }
        return row == resolved
    }

    func openPicker(_ region: String) async {
        // A settings read advances the write fence. While a POST outcome is
        // still ambiguous, its exact fence and identity must be preserved.
        guard pendingMutation == nil, !applyingMutation, !settingsLoading,
              let assignment = assignments[region],
              let profile = MagicianAccess.connectionProfile, appliedProfile == profile else { return }
        let requestGeneration = generation
        pickerRegion = region
        settings = nil
        settingsLoading = true
        slotError = nil
        // Only one settings-owning call may be in flight at a time (each one
        // guards on `settingsLoading`), so this release is unconditional: tying
        // it to a generation would strand the flag — and every slot control —
        // whenever a render cadence tick landed mid-read.
        defer { settingsLoading = false }
        do {
            let next = try await settingsHead(
                for: assignment.slotID,
                pickerLimit: AppNativeSurfaceContract.maximumSlotPickerItems,
                profile: profile
            )
            guard generation == requestGeneration, pickerRegion == region,
                  MagicianAccess.connectionProfile == profile else { return }
            guard settingsMatch(next, assignment) else {
                slotError = "The slot changed while the picker was opening. Reopen it."
                return
            }
            settings = next
            pickerCursors = Set(next.nextPickerCursor.map { [$0] } ?? [])
        } catch {
            guard !Task.isCancelled, generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            settings = nil
            slotError = "The widget picker could not be loaded."
        }
    }

    func loadMorePickerCandidates() async {
        guard let previous = settings, previous.pickerTruncated,
              let cursor = previous.nextPickerCursor,
              !settingsLoading, !applyingMutation, pendingMutation == nil,
              let region = pickerRegion,
              let profile = MagicianAccess.connectionProfile, appliedProfile == profile else { return }
        let requestGeneration = generation
        settingsLoading = true
        slotError = nil
        defer { settingsLoading = false }
        do {
            let next = try await client.slotSettings(
                profile: profile,
                query: AppNativeSlotSettingsQuery(
                    assignmentLimit: 1,
                    assignmentCursor: nil,
                    pickerLimit: AppNativeSurfaceContract.maximumSlotPickerItems,
                    pickerCursor: cursor
                )
            )
            guard generation == requestGeneration, pickerRegion == region,
                  MagicianAccess.connectionProfile == profile else { return }
            let combined = previous.picker + next.picker
            guard next.head.revision == previous.head.revision,
                  next.head.fence > previous.head.fence,
                  next.inventoryRevision == previous.inventoryRevision,
                  combined.count <= AppNativeSurfaceContract.maximumAccumulatedPickerItems,
                  AppsDirectoryContract.hasUniqueValues(combined.map(\.targetKey)),
                  // A truncated answer that adds nothing, repeats a cursor, or
                  // reissues one already walked is a loop, not a page.
                  !next.pickerTruncated || (
                    !next.picker.isEmpty
                        && combined.count < AppNativeSurfaceContract.maximumAccumulatedPickerItems
                        && next.nextPickerCursor != cursor
                        && !(next.nextPickerCursor.map(pickerCursors.contains) ?? true)
                  ) else {
                throw AppNativeSurfaceContractError.invalidField("slot picker pagination")
            }
            if let nextCursor = next.nextPickerCursor { pickerCursors.insert(nextCursor) }
            settings = AppNativeSlotSettingsPage(
                head: next.head,
                inventoryRevision: next.inventoryRevision,
                assignments: previous.assignments,
                nextAssignmentCursor: previous.nextAssignmentCursor,
                assignmentsTruncated: previous.assignmentsTruncated,
                picker: combined,
                nextPickerCursor: next.nextPickerCursor,
                pickerTruncated: next.pickerTruncated
            )
        } catch {
            guard !Task.isCancelled, generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            // The failed read may still have advanced the server fence. Discard
            // this head rather than offering a predictably stale mutation.
            settings = nil
            slotError = "More widgets could not be loaded. Reopen the picker."
        }
    }

    func closePicker() {
        guard !applyingMutation else { return }
        pickerRegion = nil
        settings = nil
        settingsLoading = false
        pickerCursors = []
        if pendingMutation == nil { slotError = nil }
    }

    /// Order the picker so a widget that names this exact slot as a suggestion
    /// comes first; the rest stay in a stable title order.
    func orderedPickerCandidates() -> [AppNativeSlotPickerCandidate] {
        guard let settings, let region = pickerRegion,
              let slotID = assignments[region]?.slotID else { return [] }
        return settings.picker.enumerated().sorted { left, right in
            let leftSuggested = left.element.suggestedSlots.contains { $0.slotID == slotID }
            let rightSuggested = right.element.suggestedSlots.contains { $0.slotID == slotID }
            if leftSuggested != rightSuggested { return leftSuggested }
            if left.element.title != right.element.title {
                return left.element.title.localizedCaseInsensitiveCompare(right.element.title)
                    == .orderedAscending
            }
            return left.offset < right.offset
        }.map(\.element)
    }

    func assign(_ candidate: AppNativeSlotPickerCandidate) async {
        guard let settings, let region = pickerRegion, let assignment = assignments[region],
              pendingMutation == nil, !applyingMutation,
              let profile = MagicianAccess.connectionProfile, appliedProfile == profile else { return }
        guard let current = settings.picker.first(where: { $0.id == candidate.id }) else {
            slotError = "That widget is no longer in the current picker snapshot."
            return
        }
        guard settingsMatch(settings, assignment) else {
            self.settings = nil
            slotError = "The slot changed elsewhere. Reopen the picker and try again."
            await reload()
            return
        }
        let pending = PendingSlotMutation(
            profile: profile,
            region: region,
            request: AppNativeSlotAssignmentWriteRequest(
                expectedRevision: settings.head.revision,
                writeFence: settings.head.fence,
                mutationID: AppNativeSlotAssignmentWriteRequest.newMutationID(),
                command: .assign(
                    slotID: assignment.slotID,
                    installationID: current.widget.package.installationID,
                    widgetID: current.widget.widgetID,
                    expectedCandidate: AppNativeSlotWidgetBindingWire(current.widget)
                )
            )
        )
        await apply(pending)
    }

    func removeAssignment(in region: String) async {
        guard pendingMutation == nil, !applyingMutation, !settingsLoading,
              let assignment = assignments[region],
              assignment.widget != nil || assignment.source != nil || assignment.hiddenReason != nil,
              let profile = MagicianAccess.connectionProfile, appliedProfile == profile else { return }
        let requestGeneration = generation
        settingsLoading = true
        slotError = nil
        defer { settingsLoading = false }
        do {
            let head = try await settingsHead(for: assignment.slotID, pickerLimit: 1, profile: profile)
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            guard settingsMatch(head, assignment) else {
                settings = nil
                slotError = "The slot changed elsewhere. Refresh and try again."
                await reload()
                return
            }
            await apply(PendingSlotMutation(
                profile: profile,
                region: region,
                request: AppNativeSlotAssignmentWriteRequest(
                    expectedRevision: head.head.revision,
                    writeFence: head.head.fence,
                    mutationID: AppNativeSlotAssignmentWriteRequest.newMutationID(),
                    command: .optOut(slotID: assignment.slotID)
                )
            ))
        } catch {
            guard !Task.isCancelled, generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            slotError = "The slot change could not be prepared."
        }
    }

    func retryPendingMutation() async {
        guard let pending = pendingMutation else { return }
        await apply(pending)
    }

    /// Apply one write. A definite client refusal (4xx that is not a retry
    /// hint) clears the pending write, because replaying it can only refuse
    /// again; anything else keeps it so the SAME idempotent write is retried
    /// rather than a second, differently-fenced one.
    private func apply(_ pending: PendingSlotMutation) async {
        guard !applyingMutation, MagicianAccess.connectionProfile == pending.profile,
              appliedProfile == pending.profile else { return }
        let requestGeneration = generation
        pendingMutation = pending
        hasPendingMutation = true
        applyingMutation = true
        slotError = nil
        // Entry is guarded on `!applyingMutation`, so exactly one write is ever
        // in flight and this release is unconditional.
        defer { applyingMutation = false }
        do {
            _ = try await client.mutateSlotAssignment(
                profile: pending.profile,
                request: pending.request
            )
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == pending.profile else { return }
            pendingMutation = nil
            hasPendingMutation = false
            pickerRegion = nil
            settings = nil
            pickerCursors = []
            await reload()
        } catch {
            guard !Task.isCancelled, generation == requestGeneration,
                  MagicianAccess.connectionProfile == pending.profile else { return }
            let status = (error as? AppNativeHTTPStatusError)?.statusCode
            if let status, (400..<500).contains(status),
               status != 408, status != 425, status != 429 {
                pendingMutation = nil
                hasPendingMutation = false
                settings = nil
                slotError = "The slot changed elsewhere. Reopen the picker and try again."
            } else {
                slotError = "The slot change could not be confirmed. Retry the same change safely."
            }
        }
    }

    private func hideProfileBoundState() {
        placements = []
        assignments = [:]
        cachedBatch = nil
        confirmedAt = nil
        etag = nil
        targetKey = nil
        appliedProfile = nil
        refreshAfter = nil
        actionNotice = nil
        actionIdempotency.reset()
        resetSlotControls()
    }
}

@MainActor
final class AppNativeIndicatorsViewModel: ObservableObject {
    @Published private(set) var indicators: [AppNativeIndicator] = []

    private let client: AppNativeSurfaceClient
    private var etag: String?
    private var appliedProfile: MobileConnectionProfile?
    private var generation: UInt64 = 0
    private var cachedIndicators: [AppNativeIndicator] = []
    private var foregroundActive = false
    private var cadenceTask: Task<Void, Never>?

    init(client: AppNativeSurfaceClient = AppNativeSurfaceClient()) { self.client = client }

    func invalidate() {
        generation &+= 1
        cadenceTask?.cancel()
        cadenceTask = nil
        indicators = []
        cachedIndicators = []
        etag = nil
        appliedProfile = nil
    }

    func setForegroundActive(_ active: Bool) {
        guard foregroundActive != active else { return }
        foregroundActive = active
        if active {
            scheduleCadence()
        } else {
            generation &+= 1
            cadenceTask?.cancel()
            cadenceTask = nil
            indicators = []
        }
    }

    func reload() async {
        generation &+= 1
        cadenceTask?.cancel()
        cadenceTask = nil
        let requestGeneration = generation
        indicators = indicators.filter(\.isUnexpired)
        guard let profile = MagicianAccess.connectionProfile else {
            invalidate()
            return
        }
        if appliedProfile != profile {
            // An ETag is scoped response authority, not a global content key.
            // Never revalidate or display one profile's indicator body in a
            // replacement profile.
            indicators = []
            cachedIndicators = []
            etag = nil
            appliedProfile = nil
        }
        let requestETag = etag
        do {
            let result = try await client.indicators(
                profile: profile,
                limit: AppNativeSurfaceContract.maximumIndicators,
                etag: requestETag
            )
            guard generation == requestGeneration, MagicianAccess.connectionProfile == profile else { return }
            switch result {
            case .notModified(let responseETag, _):
                guard requestETag != nil, appliedProfile == profile,
                      !cachedIndicators.isEmpty || indicators.isEmpty else {
                    throw AppNativeSurfaceContractError.missingCachedRepresentation
                }
                etag = responseETag
                indicators = cachedIndicators.filter(\.isUnexpired)
            case .modified(let response, let responseETag):
                cachedIndicators = response.indicators
                indicators = cachedIndicators.filter(\.isUnexpired)
                etag = responseETag
                appliedProfile = profile
            }
            scheduleCadence()
        } catch {
            guard !Task.isCancelled, generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            // Ambient indicators fail-hide. They are never shown stale.
            indicators = []
            cachedIndicators = []
            etag = nil
            appliedProfile = nil
            scheduleCadence()
        }
    }

    private func scheduleCadence() {
        cadenceTask?.cancel()
        cadenceTask = nil
        guard foregroundActive else { return }
        let earliestExpiry = cachedIndicators.compactMap {
            AppNativeSurfaceContract.date($0.expiresAt)
        }.min()
        let delay = min(max(earliestExpiry?.timeIntervalSinceNow ?? 60, 1), 60)
        cadenceTask = Task { [weak self] in
            do {
                try await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            } catch {
                return
            }
            guard !Task.isCancelled, let self, self.foregroundActive else { return }
            self.cadenceTask = nil
            await self.reload()
        }
    }
}

/// One page's slot region: every region on the page rendered from ONE shared
/// slot/render batch, with the bounded picker and removal flow the web client
/// exposes. Mirrors `AppSlotRegion.svelte` rather than forking an iOS dialect —
/// the render model is platform-neutral by design.
struct AppNativeSlotPageRegion: View {
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var theme = ThemeManager.shared
    @StateObject private var model: AppNativeSlotPageViewModel
    /// One page-bounded mini-frame lease, shared by every slot on the page: a
    /// per-view budget is not a budget. It is the page's claim on the
    /// app-session ledger, so leaving the page returns its visible slots even
    /// if an individual frame's teardown was skipped.
    @State private var frameLease: AppMiniFramePageLease?
    private let accessibilityLabel: String
    /// Applied INSIDE the populated branch. A page fits this region into a
    /// container that may already be padded, and padding an empty region from
    /// outside would reserve height on every page that resolves no slots.
    private let contentInsets: EdgeInsets

    init(
        page: String,
        regions: [String],
        accessibilityLabel: String = "App widgets",
        contentInsets: EdgeInsets = EdgeInsets()
    ) {
        _model = StateObject(wrappedValue: AppNativeSlotPageViewModel(page: page, regions: regions))
        self.accessibilityLabel = accessibilityLabel
        self.contentInsets = contentInsets
    }

    var body: some View {
        // A Group distributes lifecycle modifiers to its children. There are
        // none before the first response, so its task never starts on a cold
        // launch. Keep a concrete host even while this optional region is empty.
        VStack(alignment: .leading, spacing: 0) {
            if !model.placements.isEmpty {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(model.placements) { placement in slot(placement) }
                    if model.pickerRegion != nil { picker }
                    if let slotError = model.slotError { errorBanner(slotError) }
                }
                .padding(contentInsets)
                .accessibilityElement(children: .contain)
                .accessibilityLabel(accessibilityLabel)
                .accessibilityIdentifier("app-slot-region-\(model.page)")
            }
        }
        .task {
            model.setForegroundActive(scenePhase == .active)
            if scenePhase == .active { await model.reload() }
        }
        .onChange(of: scenePhase) { _, phase in
            model.setForegroundActive(phase == .active)
            if phase == .active { Task { await model.reload() } }
        }
        .onAppear { if frameLease == nil { frameLease = AppMiniFrameSessionLedger.shared.openPage() } }
        .onDisappear {
            model.setForegroundActive(false)
            // Leaving the page ends its claim: the budget belongs to what is on
            // screen now, not to what was.
            frameLease?.close()
            frameLease = nil
        }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            model.invalidate()
            // A scope change invalidates every frame this page admitted.
            frameLease?.close()
            frameLease = AppMiniFrameSessionLedger.shared.openPage()
            if scenePhase == .active { Task { await model.reload() } }
        }
    }

    @ViewBuilder
    private func slot(_ placement: AppNativeSlotPlacement) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            switch placement.presentation {
            case .card:
                if let item = placement.item {
                    card(item, in: placement.region, authorityKey: placement.authorityKey)
                    removeButton(placement.region)
                }
            case .unavailable:
                // Retained, not erased: an assignment the user made, or one
                // whose package is disabled or mid-update, comes back when the
                // package does and must stay removable meanwhile.
                unavailablePlaceholder
                removeButton(placement.region)
            case .add:
                // Empty, or a workspace default the server hid: not the
                // user's broken widget, so offer the slot instead.
                addButton(placement.region)
            }
        }
        .accessibilityIdentifier("app-slot-\(placement.region)")
    }

    private func card(
        _ item: AppNativeWidgetItem,
        in region: String,
        authorityKey: String
    ) -> some View {
        AppNativeWidgetCard(
            item: item,
            actionNotice: model.actionNotice,
            launchingAction: model.launchingAction,
            miniFrameLease: frameLease,
            authorityKey: authorityKey,
            launch: { action in Task { await model.launch(action, in: region) } }
        )
    }

    private var unavailablePlaceholder: some View {
        Text("This assigned widget is temporarily unavailable.")
            .font(.themed(12))
            .foregroundColor(theme.secondaryTextColor)
            .frame(maxWidth: .infinity, minHeight: 74)
            .padding(14)
            .background(theme.surfaceColor.opacity(0.82))
            .clipShape(RoundedRectangle(cornerRadius: 17, style: .continuous))
            .overlay { RoundedRectangle(cornerRadius: 17, style: .continuous).stroke(theme.cardBorderColor) }
    }

    private func addButton(_ region: String) -> some View {
        Button {
            Task { await model.openPicker(region) }
        } label: {
            Label("Add a widget", systemImage: "plus")
                .font(.themed(13))
                .foregroundColor(theme.secondaryTextColor)
                .frame(maxWidth: .infinity, minHeight: 74)
        }
        .buttonStyle(.plain)
        .background(theme.surfaceColor.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: 17, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 17, style: .continuous)
                .strokeBorder(theme.cardBorderColor, style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
        }
        .disabled(model.applyingMutation || model.hasPendingMutation || model.settingsLoading)
        .accessibilityIdentifier("app-slot-add-\(region)")
    }

    private func removeButton(_ region: String) -> some View {
        Button("Remove") { Task { await model.removeAssignment(in: region) } }
            .font(.themed(11))
            .buttonStyle(.bordered)
            .disabled(model.applyingMutation || model.hasPendingMutation || model.settingsLoading)
            .accessibilityIdentifier("app-slot-remove-\(region)")
    }

    private var picker: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Choose a widget")
                    .font(.themed(14, weight: .semibold))
                    .foregroundColor(theme.textColor)
                Spacer()
                Button("Close") { model.closePicker() }
                    .font(.themed(12))
                    .disabled(model.applyingMutation)
            }
            if model.settingsLoading && model.settings == nil {
                ProgressView().controlSize(.small)
            } else if model.settings != nil {
                let candidates = model.orderedPickerCandidates()
                if candidates.isEmpty {
                    Text("No installed native widgets are available.")
                        .font(.themed(12))
                        .foregroundColor(theme.secondaryTextColor)
                } else {
                    ForEach(candidates) { candidate in
                        Button {
                            Task { await model.assign(candidate) }
                        } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(candidate.title)
                                    .font(.themed(13, weight: .semibold))
                                    .foregroundColor(theme.textColor)
                                Text(candidate.widget.package.installationID)
                                    .font(.themed(10))
                                    .foregroundColor(theme.secondaryTextColor)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .buttonStyle(.plain)
                        .padding(10)
                        .background(theme.elevatedColor)
                        .clipShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
                        .disabled(model.applyingMutation || model.hasPendingMutation)
                    }
                    if model.settings?.pickerTruncated == true {
                        Button("Load more widgets") {
                            Task { await model.loadMorePickerCandidates() }
                        }
                        .font(.themed(12))
                        .disabled(model.settingsLoading || model.applyingMutation || model.hasPendingMutation)
                    }
                }
            }
        }
        .padding(14)
        .background(theme.surfaceColor.opacity(0.82))
        .clipShape(RoundedRectangle(cornerRadius: 17, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 17, style: .continuous).stroke(theme.cardBorderColor) }
        .accessibilityIdentifier("app-slot-picker")
    }

    private func errorBanner(_ message: String) -> some View {
        HStack(spacing: 10) {
            Text(message)
                .font(.themed(11))
                .foregroundColor(theme.secondaryTextColor)
            Spacer(minLength: 8)
            // The retry replays the SAME fenced, idempotent write; it is only
            // offered while that write's outcome is still unknown.
            if model.hasPendingMutation {
                Button("Retry change") { Task { await model.retryPendingMutation() } }
                    .font(.themed(11))
                    .disabled(model.applyingMutation)
            }
        }
        .padding(12)
        .background(theme.elevatedColor)
        .clipShape(RoundedRectangle(cornerRadius: 13, style: .continuous))
        .accessibilityIdentifier("app-slot-error")
    }
}

struct AppNativeIndicatorStrip: View {
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var theme = ThemeManager.shared
    @StateObject private var model = AppNativeIndicatorsViewModel()

    var body: some View {
        Group {
            if !model.indicators.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    LazyHStack(spacing: 8) {
                        ForEach(model.indicators) { indicator in
                            HStack(spacing: 6) {
                                Text(indicator.title)
                                    .foregroundColor(theme.secondaryTextColor)
                                Text(indicator.model.text)
                                    .fontWeight(.semibold)
                                    .foregroundColor(theme.textColor)
                            }
                            .font(.themed(11))
                            .lineLimit(1)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 7)
                            .background(theme.elevatedColor)
                            .clipShape(Capsule())
                            .overlay { Capsule().stroke(theme.cardBorderColor) }
                            .accessibilityLabel("\(indicator.title), \(indicator.model.text)")
                        }
                    }
                    .padding(.horizontal, 16)
                }
                // A horizontal ScrollView otherwise accepts the full proposed
                // height of the root safe-area inset. Its invisible scroll
                // surface can then cover the tab and intercept page touches.
                .frame(height: 32)
                // The insets live INSIDE the populated branch on purpose: this
                // strip is a shell region mounted for the app's whole life, and
                // padding applied outside the condition would reserve height on
                // every page that has no indicators at all.
                .padding(.vertical, 4)
                .accessibilityIdentifier("app-native-indicators")
            }
        }
        .task {
            model.setForegroundActive(scenePhase == .active)
            if scenePhase == .active { await model.reload() }
        }
        .onChange(of: scenePhase) { _, phase in
            model.setForegroundActive(phase == .active)
            if phase == .active { Task { await model.reload() } }
        }
        .onDisappear { model.setForegroundActive(false) }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            model.invalidate()
            if scenePhase == .active { Task { await model.reload() } }
        }
    }
}

private struct AppNativeWidgetCard: View {
    @ObservedObject private var theme = ThemeManager.shared
    let item: AppNativeWidgetItem
    let actionNotice: String?
    let launchingAction: Bool
    /// The owning page's mini-frame budget. Absent on a surface that opened
    /// none, which is the same as having no budget: the frame is a
    /// page-bounded escalation, so a widget outside a page lease renders
    /// native.
    let miniFrameLease: AppMiniFramePageLease?
    let authorityKey: String
    let launch: (AppNativeGovernedAction) -> Void

    /// The frame target this rendered widget declares, if the page can host it.
    private var miniFrameTarget: AppMiniFrameTarget? {
        guard miniFrameLease != nil else { return nil }
        return AppMiniFrameTarget(item: item)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let title = item.title {
                Text(title)
                    .font(.themed(19, weight: .bold))
                    .foregroundColor(theme.textColor)
            }
            switch item.state {
            case .ready(let nativeModel):
                // An admitted frame owns the BODY; the title above and the
                // governed-action buttons below stay host-rendered, so a
                // control the owner reviewed is never inside the frame. Until
                // the frame is admitted and minted — and after any refusal —
                // the native model is what renders.
                if let target = miniFrameTarget, let lease = miniFrameLease {
                    AppMiniFrameHostView(
                        target: target,
                        lease: lease,
                        authorityKey: authorityKey
                    ) {
                        AppNativeWidgetModelView(model: nativeModel)
                    }
                } else {
                    AppNativeWidgetModelView(model: nativeModel)
                }
                if !nativeModel.actions.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(nativeModel.actions) { action in
                                Button(action.label) { launch(action) }
                                    .buttonStyle(.bordered)
                                    .disabled(launchingAction)
                                    .accessibilityIdentifier("app-widget-action-\(action.actionID)")
                            }
                        }
                    }
                }
            case .unsupported(.message(let title, let body)):
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).font(.themed(14, weight: .semibold))
                    Text(body).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                }
            case .unsupported(.hide), .unavailable:
                EmptyView()
            }
            if let actionNotice {
                Text(actionNotice)
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(2)
            }
        }
        .padding(14)
        .background(theme.surfaceColor.opacity(0.82))
        .clipShape(RoundedRectangle(cornerRadius: 17, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 17, style: .continuous).stroke(theme.cardBorderColor) }
        .accessibilityIdentifier("app-native-widget-\(item.id)")
    }
}

private struct AppNativeWidgetModelView: View {
    let model: AppNativeWidgetModel

    @ViewBuilder
    var body: some View {
        switch model {
        case .detail(let row, let hints, _):
            if let row { AppNativeDetailView(row: row, hints: hints) }
            else { Text("No current item").foregroundStyle(.secondary) }
        case .list(let rows, let hints, _):
            AppNativeListView(rows: rows, hints: hints)
        case .table(let columns, let rows, _, _):
            AppNativeTableView(columns: columns, rows: rows)
        case .timeline(let rows, let hints, _):
            AppNativeTimelineView(rows: rows, hints: hints)
        case .tree(let rows, let hints, _):
            AppNativeTreeView(rows: rows, hints: hints)
        case .graph(let rows, let hints, _):
            AppNativeGraphView(rows: rows, hints: hints)
        }
    }
}

private enum AppNativeRowPresentation {
    static func title(_ row: AppNativeWidgetRow, hints: AppNativeWidgetHints) -> String {
        hints.displayField.flatMap { row.fields[$0]?.displayText } ?? row.recordID
    }

    static func secondary(_ row: AppNativeWidgetRow, hints: AppNativeWidgetHints) -> String? {
        let candidates = [hints.statusField, hints.partitionField, hints.actorField, hints.typeField]
        return candidates.compactMap { $0 }.compactMap { row.fields[$0]?.displayText }.first
    }
}

private struct AppNativeDetailView: View {
    let row: AppNativeWidgetRow
    let hints: AppNativeWidgetHints

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(AppNativeRowPresentation.title(row, hints: hints)).font(.headline)
            ForEach(row.fields.keys.sorted(), id: \.self) { field in
                HStack(alignment: .top) {
                    Text(field.replacingOccurrences(of: "_", with: " ").capitalized)
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 16)
                    Text(row.fields[field]?.displayText ?? "—")
                        .multilineTextAlignment(.trailing)
                }
                .font(.caption)
            }
        }
    }
}

private struct AppNativeListView: View {
    let rows: [AppNativeWidgetRow]
    let hints: AppNativeWidgetHints

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(rows) { row in
                VStack(alignment: .leading, spacing: 3) {
                    Text(AppNativeRowPresentation.title(row, hints: hints)).font(.subheadline.weight(.semibold))
                    if let secondary = AppNativeRowPresentation.secondary(row, hints: hints) {
                        Text(secondary).font(.caption).foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 8)
                if row.id != rows.last?.id { Divider() }
            }
        }
    }
}

private struct AppNativeTableView: View {
    let columns: [String]
    let rows: [AppNativeWidgetRow]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: true) {
            LazyHStack(alignment: .top, spacing: 0) {
                ForEach(columns, id: \.self) { column in
                    VStack(alignment: .leading, spacing: 0) {
                        Text(column.replacingOccurrences(of: "_", with: " ").capitalized)
                            .font(.caption.weight(.semibold))
                            .frame(width: 132, alignment: .leading)
                            .padding(.vertical, 7)
                        Divider()
                        ForEach(rows) { row in
                            Text(row.fields[column]?.displayText ?? "—")
                                .font(.caption)
                                .lineLimit(2)
                                .frame(width: 132, alignment: .leading)
                                .padding(.vertical, 7)
                            if row.id != rows.last?.id { Divider() }
                        }
                    }
                }
            }
        }
    }
}

private struct AppNativeTimelineView: View {
    let rows: [AppNativeWidgetRow]
    let hints: AppNativeWidgetHints

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(rows) { row in
                HStack(alignment: .top, spacing: 10) {
                    VStack(spacing: 0) {
                        Circle().fill(Color.accentColor).frame(width: 8, height: 8)
                        Rectangle().fill(Color.secondary.opacity(0.25)).frame(width: 1, height: 38)
                    }
                    VStack(alignment: .leading, spacing: 3) {
                        Text(AppNativeRowPresentation.title(row, hints: hints)).font(.subheadline.weight(.semibold))
                        if let field = hints.timestampField, let timestamp = row.fields[field]?.displayText {
                            Text(timestamp).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .padding(.bottom, 8)
                }
            }
        }
    }
}

private struct AppNativeTreeView: View {
    let rows: [AppNativeWidgetRow]
    let hints: AppNativeWidgetHints

    var body: some View {
        let depths = Self.depths(rows: rows, parentField: hints.parentField)
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(rows) { row in
                HStack(spacing: 6) {
                    Image(systemName: "circle.fill")
                        .font(.system(size: 5))
                        .foregroundStyle(.secondary)
                    Text(AppNativeRowPresentation.title(row, hints: hints)).font(.subheadline)
                }
                .padding(.leading, CGFloat(depths[row.recordID, default: 0]) * 16)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    static func depths(rows: [AppNativeWidgetRow], parentField: String?) -> [String: Int] {
        guard let parentField else { return [:] }
        let byID = Dictionary(uniqueKeysWithValues: rows.map { ($0.recordID, $0) })
        var result: [String: Int] = [:]
        for row in rows {
            var cursor = row
            var visited: Set<String> = [row.recordID]
            var depth = 0
            while depth < rows.count,
                  let parentID = cursor.fields[parentField]?.referenceText,
                  let parent = byID[parentID], visited.insert(parentID).inserted {
                depth += 1
                cursor = parent
            }
            result[row.recordID] = depth
        }
        return result
    }
}

private struct AppNativeGraphView: View {
    let rows: [AppNativeWidgetRow]
    let hints: AppNativeWidgetHints

    var body: some View {
        GeometryReader { geometry in
            Canvas { context, size in
                let positions = Self.positions(count: rows.count, size: size)
                let indexByID = Dictionary(uniqueKeysWithValues: rows.enumerated().map { ($0.element.recordID, $0.offset) })
                let edgeField = hints.targetField ?? hints.parentField
                if let edgeField {
                    for (index, row) in rows.enumerated() {
                        guard let targetID = row.fields[edgeField]?.referenceText,
                              let targetIndex = indexByID[targetID] else { continue }
                        var path = Path()
                        path.move(to: positions[index])
                        path.addLine(to: positions[targetIndex])
                        context.stroke(path, with: .color(Color.secondary.opacity(0.35)), lineWidth: 1)
                    }
                }
                for (index, row) in rows.enumerated() {
                    let center = positions[index]
                    let circle = CGRect(x: center.x - 8, y: center.y - 8, width: 16, height: 16)
                    context.fill(Path(ellipseIn: circle), with: .color(Color.accentColor))
                    let label = context.resolve(
                        Text(AppNativeRowPresentation.title(row, hints: hints))
                            .font(.system(size: 9))
                            .foregroundColor(.primary)
                    )
                    context.draw(label, at: CGPoint(x: center.x, y: center.y + 18))
                }
            }
            .frame(width: geometry.size.width, height: geometry.size.height)
        }
        .frame(minHeight: 180, maxHeight: 240)
    }

    static func positions(count: Int, size: CGSize) -> [CGPoint] {
        guard count > 0 else { return [] }
        let columns = max(1, Int(ceil(sqrt(Double(count)))))
        let rows = max(1, Int(ceil(Double(count) / Double(columns))))
        return (0..<count).map { index in
            let column = index % columns
            let row = index / columns
            return CGPoint(
                x: size.width * CGFloat(column + 1) / CGFloat(columns + 1),
                y: size.height * CGFloat(row + 1) / CGFloat(rows + 1)
            )
        }
    }
}

struct PinnedAppViewProjection: Identifiable, Equatable {
    let installationID: String
    let appName: String
    let icon: AppsDirectoryIcon
    let viewID: String
    let label: String
    let route: String

    var id: String { "\(installationID):\(viewID)" }
}

enum PinnedAppsTodayProjection {
    static let maximumItems = 8

    /// Projects only explicitly pinned, launchable app views. Today does not
    /// retain directory entries, app actions, storage summaries, or cursors.
    static func make(
        entries: [AppsDirectoryEntry],
        limit: Int = maximumItems
    ) -> [PinnedAppViewProjection] {
        guard limit > 0 else { return [] }
        var result: [PinnedAppViewProjection] = []
        result.reserveCapacity(min(limit, maximumItems))
        let boundedLimit = min(limit, maximumItems)

        for entry in entries where entry.status.canLaunch {
            for view in entry.views where view.pinned {
                result.append(PinnedAppViewProjection(
                    installationID: entry.installationID,
                    appName: entry.name,
                    icon: entry.icon,
                    viewID: view.viewID,
                    label: view.label,
                    route: view.route
                ))
                if result.count == boundedLimit { return result }
            }
        }
        return result
    }
}

@MainActor
final class PinnedAppsTodayViewModel: ObservableObject {
    @Published private(set) var views: [PinnedAppViewProjection] = []

    private let client: AppsDirectoryClient
    private var generation: UInt64 = 0

    init(client: AppsDirectoryClient = AppsDirectoryClient()) {
        self.client = client
    }

    /// Clears synchronously so an old self-hosted origin/scope cannot remain
    /// launchable while the replacement profile is loading.
    func invalidate() {
        generation &+= 1
        views = []
    }

    func reload() async {
        generation &+= 1
        let requestGeneration = generation
        guard let profile = MagicianAccess.connectionProfile else {
            views = []
            return
        }

        do {
            // This is one bounded metadata projection, not ownership of the
            // Apps catalog. A full server page avoids treating pinned actions
            // as pinned views while the UI still retains at most eight rows.
            let page = try await client.fetch(
                profile: profile,
                section: .pinned,
                search: "",
                limit: PinnedAppsTodayProjection.maximumItems,
                cursor: nil,
                pinnedTargetKind: .view
            )
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            views = PinnedAppsTodayProjection.make(entries: page.entries)
        } catch {
            guard !Task.isCancelled,
                  generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            // Today is an optional projection. The Apps directory remains the
            // diagnostic surface, so an unavailable projection disappears.
            views = []
        }
    }

    func recordLaunch(_ view: PinnedAppViewProjection, profile: MobileConnectionProfile) async {
        try? await client.recordLaunch(
            profile: profile,
            installationID: view.installationID,
            viewID: view.viewID
        )
    }
}

@MainActor
final class AppsLauncherViewModel: ObservableObject {
    @Published private(set) var entries: [AppsDirectoryEntry] = []
    @Published private(set) var loading = false
    @Published private(set) var loadingMore = false
    @Published private(set) var hasMore = false
    @Published var errorMessage: String?

    private let client: AppsDirectoryClient
    private var nextCursor: String?
    private var generation: UInt64 = 0

    init(client: AppsDirectoryClient = AppsDirectoryClient()) {
        self.client = client
    }

    /// Remove old-scope metadata synchronously before a newly paired runtime
    /// can begin loading. In-flight responses are ignored by generation and
    /// exact-profile checks below.
    func invalidate() {
        generation &+= 1
        entries = []
        nextCursor = nil
        hasMore = false
        loading = false
        loadingMore = false
        errorMessage = nil
    }

    func reload(section: AppsDirectorySection, search: String, clearCurrent: Bool) async {
        generation &+= 1
        let requestGeneration = generation
        loading = true
        loadingMore = false
        errorMessage = nil
        if clearCurrent {
            entries = []
            nextCursor = nil
            hasMore = false
        }
        defer { if generation == requestGeneration { loading = false } }
        guard let profile = MagicianAccess.connectionProfile else {
            if generation == requestGeneration {
                entries = []
                nextCursor = nil
                hasMore = false
                errorMessage = "Connect this iPhone to Magican in Settings first."
            }
            return
        }
        do {
            let page = try await client.fetch(
                profile: profile, section: section, search: search, limit: 25, cursor: nil
            )
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            entries = page.entries
            nextCursor = page.nextCursor
            hasMore = page.hasMore
        } catch {
            guard !Task.isCancelled else { return }
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            if clearCurrent { entries = [] }
            nextCursor = nil
            hasMore = false
            errorMessage = error.localizedDescription
        }
    }

    func loadMore(section: AppsDirectorySection, search: String) async {
        guard hasMore, let cursor = nextCursor, !loading, !loadingMore,
              let profile = MagicianAccess.connectionProfile else { return }
        let requestGeneration = generation
        loadingMore = true
        errorMessage = nil
        defer { if generation == requestGeneration { loadingMore = false } }
        do {
            let page = try await client.fetch(
                profile: profile, section: section, search: search, limit: 25, cursor: cursor
            )
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            let merged = try AppsDirectoryPagination.merge(
                existing: entries, page: page, requestedCursor: cursor
            )
            entries = merged.entries
            nextCursor = merged.nextCursor
            hasMore = merged.hasMore
        } catch {
            guard !Task.isCancelled else { return }
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            if (error as? AppsDirectoryContractError) == .invalidField("pagination did not advance") {
                hasMore = false
                nextCursor = nil
                errorMessage = "The Apps directory could not advance to the next page."
            } else {
                errorMessage = error.localizedDescription
            }
        }
    }

    func recordLaunch(_ entry: AppsDirectoryEntry, view: AppsDirectoryView, profile: MobileConnectionProfile) async {
        try? await client.recordLaunch(
            profile: profile, installationID: entry.installationID, viewID: view.viewID
        )
    }

    func setPin(
        _ pinned: Bool,
        entry: AppsDirectoryEntry,
        view: AppsDirectoryView,
        section: AppsDirectorySection
    ) async {
        guard let profile = MagicianAccess.connectionProfile else { return }
        let requestGeneration = generation
        do {
            try await client.setPin(
                profile: profile,
                installationID: entry.installationID,
                viewID: view.viewID,
                pinned: pinned
            )
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            if let index = entries.firstIndex(where: { $0.installationID == entry.installationID }) {
                let updated = entries[index].settingPin(viewID: view.viewID, pinned: pinned)
                if section == .pinned, !updated.hasPinnedTarget {
                    entries.remove(at: index)
                } else {
                    entries[index] = updated
                }
            }
            NotificationCenter.default.post(name: .magicianAppsDirectoryPinsDidChange, object: nil)
        } catch {
            guard generation == requestGeneration,
                  MagicianAccess.connectionProfile == profile else { return }
            errorMessage = "The app pin could not be updated."
        }
    }
}

private struct SelectedAppRoute: Identifiable {
    let url: URL
    let context: AppRouteContext
    let title: String
    var id: String { "\(context.profile.deviceID):\(url.absoluteString)" }
}

struct PinnedAppsTodaySection: View {
    @ObservedObject private var theme = ThemeManager.shared
    @StateObject private var model = PinnedAppsTodayViewModel()
    @State private var openRoute: SelectedAppRoute?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !model.views.isEmpty {
                VStack(alignment: .leading, spacing: 10) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Pinned apps")
                            .font(.themed(19, weight: .bold))
                            .foregroundColor(theme.textColor)
                        Text("The app views you chose to keep close.")
                            .font(.themed(12))
                            .foregroundColor(theme.secondaryTextColor)
                    }

                    ScrollView(.horizontal, showsIndicators: false) {
                        LazyHStack(spacing: 10) {
                            ForEach(model.views) { view in
                                Button { open(view) } label: {
                                    VStack(alignment: .leading, spacing: 9) {
                                        Text(view.icon.value)
                                            .font(.themed(12, weight: .bold))
                                            .foregroundColor(theme.accentColor)
                                            .frame(width: 34, height: 34)
                                            .background(theme.accentColor.opacity(0.13))
                                            .clipShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
                                        Text(view.label)
                                            .font(.themed(14, weight: .semibold))
                                            .foregroundColor(theme.textColor)
                                            .lineLimit(2)
                                        Text(view.appName)
                                            .font(.themed(11))
                                            .foregroundColor(theme.secondaryTextColor)
                                            .lineLimit(1)
                                    }
                                    .frame(width: 142, height: 112, alignment: .topLeading)
                                    .padding(12)
                                    .background(theme.elevatedColor)
                                    .clipShape(RoundedRectangle(cornerRadius: 13, style: .continuous))
                                    .overlay {
                                        RoundedRectangle(cornerRadius: 13, style: .continuous)
                                            .stroke(theme.cardBorderColor)
                                    }
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Open \(view.label) in \(view.appName)")
                                .accessibilityIdentifier("today-pinned-app-\(view.id)")
                            }
                        }
                    }
                }
                .padding(14)
                .background(theme.surfaceColor.opacity(0.82))
                .clipShape(RoundedRectangle(cornerRadius: 17, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 17, style: .continuous)
                        .stroke(theme.cardBorderColor)
                }
            }
        }
        .task { await model.reload() }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            openRoute = nil
            model.invalidate()
            Task { await model.reload() }
        }
        .onReceive(NotificationCenter.default.publisher(for: .magicianAppsDirectoryPinsDidChange)) { _ in
            model.invalidate()
            Task { await model.reload() }
        }
        .fullScreenCover(item: $openRoute) { route in
            AppRouteBrowserView(selection: route)
        }
    }

    private func open(_ view: PinnedAppViewProjection) {
        guard let profile = MagicianAccess.connectionProfile,
              let url = AppRoutePolicy.routeURL(
                view.route,
                installationID: view.installationID,
                origin: profile.publicOrigin
              ) else { return }
        let context = AppRouteContext(profile: profile, installationID: view.installationID)
        openRoute = SelectedAppRoute(url: url, context: context, title: view.appName)
        Task { await model.recordLaunch(view, profile: profile) }
    }
}

struct AppsLauncherView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var theme = ThemeManager.shared
    @StateObject private var model = AppsLauncherViewModel()
    @State private var section = AppsDirectorySection.installed
    @State private var searchText = ""
    @State private var openRoute: SelectedAppRoute?

    private var queryIdentity: String { "\(section.rawValue):\(searchText)" }

    var body: some View {
        NavigationStack {
            Group {
                if model.loading && model.entries.isEmpty {
                    ProgressView("Loading apps…")
                } else if model.entries.isEmpty {
                    ContentUnavailableView(
                        model.errorMessage == nil ? "No apps here" : "Apps unavailable",
                        systemImage: model.errorMessage == nil ? "square.grid.2x2" : "exclamationmark.triangle",
                        description: Text(model.errorMessage ?? emptyDescription)
                    )
                } else {
                    List {
                        if let error = model.errorMessage {
                            Text(error)
                                .font(.footnote)
                                .foregroundColor(theme.dangerColor)
                        }
                        ForEach(model.entries) { entry in
                            Section {
                                if entry.views.isEmpty {
                                    VStack(alignment: .leading, spacing: 6) {
                                        Text("This app does not publish an iPhone view.")
                                            .font(.footnote)
                                            .foregroundColor(theme.secondaryTextColor)
                                        // Reachability for custom-surface-only
                                        // packages (1.6): the installation
                                        // root is the one launcher address a
                                        // scripted surface can answer; the
                                        // browser view probes the host
                                        // endpoint there and mounts the
                                        // native host when a plan is minted.
                                        // The button appears only when the
                                        // directory advertises declared
                                        // custom-surface entry points, so a
                                        // package with nothing hostable
                                        // renders exactly what it did before
                                        // instead of dead-ending in the
                                        // WebView's generic error.
                                        if entry.status.canLaunch && entry.customSurfaceEntryCount > 0 {
                                            Button {
                                                openSurfaceRoot(entry)
                                            } label: {
                                                Label("Open app", systemImage: "arrow.up.forward.app")
                                                    .font(.footnote.weight(.medium))
                                            }
                                        }
                                    }
                                }
                                ForEach(entry.views) { view in
                                    Button {
                                        open(entry, view: view)
                                    } label: {
                                        HStack {
                                            Text(view.label)
                                            Spacer()
                                            if view.pinned {
                                                Image(systemName: "star.fill")
                                                    .foregroundColor(theme.accentColor)
                                            }
                                            Image(systemName: "chevron.right")
                                                .font(.caption)
                                                .foregroundColor(theme.secondaryTextColor)
                                        }
                                    }
                                    .disabled(!entry.status.canLaunch)
                                    .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                        if entry.status.canLaunch {
                                            Button {
                                                Task {
                                                    await model.setPin(
                                                        !view.pinned,
                                                        entry: entry,
                                                        view: view,
                                                        section: section
                                                    )
                                                }
                                            } label: {
                                                Label(
                                                    view.pinned ? "Unpin" : "Pin",
                                                    systemImage: view.pinned ? "star.slash" : "star"
                                                )
                                            }
                                            .tint(theme.accentColor)
                                        }
                                    }
                                }
                            } header: {
                                HStack(spacing: 10) {
                                    Text(entry.icon.value)
                                        .font(.caption.bold())
                                        .frame(width: 30, height: 30)
                                        .background(theme.accentColor.opacity(0.15))
                                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                                    VStack(alignment: .leading, spacing: 3) {
                                        Text(entry.name)
                                        Text("v\(entry.packageVersion) · \(entry.status.label)")
                                            .font(.caption2)
                                            .foregroundColor(theme.secondaryTextColor)
                                    }
                                }
                                .textCase(nil)
                            } footer: {
                                VStack(alignment: .leading, spacing: 3) {
                                    if !entry.description.isEmpty { Text(entry.description) }
                                    if let reason = entry.attentionReason {
                                        Text(reason).foregroundColor(.orange)
                                    }
                                }
                            }
                        }
                        if model.hasMore {
                            Button(model.loadingMore ? "Loading…" : "Load more") {
                                Task { await model.loadMore(section: section, search: searchText) }
                            }
                            .disabled(model.loadingMore)
                        }
                    }
                    .refreshable {
                        await model.reload(section: section, search: searchText, clearCurrent: false)
                    }
                }
            }
            .navigationTitle("Apps")
            .searchable(text: $searchText, prompt: "Search app names and descriptions")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .principal) {
                    Picker("Section", selection: $section) {
                        ForEach(AppsDirectorySection.allCases) { option in
                            Text(option.label).tag(option)
                        }
                    }
                    .pickerStyle(.menu)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        Task { await model.reload(section: section, search: searchText, clearCurrent: false) }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .disabled(model.loading)
                }
            }
        }
        .task(id: queryIdentity) {
            if !searchText.isEmpty {
                do { try await Task.sleep(for: .milliseconds(250)) }
                catch { return }
            }
            guard !Task.isCancelled else { return }
            await model.reload(section: section, search: searchText, clearCurrent: true)
        }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            openRoute = nil
            model.invalidate()
            Task { await model.reload(section: section, search: searchText, clearCurrent: true) }
        }
        .fullScreenCover(item: $openRoute) { route in
            AppRouteBrowserView(selection: route)
        }
    }

    private var emptyDescription: String {
        searchText.isEmpty
            ? "No \(section.label.lowercased()) apps match this workspace."
            : "No app metadata matches “\(searchText)”."
    }

    private func open(_ entry: AppsDirectoryEntry, view: AppsDirectoryView) {
        guard entry.status.canLaunch,
              let profile = MagicianAccess.connectionProfile,
              let url = AppRoutePolicy.routeURL(
                view.route, installationID: entry.installationID, origin: profile.publicOrigin
              ) else { return }
        let context = AppRouteContext(profile: profile, installationID: entry.installationID)
        openRoute = SelectedAppRoute(url: url, context: context, title: entry.name)
        Task { await model.recordLaunch(entry, view: view, profile: profile) }
    }

    /// Opens the installation root of a viewless, enabled app — the
    /// launcher-side address a scripted custom surface answers at. The
    /// browser view decides natively what renders there; no launch
    /// activity is recorded because there is no view to attribute it to.
    private func openSurfaceRoot(_ entry: AppsDirectoryEntry) {
        guard entry.status.canLaunch,
              let profile = MagicianAccess.connectionProfile,
              let url = AppRoutePolicy.routeURL(
                "/apps/\(entry.installationID)",
                installationID: entry.installationID,
                origin: profile.publicOrigin
              ) else { return }
        let context = AppRouteContext(profile: profile, installationID: entry.installationID)
        openRoute = SelectedAppRoute(url: url, context: context, title: entry.name)
    }
}

/// Which surface a launcher route renders (1.6 completion). The default
/// is the pre-1.6 WebView flow; `deciding` is the scripted-host probe at
/// an installation root; `scripted` mounts the native per-surface
/// WKWebView host; `unsupported` is the closed notice for a plan this
/// client cannot instantiate — never a degraded fallback.
private enum AppRouteSurfacePhase {
    case deciding
    case routePage
    case scripted(AppSurfaceHostContext, AppSurfaceScriptedPlan)
    case unsupported
}

/// One mounted scripted surface: the per-installation WKWebView host
/// until its bridge session is refused, then the closed failure notice in
/// place of the torn-down frame (web-host parity).
private struct AppSurfaceScriptedSurfaceView: View {
    let context: AppSurfaceHostContext
    let plan: AppSurfaceScriptedPlan
    @StateObject private var session = AppSurfaceScriptedSessionObserver()

    var body: some View {
        Group {
            if session.closedNotice != nil {
                AppSurfaceFailedNotice()
            } else {
                AppSurfaceScriptedWebView(context: context, plan: plan, sessionObserver: session)
            }
        }
    }
}

private struct AppRouteBrowserView: View {
    @Environment(\.dismiss) private var dismiss
    let selection: SelectedAppRoute
    @State private var surfacePhase: AppRouteSurfacePhase

    init(selection: SelectedAppRoute) {
        self.selection = selection
        // A declared MUIJ view route hydrates in the page and is never
        // probed — its open must stay pixel-identical to the pre-1.6 flow,
        // so the phase starts at the WebView unless this is an
        // installation root.
        _surfacePhase = State(
            initialValue: AppSurfaceScriptedPolicy.probesScriptedHostAtRouteRoot(selection.url.path)
                ? .deciding
                : .routePage
        )
    }

    /// The contextual fitting's page for THIS app-surface route. It is
    /// re-derived from the installation id plus the route tail rather than
    /// taken from the raw path, and a tail that cannot form a canonical static
    /// page has no contextual slot at all — falling back to the installation
    /// root would make two different entity pages share one assignment.
    private var contextualSlotPage: String? {
        let path = selection.url.path
        guard AppRoutePolicy.routeBelongsToInstallation(
            path,
            installationID: selection.context.installationID
        ) else { return nil }
        let tail = path
            .split(separator: "/", omittingEmptySubsequences: true)
            .dropFirst(2)
            .joined(separator: "/")
        return AppNativeSurfaceContract.appSurfaceSlotPage(
            installationID: selection.context.installationID,
            surfacePath: tail
        )
    }

    /// Web parity: contextual widgets appear beside surface CONTENT. A page
    /// still deciding, or one this client refused, has nothing to be
    /// contextual to.
    private var showsContextualSlots: Bool {
        switch surfacePhase {
        case .scripted, .routePage: return true
        case .deciding, .unsupported: return false
        }
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                Group {
                    switch surfacePhase {
                    case .deciding:
                        ProgressView("Loading app…")
                    case .scripted(let context, let plan):
                        AppSurfaceScriptedSurfaceView(context: context, plan: plan)
                    case .unsupported:
                        AppSurfaceUnsupportedNotice()
                    case .routePage:
                        AppRouteWebView(route: selection.url, context: selection.context)
                            .ignoresSafeArea(edges: .bottom)
                    }
                }
                if let contextualSlotPage, showsContextualSlots {
                    AppNativeSlotPageRegion(
                        page: contextualSlotPage,
                        regions: ["contextual"],
                        accessibilityLabel: "Contextual app widgets",
                        contentInsets: EdgeInsets(top: 12, leading: 16, bottom: 12, trailing: 16)
                    )
                }
            }
            .navigationTitle(selection.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .task(id: selection.id) { await resolveSurfacePhase() }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            dismiss()
        }
    }

    /// The host plan fetch mirrors the web host's flow: at an installation
    /// root, probe `custom-surface-v1/host` (no `?route=` — the kernel
    /// picks the package's first declared entry point, exactly like the
    /// web page's root fallback). A minted plan mounts the native scripted
    /// host under the plan's own digest-keyed entry document; a plan this
    /// client cannot instantiate renders the closed unsupported notice;
    /// every refusal (operator switch off, permission absent, nothing
    /// granted) keeps the unchanged launcher WebView flow.
    @MainActor
    private func resolveSurfacePhase() async {
        let route = selection.url.path
        guard AppSurfaceScriptedPolicy.probesScriptedHostAtRouteRoot(route) else {
            surfacePhase = .routePage
            return
        }
        do {
            let plan = try await AppSurfaceScriptedHostClient.fetchPlan(
                profile: selection.context.profile,
                installationID: selection.context.installationID
            )
            guard let initialURL = AppSurfaceScriptedPolicy.schemeURL(
                installationID: plan.installationID,
                digest: plan.entryDocumentDigest,
                path: plan.entryDocument,
                session: plan.sessionRef
            ) else {
                surfacePhase = .unsupported
                return
            }
            surfacePhase = .scripted(
                AppSurfaceHostContext(
                    profile: selection.context.profile,
                    installationID: selection.context.installationID,
                    initialURL: initialURL
                ),
                plan
            )
        } catch {
            surfacePhase = .routePage
        }
    }
}

private struct AppRouteWebView: UIViewRepresentable {
    let route: URL
    let context: AppRouteContext

    func makeCoordinator() -> Coordinator { Coordinator(context: context) }

    func makeUIView(context uiContext: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.setURLSchemeHandler(
            uiContext.coordinator.schemeHandler,
            forURLScheme: AppRoutePolicy.scheme
        )
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = uiContext.coordinator
        if let url = AppRoutePolicy.schemeURL(for: route, origin: context.origin) {
            webView.load(URLRequest(url: url))
        }
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}

    static func dismantleUIView(_ uiView: WKWebView, coordinator: Coordinator) {
        uiView.stopLoading()
        coordinator.shutdown()
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        private let context: AppRouteContext
        fileprivate let schemeHandler: AppRouteSchemeHandler

        init(context: AppRouteContext) {
            self.context = context
            schemeHandler = AppRouteSchemeHandler(context: context)
        }

        func shutdown() { schemeHandler.shutdown() }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard let source = navigationAction.request.url,
                  let destination = AppRoutePolicy.destinationURL(for: source, origin: context.origin),
                  context.permits(destination, method: navigationAction.request.httpMethod ?? "GET") else {
                decisionHandler(.cancel)
                return
            }
            decisionHandler(.allow)
        }
    }
}

fileprivate final class AppRouteSchemeHandler: NSObject, WKURLSchemeHandler, URLSessionDataDelegate, URLSessionTaskDelegate {
    private struct Pending {
        let schemeTask: WKURLSchemeTask
        let sourceURL: URL
        let networkTask: URLSessionDataTask
        var receivedBytes = 0
    }

    private static let maximumResourceBytes = Int(AppRouteResourceLimits.maximumResponseBytes)
    private static let maximumRequestBodyBytes = 1 * 1_024 * 1_024
    private let context: AppRouteContext
    private let deliveryQueue = DispatchQueue(label: "ai.magicbeans.magican.app-route-delivery")
    private var pendingBySessionTask: [Int: Pending] = [:]
    private lazy var session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 60
        configuration.httpCookieAcceptPolicy = .never
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        return URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
    }()

    init(context: AppRouteContext) { self.context = context }

    func shutdown() {
        let networkTasks: [URLSessionDataTask] = deliveryQueue.sync {
            let tasks = pendingBySessionTask.values.map(\.networkTask)
            pendingBySessionTask.removeAll(keepingCapacity: false)
            return tasks
        }
        networkTasks.forEach { $0.cancel() }
        session.invalidateAndCancel()
    }

    func webView(_ webView: WKWebView, start urlSchemeTask: WKURLSchemeTask) {
        guard let sourceURL = urlSchemeTask.request.url,
              let destination = AppRoutePolicy.destinationURL(for: sourceURL, origin: context.origin),
              context.permits(destination, method: urlSchemeTask.request.httpMethod ?? "GET") else {
            urlSchemeTask.didFailWithError(URLError(.badURL))
            return
        }
        var request = URLRequest(url: destination, timeoutInterval: 30)
        request.httpMethod = urlSchemeTask.request.httpMethod ?? "GET"
        do {
            request.httpBody = try Self.boundedBody(from: urlSchemeTask.request)
        } catch {
            urlSchemeTask.didFailWithError(error)
            return
        }
        for (name, value) in urlSchemeTask.request.allHTTPHeaderFields ?? [:]
        where Self.forwardedRequestHeaders.contains(name.lowercased()) {
            request.setValue(value, forHTTPHeaderField: name)
        }
        context.authorize(&request)
        let task = session.dataTask(with: request)
        deliveryQueue.sync {
            pendingBySessionTask[task.taskIdentifier] = Pending(
                schemeTask: urlSchemeTask,
                sourceURL: sourceURL,
                networkTask: task
            )
        }
        task.resume()
    }

    func webView(_ webView: WKWebView, stop urlSchemeTask: WKURLSchemeTask) {
        let identity = ObjectIdentifier(urlSchemeTask)
        let networkTask: URLSessionDataTask? = deliveryQueue.sync {
            guard let identifier = pendingBySessionTask.first(where: {
                ObjectIdentifier($0.value.schemeTask) == identity
            })?.key else { return nil }
            return pendingBySessionTask.removeValue(forKey: identifier)?.networkTask
        }
        networkTask?.cancel()
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        // A valid app route is canonical and never redirects. Rejecting all
        // redirects prevents device and Cloudflare credentials crossing origins.
        completionHandler(nil)
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        deliveryQueue.async { [weak self] in
            guard let self,
                  let pending = self.pendingBySessionTask[dataTask.taskIdentifier],
                  AppRouteResourceLimits.admitsExpectedContentLength(response.expectedContentLength),
                  let http = response as? HTTPURLResponse,
                  let rewritten = HTTPURLResponse(
                    url: pending.sourceURL,
                    statusCode: http.statusCode,
                    httpVersion: "HTTP/1.1",
                    headerFields: http.allHeaderFields.reduce(into: [String: String]()) { result, pair in
                        if let key = pair.key as? String, let value = pair.value as? String,
                           Self.forwardedResponseHeaders.contains(key.lowercased()) {
                            result[key] = value
                        }
                    }
                  ) else {
                completionHandler(.cancel)
                self?.failOnDeliveryQueue(dataTask.taskIdentifier, error: URLError(.badServerResponse))
                return
            }
            pending.schemeTask.didReceive(rewritten)
            completionHandler(.allow)
        }
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        deliveryQueue.async { [weak self] in
            guard let self, var pending = self.pendingBySessionTask[dataTask.taskIdentifier] else { return }
            let (newSize, overflow) = pending.receivedBytes.addingReportingOverflow(data.count)
            guard !overflow, newSize <= Self.maximumResourceBytes else {
                self.pendingBySessionTask.removeValue(forKey: dataTask.taskIdentifier)
                pending.networkTask.cancel()
                pending.schemeTask.didFailWithError(URLError(.dataLengthExceedsMaximum))
                return
            }
            pending.receivedBytes = newSize
            self.pendingBySessionTask[dataTask.taskIdentifier] = pending
            pending.schemeTask.didReceive(data)
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        deliveryQueue.async { [weak self] in
            guard let pending = self?.pendingBySessionTask.removeValue(forKey: task.taskIdentifier) else { return }
            if let error { pending.schemeTask.didFailWithError(error) }
            else { pending.schemeTask.didFinish() }
        }
    }

    private func failOnDeliveryQueue(_ taskIdentifier: Int, error: Error) {
        let pending = pendingBySessionTask.removeValue(forKey: taskIdentifier)
        pending?.networkTask.cancel()
        pending?.schemeTask.didFailWithError(error)
    }

    private static func boundedBody(from request: URLRequest) throws -> Data? {
        if let body = request.httpBody {
            guard body.count <= maximumRequestBodyBytes else {
                throw URLError(.dataLengthExceedsMaximum)
            }
            return body
        }
        guard let stream = request.httpBodyStream else { return nil }
        stream.open()
        defer { stream.close() }
        var result = Data()
        var buffer = [UInt8](repeating: 0, count: 8_192)
        while true {
            let count = stream.read(&buffer, maxLength: buffer.count)
            if count < 0 { throw stream.streamError ?? URLError(.cannotDecodeRawData) }
            if count == 0 { break }
            guard result.count <= maximumRequestBodyBytes - count else {
                throw URLError(.dataLengthExceedsMaximum)
            }
            result.append(contentsOf: buffer.prefix(count))
        }
        return result
    }

    private static let forwardedRequestHeaders: Set<String> = [
        "accept", "accept-language", "content-type", "if-modified-since", "if-none-match", "range"
    ]

    /// URLSession owns transport decoding and framing. Forward only semantic
    /// representation/cache headers so WebKit never tries to decode an already
    /// decoded body or follows a server-provided redirect outside this handler.
    private static let forwardedResponseHeaders: Set<String> = [
        "accept-ranges", "cache-control", "content-disposition", "content-language",
        "content-range", "content-security-policy", "content-type", "etag", "expires",
        "last-modified", "referrer-policy", "vary", "x-content-type-options"
    ]
}
