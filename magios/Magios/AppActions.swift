import SwiftUI

enum VoiceLaunchMode: String, Equatable {
    case configured
    case dictation
    case handsFree
    case realtime

    /// Resolve a mode-neutral system request at the last responsible moment.
    /// Explicit deep links remain explicit; widgets and controls follow the
    /// current device-local preference without needing to be reconfigured.
    func resolved(using configuredMode: SystemVoiceLaunchMode) -> SystemVoiceLaunchMode {
        switch self {
        case .configured: return configuredMode
        case .dictation: return .dictation
        case .handsFree: return .handsFree
        case .realtime: return .realtime
        }
    }
}

/// In-app router for one-shot actions requested by App Intents (Shortcuts / Siri).
/// The intent writes a pending action to the App Group and opens the app; the app
/// consumes it on foreground and calls the relevant `request…()` here, which the
/// UI observes (e.g. AppTabView switches to Chat + ChatView starts a new session).
final class AppActions: ObservableObject {
    static let shared = AppActions()
    init() {}

    /// Bumped to request a fresh chat. Observers use `.dropFirst()` so the initial
    /// value doesn't trigger on launch.
    @Published var newChatRequestID = 0

    /// Bumped whenever an external surface requests voice. The mode remains
    /// latched until ChatView consumes it, so a cold launch cannot lose the
    /// request before SwiftUI installs its `.onReceive` subscribers.
    @Published var voiceRequestID = 0
    @Published private(set) var pendingVoiceLaunchMode: VoiceLaunchMode?

    /// Bumped to open the Tutor straight into blackboard (source-free) mode from an
    /// App Intent (Action Button / Shortcut / Siri) — "explain a concept" from
    /// anywhere, no screenshot. App.swift presents the overlay by observing this.
    @Published var tutorBlackboardRequestID = 0

    /// Bumped when another native surface (for example a Needs You card on
    /// Today) wants the tab container to reveal the full Attention workflow.
    @Published var attentionRequestID = 0
    @Published private(set) var attentionTargetItemID: String?
    @Published var taskRequestID = 0
    @Published private(set) var taskTargetID: String?
    /// Bumped for monitor deep links (Today `monitor_update` cards,
    /// `magican://monitor/{task_id}?update=…`, canonical monitors routes). The
    /// target pins the monitor task and, when present, the EXACT update
    /// record to highlight. This is the in-app resolution path §9.3.4 push
    /// notifications ride once iOS push exists.
    @Published var monitorRequestID = 0
    @Published private(set) var monitorTargetTaskID: String?
    @Published private(set) var monitorTargetUpdateID: String?
    @Published var threadRequestID = 0
    @Published private(set) var threadTargetID: String?
    @Published var todayRequestID = 0
    @Published private(set) var todayTargetSection: TodaySection?
    @Published private(set) var todayShowActivity = false
    @Published var settingsRequestID = 0
    /// A connection QR opened by Camera/Safari. The parsed capability remains
    /// in memory only until Settings confirms and consumes it.
    @Published var mobileConnectionRequestID = 0
    @Published private(set) var pendingMobileConnection: MobileEnrollmentLink?
    /// Bumped by the "Start Listening" intent (Siri / Action Button / widget) —
    /// the app switches to Observe and starts an in-app Listen session.
    @Published var observeRequestID = 0
    /// Bumped by `magican://observe[?pane=…]` (and the Live Activity fallback
    /// link): reveal Observe WITHOUT starting a capture. The requested view is
    /// written straight to the deck's remembered selection, so it applies even
    /// when Observe mounts after this fires (cold launch).
    @Published var observeViewRequestID = 0

    /// A current or compatibility system Talk action asked for a listening window.
    ///
    /// A latch rather than a bumped request id, which every other action here
    /// uses, because arming needs no view to observe it: it is a controller
    /// action, and on a cold launch `consumePendingIntentAction()` runs before
    /// SwiftUI has installed any `.onReceive` subscriber, so a published bump can
    /// be dropped on the floor. A dropped bump means a control the user tapped
    /// visibly doing nothing. `AmbientEntryPoint` reads this in the same
    /// activation, after draining any outstanding disarm request.
    private(set) var ambientArmPending = false

