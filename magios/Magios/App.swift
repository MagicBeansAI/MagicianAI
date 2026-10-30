import SwiftUI
import UIKit
import UserNotifications
import WidgetKit

@main
struct MagiosApp: App {
    @UIApplicationDelegateAdaptor(MagiosAppDelegate.self) private var appDelegate
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var tutorRouter = TutorOverlayRouter.shared
    @StateObject private var thinkingMapRouter = ThinkingMapRouter.shared
    @StateObject private var themeManager = ThemeManager.shared
    @State private var mobileConnectionRevision = 0

    init() {
        // Account theme sync starts here — before any view can apply a theme —
        // so the very first reconcile (handleActivation below) can push. UI
        // tests never write the real account record.
        if !isUITestLaunch {
            ThemeManager.shared.themeSyncSink = { theme, mode in
                ThemeSync.push(theme: theme, appearance: mode.rawValue)
            }
        }
        // UI tests: no animations, so XCUITest's wait-for-idle resolves instantly.
        if isUITestLaunch { UIView.setAnimationsEnabled(false) }
        if !isUITestLaunch {
            // Advertise the cached primary-agent name/aliases immediately. The
            // activation refresh below replaces them if Crew identity changed.
            MagiosShortcuts.updateAppShortcutParameters()
            // Control Center caches installed templates across app upgrades.
            // Reload the stable control kind so a previously installed build
            // cannot retain a stale or unresolved glyph after the app updates.
            if #available(iOS 18.0, *) {
                ControlCenter.shared.reloadControls(
                    ofKind: SharedActions.ambientControlKind
                )
            }
        }
    }

    var body: some Scene {
        WindowGroup {
            AppTabView()
                // Several long-lived view models intentionally cache their
                // injected base URL/scope for deterministic tests. Rebuild the
                // presentation tree after an atomic QR install so none retain
                // the fail-closed pre-enrollment origin until a process restart.
                .id(mobileConnectionRevision)
                .onReceive(NotificationCenter.default.publisher(
                    for: .magicianMobileConnectionDidChange
                )) { _ in
                    mobileConnectionRevision &+= 1
                    if !isUITestLaunch {
                        Task { await MobileAtAGlanceUpdates.shared.configureIfAllowed() }
                    }
                }
                // Content shared in from other apps lands in the App Group inbox;
                // drain it on the magican://share deep-link and whenever we become
                // active (so it works even if the extension's open was suppressed).
                .onOpenURL { url in
                    handleURL(url)
                }
                // Note links (`/notes?path=…&workspace=…`) in any Markdown —
                // chat replies, task summaries, Attention details — open the
                // native Notes screen; other links keep the system behavior.
                .environment(\.openURL, NoteLinkRouter.openURLAction)
                .onAppear { handleActivation() }
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active { handleActivation() }
                    if phase == .background { CodingRunsActivity.shared.pause() }
                }
                .fullScreenCover(item: $tutorRouter.request) { request in
                    TutorOverlayView(screenshot: request.screenshot,
                                     canvasMode: request.canvasMode,
                                     initialQuestion: request.question,
                                     autoStart: request.autoStart,
                                     voiceAdmissionID: request.voiceAdmissionID)
                }
                .fullScreenCover(item: $thinkingMapRouter.request) { request in
                    ThinkingMapView(initialThought: request.initialThought,
                                    initialDetail: request.initialDetail,
                                    seedDisposition: request.disposition)
                }
                // The "Ask Tutor" App Intent (Action Button / Shortcut / Siri)
                // opens a source-free blackboard tutor ready for the concept.
                .onReceive(AppActions.shared.$tutorBlackboardRequestID.dropFirst()) { _ in
                    tutorRouter.present(question: "", image: nil)
                }
                // Apply the selected theme at the presentation boundary so every
                // tab, sheet, and UIKit-hosted navigation view changes immediately.
                // In `.system` mode force NOTHING so the app (and the window trait
                // that `systemIsDark` reads) follows the device; only `.day`/`.night`
                // override. Forcing here unconditionally locked `.system` to dark.
                .transformEnvironment(\.colorScheme) { scheme in
                    if let forced = themeManager.forcedColorScheme { scheme = forced }
                }
                .preferredColorScheme(themeManager.forcedColorScheme)
                .tint(themeManager.accentColor)
        }
    }

    /// On launch / foreground / deep-link: drain shared content, route any pending
    /// App Intent action (e.g. New Chat), and (once) adopt the backend's audio
    /// preferences as the local defaults.
    private func handleActivation() {
        ShareRouter.shared.drainInbox()
        AppActions.shared.consumePendingIntentAction()
        // The theme follows the account: re-check the stored choice whenever
        // the app comes forward, as the web does on focus. Fire-and-forget —
        // a failure leaves the locally saved theme in charge.
        ThemeSync.reconcile(systemDark: ThemeManager.systemIsDark)
        if let token = TutorOverlayInbox.claimPendingToken() {
            _ = tutorRouter.present(token: token)
        }
        if !isUITestLaunch { AudioSettings.shared.seedFromBackendIfNeeded() }
        if !isUITestLaunch { PrimaryAgentSiriAdvertiser.shared.refreshAndAdvertise() }
        if !isUITestLaunch { AudioNoteUploadQueue.shared.resume() }
        if !isUITestLaunch { CodingRunsActivity.shared.resume() }
        if !isUITestLaunch { FollowedItemActivity.shared.resume() }
        if !isUITestLaunch {
            Task { await MobileAtAGlanceUpdates.shared.requestRequiredPermissionsOnLaunch() }
            // Asks whose notification window has closed come off the lock
            // screen as soon as the app is opened.
            Task { await MobileAtAGlanceUpdates.withdrawExpiredAttention() }
        }
        // The whole app-side ambient lifecycle: collect an orphaned orb once at
        // launch, drain a disarm the orb asked for while this process was not
        // listening, then start Talk if `ArmAmbientIntent` requested it. The order is
        // internal to that call because it is load-bearing — see
        // `AmbientEntryPoint.handleActivation`. It runs after
        // `consumePendingIntentAction()` above, which is what sets the request latch.
        //
        // Skipped under UI test, where a launch must not open a microphone.
        if !isUITestLaunch { Task { await AmbientEntryPoint.handleActivation() } }
    }

    private func handleURL(_ url: URL) {
        guard MagicanAppURL.isScheme(url.scheme) else { return }
        if url.host == "connect" {
            if let link = try? MobileEnrollmentLink.parse(url.absoluteString) {
                AppActions.shared.requestMobileConnection(link)
            }
            return
        }
        // magican://notes?path=…&workspace=… — one note in the Notes app.
        if url.host == "notes" {
            if !NoteLinkRouter.open(url) { handleActivation() }
            return
        }
        if url.host == "tutor-overlay",
           let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
           let token = components.queryItems?.first(where: { $0.name == "token" })?.value,
           tutorRouter.present(token: token) {
            TutorOverlayInbox.clearPendingToken(ifMatching: token)
            return
        }
        if url.host == "today" {
            let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
            let rawTab = components?.queryItems?.first(where: { $0.name == "tab" })?.value
            let activity = components?.queryItems?.first(where: { $0.name == "activity" })?.value == "true"
            AppActions.shared.requestToday(section: rawTab.flatMap(TodaySection.init(rawValue:)), activity: activity)
            return
        }
        // magican://vibe — VibeDev Now (the coding-runs Live Activity's link).
        if url.host == "vibe" || url.host == "vibedev" {
            AppActions.shared.requestVibeDev()
            return
        }
        if url.host == "attention" {
            let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
            let itemID = components?.queryItems?.first(where: {
                $0.name == "item" || $0.name == "correlation_id"
            })?.value
            AppActions.shared.requestAttention(itemID: itemID)
            return
        }
        if url.host == "voice" {
            let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
            let mode = components?.queryItems?.first(where: { $0.name == "mode" })?.value
            switch mode {
            case "dictation": AppActions.shared.requestVoice(.dictation)
            case "hands_free", "hands-free": AppActions.shared.requestVoice(.handsFree)
            case "realtime", "live": AppActions.shared.requestVoice(.realtime)
            default:
                // Mode-neutral legacy links are system Talk entry points, not a
                // request for the retired one-shot Chat launcher. Start the same
                // ambient lifecycle as the current widget/control: one immediate
                // turn, eight seconds for continuous follow-up, then wake-ready.
                AppActions.shared.requestAmbientArm()
                if !isUITestLaunch {
                    Task { await AmbientEntryPoint.handleActivation() }
                }
            }
            return
        }
        if url.host == "ambient" {
            // The Live Activity's Open link only reveals the app. Foreground
            // activation reconciles the existing window; it must not start a
            // second conversation or route into a one-shot Chat call.
            return
        }
        if let target = WorkNavigationTarget.parse(url.absoluteString) { AppActions.shared.requestWork(target); return }
        if url.host == "task" {
            // TasksView falls back to MONITOR mode when the id is a monitor
            // (probe against GET /monitors/{id} — §9.3.4).
            AppActions.shared.requestTask(url.pathComponents.dropFirst().first)
            return
        }
        // magican://monitor/{task_id}?update=mu_… — the exact-update monitor
        // deep link. No OS push exists on iOS today; when push lands it only
        // needs to open this URL to ride the same in-app resolution path.
        if let target = Monitors.parseDeepLinkURL(url) {
            AppActions.shared.requestMonitor(taskID: target.taskID,
                                             updateID: target.updateID)
            return
        }
        if url.host == "thread" {
            AppActions.shared.requestThread(url.pathComponents.dropFirst().first)
            return
        }
        // magican://observe[?pane=now|sources|audio|notes] — open Observe on a
        // view (the Live Activity's fallback link). Never starts a capture.
        if let pane = ObservePane.deepLink(url) {
            AppActions.shared.requestObserveView(pane: pane)
            return
        }
        handleActivation()
    }
}

