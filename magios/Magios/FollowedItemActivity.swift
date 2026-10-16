import ActivityKit
import Foundation
import SwiftUI
import UIKit

/// Follow: a Live Activity of its own for one coding run, crew or Work task
/// the owner chose, until it ends (`FollowedItemActivityAttributes`).
///
/// Following asks the server first (`POST /work/follow`, at most three per
/// scope; a fourth answers 409 `follow_limit`, shown to the owner), then
/// starts the activity here and registers its push token as a `work_item`
/// route carrying the item id in `task_id`, so the server moves it while the
/// app is closed and ends it with the item's final state. Unfollowing deletes
/// the follow (`DELETE /work/follow/{item_id}`), ends the activity and
/// removes the route.
///
/// In the foreground the summary poll (`CodingRunsActivity`) hands every
/// `GET /work/live` answer here: `followed` drives the Follow buttons, items
/// still followed are updated, items that ended are ended with their final
/// state, and an activity whose item is no longer followed ends at once. An
/// answer asked before the last follow or unfollow here does not change the
/// follow list, so a slow poll cannot undo a tap. A machine switch ends every
/// followed activity (they follow the previous machine's items).
@MainActor
final class FollowedItemActivity: ObservableObject {
    static let shared = FollowedItemActivity()

    /// The server's follow limit per scope.
    static let limit = 3

    /// What a screen knows about an item, for the activity's first content:
    /// `/work/live` lists an item only once it has run 60 s or needs the
    /// owner, and its own state replaces this once it does.
    struct Seed: Equatable {
        /// `run:<id>`, `crew:<id>` or `task:<id>`.
        let itemID: String
        /// `coding_run`, `crew` or `work_task`.
        let kind: String
        let title: String
        var context: String = ""
        var state: String = "working"
        var deepLink: String? = nil
    }

    /// Item ids this scope follows, as the server last said.
    @Published private(set) var followed: Set<String> = []
    /// Item ids with a follow or unfollow in flight.
    @Published private(set) var busy: Set<String> = []

    /// A `work_item` route: which activity's token, on which machine, at
    /// which server revision.
    private struct Route {
        let activityID: String
        let origin: String
        let revision: Int64
    }

    private var routes: [String: Route] = [:]
    /// Push-token observers, by activity id.
    private var tokenTasks: [String: Task<Void, Never>] = [:]
    /// The tail of the route queue: PUTs and DELETEs run one at a time.
    private var routeChain: Task<Void, Never>?
    private var connectionObserver: NSObjectProtocol?
    private var connectedOrigin: String?
    /// When the last follow or unfollow was sent here.
    private var lastMutation = Date.distantPast
    private var lastRefresh = Date.distantPast

    private enum Failure: Error {
        case limit
        case rejected
    }

    private struct FollowedEnvelope: Decodable {
        let followed: [String]
    }

    private struct ErrorEnvelope: Decodable {
        let error: String?
    }

    /// Push type for a locally requested activity.
    static var pushType: PushType? {
        MobilePushBuildSupport.remoteNotificationsEnabled ? .token : nil
    }

    func isFollowed(_ itemID: String) -> Bool { followed.contains(itemID) }

    /// Launch and every foreground: watch the machine in use and re-observe
    /// the tokens of activities that survived a relaunch. The summary poll
    /// that `CodingRunsActivity.resume` starts brings the follow list.
    func resume() {
        observeConnection()
        for activity in Self.running() { observeToken(activity) }
    }

    /// Read `/work/live` now unless it was read in the last 20 seconds (a
    /// screen with Follow buttons calls this when it appears).
    func refreshIfStale() {
        guard Date().timeIntervalSince(lastRefresh) > 20 else { return }
        refresh()
    }

    func refresh() {
        guard MagicianAccess.isConfigured, let origin = MagicianAccess.activeOrigin else { return }
        lastRefresh = Date()
        let askedAt = Date()
        Task {
            guard let live = await Self.fetchLive() else { return }
            FollowedItemActivity.shared.apply(live, from: origin, askedAt: askedAt)
        }
    }

    // MARK: Follow / unfollow

    /// Follow the item and start its activity. Returns what to tell the owner
    /// when it did not work, nil when it did.
    func follow(_ seed: Seed) async -> String? {
        guard MagicianAccess.isConfigured else { return "Connect to your Magician first." }
        guard !busy.contains(seed.itemID) else { return nil }
        busy.insert(seed.itemID)
        defer { busy.remove(seed.itemID) }
        lastMutation = Date()
        do {
            let ids = try await send("POST", url: Self.followURL(nil), body: ["item_id": seed.itemID])
            followed = Set(ids)
        } catch Failure.limit {
            return "You already follow \(Self.limit) items. Unfollow one to follow this."
        } catch {
            return "Couldn't follow this right now. Try again in a moment."
        }
        lastMutation = Date()
        start(seed)
        refresh()
        return nil
    }

