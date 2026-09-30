import Combine
import SwiftUI

struct AppTabView: View {
    // The singleton is owned by ThemeManager; this view only observes it. Keeping
    // the root subscribed ensures UIKit-backed tab labels refresh with the theme.
    @ObservedObject private var themeManager = ThemeManager.shared
    @ObservedObject private var actions = AppActions.shared
    // In `.system` mode the app doesn't force a scheme, so this reflects the device
    // appearance and drives day/night following.
    @Environment(\.colorScheme) private var envColorScheme
    // Shared so the Attention tab badge reads the same live count the view uses.
    @StateObject private var attentionVM = AttentionViewModel()
    @ObservedObject private var pendingHitl = PendingHitlTracker.shared
    // Today is the center tab (position 3 of 5) and the default landing tab.
    @AppStorage("selectedAppTab") private var selectedTab = 2
#if DEBUG
    @State private var didEmitInternalTaskUITestRequest = false
#endif

    /// Live Attention badge — same formula + backend inputs as the web
    /// (max(pendingHitl, needs_action) + failed), reactive to both the
    /// pending-HITL tracker and the feed counts.
    private var attentionBadge: Int {
        AttentionViewModel.resolveAttentionBadgeCount(
            pendingHitl: pendingHitl.count,
            needsAction: attentionVM.counts?.needsAction ?? 0,
            failed: attentionVM.counts?.failed ?? 0
        )
    }

    var body: some View {
        ZStack {
        TabView(selection: $selectedTab) {
            ChatView()
                .tabItem {
                    Label("Chat", systemImage: "bubble.left.and.bubble.right.fill")
                }
                .tag(0)

            TasksView()
                .tabItem {
                    Label("Tasks", systemImage: "checklist")
                }
                .tag(1)

            TodayView()
                .tabItem {
                    Label("Today", systemImage: "sun.max.fill")
                }
                .tag(2)

            AttentionView(viewModel: attentionVM)
                .tabItem {
                    Label("Attention", systemImage: "bell.badge.fill")
                }
                .badge(attentionBadge)
                .tag(3)

            // Settings moved into the left hamburger drawer; Observe takes its
            // slot in the bottom bar.
            ObserveView()
                .tabItem {
                    Label("Observe", systemImage: "waveform.badge.mic")
                }
                .tag(4)
        }
        .accentColor(themeManager.accentColor)
        .tint(themeManager.accentColor)
        // Force the color scheme to match the active theme so SwiftUI's semantic
        // colors (secondary text, nav titles, Menus, pickers) never render
        // dark-on-dark. In `.system` mode leave it unforced so the app follows the
        // device day/night.
        .preferredColorScheme(themeManager.forcedColorScheme)
        // Follow the device appearance while in `.system` mode.
        .onAppear { themeManager.systemAppearanceChanged(dark: ThemeManager.systemIsDark) }
        .onChange(of: envColorScheme) { _, newValue in
            themeManager.systemAppearanceChanged(dark: newValue == .dark)
        }
        // A "New Chat" intent switches to the Chat tab (ChatView starts the
        // fresh session itself by observing the same signal).
        .onReceive(actions.$newChatRequestID.dropFirst()) { _ in selectedTab = 0 }
        // Every external voice surface first reveals Chat. ChatView owns and
        // consumes the latched mode so the canonical voice-call state stays in
        // one place.
        .onReceive(actions.$voiceRequestID.dropFirst()) { _ in selectedTab = 0 }
        .onReceive(actions.$attentionRequestID.dropFirst()) { _ in selectedTab = 3 }
        .onReceive(actions.$taskRequestID.dropFirst()) { _ in selectedTab = 1 }
        // Monitor deep links live on the Tasks surface (Monitors lane).
        .onReceive(actions.$monitorRequestID.dropFirst()) { _ in selectedTab = 1 }
        .onReceive(actions.$threadRequestID.dropFirst()) { _ in selectedTab = 0 }
        .onReceive(actions.$todayRequestID.dropFirst()) { _ in selectedTab = 2 }
        .onReceive(actions.$settingsRequestID.dropFirst()) { _ in DrawerController.shared.open() }
        // "Start Listening" (Siri / Action Button / Shortcuts): open Observe and
        // start an in-app room capture.
        .onReceive(actions.$observeRequestID.dropFirst()) { _ in
            selectedTab = 4
            Task { await ListenController.shared.start(title: nil, url: nil) }
        }
        // magican://observe[?pane=…]: just reveal Observe (the pane is already
        // stored by `requestObserveView`).
        .onReceive(actions.$observeViewRequestID.dropFirst()) { _ in selectedTab = 4 }
        // Keep the Attention tab badge current even when the tab isn't on screen:
        // fetch the backend counts at launch and on every tab switch. (The live
        // realtime refresh runs while the Attention tab itself is visible.)
        .onAppear {
            // `consumePendingIntentAction()` can run before this TabView mounts
            // during a cold launch. The durable mode closes that subscription
            // race and still routes directly to Chat.
            if actions.pendingVoiceLaunchMode != nil { selectedTab = 0 }
            attentionVM.fetch()
#if DEBUG
            // Deterministic end-to-end coverage for the same one-shot router
            // signal used by Today, extensions, Siri, and magican://task links.
            if !didEmitInternalTaskUITestRequest,
               ProcessInfo.processInfo.arguments.contains("--open-internal-task-ui-test") {
                didEmitInternalTaskUITestRequest = true
                DispatchQueue.main.async { actions.requestTask("fixture-internal") }
            }
            // Lands directly on Attention so UI tests / diagnostics skip the
            // tab-bar dance (same shape as the internal-task arg above).
            if ProcessInfo.processInfo.arguments.contains("--open-attention-ui-test") {
                DispatchQueue.main.async { selectedTab = 3 }
            }
#endif
        }
        // Refetch only when ENTERING Attention, not on every bottom-tab hop —
        // the throttled dirty loop keeps the badge fresh everywhere else.
        .onChange(of: selectedTab) { _, newValue in
            if newValue == 3 { attentionVM.fetch() }
        }
        // Any HITL/Attention event on the always-on global WS (BackgroundEngine)
        // refetches the counts so the badge stays live off the Attention tab too.
        // THROTTLED: a busy system emits these continuously, and an unthrottled
        // loop kept the feed (and its spinners) refetching forever. One fetch
        // per 8s window is plenty for a badge; while the Attention tab is
        // showing, its own realtime loop (0.6s debounce) owns freshness.
        .onReceive(
            actions.$attentionDirtyID.dropFirst().throttle(
                for: .seconds(8), scheduler: RunLoop.main, latest: true
            )
        ) { _ in
            if selectedTab != 3 { attentionVM.fetch() }
        }

            // Persistent live-capture pills, floated above the tab bar on every
            // tab. Stacked rather than exclusive: they are two separate captures
            // with two separate stop controls, and nothing structurally prevents
            // an observation from being started while an ambient window is armed.
            VStack(spacing: 8) {
                Spacer()
                // "Now observing" — tap → Observe tab, Stop ends it in place.
                ObservationMiniBar { selectedTab = 4 }
                // The only reachable disarm while the user is inside the app —
                // see `AmbientMiniBar`.
                AmbientMiniBar()
            }
            .padding(.bottom, 52)

            // Left slide-out drawer (Settings + scope + About), rendered once at
            // the root so it slides over the tabs and the tab bar.
            SideMenuOverlay()
        }
    }
}
