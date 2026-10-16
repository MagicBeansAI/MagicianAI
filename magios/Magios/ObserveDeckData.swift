import ActivityKit
import AVFoundation
import Foundation
import UIKit
import UserNotifications

// Networking + view models behind the Observe deck's Recent list, Sources view
// (view-only web & account blocks + this phone's capture settings) and Audio
// view. Decoding lives in `ObserveDeckModel.swift`.

// MARK: - Client

/// Small JSON client for the read-mostly deck endpoints. `session` is injectable
/// so tests drive it via `MockURLProtocol`.
struct ObserveDeckClient {
    var session: URLSession = .shared
    var baseURL: URL = MagicianAccess.baseURL
    var timeout: TimeInterval = 15

    func getJSON(_ path: String, query: [URLQueryItem] = []) async throws -> [String: Any] {
        var comps = URLComponents(url: baseURL.appendingPathComponent(path), resolvingAgainstBaseURL: false)!
        if !query.isEmpty { comps.queryItems = query }
        var req = URLRequest(url: comps.url!, timeoutInterval: timeout)
        req.httpMethod = "GET"
        MagicianAccess.authorize(&req)
        return try await perform(req)
    }

    func putJSON(_ path: String, body: [String: Any]) async throws -> [String: Any] {
        var req = URLRequest(url: baseURL.appendingPathComponent(path), timeoutInterval: timeout)
        req.httpMethod = "PUT"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&req)
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        return try await perform(req)
    }

    private func perform(_ req: URLRequest) async throws -> [String: Any] {
        let (data, resp) = try await session.data(for: req)
        if let http = resp as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            throw ObserveDeckError.http(http.statusCode, obj?["error"] as? String)
        }
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw URLError(.cannotParseResponse)
        }
        return obj
    }
}

enum ObserveDeckError: LocalizedError {
    case http(Int, String?)

    var errorDescription: String? {
        switch self {
        case .http(let code, let message):
            if let message, !message.isEmpty { return message }
            return "The server returned HTTP \(code)."
        }
    }

    static func describe(_ error: Error) -> String {
        if let e = error as? ObserveDeckError { return e.errorDescription ?? "Request failed." }
        if let e = error as? URLError {
            switch e.code {
            case .notConnectedToInternet, .networkConnectionLost, .cannotConnectToHost, .cannotFindHost, .dnsLookupFailed, .timedOut:
                return "Magician is unreachable — check your connection."
            default: break
            }
        }
        return "Request failed."
    }
}

/// One independently loading block: its last good value, whether a load is in
/// flight, and the last failure (kept alongside a stale value).
struct ObserveBlock<Value: Equatable>: Equatable {
    var value: Value?
    var loading = false
    var error: String?
    /// True once any load has finished (success or failure).
    var settled = false
}

// MARK: - Recent

@MainActor
final class ObserveRecentViewModel: ObservableObject {
    @Published private(set) var block = ObserveBlock<[RecentCapture]>()

    private let meetings: MeetingsClient

    init(meetings: MeetingsClient = MeetingsClient()) { self.meetings = meetings }

    /// Meetings are required; screen-watch sessions are best-effort extras.
    func reload() async {
        block.loading = true
        defer { block.loading = false; block.settled = true }
        do {
            let recent = try await meetings.fetchRecent()
            let watch = (try? await meetings.fetchWatchSessions()) ?? []
            block.value = RecentCapture.merged(recent, watch)
            block.error = nil
        } catch {
            block.error = "Couldn't load recent captures. \(ObserveDeckError.describe(error))"
        }
    }
}

// MARK: - Sources (web & accounts, view-only)

@MainActor
final class ObserveSourcesViewModel: ObservableObject {
    @Published private(set) var channels = ObserveBlock<[ObserveChannel]>()
    @Published private(set) var calendar = ObserveBlock<ObserveCalendarStatus>()
    @Published private(set) var subscriptions = ObserveBlock<[ObserveSubscription]>()
    @Published private(set) var enabledSubscriptionTotal: Int?
    @Published private(set) var ambient = ObserveBlock<ObserveAmbientStatus>()
    @Published private(set) var catchUp = ObserveBlock<ObserveCatchUpStatus>()

    private let client: ObserveDeckClient

    init(client: ObserveDeckClient = ObserveDeckClient()) { self.client = client }

    var enabledCount: Int? {
        ObserveSourcesCount.enabled(
            channels: channels.value,
            calendar: calendar.value,
            ambient: ambient.value,
            enabledSubscriptions: enabledSubscriptionTotal
        )
    }

    /// Every block loads independently; one failure never blanks the others.
    func reloadAll() async {
        async let a: Void = reloadChannels()
        async let b: Void = reloadCalendar()
        async let c: Void = reloadSubscriptions()
        async let d: Void = reloadAmbient()
        async let e: Void = reloadCatchUp()
        _ = await (a, b, c, d, e)
    }

    func reloadChannels() async {
        await load(\.channels) { client in
            ObserveChannel.list(from: try await client.getJSON("/api/magician/v2/channel-assist/channels"))
        }
    }

    func reloadCalendar() async {
        await load(\.calendar) { client in
            ObserveCalendarStatus(json: try await client.getJSON("/api/magician/v2/observe/calendar/status"))
        }
    }

