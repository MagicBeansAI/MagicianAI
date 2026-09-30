import Foundation
import AVFoundation
import ActivityKit

@MainActor
private final class ActivityPushRouteLease {
    var revision: Int64?
}

@MainActor
final class BackgroundEngine {
    static let shared = BackgroundEngine()
    
    private var webSocket: URLSessionWebSocketTask?
    
    private let engine = AVAudioEngine()
    private let playerNode = AVAudioPlayerNode()
    
    private var currentActivity: Activity<MagicianTaskAttributes>?
    private var activityPushTokenTask: Task<Void, Never>?
    private var activityPushRouteLease: ActivityPushRouteLease?
    private var lifecycleID: UUID?

    /// The task this Live Activity tracks. The realtime stream carries deltas for
    /// EVERY run, so updates are filtered to this id. An unbound activity stays
    /// on its local initializing state; accepting an arbitrary run here can put
    /// another task's progress on the Lock Screen.
    private var currentTaskId: String?
    /// Safety timer — ends the activity + background audio if no completion event
    /// ever arrives, so a stalled/lost task can't drain the battery indefinitely.
    private var watchdog: DispatchWorkItem?
    private static let taskLeashSeconds: TimeInterval = 20 * 60

    /// The armed-ambient-window rail. See `AmbientRail`, and `playSilence` /
    /// `releaseAudioSession` for what it changes.
    ///
    /// **This one only ever asks, and never yields.** The other two rails in the app
    /// end an ambient window because the user just asked, in the foreground, for
    /// something that needs the microphone. Nobody asked for this: a Siri or
    /// App-Intent task dispatch finishing in the background is not a decision to
    /// stop listening, and killing a listening window because a shortcut completed
    /// would be inexplicable from the user's side. So the keepalive defers — it
    /// leaves the shared session exactly as it found it — and the task tracking it
    /// exists for is unaffected.
    var ambientRail = AmbientRail.live

    /// Starts the silent audio loop to trick iOS into keeping the app alive,
    /// and opens the WebSocket connection to listen for backend completion.
    @discardableResult
    func start(taskName: String) -> UUID? {
        // One visible task surface at a time. Settle the previous generation
        // before publishing the new identity; late callbacks carry the old id
        // and therefore cannot bind or stop this run.
        stop()
        let lifecycleID = UUID()
        self.lifecycleID = lifecycleID
        currentTaskId = nil
        guard startLiveActivity(taskName: taskName) else {
            // Task dispatch remains valid without ActivityKit, but there is no
            // visible surface to justify an invisible audio/WebSocket leash.
            self.lifecycleID = nil
            return nil
        }
        playSilence(lifecycleID: lifecycleID)
        connectWebSocket(lifecycleID: lifecycleID)
        armWatchdog(lifecycleID: lifecycleID)
        return lifecycleID
    }

    /// Bind the created task's id so Live Activity updates track ONLY this task
    /// out of the shared realtime stream. Call once the create-task POST returns.
    @discardableResult
    func bindTask(_ taskId: String, lifecycleID: UUID) -> Bool {
        guard Self.ownsLifecycle(current: self.lifecycleID, expected: lifecycleID) else {
            return false
        }
        guard currentTaskId == nil else { return false }
        currentTaskId = taskId
        let routeLease = ActivityPushRouteLease()
        activityPushRouteLease = routeLease
        observeRemotePushToken(
            for: taskId,
            lifecycleID: lifecycleID,
            routeLease: routeLease
        )
        return true
    }

    /// End the activity after a hard cap even if no completion event arrives.
    /// The Live Activity applies only an event whose task id matches its bound
    /// task. Pure + static so it's unit-testable
    /// (the realtime stream carries deltas for every run).
    nonisolated static func shouldApply(bound: String?, event: String?) -> Bool {
        guard let bound, let event else { return false }
        return bound == event
    }

    nonisolated static func ownsLifecycle(current: UUID?, expected: UUID) -> Bool {
        current == expected
    }

