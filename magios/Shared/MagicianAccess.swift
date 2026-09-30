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

    init(
        publicOrigin: URL,
        principal: String,
        workspace: String,
        deviceID: String,
        deviceToken: String,
        cloudflareClientID: String = "",
        cloudflareClientSecret: String = ""
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

/// One runtime connection profile shared by the app and its extensions.
/// Customer hosts and Cloudflare credentials are received only through a
/// short-lived enrollment exchange and are never compiled into the app.
enum MagicianAccess {
    static let productName = ProductIdentity.productName
    static let assistantFallbackName = ProductIdentity.assistantFallbackName
    static let backendServiceLabel = "Magician backend"
    static let appGroup = "group.ai.magicbeans.magician.shared"

    static let clientIdKey = "cfAccessClientId"
    static let clientSecretKey = "cfAccessClientSecret"
    private static let profileAccount = "mobileConnectionProfileV1"
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

    static var connectionProfile: MobileConnectionProfile? {
        guard let raw = keychainValue(service: credentialService, account: profileAccount),
              let data = raw.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(MobileConnectionProfile.self, from: data)
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

    static func install(_ profile: MobileConnectionProfile) throws {
        let encoded = try JSONEncoder().encode(profile)
        guard let value = String(data: encoded, encoding: .utf8) else {
            throw MobileConnectionError.invalidProfile
        }
        setKeychainValue(value, service: credentialService, account: profileAccount)
        guard connectionProfile == profile else { throw MobileConnectionError.invalidProfile }
        store.set(profile.deviceID, forKey: deviceIDKey)
        clearLegacyCredentials()
        publishConnectionChange()
    }

    static func clearConnection() {
        setKeychainValue("", service: credentialService, account: profileAccount)
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
                cloudflareClientSecret: clientSecret
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
    /// (for example SilverBullet) may receive this pair, but must never receive
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
            result["Authorization"] = "Bearer \(profile.deviceToken)"
        }
        return result
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