    func reloadSubscriptions() async {
        await load(\.subscriptions) { [weak self] client in
            let page = ObserveSubscription.page(from: try await client.getJSON(
                "/api/magician/v2/observe/subscriptions",
                query: [URLQueryItem(name: "enabled", value: "true"), URLQueryItem(name: "limit", value: "20")]
            ))
            await MainActor.run { self?.enabledSubscriptionTotal = page.total }
            return page.items
        }
    }

    func reloadAmbient() async {
        await load(\.ambient) { client in
            ObserveAmbientStatus(json: try await client.getJSON("/api/magician/v2/ambient/status"))
        }
    }

    func reloadCatchUp() async {
        await load(\.catchUp) { client in
            ObserveCatchUpStatus(json: try await client.getJSON("/api/magician/v2/observe/catch-up"))
        }
    }

    private func load<V: Equatable>(
        _ key: ReferenceWritableKeyPath<ObserveSourcesViewModel, ObserveBlock<V>>,
        _ fetch: @escaping (ObserveDeckClient) async throws -> V
    ) async {
        self[keyPath: key].loading = true
        do {
            let value = try await fetch(client)
            self[keyPath: key].value = value
            self[keyPath: key].error = nil
        } catch {
            self[keyPath: key].error = ObserveDeckError.describe(error)
        }
        self[keyPath: key].loading = false
        self[keyPath: key].settled = true
    }
}

// MARK: - Audio profiles

@MainActor
final class ObserveAudioProfilesViewModel: ObservableObject {
    @Published private(set) var catalog = ObserveAudioCatalog.empty
    @Published private(set) var selections: [ObserveAudioSurface: String] = [:]
    @Published private(set) var loading = false
    @Published private(set) var loaded = false
    @Published private(set) var loadError: String?
    @Published private(set) var saving: ObserveAudioSurface?
    @Published var saveError: String?

    private let client: ObserveDeckClient

    init(client: ObserveDeckClient = ObserveDeckClient()) { self.client = client }

    /// 2 (meeting + listening) once preferences load; nil when unavailable.
    var configuredSurfaces: Int? { loaded ? ObserveAudioSurface.allCases.count : nil }

    func profiles(for surface: ObserveAudioSurface) -> [ObserveAudioProfile] {
        catalog.profiles[surface] ?? []
    }

    /// The effective profile id: the explicit selection, else the configured default.
    func activeProfileId(for surface: ObserveAudioSurface) -> String? {
        selections[surface] ?? catalog.defaults[surface]
    }

    func reload() async {
        loading = true
        defer { loading = false }
        do {
            let prefs = try await client.getJSON("/api/magician/v2/media/preferences")
            selections = ObserveAudioPreferences.selections(from: prefs)
            loaded = true
            loadError = nil
        } catch {
            loadError = "Couldn't load audio preferences. \(ObserveDeckError.describe(error))"
        }
        // The catalog is what makes the pickers useful, but the selection still
        // shows without it.
        if let providers = try? await client.getJSON("/api/magician/v2/media/providers") {
            catalog = ObserveAudioCatalog.decode(providers: providers)
        }
    }

    /// Optimistic save with rollback. `profileId == nil` → configured default.
    func select(_ profileId: String?, for surface: ObserveAudioSurface) async {
        guard saving == nil else { return }
        let previous = selections
        if let profileId { selections[surface] = profileId } else { selections.removeValue(forKey: surface) }
        saving = surface
        saveError = nil
        defer { saving = nil }
        do {
            let saved = try await client.putJSON(
                "/api/magician/v2/media/preferences",
                body: ObserveAudioPreferences.patch(surface: surface, profileId: profileId)
            )
            selections = ObserveAudioPreferences.selections(from: saved)
        } catch {
            selections = previous
            saveError = "Couldn't save the \(surface.label.lowercased()) profile. \(ObserveDeckError.describe(error))"
        }
    }
}

// MARK: - This phone (device capture settings)

/// Live permission state for the phone's own capture: microphone, notifications
/// and Live Activities. Refreshed on appear and whenever the app returns to the
/// foreground (the user may have flipped a switch in Settings).
@MainActor
final class ObserveDeviceStatus: ObservableObject {
    enum Permission: Equatable {
        case granted, denied, notAsked

        var label: String {
            switch self {
            case .granted: return "Allowed"
            case .denied: return "Denied"
            case .notAsked: return "Not asked yet"
            }
        }
    }

    @Published private(set) var microphone: Permission = .notAsked
    @Published private(set) var notifications: Permission = .notAsked
    @Published private(set) var liveActivitiesEnabled = false

    func refresh() async {
        switch AVAudioApplication.shared.recordPermission {
        case .granted: microphone = .granted
        case .denied: microphone = .denied
        default: microphone = .notAsked
        }
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral: notifications = .granted
        case .denied: notifications = .denied
        default: notifications = .notAsked
        }
        liveActivitiesEnabled = ActivityAuthorizationInfo().areActivitiesEnabled
    }

    func requestMicrophone() async {
        _ = await AVAudioApplication.requestRecordPermission()
        await refresh()
    }

    func requestNotifications() async {
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound])
        await refresh()
    }

    func openSystemSettings() {
        if let url = URL(string: UIApplication.openSettingsURLString) {
            UIApplication.shared.open(url)
        }
    }
}
