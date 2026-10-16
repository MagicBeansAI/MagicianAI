import ActivityKit
import Foundation
import UIKit

/// Keeps the "Magician at work" Live Activity (`CodingRunsActivityAttributes`)
/// in step with the server's work summary (`GET /work/live`: coding runs,
/// crews and Work tasks, one featured): starts it when something is going,
/// updates it, and ends it when nothing is. A server without `/work/live`
/// (404) is read on `GET /vibedev/runs/live` instead, coding runs only, until
/// the app next comes to the front. Refreshed when the app becomes active and
/// every 30 seconds while it is; the same answer refreshes the followed-item
/// activities (`FollowedItemActivity`). After a relaunch it adopts the
/// activity that survived.
///
/// With remote push built in (`MobilePushBuildSupport`), the activity is
/// requested with a push token and that token is registered as `coding_runs`,
/// so the server moves it while the app is closed; the push-to-start token
/// (iOS 17.2+) is registered as `coding_runs_start`, so the server can start
/// it; and an activity the server started is adopted here, so the app keeps
/// updating it and registers its token. Without push, only the local poll runs.
///
/// There is only ever one, chosen the same way every time
/// (`CodingRunsAdoption`): the newest running activity is kept and every
/// other one ends at once; an activity ended here is never adopted again.
/// Remote observation also starts at launch (`MagiosAppDelegate`), so a
/// background launch for a server-started activity registers its token.
///
/// Both routes belong to the machine in use. A machine switch ends the
/// activity (it counts the previous machine's runs), removes the routes from
/// the previous machine while its credential lasts, and sends the push-to-start
/// token to the new one; forgetting a machine removes them there, best-effort
/// and without holding the forget up (`retireRoutes(for:)`). Otherwise route
/// PUTs and DELETEs run one at a time, in the order asked, and a DELETE names
/// the revision its PUT returned, so neither a late PUT nor an old DELETE can
/// undo a newer route. A poll's answer never ends an activity tracked after
/// the poll was sent (`CodingRunsPoll`).
@MainActor
final class CodingRunsActivity {
    static let shared = CodingRunsActivity()

    nonisolated init() {}

    /// A `coding_runs` route: which activity's token it holds, on which
    /// machine (origin key), at which server revision.
    private struct ActivityRoute {
        let activityID: String
        let origin: String
        let revision: Int64
    }

    /// The `coding_runs_start` route: the token sent, where, at which revision.
    private struct StartRoute {
        let token: Data
        let origin: String
        let revision: Int64
    }

    private var activity: Activity<CodingRunsActivityAttributes>?
    private var lastState: CodingRunsActivityAttributes.ContentState?
    private var timer: Timer?
    private var tokenTask: Task<Void, Never>?
    private var startTokenTask: Task<Void, Never>?
    private var adoptTask: Task<Void, Never>?
    private var connectionObserver: NSObjectProtocol?
    /// The machine the activity and the routes belong to.
    private var connectedOrigin: String?
    /// Activities ended or retired here; never adopted again, even while iOS
    /// still lists them as running.
    private var retiredIDs: Set<String> = []
    /// The last push-to-start token iOS issued. iOS yields it only when it
    /// changes, so it is kept to send again after a failed PUT (before
    /// pairing, say) or a machine switch.
    private var startToken: Data?
    private var startRoute: StartRoute?
    private var startRegistrationQueued = false
    /// The last `coding_runs` PUT that succeeded.
    private var activityRoute: ActivityRoute?
    /// The tail of the route queue (see `enqueueRoute`).
    private var routeChain: Task<Void, Never>?
    /// Orders polls against adoptions (`CodingRunsPoll`): every poll sent and
    /// every activity tracked takes the next tick.
    private var clock: UInt64 = 0
    /// The tick at which the tracked activity was tracked.
    private var trackedAt: UInt64 = 0
    /// The tick of the newest poll whose answer was applied.
    private var newestAppliedPoll: UInt64 = 0
    /// The machine that answered `/work/live` with 404 (an older server):
    /// polled on `/vibedev/runs/live` until the app next comes to the front.
    private var legacyOrigin: String?

    /// Push type for a locally requested activity.
    nonisolated static var pushType: PushType? {
        MobilePushBuildSupport.remoteNotificationsEnabled ? .token : nil
    }

    /// Start refreshing while the app is in the foreground.
    func resume() {
        // The server may have been updated meanwhile: try `/work/live` again.
        legacyOrigin = nil
        observeRemotePush()
        // A push-to-start PUT that failed earlier is retried on activation.
        registerStartToken()
        refresh()
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { _ in
            Task { @MainActor in CodingRunsActivity.shared.refresh() }
        }
    }