    /// Bumped whenever a HITL/Attention realtime event lands on the always-on
    /// global WS, so the Attention tab badge refetches its counts regardless of
    /// which tab is showing. Distinct from `attentionRequestID` (navigation).
    @Published var attentionDirtyID = 0

    func requestNewChat() { newChatRequestID += 1 }
    func requestVoice(_ mode: VoiceLaunchMode = .configured) {
        pendingVoiceLaunchMode = mode
        voiceRequestID += 1
    }

    func consumeVoiceLaunchMode() -> VoiceLaunchMode? {
        defer { pendingVoiceLaunchMode = nil }
        return pendingVoiceLaunchMode
    }
    func requestBlackboardTutor() { tutorBlackboardRequestID += 1 }
    func requestAttention(itemID: String? = nil) {
        attentionTargetItemID = itemID
        attentionRequestID += 1
    }

    func consumeAttentionTarget() { attentionTargetItemID = nil }
    func requestTask(_ id: String?) { taskTargetID = id; taskRequestID += 1 }
    func consumeTaskTarget() { taskTargetID = nil }
    func requestMonitor(taskID: String, updateID: String? = nil) {
        monitorTargetTaskID = taskID
        monitorTargetUpdateID = updateID
        monitorRequestID += 1
    }
    func consumeMonitorTarget() {
        monitorTargetTaskID = nil
        monitorTargetUpdateID = nil
    }
    func requestThread(_ id: String?) { threadTargetID = id; threadRequestID += 1 }
    func consumeThreadTarget() { threadTargetID = nil }
    func requestToday(section: TodaySection? = nil, activity: Bool = false) {
        todayTargetSection = section; todayShowActivity = activity; todayRequestID += 1
    }
    func consumeTodayTarget() { todayTargetSection = nil; todayShowActivity = false }
    func requestSettings() { settingsRequestID += 1 }
    func requestMobileConnection(_ link: MobileEnrollmentLink) {
        pendingMobileConnection = link
        mobileConnectionRequestID += 1
    }
    func consumeMobileConnection() -> MobileEnrollmentLink? {
        defer { pendingMobileConnection = nil }
        return pendingMobileConnection
    }
    func requestObserve() { observeRequestID += 1 }
    func requestObserveView(pane: ObservePane?, defaults: UserDefaults = .standard) {
        if let pane { defaults.set(pane.rawValue, forKey: ObservePane.storageKey) }
        observeViewRequestID += 1
    }
    func requestAmbientArm() { ambientArmPending = true }

    /// True once, for the activation that routed the intent.
    func consumeAmbientArmRequest() -> Bool {
        defer { ambientArmPending = false }
        return ambientArmPending
    }

    /// Called from the global WS receive loop (possibly off-main) — marshals to main.
    func markAttentionDirty() {
        if Thread.isMainThread { attentionDirtyID += 1 }
        else { DispatchQueue.main.async { self.attentionDirtyID += 1 } }
    }

    /// Consume any pending App Group action left by an intent and route it.
    func consumePendingIntentAction() {
        let pending = SharedActions.consumePending()
        switch pending {
        case SharedActions.PendingAction.newChat: requestNewChat()
        // Migrate every mode-neutral system voice action onto the one public
        // Talk lifecycle. These values can still arrive from an intent/widget
        // installed by an older build; routing them into Chat would preserve the
        // old one-turn close behind a control that now presents as Talk to Magican.
        // Explicit `magican://voice?mode=…` links remain explicit in `App.swift`.
        case SharedActions.PendingAction.voiceConfigured,
             SharedActions.PendingAction.legacyVoiceCall,
             SharedActions.PendingAction.voiceDictation:
            requestAmbientArm()
        case SharedActions.PendingAction.observe: requestObserve()
        case SharedActions.PendingAction.tutorBlackboard: requestBlackboardTutor()
        // Must be decoded here even though nothing in the UI observes it: this
        // method CONSUMES the App Group key, so an unhandled action falls through
        // to `default` and is silently discarded.
        case SharedActions.PendingAction.ambientArm: requestAmbientArm()
        default:
            if let pending, pending.hasPrefix("thread:") {
                requestThread(String(pending.dropFirst("thread:".count)))
            } else if let pending, pending.hasPrefix("task:") {
                requestTask(String(pending.dropFirst("task:".count)))
            }
        }
    }
}
