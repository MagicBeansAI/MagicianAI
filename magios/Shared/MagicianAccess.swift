import Foundation
import Security

extension Notification.Name {
    static let magicianMobileConnectionDidChange = Notification.Name(
        "ai.magicbeans.magician.mobileConnectionDidChange"
    )
}

struct MobileConnectionProfile: Codable, Equatable {
    let publicOrigin: URL
    let principal: String
    let workspace: String
    let deviceID: String
    let deviceToken: String
    let cloudflareClientID: String
    let cloudflareClientSecret: String
    /// The paired machine's registry name (exchange `machine_name`), when the
    /// host reported one. Optional so V1/V2 blobs stored before it decode.
    let machineName: String?

    init(
        publicOrigin: URL,
        principal: String,
        workspace: String,
        deviceID: String,
        deviceToken: String,
        cloudflareClientID: String = "",
        cloudflareClientSecret: String = "",
        machineName: String? = nil
    ) throws {
        guard let normalized = MobileEnrollmentLink.normalizedOrigin(publicOrigin.absoluteString),
              !principal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !workspace.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !deviceID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !deviceToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw MobileConnectionError.invalidProfile
        }
        self.publicOrigin = normalized
        self.principal = principal.trimmingCharacters(in: .whitespacesAndNewlines)
        self.workspace = workspace.trimmingCharacters(in: .whitespacesAndNewlines)
        self.deviceID = deviceID.trimmingCharacters(in: .whitespacesAndNewlines)
        self.deviceToken = deviceToken.trimmingCharacters(in: .whitespacesAndNewlines)
        self.cloudflareClientID = cloudflareClientID.trimmingCharacters(in: .whitespacesAndNewlines)
        self.cloudflareClientSecret = cloudflareClientSecret.trimmingCharacters(in: .whitespacesAndNewlines)
        let name = machineName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.machineName = name.isEmpty ? nil : name
    }
}

/// Every Magician machine this iPhone has paired with, keyed by normalized
/// origin, plus which one is active. Stored as one Keychain JSON blob
/// (`mobileConnectionProfilesV2`) so the app and its extensions always read a
/// consistent snapshot. Each profile keeps its own device token and Cloudflare
/// credential; nothing moves between machines.
struct MobileConnectionProfiles: Codable, Equatable {
    var activeOrigin: String?
    var profiles: [String: MobileConnectionProfile]

    enum CodingKeys: String, CodingKey {
        case activeOrigin = "active_origin"
        case profiles
    }

    static let empty = MobileConnectionProfiles(activeOrigin: nil, profiles: [:])

    /// The comparison key for an origin: normalized like an enrollment link
    /// (no path, query or credentials) with scheme and host lowercased.
    static func originKey(_ raw: String) -> String? {
        MobileEnrollmentLink.normalizedOrigin(raw)?.absoluteString.lowercased()
    }

    static func originKey(_ url: URL) -> String? { originKey(url.absoluteString) }

    var active: MobileConnectionProfile? {
        guard let activeOrigin else { return nil }
        return profiles[activeOrigin]
    }

    /// Profiles in a stable order: active first, then by host.
    var ordered: [MobileConnectionProfile] {
        profiles.sorted { lhs, rhs in
            if lhs.key == activeOrigin { return true }
            if rhs.key == activeOrigin { return false }
            return lhs.key < rhs.key
        }.map(\.value)
    }

    func profile(forOrigin raw: String) -> MobileConnectionProfile? {
        guard let key = Self.originKey(raw) else { return nil }
        return profiles[key]
    }

    /// Adds or replaces the profile for its origin and makes it active.
    ///
    /// The host keeps one pairing per device per machine, so pairing a machine
    /// by its other route (same-Wi-Fi vs remote) revokes the older profile's
    /// token. Every OTHER stored profile of the same machine — same non-nil
    /// `machineName`, or an origin listed in `sameMachineOrigins` (exchange
    /// `machine_origins`) — is removed and returned.
    @discardableResult
    mutating func install(
        _ profile: MobileConnectionProfile,
        sameMachineOrigins: [String] = []
    ) -> [MobileConnectionProfile] {
        guard let key = Self.originKey(profile.publicOrigin) else { return [] }
        let machineOriginKeys = Set(sameMachineOrigins.compactMap { Self.originKey($0) })
        let machineName = profile.machineName.map(Self.machineNameKey)
        let replaced = profiles.filter { otherKey, other in
            guard otherKey != key else { return false }
            if machineOriginKeys.contains(otherKey) { return true }
            guard let machineName, let otherName = other.machineName else { return false }
            return Self.machineNameKey(otherName) == machineName
        }
        for otherKey in replaced.keys { profiles.removeValue(forKey: otherKey) }
        profiles[key] = profile
        activeOrigin = key
        return replaced.keys.sorted().compactMap { replaced[$0] }
    }