    /// `activityLog` is chronological, but the capped legacy
    /// `recentActivity` fallback is newest-first.
    nonisolated static func activityDigest(
        activityLog: [FeedItem]?,
        recentActivity: [FeedItem]
    ) -> (latest: FeedItem?, count: Int) {
        if let activityLog {
            return (activityLog.last, activityLog.count)
        }
        return (recentActivity.first, recentActivity.count)
    }

    nonisolated static func taskStatusText(status: String, latestActivity: String?) -> String {
        switch status {
        case "completed": return "Done."
        case "failed": return "Did not finish."
        case "cancelled": return "Cancelled."
        default: return latestActivity ?? "Working..."
        }
    }

    enum TaskActivityMutation: Equatable {
        case update
        case end
    }

    nonisolated static func taskActivityMutation(isDone: Bool) -> TaskActivityMutation {
        isDone ? .end : .update
    }

    nonisolated static var taskActivityPushType: PushType? {
        MobilePushBuildSupport.remoteNotificationsEnabled ? .token : nil
    }

    /// Apply exactly one ActivityKit mutation for one canonical realtime state.
    /// Scheduling an update and an end in separate unstructured tasks lets them
    /// overtake one another, which can leave the last rendered state stale or
    /// briefly resurrect progress after a terminal event.
    private func applyRealtimeState(
        _ updatedState: MagicianTaskAttributes.ContentState,
        lifecycleID: UUID
    ) {
        switch Self.taskActivityMutation(isDone: updatedState.isDone) {
        case .end:
            stop(lifecycleID: lifecycleID, finalState: updatedState)
        case .update:
            let liveActivity = currentActivity
            Task {
                await liveActivity?.update(
                    ActivityContent(
                        state: updatedState,
                        staleDate: Date().addingTimeInterval(Self.taskLeashSeconds)
                    )
                )
            }
        }
    }

