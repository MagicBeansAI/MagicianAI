import ActivityKit
import Combine
import Foundation
import UIKit
import UserNotifications
import WidgetKit

enum MobilePushRegistrationKind: String, Codable {
    case application
    case taskActivity = "task_activity"
    /// The coding-runs Live Activity's update token (no task id).
    case codingRuns = "coding_runs"
    /// The coding-runs push-to-start token, iOS 17.2+ (no task id).
    case codingRunsStart = "coding_runs_start"
    /// A followed item's Live Activity update token; the item id rides in
    /// `task_id` (`FollowedItemActivity`).
    case workItem = "work_item"
}

/// APNs is intentionally a build capability, not a runtime guess. The ordinary
/// Debug lane is signed by Xcode's seven-day Personal Team profile, which Apple
/// never grants `aps-environment`; Release and the explicit Debug push lane set
/// `MAGIOS_REMOTE_PUSH` only when the matching entitlement is signed in.
enum MobilePushBuildSupport {
    #if MAGIOS_REMOTE_PUSH
    static let remoteNotificationsEnabled = true
    #else
    static let remoteNotificationsEnabled = false
    #endif
}

struct MobilePushRouteRegistration: Decodable, Equatable {
    let revision: Int64
}

enum MobilePushRegistrationClient {
    private enum RequestError: Error {
        case notConfigured
        case providerNotConfigured
        case rejected(Int)
    }

    private struct ErrorEnvelope: Decodable {
        let error: String?
    }

    private struct RegisterBody: Encodable {
        let platform = "apns"
        let kind: MobilePushRegistrationKind
        let token: String
        let environment: String
        let taskID: String?

        enum CodingKeys: String, CodingKey {
            case platform, kind, token, environment
            case taskID = "task_id"
        }
    }

    /// The APNs environment this build's device token belongs to. Resolved
    /// once per launch; see `apnsEnvironment(provisioningProfile:isDebugBuild:)`.
    static let environment: String = apnsEnvironment(
        provisioningProfile: Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision")
            .flatMap { try? Data(contentsOf: $0) },
        isDebugBuild: isDebugBuild
    )

    private static var isDebugBuild: Bool {
        #if DEBUG
        true
        #else
        false
        #endif
    }

    /// Which APNs gateway issued this device's token — the SIGNED
    /// `aps-environment` in the embedded provisioning profile, never the build
    /// configuration alone. Xcode rewrites the entitlement to the profile's
    /// value, so a Release build installed with a development profile holds a
    /// **sandbox** token; registering it as `production` makes APNs answer
    /// `BadDeviceToken`, and the server then drops the route (every iPhone
    /// route vanished that way). App Store / TestFlight builds carry no
    /// embedded profile and are production; a simulator or unreadable profile
    /// falls back to the configuration.
    static func apnsEnvironment(provisioningProfile: Data?, isDebugBuild: Bool) -> String {
        if let profile = provisioningProfile, let signed = apsEnvironment(inProvisioningProfile: profile) {
            return signed == "development" ? "sandbox" : "production"
        }
        return isDebugBuild ? "sandbox" : "production"
    }

    /// `Entitlements.aps-environment` from an `embedded.mobileprovision` (a
    /// CMS envelope around a plain XML plist).
    static func apsEnvironment(inProvisioningProfile data: Data) -> String? {
        guard let start = data.range(of: Data("<?xml".utf8)),
              let end = data.range(of: Data("</plist>".utf8), in: start.lowerBound..<data.endIndex),
              let plist = try? PropertyListSerialization.propertyList(
                from: data.subdata(in: start.lowerBound..<end.upperBound), options: [], format: nil
              ) as? [String: Any],
              let entitlements = plist["Entitlements"] as? [String: Any],
              let value = entitlements["aps-environment"] as? String, !value.isEmpty else { return nil }
        return value
    }

    static func register(
        token: Data,
        kind: MobilePushRegistrationKind,
        taskID: String? = nil
    ) async throws -> MobilePushRouteRegistration {
        guard MagicianAccess.isConfigured else { throw RequestError.notConfigured }
        var request = URLRequest(
            url: MagicianAccess.baseURL
                .appendingPathComponent("api/magician/v2/devices/me/push")
        )
        request.httpMethod = "PUT"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try JSONEncoder().encode(RegisterBody(
            kind: kind,
            token: token.map { String(format: "%02x", $0) }.joined(),
            environment: environment,
            taskID: taskID
        ))
        let (data, response) = try await URLSession.shared.data(for: request)
        try validate(response, body: data)
        return try JSONDecoder().decode(MobilePushRouteRegistration.self, from: data)
    }