    /// Machine names compare trimmed and case-insensitively.
    static func machineNameKey(_ name: String) -> String {
        name.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    /// Switches only to an origin that already holds its own credential.
    @discardableResult
    mutating func activate(origin raw: String) -> Bool {
        guard let key = Self.originKey(raw), profiles[key] != nil else { return false }
        activeOrigin = key
        return true
    }

    /// Removes one origin's profile. Forgetting the active machine falls back
    /// to another paired one (stable order), or to none.
    @discardableResult
    mutating func forget(origin raw: String) -> MobileConnectionProfile? {
        guard let key = Self.originKey(raw),
              let removed = profiles.removeValue(forKey: key) else { return nil }
        if activeOrigin == key {
            activeOrigin = profiles.keys.sorted().first
        }
        return removed
    }

    /// V2 is authoritative once it exists. Without it, a V1 single profile
    /// becomes the only, active profile. `needsWrite` is true only for that
    /// migration, so callers persist it once.
    static func resolve(
        v2Raw: String?,
        v1Raw: String?
    ) -> (state: MobileConnectionProfiles, needsWrite: Bool) {
        if let v2Raw, !v2Raw.isEmpty {
            guard let data = v2Raw.data(using: .utf8),
                  var decoded = try? JSONDecoder().decode(MobileConnectionProfiles.self, from: data) else {
                // A corrupt V2 blob must not silently resurrect V1 credentials.
                return (.empty, false)
            }
            // Re-key defensively so lookups always use the normalized origin.
            var rekeyed: [String: MobileConnectionProfile] = [:]
            for profile in decoded.profiles.values {
                if let key = originKey(profile.publicOrigin) { rekeyed[key] = profile }
            }
            decoded.profiles = rekeyed
            if let active = decoded.activeOrigin {
                decoded.activeOrigin = originKey(active).flatMap { rekeyed[$0] != nil ? $0 : nil }
            }
            return (decoded, false)
        }
        guard let v1Raw, !v1Raw.isEmpty,
              let data = v1Raw.data(using: .utf8),
              let legacy = try? JSONDecoder().decode(MobileConnectionProfile.self, from: data) else {
            return (.empty, false)
        }
        var migrated = MobileConnectionProfiles.empty
        migrated.install(legacy)
        return (migrated, migrated.active != nil)
    }
}

enum MobileConnectionError: LocalizedError {
    case invalidLink
    case wrongPlatform
    case invalidProfile
    case routeMismatch(selected: MobileConnectionMode, scanned: MobileConnectionMode)
    case exchangeFailed(String)
    case verificationFailed

    var errorDescription: String? {
        switch self {
        case .invalidLink: return "This is not a valid Magican connection code."
        case .wrongPlatform: return "This connection code was created for Android. Create an iPhone code instead."
        case .invalidProfile: return "Magician returned an incomplete connection profile."
        case .routeMismatch(let selected, let scanned):
            return "This is a \(scanned.title) code. Choose \(scanned.title) on this iPhone, or create a \(selected.title) code on the computer, then scan again."
        case .exchangeFailed(let message): return message
        case .verificationFailed: return "The new Magician connection could not be verified. Your previous connection was kept."
        }
    }
}

enum MobileConnectionMode: String, CaseIterable, Identifiable {
    case sameWifi = "same_wifi"
    case remote

    var id: String { rawValue }
    var title: String {
        switch self {
        case .sameWifi: return "Same Wi-Fi"
        case .remote: return "Remote"
        }
    }