    func pause() {
        timer?.invalidate()
        timer = nil
    }

    func refresh() {
        guard let origin = MagicianAccess.activeOrigin else { return }
        let sentAt = tick()
        if legacyOrigin == origin {
            refreshLegacy(origin: origin, sentAt: sentAt)
            return
        }
        var components = URLComponents(
            url: MagicianAccess.baseURL.appendingPathComponent("/api/magician/v2/work/live"),
            resolvingAgainstBaseURL: false
        )
        components?.queryItems = [URLQueryItem(name: "device", value: MagicianAccess.deviceID)]
        guard let url = components?.url else { return }
        let askedAt = Date()
        URLSession.shared.dataTask(with: Self.liveRequest(url)) { data, response, _ in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let live = (200..<300).contains(status) ? data.flatMap(WorkLiveSummary.parse) : nil
            guard let live else {
                // An older server has no work summary: read its live runs,
                // and keep doing so until the app next comes to the front.
                if status == 404 || (200..<300).contains(status) {
                    Task { @MainActor in
                        let shared = CodingRunsActivity.shared
                        shared.legacyOrigin = origin
                        shared.refreshLegacy(origin: origin, sentAt: sentAt)
                    }
                }
                return
            }
            // The left-menu VibeDev badge rides the same poll.
            VibeDevBadge.shared.update(runsNeedingYou: live.codingRunsNeedingYou)
            let state = WorkLiveSummary.state(for: live)
            Task { @MainActor in
                CodingRunsActivity.shared.apply(state, from: origin, sentAt: sentAt)
                FollowedItemActivity.shared.apply(live, from: origin, askedAt: askedAt)
            }
        }.resume()
    }

    /// `GET /vibedev/runs/live`, for a server without `/work/live`: coding
    /// runs only, headline and detail only.
    private func refreshLegacy(origin: String, sentAt: UInt64) {
        let url = MagicianAccess.baseURL.appendingPathComponent("/api/magician/v2/vibedev/runs/live")
        URLSession.shared.dataTask(with: Self.liveRequest(url)) { data, response, _ in
            let ok = (200..<300).contains((response as? HTTPURLResponse)?.statusCode ?? 0)
            guard ok, let data, let runs = CodingRunsSummary.parse(data) else { return }
            VibeDevBadge.shared.update(runsNeedingYou: runs.filter { $0.state == "needs_you" }.count)
            let state = CodingRunsSummary.state(for: runs)
            Task { @MainActor in CodingRunsActivity.shared.apply(state, from: origin, sentAt: sentAt) }
        }.resume()
    }

    private static func liveRequest(_ url: URL) -> URLRequest {
        var request = URLRequest(url: url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&request)
        return request
    }

    private func tick() -> UInt64 {
        clock += 1
        return clock
    }

    private func apply(_ state: CodingRunsActivityAttributes.ContentState, from origin: String, sentAt: UInt64) {
        // A read of the machine switched away from must not restart the activity.
        guard origin == MagicianAccess.activeOrigin,
              CodingRunsPoll.isCurrent(sentAt: sentAt, newestApplied: newestAppliedPoll) else { return }
        newestAppliedPoll = sentAt
        reconcile()
        guard state.isActive else {
            // An activity the server started while this poll was out is
            // newer than its answer, and is started only once a run: it
            // stays, and the next poll decides.
            if CodingRunsPoll.mayEnd(trackedAt: activity == nil ? nil : trackedAt, sentAt: sentAt) {
                end(with: state)
            }
            return
        }
        guard state != lastState else { return }
        let content = ActivityContent(
            state: state,
            staleDate: Date().addingTimeInterval(5 * 60),
            relevanceScore: state.relevanceScore
        )
        if let activity {
            // An update reports no failure; it counts as delivered.
            lastState = state
            Task { await activity.update(content) }
            return
        }
        // A request from the background fails; `lastState` stays unset so
        // the next foreground poll asks again.
        guard UIApplication.shared.applicationState == .active,
              ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        guard let started = try? Activity.request(
            attributes: CodingRunsActivityAttributes(),
            content: content,
            pushType: Self.pushType
        ) else { return }
        track(started)
    }

    private static func isRunning(_ activity: Activity<CodingRunsActivityAttributes>) -> Bool {
        activity.activityState == .active || activity.activityState == .stale
    }

