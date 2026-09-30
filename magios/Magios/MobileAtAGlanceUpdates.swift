import ActivityKit
import Combine
import Foundation
import UIKit
import UserNotifications
import WidgetKit

enum MobilePushRegistrationKind: String, Codable {
    case application
    case taskActivity = "task_activity"
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

    static var environment: String {
        #if DEBUG
        "sandbox"
        #else
        "production"
        #endif
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

    static func unregister(
        kind: MobilePushRegistrationKind,
        taskID: String? = nil,
        expectedRevision: Int64? = nil
    ) async throws {
        guard MagicianAccess.isConfigured else { return }
        var components = URLComponents(
            url: MagicianAccess.baseURL
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
        MagicianAccess.authorize(&request)
        let (data, response) = try await URLSession.shared.data(for: request)
        try validate(response, body: data)
    }

    static func unregisterWithRetry(
        kind: MobilePushRegistrationKind,
        taskID: String? = nil,
        expectedRevision: Int64? = nil
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
                    expectedRevision: expectedRevision
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

/// One user-facing permission and health surface for Home Screen widget,
/// Attention notifications, and remote task Live Activity updates. Observation
/// and ambient microphone activities intentionally remain local-only.
@MainActor
final class MobileAtAGlanceUpdates: ObservableObject {
    static let shared = MobileAtAGlanceUpdates()

    @Published private(set) var authorizationStatus: UNAuthorizationStatus = .notDetermined
    @Published private(set) var statusMessage: String?
    private let glanceRefreshGate = MobileGlanceRefreshGate()

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

    func registeredApplicationToken(_ token: Data) {
        guard remoteRegistrationSupported else { return }
        Task {
            do {
                _ = try await MobilePushRegistrationClient.registerWithRetry(
                    token: token,
                    kind: .application
                )
                statusMessage = "This iPhone’s Attention alert route is ready."
            } catch {
                statusMessage = MagicianAccess.isConfigured
                    ? "Connected, but the push route could not be registered yet."
                    : "Attention alerts are allowed. Connect this iPhone to enable remote updates."
            }
        }
    }

    func registrationFailed() {
        guard remoteRegistrationSupported else { return }
        statusMessage = "iOS could not register this device for remote updates."
    }

    func handleRemotePayload(_ userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
        if let kind = userInfo["kind"] as? String {
            switch kind {
            case "attention_requested", "attention_resolved":
                AppActions.shared.markAttentionDirty()
            default:
                break
            }
        }
        return await refreshGlanceSnapshot() ? .newData : .noData
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