    private func armWatchdog(lifecycleID: UUID) {
        watchdog?.cancel()
        let work = DispatchWorkItem { [weak self] in
            Task { @MainActor [weak self] in self?.stop(lifecycleID: lifecycleID) }
        }
        watchdog = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + Self.taskLeashSeconds,
            execute: work
        )
    }
    
    private func startLiveActivity(taskName: String) -> Bool {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return false }
        
        let attributes = MagicianTaskAttributes(taskName: taskName)
        let contentState = MagicianTaskAttributes.ContentState(status: "Initializing...", isDone: false)
        
        do {
            currentActivity = try Activity.request(
                attributes: attributes,
                content: ActivityContent(
                    state: contentState,
                    staleDate: Date().addingTimeInterval(Self.taskLeashSeconds)
                ),
                pushType: Self.taskActivityPushType
            )
            return true
        } catch {
            debugLog("Failed to start Live Activity: \(error)")
            return false
        }
    }
    
    private func endLiveActivity(finalState: MagicianTaskAttributes.ContentState) {
        let activity = currentActivity
        let taskID = currentTaskId
        let pushTokenTask = activityPushTokenTask
        let routeLease = activityPushRouteLease
        currentActivity = nil
        currentTaskId = nil
        pushTokenTask?.cancel()
        activityPushTokenTask = nil
        activityPushRouteLease = nil
        Task {
            // Cancellation is cooperative. Drain the registration observer
            // before deleting the route, or an in-flight PUT can finish after
            // the DELETE and resurrect an abandoned task token.
            if let pushTokenTask { await pushTokenTask.value }
            await activity?.end(
                ActivityContent(state: finalState, staleDate: nil),
                dismissalPolicy: .default
            )
            if let taskID, let revision = routeLease?.revision {
                await MobilePushRegistrationClient.unregisterWithRetry(
                    kind: .taskActivity,
                    taskID: taskID,
                    expectedRevision: revision
                )
            }
        }
    }

    private func observeRemotePushToken(
        for taskID: String,
        lifecycleID: UUID,
        routeLease: ActivityPushRouteLease
    ) {
        activityPushTokenTask?.cancel()
        guard MobilePushBuildSupport.remoteNotificationsEnabled else { return }
        guard let activity = currentActivity else { return }
        activityPushTokenTask = Task { [weak self, weak activity] in
            guard let activity else { return }
            for await token in activity.pushTokenUpdates {
                guard !Task.isCancelled,
                      Self.ownsLifecycle(current: self?.lifecycleID, expected: lifecycleID),
                      self?.currentTaskId == taskID else { return }
                do {
                    let registration = try await MobilePushRegistrationClient.registerWithRetry(
                        token: token,
                        kind: .taskActivity,
                        taskID: taskID
                    )
                    routeLease.revision = registration.revision
                    self?.remoteDeliveryBecameReady(
                        taskID: taskID,
                        lifecycleID: lifecycleID
                    )
                } catch {
                    debugLog("Could not register remote task activity updates")
                }
            }
        }
    }
    
    /// Cleans up the audio engine and socket, letting iOS put the app to sleep.
    func stop(
        lifecycleID expectedLifecycleID: UUID? = nil,
        finalState: MagicianTaskAttributes.ContentState? = nil
    ) {
        if let expectedLifecycleID,
           !Self.ownsLifecycle(current: lifecycleID, expected: expectedLifecycleID) { return }
        lifecycleID = nil
        watchdog?.cancel()
        watchdog = nil
        endLiveActivity(finalState: finalState ?? MagicianTaskAttributes.ContentState(
            status: "Tracking ended",
            isDone: true
        ))
        playerNode.stop()
        engine.stop()
        let socket = webSocket
        webSocket = nil
        socket?.cancel(with: .goingAway, reason: nil)
        releaseAudioSession()
    }

    /// Once the server has accepted an APNs ActivityKit route, keeping a
    /// silent player and an open realtime socket alive for the same progress
    /// stream only spends battery. Preserve the Activity + watchdog, but let
    /// iOS suspend this process and receive subsequent state through APNs.
    private func remoteDeliveryBecameReady(taskID: String, lifecycleID: UUID) {
        guard Self.ownsLifecycle(current: self.lifecycleID, expected: lifecycleID),
              currentTaskId == taskID else { return }
        playerNode.stop()
        engine.stop()
        let socket = webSocket
        webSocket = nil
        socket?.cancel(with: .goingAway, reason: nil)
        releaseAudioSession(forRemoteLifecycle: lifecycleID)
    }

    private func releaseAudioSession(forRemoteLifecycle lifecycleID: UUID) {
        let rail = ambientRail
        Task { @MainActor [weak self] in
            guard Self.ownsLifecycle(current: self?.lifecycleID, expected: lifecycleID),
                  !rail.windowIsLive() else { return }
            do {
                try AVAudioSession.sharedInstance().setActive(false)
            } catch {
                debugLog("Failed to release the task keepalive audio session")
            }
        }
    }

    /// Deactivate the shared session — **unless an ambient window is armed.**
    ///
    /// `stop()` is reachable from three places and one of them is the reason this
    /// guard exists: it is called on task completion, and a task can be dispatched
    /// by `MagiosIntents`' Siri / App-Intent path *while a listening window is
    /// open*. Deactivating there would be unrecoverable rather than merely rude —
    /// iOS refuses to reactivate a recording session from the background (Apple DTS
    /// 826462), so the window would not fail at this line, it would fail at the
    /// next wake word, off screen, with the orb still claiming the user is being
    /// heard. Design §15 lists this site by name as Task 11's rail.
    ///
    /// The hop is not incidental. A stop may be requested by the WebSocket
    /// callback, so cleanup is kept on the main actor with the rest of this
    /// lifecycle. Deferring it by one main-actor turn costs nothing.
    private func releaseAudioSession() {
        let rail = ambientRail
        Task { @MainActor [weak self] in
            // `start()` may already have installed the successor generation.
            // An old stop must not deactivate that new task's audio session.
            guard self?.lifecycleID == nil else { return }
            guard !rail.windowIsLive() else {
                debugLog("Leaving the audio session active: an ambient window is armed")
                return
            }
            do {
                try AVAudioSession.sharedInstance().setActive(false)
            } catch {
                debugLog("Failed to deactivate audio session")
            }
        }
    }

    /// The silent-audio keepalive.
    ///
    /// **The session configuration is skipped while an ambient window is armed, and
    /// the keepalive still works.** Design §15 lists this site for the
    /// `.mixWithOthers` contradiction; the sharper problem for an armed window is
    /// the `.playback` category itself, which has no input — swapping to it takes
    /// the microphone out from under a live ambient tap, and the symptom is a wake
    /// word that silently stops working rather than an error.
    ///
    /// Nothing is lost by skipping it, which is why this is a deferral rather than
    /// a refusal: an armed window is already holding an active `.playAndRecord`
    /// session under the `audio` background mode, which is the same keepalive this
    /// method exists to fabricate. So the player is still attached and still loops
    /// silence — it renders perfectly well on `.playAndRecord`, which routes to the
    /// speaker via ambient's `.defaultToSpeaker` — and the task tracking continues
    /// exactly as before.
    ///
    /// Hopped to the main actor as one unit, because the rail has to be read there
    /// and `start(taskName:)` is reached from `AskMagicianIntent.perform()` — a
    /// plain `async` method on a non-isolated struct, so it runs on the cooperative
    /// pool, not the main thread. Hopping the *whole* body rather than just the rail
    /// read keeps the original ordering intact: the session is configured before
    /// `engine.start()`, which needs it.
    private func playSilence(lifecycleID: UUID) {
        let rail = ambientRail
        Task { @MainActor [weak self] in
            guard Self.ownsLifecycle(current: self?.lifecycleID, expected: lifecycleID) else {
                return
            }
            self?.startSilentPlayer(ambientWindowIsLive: rail.windowIsLive())
        }
    }

    private func startSilentPlayer(ambientWindowIsLive: Bool) {
        do {
            if ambientWindowIsLive {
                debugLog("Leaving the audio session as ambient configured it: a window is armed")
            } else {
                // Crucial: Set category to playback and mixWithOthers so it doesn't pause the user's Spotify/Apple Music
                try AVAudioSession.sharedInstance().setCategory(.playback, mode: .default, options: [.mixWithOthers])
                try AVAudioSession.sharedInstance().setActive(true)
            }

            let format = engine.outputNode.inputFormat(forBus: 0)
            // The same singleton tracks more than one task over its lifetime.
            // Stopping an AVAudioEngine does not detach its nodes; attaching the
            // same node again raises an Objective-C exception rather than a
            // catchable Swift error.
            if playerNode.engine == nil {
                engine.attach(playerNode)
                engine.connect(playerNode, to: engine.outputNode, format: format)
            }
            
            try engine.start()
            
            // Generate a programmatic silent buffer (all zeros) so we don't need a dummy mp3 file
            guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4096) else { return }
            buffer.frameLength = 4096
            
            for i in 0..<Int(buffer.format.channelCount) {
                if let channelData = buffer.floatChannelData?[i] {
                    memset(channelData, 0, Int(buffer.frameLength) * MemoryLayout<Float>.size)
                }
            }
            
            // Loop the silence indefinitely
            playerNode.scheduleBuffer(buffer, at: nil, options: .loops, completionHandler: nil)
            playerNode.play()
            
            debugLog("Background Audio Hack Activated: Process will stay alive.")
        } catch {
            debugLog("Audio engine error: \(error)")
        }
    }
    
    private func connectWebSocket(lifecycleID: UUID) {
        guard !isRunningUnderTests else { return }   // no real WebSocket in unit tests
        // Realtime event stream — the same WebSocket the web UI + chat use.
        // (magician serves /realtime/ws under the v2 API scope; there is no v3.)
        let baseURL = "\(MagicianAccess.webSocketBaseURL.absoluteString)"
        // The paired bearer authorizes the upgrade and binds the event scope.
        guard let url = URL(string: "\(baseURL)/api/magician/v2/realtime/ws") else { return }
        
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        webSocket = URLSession.shared.webSocketTask(with: request)
        webSocket?.resume()

        if let webSocket {
            receiveMessage(lifecycleID: lifecycleID, socket: webSocket)
        }
    }

    private func receiveMessage(lifecycleID: UUID, socket: URLSessionWebSocketTask) {
        socket.receive { [weak self, weak socket] result in
            Task { @MainActor [weak self] in
                guard let socket,
                      self?.webSocket === socket,
                      Self.ownsLifecycle(current: self?.lifecycleID, expected: lifecycleID) else {
                    return
                }
                switch result {
            case .success(let message):
                switch message {
                case .string(let text):
                    guard let data = text.data(using: .utf8) else { break }
                    
                    do {
                        if let jsonObj = try JSONSerialization.jsonObject(with: data, options: []) as? [String: Any],
                           let eventType = jsonObj["event_type"] as? String {
                            debugLog("Background WebSocket received event: \(eventType)")

                            // Attention tab badge upkeep off the always-on global WS:
                            // maintain the pending-HITL count, and refetch the Attention
                            // counts on any HITL/Attention/UserRequest event (regardless of
                            // which tab is showing). Both marshal to the main thread.
                            PendingHitlTracker.shared.apply(eventText: text)
                            if eventType.contains("Hitl") || eventType.contains("Attention")
                                || eventType.contains("UserRequest") {
                                AppActions.shared.markAttentionDirty()
                            }

                            if eventType == "ExecutionPanelDelta" {
                                if let eventDataDict = jsonObj["data"] as? [String: Any],
                                   let eventDataJson = try? JSONSerialization.data(withJSONObject: eventDataDict) {
                                    
                                    if let delta = decodeExecutionPanelDelta(from: eventDataJson) {

                                        // The stream carries deltas for every run — apply only
                                        // our task's. An unbound activity remains initializing.
                                        let deltaTaskId = delta.taskId ?? delta.state.overview.taskId
                                        let bound: String? = self?.currentTaskId ?? nil
                                        if BackgroundEngine.shouldApply(bound: bound, event: deltaTaskId) {
                                            let isDone: Bool = delta.state.overview.isTerminal

                                            // Pick the most recent activity title/content as the status
                                            let activity = Self.activityDigest(
                                                activityLog: delta.state.run.activityLog,
                                                recentActivity: delta.state.run.recentActivity
                                            )
                                            let latestActivity = activity.latest?.title
                                                ?? activity.latest?.content
                                            let statusText = Self.taskStatusText(
                                                status: delta.state.overview.status,
                                                latestActivity: latestActivity
                                            )
                                            let stepCount = activity.count
                                            let cardTitle = delta.state.overview.title

                                            let updatedState = MagicianTaskAttributes.ContentState(
                                                status: statusText,
                                                isDone: isDone,
                                                stepCount: stepCount,
                                                cardTitle: cardTitle
                                            )

                                            self?.applyRealtimeState(
                                                updatedState,
                                                lifecycleID: lifecycleID
                                            )
                                        }
                                    }
                                }
                            } else if eventType == "MessageCompleted" {
                                // End only on OUR bound task's completion.
                                let msgTaskId = (jsonObj["data"] as? [String: Any])?["task_id"] as? String
                                let bound: String? = self?.currentTaskId ?? nil
                                if BackgroundEngine.shouldApply(bound: bound, event: msgTaskId) {
                                    self?.applyRealtimeState(
                                        MagicianTaskAttributes.ContentState(
                                            status: "Done.",
                                            isDone: true
                                        ),
                                        lifecycleID: lifecycleID
                                    )
                                }
                            }
                        }
                    } catch {
                        debugLog("Failed to parse background websocket message: \(error)")
                    }
                    
                case .data(_):
                    break
                @unknown default:
                    break
                }
                
                // Keep listening
                if self?.webSocket === socket {
                    self?.receiveMessage(lifecycleID: lifecycleID, socket: socket)
                }
                
            case .failure(let error):
                debugLog("WebSocket error: \(error)")
                // If the socket dies, stop the background audio so we don't drain the battery
                self?.stop(lifecycleID: lifecycleID)
                }
            }
        }
    }
}