    static func forOrigin(_ origin: URL) -> MobileConnectionMode {
        origin.scheme == "http" && MobileEnrollmentLink.isPrivateNetworkHost(origin.host ?? "")
            ? .sameWifi
            : .remote
    }
}

/// Inbound custom-scheme contract. Magican links use `magican://` exclusively.
enum MagicanAppURL {
    static func isScheme(_ scheme: String?) -> Bool {
        scheme == "magican"
    }
}

struct MobileEnrollmentLink: Equatable, Identifiable {
    let publicOrigin: URL
    let enrollmentID: String
    let secret: String
    let clientKind: String
    var id: String { enrollmentID }
    var connectionMode: MobileConnectionMode { .forOrigin(publicOrigin) }
    var usesSameWifi: Bool { connectionMode == .sameWifi }

    static func parse(_ raw: String) throws -> MobileEnrollmentLink {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count <= 4_096,
              let components = URLComponents(string: trimmed),
              MagicanAppURL.isScheme(components.scheme),
              components.host == "connect",
              components.path.isEmpty,
              components.fragment == nil,
              let items = components.queryItems else {
            throw MobileConnectionError.invalidLink
        }
        let grouped = Dictionary(grouping: items, by: \.name)
        guard Set(grouped.keys) == Set(["base", "id", "secret", "kind"]),
              grouped.values.allSatisfy({ $0.count == 1 }),
              let base = grouped["base"]?.first?.value,
              let origin = normalizedOrigin(base),
              let enrollmentID = grouped["id"]?.first?.value,
              enrollmentID.count >= 16, enrollmentID.count <= 128,
              let secret = grouped["secret"]?.first?.value,
              secret.count >= 32, secret.count <= 256,
              let kind = grouped["kind"]?.first?.value else {
            throw MobileConnectionError.invalidLink
        }
        guard kind == "ios" else { throw MobileConnectionError.wrongPlatform }
        return MobileEnrollmentLink(
            publicOrigin: origin,
            enrollmentID: enrollmentID,
            secret: secret,
            clientKind: kind
        )
    }

    static func normalizedOrigin(_ raw: String) -> URL? {
        guard var components = URLComponents(string: raw.trimmingCharacters(in: .whitespacesAndNewlines)),
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil,
              components.host?.isEmpty == false,
              components.path.isEmpty || components.path == "/" else { return nil }
        let privateHTTP = components.scheme == "http"
            && isPrivateNetworkHost(components.host ?? "")
        guard components.scheme == "https" || privateHTTP else { return nil }
        components.path = ""
        return components.url
    }

    fileprivate static func isPrivateNetworkHost(_ rawHost: String) -> Bool {
        let host = rawHost.lowercased()
            .trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        if host == "localhost" || host == "::1" { return true }
        if host.contains(":") {
            return host.hasPrefix("fc") || host.hasPrefix("fd")
                || (host.count >= 3
                    && host.hasPrefix("fe")
                    && ["8", "9", "a", "b"].contains(String(host.dropFirst(2).prefix(1))))
        }
        let octets = host.split(separator: ".").compactMap { UInt8($0) }
        guard octets.count == 4 else { return false }
        return octets[0] == 10
            || octets[0] == 127
            || (octets[0] == 169 && octets[1] == 254)
            || (octets[0] == 172 && (16...31).contains(octets[1]))
            || (octets[0] == 192 && octets[1] == 168)
    }
}

/// Runtime connection profiles shared by the app and its extensions: one per
/// paired Magician machine, one of them active. Every accessor below
/// (`connectionProfile`, `baseURL`, `headers()`, ...) reads the ACTIVE profile.
/// Customer hosts and Cloudflare credentials are received only through a
/// short-lived enrollment exchange and are never compiled into the app.
enum MagicianAccess {
    static let productName = ProductIdentity.productName
    static let assistantFallbackName = ProductIdentity.assistantFallbackName
    static let backendServiceLabel = "Magician backend"
    static let appGroup = "group.ai.magicbeans.magician.shared"

    static let clientIdKey = "cfAccessClientId"
    static let clientSecretKey = "cfAccessClientSecret"
    /// Single-profile store from releases before multi-machine support. Read
    /// only to migrate into V2; kept for one release so a downgrade still
    /// connects. Deleted only when its own origin is forgotten.
    private static let profileAccount = "mobileConnectionProfileV1"
    private static let profilesAccount = "mobileConnectionProfilesV2"
    /// Serializes read-modify-write of the V2 blob within this process.
    private static let profilesLock = NSRecursiveLock()
    private static let deviceIDKey = "mobileDeviceIDV1"
    private static let unconfiguredOrigin = URL(string: "https://unconfigured.invalid")!