    static func registerWithRetry(
        token: Data,
        kind: MobilePushRegistrationKind,
        taskID: String? = nil
    ) async throws -> MobilePushRouteRegistration {
        var lastError: Error?
        for delaySeconds in [UInt64(0), 1, 2, 4] {
            try Task.checkCancellation()
            if delaySeconds > 0 {
                try await Task.sleep(for: .seconds(delaySeconds))
            }
            do {
                return try await register(token: token, kind: kind, taskID: taskID)
            } catch let error as RequestError {
                throw error
            } catch {
                lastError = error
            }
        }
        throw lastError ?? URLError(.cannotConnectToHost)
    }

    /// `profile` targets a specific machine (for example the one this iPhone
    /// just switched away from); nil means the active machine.
    static func unregister(
        kind: MobilePushRegistrationKind,
        taskID: String? = nil,
        expectedRevision: Int64? = nil,
        profile: MobileConnectionProfile? = nil
    ) async throws {
        guard let target = profile ?? MagicianAccess.connectionProfile else { return }
        var components = URLComponents(
            url: target.publicOrigin
                .appendingPathComponent("api/magician/v2/devices/me/push"),
            resolvingAgainstBaseURL: false
        )
        var query = [URLQueryItem(name: "kind", value: kind.rawValue)]
        if let taskID { query.append(URLQueryItem(name: "task_id", value: taskID)) }
        if let expectedRevision {
            query.append(URLQueryItem(name: "revision", value: String(expectedRevision)))
        }
        components?.queryItems = query
        guard let url = components?.url else { throw URLError(.badURL) }
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(
            &request,
            principal: target.principal,
            workspace: target.workspace,
            profile: target
        )
        let (data, response) = try await URLSession.shared.data(for: request)
        try validate(response, body: data)
    }

    static func unregisterWithRetry(
        kind: MobilePushRegistrationKind,
        taskID: String? = nil,
        expectedRevision: Int64? = nil,
        profile: MobileConnectionProfile? = nil
    ) async {
        var lastError: Error?
        for delaySeconds in [UInt64(0), 1, 2, 4] {
            if Task.isCancelled { return }
            if delaySeconds > 0 {
                try? await Task.sleep(for: .seconds(delaySeconds))
                if Task.isCancelled { return }
            }
            do {
                try await unregister(
                    kind: kind,
                    taskID: taskID,
                    expectedRevision: expectedRevision,
                    profile: profile
                )
                return
            } catch is RequestError {
                return
            } catch {
                lastError = error
            }
        }
        if lastError != nil {
            debugLog("Could not remove the remote mobile push route after bounded retries")
        }
    }

    /// Give route DELETEs for a machine being forgotten `limit` in all, then
    /// cancel what is still out: they are best-effort, and an unreachable
    /// machine must not keep them (and their retries) going.
    static func giveUp(on deletes: [Task<Void, Never>], after limit: Duration) {
        guard !deletes.isEmpty else { return }
        Task.detached {
            try? await Task.sleep(for: limit)
            for delete in deletes { delete.cancel() }
        }
    }

    static func shouldRetryHTTPStatus(_ statusCode: Int) -> Bool {
        shouldRetryHTTPFailure(statusCode, errorCode: nil)
    }

    static func shouldRetryHTTPFailure(_ statusCode: Int, errorCode: String?) -> Bool {
        (statusCode == 408 || statusCode == 429 || statusCode >= 500)
            && errorCode != "mobile_push_provider_not_configured"
    }

    private static func validate(_ response: URLResponse, body: Data) throws {
        guard let http = response as? HTTPURLResponse else {
            throw URLError(.badServerResponse)
        }
        guard !(200..<300).contains(http.statusCode) else { return }
        let errorCode = (try? JSONDecoder().decode(ErrorEnvelope.self, from: body))?.error
        if errorCode == "mobile_push_provider_not_configured" {
            throw RequestError.providerNotConfigured
        }
        if shouldRetryHTTPFailure(http.statusCode, errorCode: errorCode) {
            throw URLError(.cannotConnectToHost)
        }
        throw RequestError.rejected(http.statusCode)
    }
}

@MainActor
final class MobileGlanceRefreshGate {
    private var inFlight: Task<Bool, Never>?

    func run(_ operation: @escaping @MainActor () async -> Bool) async -> Bool {
        if let inFlight { return await inFlight.value }
        let task = Task { await operation() }
        inFlight = task
        let result = await task.value
        inFlight = nil
        return result
    }
}