    /// Keep one running activity, chosen by `CodingRunsAdoption`, and end
    /// every other. `started` is one `activityUpdates` just delivered, which
    /// `activities` may not list yet.
    private func reconcile(including started: Activity<CodingRunsActivityAttributes>? = nil) {
        // The server may have ended it by push (and dropped its route); start
        // over then.
        if let current = activity, !Self.isRunning(current) {
            retiredIDs.insert(current.id)
            activity = nil
            lastState = nil
            tokenTask?.cancel()
            tokenTask = nil
        }
        var running = Activity<CodingRunsActivityAttributes>.activities.filter(Self.isRunning)
        if let started, Self.isRunning(started), !running.contains(where: { $0.id == started.id }) {
            running.append(started)
        }
        let keep = CodingRunsAdoption.survivor(
            tracked: activity?.id,
            running: running.map(\.id),
            retired: retiredIDs
        )
        if let keep, keep != activity?.id, let chosen = running.first(where: { $0.id == keep }) {
            track(chosen)
        }
        for extra in running where extra.id != activity?.id && !retiredIDs.contains(extra.id) {
            retire(extra)
        }
    }

    private func end(
        with state: CodingRunsActivityAttributes.ContentState,
        dismissalPolicy: ActivityUIDismissalPolicy = .after(Date().addingTimeInterval(10 * 60))
    ) {
        guard let ended = activity else { return }
        activity = nil
        lastState = nil
        retiredIDs.insert(ended.id)
        tokenTask?.cancel()
        tokenTask = nil
        Task {
            await ended.end(ActivityContent(state: state, staleDate: nil), dismissalPolicy: dismissalPolicy)
        }
        guard MobilePushBuildSupport.remoteNotificationsEnabled else { return }
        // Queued behind any PUT of its token, so the DELETE comes last.
        enqueueRoute { await CodingRunsActivity.shared.deleteActivityRoute() }
    }

    // MARK: Remote push

    /// Make `adopted` the one this app updates, and register its token. A
    /// different running activity tracked until now is retired.
    private func track(_ adopted: Activity<CodingRunsActivityAttributes>) {
        guard activity?.id != adopted.id else { return }
        if let previous = activity, Self.isRunning(previous) {
            retire(previous)
        }
        tokenTask?.cancel()
        tokenTask = nil
        activity = adopted
        trackedAt = tick()
        lastState = adopted.content.state
        guard MobilePushBuildSupport.remoteNotificationsEnabled else { return }
        let id = adopted.id
        tokenTask = Task {
            for await token in adopted.pushTokenUpdates {
                if Task.isCancelled { return }
                CodingRunsActivity.shared.enqueueRoute {
                    await CodingRunsActivity.shared.putActivityRoute(token, for: id)
                }
            }
        }
    }

    /// End a second activity at once. Its token shares the device's one
    /// `coding_runs` route with the tracked activity's, so when it may have
    /// held that route last, the tracked token is put back (deleting the
    /// route, as `end` does, would drop the survivor's too).
    private func retire(_ extra: Activity<CodingRunsActivityAttributes>) {
        retiredIDs.insert(extra.id)
        Task { await extra.end(nil, dismissalPolicy: .immediate) }
        guard MobilePushBuildSupport.remoteNotificationsEnabled else { return }
        enqueueRoute {
            let shared = CodingRunsActivity.shared
            guard let survivor = shared.activity,
                  shared.activityRoute?.activityID != survivor.id,
                  let token = survivor.pushToken else { return }
            await shared.putActivityRoute(token, for: survivor.id)
        }
    }

    /// PUT `token` as `coding_runs` when `id` is, right now, the tracked and
    /// running activity, and keep the revision for the DELETE that ends it.
    /// Only ever called from the route queue.
    private func putActivityRoute(_ token: Data, for id: String) async {
        guard let tracked = activity, tracked.id == id, Self.isRunning(tracked),
              let origin = MagicianAccess.activeOrigin else { return }
        do {
            let route = try await MobilePushRegistrationClient.registerWithRetry(token: token, kind: .codingRuns)
            activityRoute = ActivityRoute(activityID: id, origin: origin, revision: route.revision)
        } catch {
            debugLog("Could not register coding-runs activity updates")
        }
    }