    // Hosted unit tests execute inside the signed app, with its real Keychain
    // entitlement. Give the entire unit-test process a private namespace so
    // credential-reset and migration fixtures cannot unenroll a physical phone.
    // Live UI tests run in a separate runner and keep the app's real connection.
    private static let storageSuffix = isRunningUnderTests ? ".tests.\(UUID().uuidString)" : ""
    static var store: UserDefaults { UserDefaults(suiteName: appGroup + storageSuffix) ?? .standard }

    private static let credentialService = "ai.magicbeans.magician.mobile-access" + storageSuffix
    private static let legacyCredentialService = "ai.magicbeans.magician.cloudflare-access" + storageSuffix
    // The shared keychain group ("$(AppIdentifierPrefix)ai.magicbeans.magician.shared")
    // is deliberately the ONLY keychain-access-groups entry in every target's
    // entitlements, which makes it the DEFAULT group for every keychain call in
    // every target. The code relies on that default instead of naming the group:
    // spelling it out would require the Apple Team ID, and no Team-ID literal may
    // appear in source (the operator identity layer / public-mirror gate forbid it).

    /// The active machine's profile. Every existing call site, widget, intent
    /// and extension follows the active machine through this accessor.
    static var connectionProfile: MobileConnectionProfile? {
        connectionProfiles.active
    }

    /// Every paired machine (V1 is migrated into V2 on first read).
    static var connectionProfiles: MobileConnectionProfiles {
        profilesLock.lock()
        defer { profilesLock.unlock() }
        let v2Raw = keychainValue(service: credentialService, account: profilesAccount)
        let resolved = MobileConnectionProfiles.resolve(
            v2Raw: v2Raw,
            // V1 is consulted only while V2 does not exist yet.
            v1Raw: (v2Raw?.isEmpty ?? true)
                ? keychainValue(service: credentialService, account: profileAccount)
                : nil
        )
        if resolved.needsWrite {
            // Best effort: an extension that cannot write still sees the
            // migrated state; the next writer persists it.
            writeProfiles(resolved.state)
        }
        return resolved.state
    }

    /// Paired profiles, active first.
    static var allProfiles: [MobileConnectionProfile] { connectionProfiles.ordered }

    /// Normalized origin key of the active machine, if any.
    static var activeOrigin: String? { connectionProfiles.activeOrigin }

    static func profile(forOrigin origin: String) -> MobileConnectionProfile? {
        connectionProfiles.profile(forOrigin: origin)
    }

    /// A non-optional URL keeps existing networking call sites safe while an
    /// unconfigured app fails closed at a reserved non-routable origin.
    static var baseURL: URL { connectionProfile?.publicOrigin ?? unconfiguredOrigin }
    static var isConfigured: Bool { connectionProfile != nil }

    static var webSocketBaseURL: URL {
        var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false)!
        components.scheme = components.scheme == "http" ? "ws" : "wss"
        return components.url!
    }

    static var principal: String { connectionProfile?.principal ?? "anonymous" }
    static var workspace: String { connectionProfile?.workspace ?? "default" }
    static var deviceID: String { connectionProfile?.deviceID ?? pendingDeviceID }
    static var deviceToken: String { connectionProfile?.deviceToken ?? "" }
    static var clientId: String { connectionProfile?.cloudflareClientID ?? legacyCredential(account: clientIdKey) }
    static var clientSecret: String { connectionProfile?.cloudflareClientSecret ?? legacyCredential(account: clientSecretKey) }

    static var pendingDeviceID: String {
        if let existing = store.string(forKey: deviceIDKey)?.trimmingCharacters(in: .whitespacesAndNewlines),
           !existing.isEmpty { return existing }
        let minted = "magios-\(UUID().uuidString.lowercased())"
        store.set(minted, forKey: deviceIDKey)
        return minted
    }