    /// Stop following the item and end its activity.
    func unfollow(_ itemID: String) async -> String? {
        guard MagicianAccess.isConfigured else { return "Connect to your Magician first." }
        guard !busy.contains(itemID) else { return nil }
        busy.insert(itemID)
        defer { busy.remove(itemID) }
        lastMutation = Date()
        do {
            let ids = try await send("DELETE", url: Self.followURL(itemID), body: nil)
            followed = Set(ids)
        } catch Failure.rejected {
            // Not followed there (it ended and left the list, say): end it here too.
            followed.remove(itemID)
        } catch {
            return "Couldn't unfollow this right now. Try again in a moment."
        }
        lastMutation = Date()
        for activity in Self.running() where activity.attributes.itemId == itemID {
            finish(activity, finalState: nil, dismissal: .immediate, deleteUnrecorded: true)
        }
        return nil
    }

    // MARK: Keeping activities in step

    /// Apply a `/work/live` answer from `origin`, asked at `askedAt`.
    func apply(_ live: WorkLive, from origin: String, askedAt: Date) {
        guard origin == MagicianAccess.activeOrigin else { return }
        lastRefresh = max(lastRefresh, askedAt)
        // An answer asked before the last follow or unfollow here may not
        // know about it: it updates content, never the follow list.
        let current = askedAt > lastMutation
        if current {
            let ids = Set(live.followed)
            if ids != followed { followed = ids }
        }
        var items: [String: WorkLiveItem] = [:]
        for item in live.items + live.next + [live.featured].compactMap({ $0 }) where items[item.id] == nil {
            items[item.id] = item
        }
        var kept = 0
        for activity in Self.running() {
            let itemID = activity.attributes.itemId
            if current && !followed.contains(itemID) {
                finish(activity, finalState: nil, dismissal: .immediate, deleteUnrecorded: true)
                continue
            }
            if kept >= Self.limit {
                finish(activity, finalState: nil, dismissal: .immediate, deleteUnrecorded: true)
                continue
            }
            kept += 1
            observeToken(activity)
            guard let item = items[itemID] else { continue }
            let state = FollowedItemActivityAttributes.ContentState(item: item)
            if item.isEnded {
                finish(activity, finalState: state, dismissal: .default, deleteUnrecorded: false)
            } else if state != activity.content.state {
                let content = ActivityContent(
                    state: state,
                    staleDate: nil,
                    relevanceScore: FollowedItemActivityAttributes.ContentState.relevanceScore
                )
                Task { await activity.update(content) }
            }
        }
    }

    private static func running() -> [Activity<FollowedItemActivityAttributes>] {
        Activity<FollowedItemActivityAttributes>.activities.filter {
            $0.activityState == .active || $0.activityState == .stale
        }
    }

    /// Start the item's activity unless one runs already. A request from the
    /// background fails; the follow stands, and the activity starts on the
    /// next Follow tap.
    private func start(_ seed: Seed) {
        if let existing = Self.running().first(where: { $0.attributes.itemId == seed.itemID }) {
            observeToken(existing)
            return
        }
        guard UIApplication.shared.applicationState == .active,
              ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        let state = FollowedItemActivityAttributes.ContentState(
            state: seed.state,
            title: seed.title,
            context: seed.context,
            deepLink: seed.deepLink,
            updatedAt: Date().timeIntervalSince1970
        )
        guard let started = try? Activity.request(
            attributes: FollowedItemActivityAttributes(itemId: seed.itemID, kind: seed.kind),
            content: ActivityContent(
                state: state,
                staleDate: nil,
                relevanceScore: FollowedItemActivityAttributes.ContentState.relevanceScore
            ),
            pushType: Self.pushType
        ) else { return }
        observeToken(started)
    }