    /// Delete the `coding_runs` route after an activity ended, on the machine
    /// that holds it and only at the revision its PUT returned. A route the
    /// tracked activity holds stays. Only ever called from the route queue.
    private func deleteActivityRoute() async {
        if let route = activityRoute {
            guard route.activityID != activity?.id else { return }
            activityRoute = nil
            guard let profile = MagicianAccess.profile(forOrigin: route.origin) else { return }
            await MobilePushRegistrationClient.unregisterWithRetry(
                kind: .codingRuns,
                expectedRevision: route.revision,
                profile: profile
            )
        } else if activity == nil {
            // No PUT succeeded this launch (it failed, or an earlier launch
            // made it) and nothing newer is tracked: a leftover route goes, so
            // the server can push-to-start the next one.
            await MobilePushRegistrationClient.unregisterWithRetry(kind: .codingRuns)
        }
    }

    /// Send the last push-to-start token to the machine in use unless it
    /// already holds it: when iOS issues one, on activation, and after a
    /// machine switch.
    private func registerStartToken() {
        guard MobilePushBuildSupport.remoteNotificationsEnabled, !startRegistrationQueued,
              let token = startToken, let origin = MagicianAccess.activeOrigin,
              startRoute?.token != token || startRoute?.origin != origin else { return }
        startRegistrationQueued = true
        enqueueRoute {
            let shared = CodingRunsActivity.shared
            let sent = await shared.putStartRoute()
            shared.startRegistrationQueued = false
            // The token or the machine may have changed during the PUT; a
            // failure waits for the next activation instead of looping.
            if sent { shared.registerStartToken() }
        }
    }

    /// Only ever called from the route queue.
    private func putStartRoute() async -> Bool {
        guard let token = startToken, let origin = MagicianAccess.activeOrigin,
              startRoute?.token != token || startRoute?.origin != origin else { return false }
        await deleteStartRoute(off: origin)
        do {
            let route = try await MobilePushRegistrationClient.registerWithRetry(token: token, kind: .codingRunsStart)
            startRoute = StartRoute(token: token, origin: origin, revision: route.revision)
            return true
        } catch {
            debugLog("Could not register coding-runs push-to-start")
            return false
        }
    }

    /// Remove the push-to-start route from a machine other than `active`, with
    /// that machine's credential while it lasts, so it no longer starts an
    /// activity here. Only ever called from the route queue.
    private func deleteStartRoute(off active: String?) async {
        guard let route = startRoute, route.origin != active else { return }
        startRoute = nil
        guard let profile = MagicianAccess.profile(forOrigin: route.origin) else { return }
        await MobilePushRegistrationClient.unregisterWithRetry(
            kind: .codingRunsStart,
            expectedRevision: route.revision,
            profile: profile
        )
    }

    /// Run route PUTs and DELETEs one at a time, in the order asked: a
    /// DELETE queued after a PUT sees the revision that PUT returned.
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

    /// Removes this iPhone's coding-runs routes from `profile`'s machine, so
    /// it can neither start nor move an activity here afterwards. Call before
    /// forgetting the machine: its routes stop being tracked at once, and the
    /// returned task (nil without push) sends both DELETEs together with
    /// `profile`'s own credential. It stays off the route queue, so the
    /// machine in use is not held up behind it; when `profile` is the machine
    /// in use, it first waits (at most `queuedWorkWait`) for the route work
    /// already queued, so a PUT there does not land after the DELETEs. It
    /// keeps its own time limits: the DELETEs always go out, and are given up
    /// after `deleteBudget`, however long a retrying PUT held the queue.
    @discardableResult
    func retireRoutes(for profile: MobileConnectionProfile) -> Task<Void, Never>? {
        guard MobilePushBuildSupport.remoteNotificationsEnabled,
              let key = MobileConnectionProfiles.originKey(profile.publicOrigin) else { return nil }
        dropRoutes(on: key)
        let pending = key == MagicianAccess.activeOrigin ? routeChain : nil
        return Task { @MainActor in
            if let pending {
                await Self.wait(for: pending, atMost: Self.queuedWorkWait)
            }
            // A PUT that was in flight may have recorded a route there.
            CodingRunsActivity.shared.dropRoutes(on: key)
            let deletes = Task {
                async let updates: Void = MobilePushRegistrationClient.unregisterWithRetry(
                    kind: .codingRuns, profile: profile
                )
                async let starts: Void = MobilePushRegistrationClient.unregisterWithRetry(
                    kind: .codingRunsStart, profile: profile
                )
                _ = await (updates, starts)
            }
            MobilePushRegistrationClient.giveUp(on: [deletes], after: Self.deleteBudget)
            await deletes.value
        }
    }

    /// How long forgetting the machine in use waits for queued route work.
    static let queuedWorkWait: Duration = .seconds(15)
    /// How long the route DELETEs of a forgotten machine may take in all.
    static let deleteBudget: Duration = .seconds(10)