    /// Adds or replaces the profile for its origin and makes it active. Other
    /// machines' profiles are kept; this machine's profiles on its other
    /// routes (`sameMachineOrigins` / same `machineName`) are replaced, since
    /// the host revoked their tokens when this pairing was issued.
    static func install(
        _ profile: MobileConnectionProfile,
        sameMachineOrigins: [String] = []
    ) throws {
        profilesLock.lock()
        var state = connectionProfiles
        let replaced = state.install(profile, sameMachineOrigins: sameMachineOrigins)
        let written = writeProfiles(state)
        if written {
            for stale in replaced { deleteLegacyProfileIfMatches(stale.publicOrigin) }
        }
        profilesLock.unlock()
        guard written, connectionProfile == profile else { throw MobileConnectionError.invalidProfile }
        store.set(profile.deviceID, forKey: deviceIDKey)
        clearLegacyCredentials()
        publishConnectionChange()
    }

    /// Makes an already-paired machine active. Returns false (and changes
    /// nothing) for an origin without a stored profile: switching never
    /// creates a credential.
    @discardableResult
    static func activate(origin: String) -> Bool {
        profilesLock.lock()
        var state = connectionProfiles
        let previous = state.activeOrigin
        guard state.activate(origin: origin), writeProfiles(state) else {
            profilesLock.unlock()
            return false
        }
        profilesLock.unlock()
        if previous != state.activeOrigin { publishConnectionChange() }
        return true
    }

    /// Removes only that origin's profile. Forgetting the active machine
    /// activates another paired one, or leaves the app unconfigured.
    @discardableResult
    static func forget(origin: String) -> MobileConnectionProfile? {
        profilesLock.lock()
        var state = connectionProfiles
        guard let removed = state.forget(origin: origin), writeProfiles(state) else {
            profilesLock.unlock()
            return nil
        }
        deleteLegacyProfileIfMatches(removed.publicOrigin)
        profilesLock.unlock()
        if state.active == nil { clearLegacyCredentials() }
        publishConnectionChange()
        return removed
    }

    /// Forgets every paired machine.
    static func forgetAll() {
        profilesLock.lock()
        writeProfiles(.empty)
        setKeychainValue("", service: credentialService, account: profileAccount)
        profilesLock.unlock()
        clearLegacyCredentials()
        publishConnectionChange()
    }

    /// Disconnects the ACTIVE machine (other paired machines are kept; one of
    /// them becomes active). With no active profile it still clears legacy
    /// recovery credentials, as before.
    static func clearConnection() {
        if let active = activeOrigin, forget(origin: active) != nil { return }
        clearLegacyCredentials()
        publishConnectionChange()
    }

    /// Compatibility for the former recovery fields. It can rotate the outer
    /// credential of an existing runtime profile but cannot create a host.
    static func setCredentials(clientId: String, clientSecret: String) {
        guard let profile = connectionProfile,
              let updated = try? MobileConnectionProfile(
                publicOrigin: profile.publicOrigin,
                principal: profile.principal,
                workspace: profile.workspace,
                deviceID: profile.deviceID,
                deviceToken: profile.deviceToken,
                cloudflareClientID: clientId,
                cloudflareClientSecret: clientSecret,
                machineName: profile.machineName
              ) else { return }
        try? install(updated)
    }

    static func clearCredentials() { clearConnection() }

    static func authorize(_ request: inout URLRequest) {
        // Resolve one profile snapshot so a concurrent enrollment cannot mix
        // one connection's scope with another connection's credentials.
        let profile = connectionProfile
        authorize(
            &request,
            principal: profile?.principal ?? "anonymous",
            workspace: profile?.workspace ?? "default",
            profile: profile
        )
    }

    static func authorize(
        _ request: inout URLRequest,
        principal: String,
        workspace: String,
        profile: MobileConnectionProfile? = connectionProfile
    ) {
        // `principal` and `workspace` remain in the signature only to keep
        // immutable-profile call sites source-compatible. Neither is sent or
        // trusted: the opaque bearer resolves to its enrolled scope server-side.
        request.setValue(nil, forHTTPHeaderField: "X-Principal")
        request.setValue(nil, forHTTPHeaderField: "X-Workspace")
        guard isMagicianRuntimeURL(request.url, profile: profile) else { return }
        for (key, value) in headers(profile: profile) {
            request.setValue(value, forHTTPHeaderField: key)
        }
    }