    /// End `activity` (with `finalState` when the item ended) and remove its route.
    /// `deleteUnrecorded` also removes a route this launch did not record
    /// (one an earlier launch made), on the machine in use.
    private func finish(
        _ activity: Activity<FollowedItemActivityAttributes>,
        finalState: FollowedItemActivityAttributes.ContentState?,
        dismissal: ActivityUIDismissalPolicy,
        deleteUnrecorded: Bool
    ) {
        let activityID = activity.id
        let itemID = activity.attributes.itemId
        tokenTasks[activityID]?.cancel()
        tokenTasks[activityID] = nil
        let content = finalState.map {
            ActivityContent(
                state: $0,
                staleDate: nil,
                relevanceScore: FollowedItemActivityAttributes.ContentState.relevanceScore
            )
        }
        Task { await activity.end(content, dismissalPolicy: dismissal) }
        guard MobilePushBuildSupport.remoteNotificationsEnabled else { return }
        enqueueRoute {
            await FollowedItemActivity.shared.deleteRoute(
                itemID: itemID, activityID: activityID, deleteUnrecorded: deleteUnrecorded)
        }
    }

    // MARK: Routes

    private func observeToken(_ activity: Activity<FollowedItemActivityAttributes>) {
        guard MobilePushBuildSupport.remoteNotificationsEnabled, tokenTasks[activity.id] == nil else { return }
        let activityID = activity.id
        let itemID = activity.attributes.itemId
        tokenTasks[activityID] = Task {
            for await token in activity.pushTokenUpdates {
                if Task.isCancelled { return }
                FollowedItemActivity.shared.enqueueRoute {
                    await FollowedItemActivity.shared.putRoute(token, itemID: itemID, activityID: activityID)
                }
            }
        }
    }

    /// PUT `token` as the item's `work_item` route while its activity still
    /// runs. Only ever called from the route queue.
    private func putRoute(_ token: Data, itemID: String, activityID: String) async {
        guard Self.running().contains(where: { $0.id == activityID }),
              let origin = MagicianAccess.activeOrigin else { return }
        do {
            let route = try await MobilePushRegistrationClient.registerWithRetry(
                token: token, kind: .workItem, taskID: itemID)
            routes[itemID] = Route(activityID: activityID, origin: origin, revision: route.revision)
        } catch {
            debugLog("Could not register followed-item activity updates")
        }
    }

    /// Delete the item's route after its activity ended, on the machine that
    /// holds it and only at the revision its PUT returned. A route a newer
    /// activity for the same item holds stays. Only ever called from the
    /// route queue.
    private func deleteRoute(itemID: String, activityID: String, deleteUnrecorded: Bool) async {
        if let route = routes[itemID] {
            guard route.activityID == activityID else { return }
            routes[itemID] = nil
            guard let profile = MagicianAccess.profile(forOrigin: route.origin) else { return }
            await MobilePushRegistrationClient.unregisterWithRetry(
                kind: .workItem,
                taskID: itemID,
                expectedRevision: route.revision,
                profile: profile
            )
        } else if deleteUnrecorded, !Self.running().contains(where: { $0.attributes.itemId == itemID }) {
            await MobilePushRegistrationClient.unregisterWithRetry(kind: .workItem, taskID: itemID)
        }
    }

    @discardableResult
    private func enqueueRoute(_ operation: @escaping @MainActor () async -> Void) -> Task<Void, Never> {
        let previous = routeChain
        let next = Task { @MainActor in
            await previous?.value
            await operation()
        }
        routeChain = next
        return next
    }

    // MARK: Machine in use

    private func observeConnection() {
        guard connectionObserver == nil else { return }
        connectedOrigin = MagicianAccess.activeOrigin
        connectionObserver = NotificationCenter.default.addObserver(
            forName: .magicianMobileConnectionDidChange, object: nil, queue: .main
        ) { _ in
            Task { @MainActor in FollowedItemActivity.shared.connectionChanged() }
        }
    }

    /// The followed items belong to the previous machine: end their
    /// activities, and remove their routes there while its credential lasts.
    private func connectionChanged() {
        let origin = MagicianAccess.activeOrigin
        guard origin != connectedOrigin else { return }
        connectedOrigin = origin
        followed = []
        lastMutation = Date()
        for activity in Self.running() {
            finish(activity, finalState: nil, dismissal: .immediate, deleteUnrecorded: false)
        }
    }

    // MARK: HTTP

    private static func followURL(_ itemID: String?) -> URL {
        let base = MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/work/follow")
        return itemID.map { base.appendingPathComponent($0) } ?? base
    }