final class MagiosAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        AttentionNotificationActions.register()
        // The server can start the coding-runs Live Activity by push while the
        // app is closed; iOS then launches it in the background, where no
        // scene activates. Observe here too, so that activity's update token
        // is registered without the owner opening the app.
        if !isUITestLaunch {
            Task { @MainActor in CodingRunsActivity.shared.observeRemotePush() }
        }
        return true
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { @MainActor in
            MobileAtAGlanceUpdates.shared.registeredApplicationToken(deviceToken)
        }
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        Task { @MainActor in MobileAtAGlanceUpdates.shared.registrationFailed() }
    }

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        Task { @MainActor in
            completionHandler(await MobileAtAGlanceUpdates.shared.handleRemotePayload(userInfo))
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        // Presentation must not wait behind the Today refresh's network
        // timeout. The banner is already complete; refresh the glance cache in
        // parallel and let the OS show the alert immediately.
        let userInfo = notification.request.content.userInfo
        Task { @MainActor in
            _ = await MobileAtAGlanceUpdates.shared.handleRemotePayload(userInfo)
        }
        return [.banner, .list, .sound]
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let userInfo = response.notification.request.content.userInfo
        // Answer buttons the notification service extension added (confirm,
        // one option, or a typed reply) post the answer and replace the
        // notification with its result.
        if await AttentionPushAnswerHandler.handle(response) {
            return
        }
        // Approve / Reject answer the ask in place; any other response opens it.
        if await AttentionNotificationActions.handle(
            actionIdentifier: response.actionIdentifier,
            userInfo: userInfo
        ) {
            return
        }
        await MainActor.run {
            MobileAtAGlanceUpdates.shared.openRemotePayload(userInfo)
        }
    }

    func application(
        _ application: UIApplication,
        handleEventsForBackgroundURLSession identifier: String,
        completionHandler: @escaping () -> Void
    ) {
        guard identifier == AudioNoteUploadQueue.backgroundSessionIdentifier else {
            completionHandler()
            return
        }
        AudioNoteUploadQueue.shared.setBackgroundCompletionHandler(completionHandler)
    }
}