    /// Never attach the device bearer or Cloudflare client secret to an
    /// artifact URL on a third-party origin.
    static func isMagicianRuntimeURL(
        _ candidate: URL?,
        profile: MobileConnectionProfile?
    ) -> Bool {
        guard let candidate else { return false }
        let origin = profile?.publicOrigin ?? baseURL
        let normalizedScheme: (String?) -> String? = { scheme in
            switch scheme?.lowercased() {
            case "ws": return "http"
            case "wss": return "https"
            default: return scheme?.lowercased()
            }
        }
        return normalizedScheme(candidate.scheme) == normalizedScheme(origin.scheme)
            && candidate.host?.lowercased() == origin.host?.lowercased()
            && effectivePort(candidate, normalizedScheme: normalizedScheme)
                == effectivePort(origin, normalizedScheme: normalizedScheme)
    }

    private static func effectivePort(
        _ url: URL,
        normalizedScheme: (String?) -> String?
    ) -> Int? {
        if let port = url.port { return port }
        switch normalizedScheme(url.scheme) {
        case "http": return 80
        case "https": return 443
        default: return nil
        }
    }

    static func authorizedHeaders(
        for url: URL,
        profile: MobileConnectionProfile? = connectionProfile
    ) -> [String: String] {
        isMagicianRuntimeURL(url, profile: profile) ? headers(profile: profile) : [:]
    }

    /// The shared Cloudflare service credential is outer-gateway evidence,
    /// not Magician device identity. A separately hosted protected surface
    /// behind the same gateway may receive this pair, but must never receive
    /// the device id or workspace-bound bearer.
    static func cloudflareAccessHeaders(
        profile: MobileConnectionProfile? = connectionProfile
    ) -> [String: String] {
        var result: [String: String] = [:]
        let id = profile?.cloudflareClientID ?? clientId
        let secret = profile?.cloudflareClientSecret ?? clientSecret
        if !id.isEmpty, !secret.isEmpty {
            result["CF-Access-Client-Id"] = id
            result["CF-Access-Client-Secret"] = secret
        }
        return result
    }

    static func headers(profile: MobileConnectionProfile? = connectionProfile) -> [String: String] {
        var result = cloudflareAccessHeaders(profile: profile)
        if let profile {
            result["X-Magician-Device-Id"] = profile.deviceID
            if let appVersion = appVersionHeaderValue {
                result[appVersionHeader] = appVersion
            }
            result["Authorization"] = "Bearer \(profile.deviceToken)"
        }
        return result
    }

    /// Sent with the device id so the paired machine can show this app's
    /// version in web Settings → Devices.
    static let appVersionHeader = "X-Magician-App-Version"

    /// `"<CFBundleShortVersionString> (<CFBundleVersion>)"`, e.g. `0.4.11 (222)`,
    /// read once from the running bundle (extensions share the app's version).
    static let appVersionHeaderValue: String? = makeAppVersionHeaderValue(info: Bundle.main.infoDictionary)

    /// Nil when the short version is missing, so the header is omitted. The
    /// value keeps only printable ASCII and is capped at 40 characters.
    static func makeAppVersionHeaderValue(info: [String: Any]?) -> String? {
        func clean(_ key: String) -> String {
            let raw = (info?[key] as? String ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return String(raw.unicodeScalars.filter { $0.value >= 0x20 && $0.value < 0x7F })
        }
        let version = clean("CFBundleShortVersionString")
        guard !version.isEmpty else { return nil }
        let build = clean("CFBundleVersion")
        let value = build.isEmpty ? version : "\(version) (\(build))"
        return String(value.prefix(40))
    }

    /// Browser-equivalent WebSocket offers: a stable application protocol, then
    /// the auth-only `magician-bearer.<token>` token. URLSession does not always
    /// send `Authorization` on the upgrade, so voice-control (and any other
    /// scoped socket) must offer the same subprotocol the web client uses.
    static func webSocketProtocols(_ applicationProtocols: [String]) -> [String] {
        let application = applicationProtocols
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty && !$0.hasPrefix("magician-bearer.") }
        let token = deviceToken.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !token.isEmpty else { return application }
        return application + ["magician-bearer.\(token)"]
    }

    static func authorizeWebSocket(
        _ request: inout URLRequest,
        applicationProtocols: [String] = []
    ) {
        authorize(&request)
        let protocols = webSocketProtocols(applicationProtocols)
        guard !protocols.isEmpty else { return }
        request.setValue(protocols.joined(separator: ", "), forHTTPHeaderField: "Sec-WebSocket-Protocol")
    }