    /// Wait for `task` or `limit`, whichever comes first. Awaiting another
    /// task's value ignores the waiter's cancellation, so the limit is raced
    /// rather than applied by cancelling.
    static func wait(for task: Task<Void, Never>, atMost limit: Duration) async {
        let once = ResumeOnce()
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            once.set(continuation)
            Task { @MainActor in
                await task.value
                once.resume()
            }
            Task { @MainActor in
                try? await Task.sleep(for: limit)
                once.resume()
            }
        }
    }

    private final class ResumeOnce: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Void, Never>?

        func set(_ continuation: CheckedContinuation<Void, Never>) {
            lock.withLock { self.continuation = continuation }
        }

        func resume() {
            let continuation = lock.withLock {
                let taken = self.continuation
                self.continuation = nil
                return taken
            }
            continuation?.resume()
        }
    }

    private func dropRoutes(on origin: String) {
        if activityRoute?.origin == origin { activityRoute = nil }
        if startRoute?.origin == origin { startRoute = nil }
    }

    /// Follow the machine in use: a first pairing, a switch, or forgetting
    /// the active one.
    private func observeConnection() {
        guard connectionObserver == nil else { return }
        connectedOrigin = MagicianAccess.activeOrigin
        connectionObserver = NotificationCenter.default.addObserver(
            forName: .magicianMobileConnectionDidChange, object: nil, queue: .main
        ) { _ in
            Task { @MainActor in CodingRunsActivity.shared.connectionChanged() }
        }
    }

    private func connectionChanged() {
        let origin = MagicianAccess.activeOrigin
        guard origin != connectedOrigin else { return }
        connectedOrigin = origin
        // The activity counts the previous machine's runs. Ending it also
        // deletes its route there, while that machine's credential lasts.
        end(with: lastState ?? .init(working: 0, needsYou: 0), dismissalPolicy: .immediate)
        if MobilePushBuildSupport.remoteNotificationsEnabled {
            enqueueRoute { await CodingRunsActivity.shared.deleteStartRoute(off: origin) }
            registerStartToken()
        }
        refresh()
    }

    /// Push-to-start tokens and server-started activities, observed once.
    /// Safe to call again; launch and every foreground call it.
    func observeRemotePush() {
        observeConnection()
        guard MobilePushBuildSupport.remoteNotificationsEnabled, adoptTask == nil else { return }
        reconcile()
        adoptTask = Task {
            for await started in Activity<CodingRunsActivityAttributes>.activityUpdates {
                CodingRunsActivity.shared.reconcile(including: started)
            }
        }
        if #available(iOS 17.2, *) {
            startTokenTask = Task {
                for await token in Activity<CodingRunsActivityAttributes>.pushToStartTokenUpdates {
                    CodingRunsActivity.shared.startToken = token
                    CodingRunsActivity.shared.registerStartToken()
                }
            }
        }
    }
}

/// What a live-runs poll may do, by when it was sent. Polls and adoptions
/// take ticks from one clock, so an answer can be placed against the
/// activity it would act on.
enum CodingRunsPoll {
    /// An answer older than one already applied is dropped: a slow poll must
    /// not undo a newer one.
    static func isCurrent(sentAt: UInt64, newestApplied: UInt64) -> Bool {
        sentAt > newestApplied
    }

    /// "Nothing running" may end the tracked activity only when it was
    /// tracked before the poll was sent. One tracked since (the server
    /// push-started it, and this app adopted it, while the poll was out) is
    /// newer than the answer; ending it would lose it for the whole run.
    static func mayEnd(trackedAt: UInt64?, sentAt: UInt64) -> Bool {
        guard let trackedAt else { return true }
        return trackedAt < sentAt
    }
}

/// Which coding-runs activity to keep when iOS lists more than one running.
/// One rule for every path (launch, a poll, a server-started arrival): the
/// newest running activity wins. Every activity this app has seen is either
/// tracked or retired, so a running one that is neither arrived after the
/// tracked one; among several such (only at launch), the last listed wins.
/// A retired activity is never kept, even while it is still ending.
enum CodingRunsAdoption {
    static func survivor(tracked: String?, running: [String], retired: Set<String>) -> String? {
        let candidates = running.filter { !retired.contains($0) }
        if let newcomer = candidates.last(where: { $0 != tracked }) { return newcomer }
        guard let tracked, candidates.contains(tracked) else { return nil }
        return tracked
    }
}