    /// Send a follow change; the answer is the scope's follow list.
    private func send(_ method: String, url: URL, body: [String: String]?) async throws -> [String] {
        var request = URLRequest(url: url, timeoutInterval: 15)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONEncoder().encode(body)
        }
        MagicianAccess.authorize(&request)
        let (data, response) = try await URLSession.shared.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        if status == 409, (try? JSONDecoder().decode(ErrorEnvelope.self, from: data))?.error == "follow_limit" {
            throw Failure.limit
        }
        if status == 404 { throw Failure.rejected }
        guard (200..<300).contains(status) else { throw URLError(.badServerResponse) }
        return (try? JSONDecoder().decode(FollowedEnvelope.self, from: data))?.followed ?? []
    }

    private static func fetchLive() async -> WorkLive? {
        var components = URLComponents(
            url: MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/work/live"),
            resolvingAgainstBaseURL: false
        )
        components?.queryItems = [URLQueryItem(name: "device", value: MagicianAccess.deviceID)]
        guard let url = components?.url else { return nil }
        var request = URLRequest(url: url, timeoutInterval: 15)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&request)
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else { return nil }
        return WorkLiveSummary.parse(data)
    }

    // MARK: Seeds for the screens that offer Follow

    nonisolated static func runSeed(runID: String, projectID: String, title: String, projectName: String,
                        state: String) -> Seed {
        Seed(itemID: "run:\(runID)", kind: "coding_run", title: title, context: projectName, state: state,
             deepLink: vibeLink(project: projectID, key: "run", id: runID))
    }

    nonisolated static func crewSeed(crewID: String, projectID: String?, goal: String, projectName: String?,
                         needsYou: Bool) -> Seed {
        Seed(itemID: "crew:\(crewID)", kind: "crew", title: goal, context: projectName ?? "",
             state: needsYou ? "needs_you" : "working",
             deepLink: vibeLink(project: projectID, key: "crew", id: crewID))
    }

    nonisolated static func taskSeed(taskID: String, title: String) -> Seed {
        var components = URLComponents()
        components.scheme = "magican"
        components.host = "work"
        components.queryItems = [URLQueryItem(name: "task", value: taskID)]
        return Seed(itemID: "task:\(taskID)", kind: "work_task", title: title,
                    deepLink: components.url?.absoluteString)
    }

    nonisolated private static func vibeLink(project: String?, key: String, id: String) -> String? {
        var components = URLComponents()
        components.scheme = "magican"
        components.host = "vibe"
        var query: [URLQueryItem] = []
        if let project, !project.isEmpty { query.append(URLQueryItem(name: "project", value: project)) }
        query.append(URLQueryItem(name: key, value: id))
        components.queryItems = query
        return components.url?.absoluteString
    }

    /// A Work task status that means the task is over (Follow is not offered).
    nonisolated static func isEndedTaskStatus(_ status: String) -> Bool {
        ["completed", "complete", "done", "succeeded", "failed", "error", "cancelled", "canceled", "stopped"]
            .contains(status.lowercased())
    }
}

/// Follow / Following for a run, crew or task screen. Following starts the
/// item's own Live Activity; the follow-limit answer is shown in an alert.
struct WorkFollowButton: View {
    let seed: FollowedItemActivity.Seed
    /// Icon only (a toolbar).
    var iconOnly = false

    @ObservedObject private var follows = FollowedItemActivity.shared
    @ObservedObject private var theme = ThemeManager.shared
    @State private var message: String?

    var body: some View {
        let isFollowed = follows.followed.contains(seed.itemID)
        Button {
            Task {
                if isFollowed {
                    message = await follows.unfollow(seed.itemID)
                } else {
                    message = await follows.follow(seed)
                }
            }
        } label: {
            if iconOnly {
                Image(systemName: isFollowed ? "bell.fill" : "bell")
                    .frame(minWidth: Lively.tapMin, minHeight: Lively.tapMin)
                    .contentShape(Rectangle())
            } else {
                Label(isFollowed ? "Following" : "Follow", systemImage: isFollowed ? "bell.fill" : "bell")
                    .font(.themed(12, weight: .bold))
                    .foregroundStyle(isFollowed ? theme.accentColor : theme.secondaryTextColor)
                    .frame(minHeight: Lively.tapMin)
                    .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .disabled(follows.busy.contains(seed.itemID))
        .accessibilityLabel(isFollowed ? "Unfollow" : "Follow")
        .accessibilityHint(isFollowed ? "Ends its Live Activity" : "Shows it as its own Live Activity until it ends")
        .alert("Follow", isPresented: Binding(get: { message != nil }, set: { if !$0 { message = nil } })) {
            Button("OK", role: .cancel) { message = nil }
        } message: {
            Text(message ?? "")
        }
        .onAppear { follows.refreshIfStale() }
    }
}