    static var hasAccessCredentials: Bool { !clientId.isEmpty && !clientSecret.isEmpty }

    private static func legacyCredential(account: String) -> String {
        if let secured = keychainValue(service: legacyCredentialService, account: account),
           !secured.isEmpty {
            return secured
        }
        guard let legacy = store.string(forKey: account), !legacy.isEmpty else { return "" }

        // Old releases kept Cloudflare recovery fields in the shared defaults
        // suite. Preserve availability while migrating, but delete the
        // plaintext copy only after the exact Keychain value can be read back.
        setKeychainValue(legacy, service: legacyCredentialService, account: account)
        guard keychainValue(service: legacyCredentialService, account: account) == legacy else {
            return legacy
        }
        store.removeObject(forKey: account)
        return legacy
    }

    private static func clearLegacyCredentials() {
        setKeychainValue("", service: legacyCredentialService, account: clientIdKey)
        setKeychainValue("", service: legacyCredentialService, account: clientSecretKey)
        store.removeObject(forKey: clientIdKey)
        store.removeObject(forKey: clientSecretKey)
    }

    @discardableResult
    private static func writeProfiles(_ state: MobileConnectionProfiles) -> Bool {
        guard let encoded = try? JSONEncoder().encode(state),
              let value = String(data: encoded, encoding: .utf8) else { return false }
        setKeychainValue(value, service: credentialService, account: profilesAccount)
        return keychainValue(service: credentialService, account: profilesAccount) == value
    }

    /// A forgotten credential must not linger in the V1 downgrade copy.
    private static func deleteLegacyProfileIfMatches(_ origin: URL) {
        guard let raw = keychainValue(service: credentialService, account: profileAccount),
              let data = raw.data(using: .utf8),
              let legacy = try? JSONDecoder().decode(MobileConnectionProfile.self, from: data),
              MobileConnectionProfiles.originKey(legacy.publicOrigin)
                == MobileConnectionProfiles.originKey(origin) else { return }
        setKeychainValue("", service: credentialService, account: profileAccount)
    }

    #if DEBUG
    /// Test seams for the hosted unit tests' private Keychain namespace.
    static func testingWriteLegacyV1Profile(_ profile: MobileConnectionProfile) throws {
        let encoded = try JSONEncoder().encode(profile)
        setKeychainValue(String(decoding: encoded, as: UTF8.self), service: credentialService, account: profileAccount)
    }

    static var testingLegacyV1Profile: MobileConnectionProfile? {
        guard let raw = keychainValue(service: credentialService, account: profileAccount) else { return nil }
        return try? JSONDecoder().decode(MobileConnectionProfile.self, from: Data(raw.utf8))
    }

    static var testingHasV2Store: Bool {
        keychainValue(service: credentialService, account: profilesAccount) != nil
    }

    static func testingResetStores() {
        setKeychainValue("", service: credentialService, account: profilesAccount)
        setKeychainValue("", service: credentialService, account: profileAccount)
    }
    #endif

    private static func publishConnectionChange() {
        let publish = {
            NotificationCenter.default.post(
                name: .magicianMobileConnectionDidChange,
                object: nil
            )
        }
        if Thread.isMainThread { publish() }
        else { DispatchQueue.main.async(execute: publish) }
    }

    private static func keychainValue(service: String, account: String) -> String? {
        // No kSecAttrAccessGroup: a group-less search covers every group this
        // target can access, so items written under the shared group (explicitly
        // by older builds, or as the default group now) are all found.
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        if SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
           let data = item as? Data,
           let value = String(data: data, encoding: .utf8) { return value }
        return nil
    }

    private static func setKeychainValue(_ value: String, service: String, account: String) {
        let data = Data(value.utf8)
        var base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        // Group-less calls resolve to the default access group — the shared
        // group, per the entitlements invariant above — and a group-less delete
        // also removes items older builds wrote with an explicit group.
        if value.isEmpty {
            SecItemDelete(base as CFDictionary)
            return
        }
        if SecItemCopyMatching(base as CFDictionary, nil) == errSecSuccess {
            SecItemUpdate(base as CFDictionary, [kSecValueData as String: data] as CFDictionary)
            return
        }
        base[kSecValueData as String] = data
        base[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(base as CFDictionary, nil)
    }
}