/// Approve / Reject on an attention notification for a plain confirmation ask
/// (the backend adds `actions: approve_reject` and the APNs category). The
/// answer is posted to `/hitl/{correlation_id}/respond` like an answer given in
/// the app, so every other surface clears; the push itself never carries the
/// question, and both actions require an unlocked device.
enum AttentionNotificationActions {
    static let category = "MAGICAN_ATTENTION_APPROVAL"
    static let approve = "MAGICAN_ATTENTION_APPROVE"
    static let reject = "MAGICAN_ATTENTION_REJECT"

    static func register() {
        let approveAction = UNNotificationAction(
            identifier: approve,
            title: "Approve",
            options: [.authenticationRequired]
        )
        let rejectAction = UNNotificationAction(
            identifier: reject,
            title: "Reject",
            options: [.authenticationRequired, .destructive]
        )
        let approval = UNNotificationCategory(
            identifier: category,
            actions: [approveAction, rejectAction],
            intentIdentifiers: [],
            options: []
        )
        let center = UNUserNotificationCenter.current()
        center.getNotificationCategories { existing in
            var categories = existing.filter { $0.identifier != category }
            categories.insert(approval)
            center.setNotificationCategories(categories)
        }
    }

    /// Answer the ask a notification action was taken on. Returns `false` for
    /// any other action (the caller then opens the app as before).
    static func handle(actionIdentifier: String, userInfo: [AnyHashable: Any]) async -> Bool {
        guard actionIdentifier == approve || actionIdentifier == reject else { return false }
        guard let correlationID = userInfo["correlation_id"] as? String, !correlationID.isEmpty,
              let source = userInfo["source"] as? String, !source.isEmpty else { return false }
        let sent = await post(correlationID: correlationID, source: source, confirmed: actionIdentifier == approve)
        if sent {
            await MobileAtAGlanceUpdates.withdrawDeliveredAttention(correlationID: correlationID)
        } else {
            let content = UNMutableNotificationContent()
            content.title = "Couldn't send your answer"
            content.body = "Open Magican to answer it there."
            content.userInfo = userInfo
            content.threadIdentifier = "magican-attention"
            try? await UNUserNotificationCenter.current().add(
                UNNotificationRequest(identifier: "answer-failed-\(correlationID)", content: content, trigger: nil)
            )
        }
        return true
    }

    private static func post(correlationID: String, source: String, confirmed: Bool) async -> Bool {
        guard let base = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/hitl") else {
            return false
        }
        var request = URLRequest(url: base.appendingPathComponent(correlationID).appendingPathComponent("respond"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        let payload: [String: Any] = [
            "source": source,
            "value": ["type": "confirmation", "confirmed": confirmed],
            "correlation_id": correlationID,
            "channel": "ios_notification"
        ]
        request.httpBody = try? JSONSerialization.data(withJSONObject: payload)
        guard let (_, response) = try? await URLSession.shared.data(for: request),
              let http = response as? HTTPURLResponse else { return false }
        return (200..<300).contains(http.statusCode)
    }
}

/// One user-facing permission and health surface for Home Screen widget,
/// Attention notifications, and remote task Live Activity updates. Observation
/// and ambient microphone activities intentionally remain local-only.
@MainActor
final class MobileAtAGlanceUpdates: ObservableObject {
    static let shared = MobileAtAGlanceUpdates()

    @Published private(set) var authorizationStatus: UNAuthorizationStatus = .notDetermined
    @Published private(set) var statusMessage: String?
    private let glanceRefreshGate = MobileGlanceRefreshGate()
    private var notificationPromptInFlight = false

    var remoteRegistrationSupported: Bool {
        MobilePushBuildSupport.remoteNotificationsEnabled
    }

    var statusLabel: String {
        guard remoteRegistrationSupported else { return "Local only" }
        return switch authorizationStatus {
        case .authorized, .provisional, .ephemeral: "Allowed"
        case .denied: "Off"
        case .notDetermined: "Not set"
        @unknown default: "Unavailable"
        }
    }

    func refreshStatus() async {
        authorizationStatus = await UNUserNotificationCenter.current().notificationSettings()
            .authorizationStatus
    }

    /// True only while iOS will still show the system notification prompt.
    /// A denial cannot be asked again in-app; Settings opens iOS Settings.
    nonisolated static func shouldPromptForNotificationAuthorization(
        _ status: UNAuthorizationStatus
    ) -> Bool {
        status == .notDetermined
    }

    /// Launch asks while the system prompt is still available, then registers
    /// this iPhone when permission and a Magician connection are both present.
    /// Later visits do not raise the prompt again after the owner has answered.
    func requestRequiredPermissionsOnLaunch() async {
        await refreshStatus()
        guard remoteRegistrationSupported else {
            statusMessage = "Widgets and local Live Activities are ready. Remote alerts require a push-enabled build."
            return
        }
        if Self.shouldPromptForNotificationAuthorization(authorizationStatus) {
            guard !notificationPromptInFlight else { return }
            notificationPromptInFlight = true
            await enable()
            notificationPromptInFlight = false
            return
        }
        if authorizationStatus == .denied {
            statusMessage = "Notifications are off. Settings → Notifications opens iOS Settings so you can allow them."
            return
        }
        await configureIfAllowed()
    }

    func configureIfAllowed() async {
        await refreshStatus()
        guard remoteRegistrationSupported else {
            statusMessage = "Widgets and local Live Activities are ready. Remote alerts require a push-enabled build."
            return
        }
        guard authorizationStatus == .authorized
                || authorizationStatus == .provisional
                || authorizationStatus == .ephemeral else { return }
        guard MagicianAccess.isConfigured else { return }
        UIApplication.shared.registerForRemoteNotifications()
    }

    func enable() async {
        guard remoteRegistrationSupported else {
            await refreshStatus()
            statusMessage = "Widgets and local Live Activities are ready. Remote alerts require a push-enabled build."
            return
        }
        do {
            let allowed = try await UNUserNotificationCenter.current().requestAuthorization(
                options: [.alert, .badge, .sound]
            )
            await refreshStatus()
            if allowed {
                if MagicianAccess.isConfigured {
                    UIApplication.shared.registerForRemoteNotifications()
                    statusMessage = "Attention alerts are allowed; registering this iPhone."
                } else {
                    statusMessage = "Attention alerts are allowed. Connect this iPhone to enable remote updates."
                }
            } else {
                statusMessage = "Enable notifications in iOS Settings to receive remote updates."
            }
        } catch {
            statusMessage = "iOS could not enable notifications."
        }
    }

    /// Origin key of the machine that holds this iPhone's application push
    /// route. Only the active machine is registered: switching machines
    /// removes the route from the previous one before registering the new.
    nonisolated static let applicationPushOriginKey = "mobilePushApplicationOriginV1"

    func registeredApplicationToken(_ token: Data) {
        guard remoteRegistrationSupported else { return }
        Task {
            await retireApplicationRouteIfMachineChanged()
            do {
                let activeOrigin = MagicianAccess.activeOrigin
                _ = try await MobilePushRegistrationClient.registerWithRetry(
                    token: token,
                    kind: .application
                )
                if let activeOrigin, activeOrigin == MagicianAccess.activeOrigin {
                    MagicianAccess.store.set(activeOrigin, forKey: Self.applicationPushOriginKey)
                }
                statusMessage = "This iPhone’s Attention alert route is ready."
            } catch {
                statusMessage = MagicianAccess.isConfigured
                    ? "Connected, but the push route could not be registered yet."
                    : "Attention alerts are allowed. Connect this iPhone to enable remote updates."
            }
        }
    }

    /// Removes this iPhone's application push route from `profile`'s machine
    /// when that machine holds it. Call before forgetting a machine: the route
    /// stops being tracked at once, and the returned task (nil when there is
    /// none to remove) sends the DELETE with `profile`'s own credential.
    /// Cancelling it gives up.
    @discardableResult
    func retireApplicationRoute(for profile: MobileConnectionProfile) -> Task<Void, Never>? {
        guard remoteRegistrationSupported,
              let key = MobileConnectionProfiles.originKey(profile.publicOrigin),
              MagicianAccess.store.string(forKey: Self.applicationPushOriginKey) == key else { return nil }
        MagicianAccess.store.removeObject(forKey: Self.applicationPushOriginKey)
        return Task {
            await MobilePushRegistrationClient.unregisterWithRetry(kind: .application, profile: profile)
        }
    }

    /// The previous machine keeps pushing to this iPhone until its route is
    /// deleted, so a switch first removes it there (with that machine's own
    /// credential). A machine already forgotten has no credential left; its
    /// route is dropped from tracking only.
    private func retireApplicationRouteIfMachineChanged() async {
        guard let previous = MagicianAccess.store.string(forKey: Self.applicationPushOriginKey),
              previous != MagicianAccess.activeOrigin else { return }
        MagicianAccess.store.removeObject(forKey: Self.applicationPushOriginKey)
        guard let profile = MagicianAccess.profile(forOrigin: previous) else { return }
        await MobilePushRegistrationClient.unregisterWithRetry(kind: .application, profile: profile)
    }

    func registrationFailed() {
        guard remoteRegistrationSupported else { return }
        statusMessage = "iOS could not register this device for remote updates."
    }

    func handleRemotePayload(_ userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
        // Any push that wakes the app is a chance to clear asks that have
        // outlived their notification window.
        await Self.withdrawExpiredAttention()
        if let kind = userInfo["kind"] as? String {
            switch kind {
            case "attention_requested":
                AppActions.shared.markAttentionDirty()
            case "attention_resolved":
                AppActions.shared.markAttentionDirty()
                // Answered on another surface: take its banner down, so the
                // phone never offers an ask that is already settled.
                if let correlationID = userInfo["correlation_id"] as? String, !correlationID.isEmpty {
                    await Self.withdrawDeliveredAttention(correlationID: correlationID)
                }
            case "attention_expired":
                // The ask's notification window closed (the ask itself stays
                // open in Attention): iOS has no timeout for a delivered
                // notification, so the backend says when to take it down.
                if let correlationID = userInfo["correlation_id"] as? String, !correlationID.isEmpty {
                    await Self.withdrawDeliveredAttention(correlationID: correlationID)
                }
            default:
                break
            }
        }
        return await refreshGlanceSnapshot() ? .newData : .noData
    }

    /// Remove delivered attention notifications for one ask (matched on the
    /// `correlation_id` the backend puts in every attention push).
    static func withdrawDeliveredAttention(correlationID: String) async {
        let center = UNUserNotificationCenter.current()
        let delivered = await center.deliveredNotifications()
        let identifiers = delivered
            .filter { ($0.request.content.userInfo["correlation_id"] as? String) == correlationID }
            .map(\.request.identifier)
        if !identifiers.isEmpty {
            center.removeDeliveredNotifications(withIdentifiers: identifiers)
        }
    }

    /// Remove delivered attention notifications past their `expires_at`
    /// (epoch ms, set by the backend: the ask's deadline or its TTL). Backs up
    /// the backend's `attention_expired`, which a restart or a throttled
    /// silent push can lose.
    static func withdrawExpiredAttention(now: Date = Date()) async {
        let center = UNUserNotificationCenter.current()
        let delivered = await center.deliveredNotifications()
        let identifiers = delivered
            .filter { attentionHasExpired($0.request.content.userInfo, now: now) }
            .map(\.request.identifier)
        if !identifiers.isEmpty {
            center.removeDeliveredNotifications(withIdentifiers: identifiers)
        }
    }

    nonisolated static func attentionHasExpired(_ userInfo: [AnyHashable: Any], now: Date) -> Bool {
        let raw = userInfo["expires_at"]
        let expiresAtMs: Double? = (raw as? NSNumber)?.doubleValue ?? (raw as? String).flatMap(Double.init)
        guard let expiresAtMs, expiresAtMs > 0 else { return false }
        return expiresAtMs / 1000 <= now.timeIntervalSince1970
    }

    func openRemotePayload(_ userInfo: [AnyHashable: Any]) {
        guard let raw = userInfo["deep_link"] as? String,
              let url = URL(string: raw), MagicanAppURL.isScheme(url.scheme) else { return }
        if url.host == "attention" {
            let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
            let itemID = components?.queryItems?.first(where: {
                $0.name == "item" || $0.name == "correlation_id"
            })?.value
            AppActions.shared.requestAttention(itemID: itemID)
        } else if let target = WorkNavigationTarget.parse(url.absoluteString) {
            AppActions.shared.requestWork(target)
        } else if url.host == "task" {
            AppActions.shared.requestTask(url.pathComponents.dropFirst().first)
        }
    }

    private func refreshGlanceSnapshot() async -> Bool {
        await glanceRefreshGate.run { [weak self] in
            guard let self else { return false }
            return await self.performGlanceRefresh()
        }
    }

    private func performGlanceRefresh() async -> Bool {
        guard MagicianAccess.isConfigured else { return false }
        var components = URLComponents(
            url: MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/today"),
            resolvingAgainstBaseURL: false
        )
        components?.queryItems = [
            URLQueryItem(name: "per_section", value: "1"),
            URLQueryItem(name: "digest_limit", value: "1")
        ]
        guard let url = components?.url else { return false }
        var request = URLRequest(url: url)
        request.timeoutInterval = 12
        MagicianAccess.authorize(&request)
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode),
              let snapshot = try? MagicanGlanceSnapshot.reducingToday(data) else { return false }
        if MagicanGlanceCache.save(snapshot) {
            WidgetCenter.shared.reloadTimelines(ofKind: "MagiosWidget")
            return true
        }
        return false
    }
}
