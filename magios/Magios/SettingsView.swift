import SwiftUI
import AppIntents
import ActivityKit
import UIKit
import UserNotifications
import VisionKit

enum SiriPhrasePresentation {
    static let customPhraseKey = "siri.personal-shortcut-phrase.v1"

    static func automaticPhrases(
        identity: PrimaryAgentSiriIdentity?,
        applicationName: String = MagicianAccess.productName
    ) -> [String] {
        // An advertised name equal to the app name is already covered by the
        // plain "Ask <app>" shortcut below; qualifying it would read
        // "Ask <app> using <app>".
        let qualified = (identity?.advertisedNames ?? [])
            .filter { $0.caseInsensitiveCompare(applicationName) != .orderedSame }
            .map { "Ask \($0) using \(applicationName)" }
        return distinct(qualified + ["Ask \(applicationName)"])
    }

    static func personalSuggestions(
        identity: PrimaryAgentSiriIdentity?,
        applicationName: String = MagicianAccess.productName
    ) -> [String] {
        // "Ask <app>" already ships as an App Shortcut, so offering it again as
        // a personal Shortcut would collide with it. Fall back to the unfiltered
        // list when the primary name is the only advertised name.
        let advertised = identity?.advertisedNames ?? [applicationName]
        let aliasesOnly = advertised.filter {
            $0.caseInsensitiveCompare(applicationName) != .orderedSame
        }
        let usable = aliasesOnly.isEmpty ? advertised : aliasesOnly
        return distinct(usable.flatMap { ["Ask \($0)", "Ask \($0) AI"] })
    }

    static func initialPersonalPhrase(stored: String?, identity: PrimaryAgentSiriIdentity?) -> String {
        let saved = stored?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return saved.isEmpty
            ? (personalSuggestions(identity: identity).first ?? "Ask \(MagicianAccess.productName)")
            : saved
    }

    private static func distinct(_ values: [String]) -> [String] {
        var seen = Set<String>()
        return values.filter {
            let key = $0.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
            return seen.insert(key).inserted
        }
    }
}

/// The groups of the native Settings list, in display order. Only device
/// settings live here; everything shared lives in the embedded web Settings.
enum SettingsSearchSection: String, CaseIterable {
    case top, voice, notifications, keyboard, connection, help

    /// The sections the chip bar offers, in display order. The top row
    /// (Magican settings) has no heading and no chip.
    static var navigable: [SettingsSearchSection] { allCases.filter { $0 != .top } }

    /// The scroll target on the section's heading.
    var anchorID: String { "settings-section-\(rawValue)" }

    var title: String {
        switch self {
        case .top: return ""
        case .voice: return "Voice & audio"
        case .notifications: return "Notifications"
        case .keyboard: return "Keyboard"
        case .connection: return "Connection"
        case .help: return "Help & about"
        }
    }
}

/// One searchable Settings row: what it is called and the words that find it.
enum SettingsSearchRow: CaseIterable {
    case webSettings
    case deviceVoice, talkToProduct, talkBehavior
    case notificationPermissions, atAGlance
    case keyboardSettings, keyboardReset
    case machines, selfHostedConnection, services
    case howToUse, about

    var section: SettingsSearchSection {
        switch self {
        case .webSettings: return .top
        case .deviceVoice, .talkToProduct, .talkBehavior: return .voice
        case .notificationPermissions, .atAGlance: return .notifications
        case .keyboardSettings, .keyboardReset: return .keyboard
        case .machines, .selfHostedConnection, .services: return .connection
        case .howToUse, .about: return .help
        }
    }

    var title: String {
        switch self {
        case .webSettings: return "\(ProductIdentity.productName) settings"
        case .deviceVoice: return "This iPhone · speaking voice"
        case .talkToProduct: return "Talk to \(ProductIdentity.productName)"
        case .talkBehavior: return "Talk behavior"
        case .notificationPermissions: return "Notifications"
        case .atAGlance: return "At a glance"
        case .keyboardSettings: return "\(ProductIdentity.productName) Keyboard"
        case .keyboardReset: return "Reset learned words"
        case .machines: return "Machines"
        case .selfHostedConnection: return "Self-hosted connection"
        case .services: return "Services"
        case .howToUse: return "How to Use"
        case .about: return "About"
        }
    }

    var keywords: [String] {
        switch self {
        case .webSettings:
            // The shared settings that moved off the phone, so searching
            // for one of them still leads somewhere.
            return [
                "web", "shared", "account", "budgets", "budget", "spend", "spending", "limit", "cost",
                "harness", "credentials", "engines", "coding", "sign-in", "keys", "api keys",
                "preferences", "notes from recent checks", "storage", "maintenance", "voices",
                "speaking voices", "tts", "trust", "safety", "rules", "approvals", "workspaces",
                "models", "providers", "agents", "memory", "notes", "notifications", "devices",
                "mobile devices", "integrations", "skills", "privacy",
            ]
        case .deviceVoice:
            return ["voice", "speech", "speak", "speaking", "read aloud", "tts", "device", "iphone", "audio"]
        case .talkToProduct:
            return ["siri", "shortcut", "shortcuts", "phrase", "hey siri", "ask", "voice", "talk"]
        case .talkBehavior:
            return [
                "ambient", "wake word", "listening", "listen", "conversation mode", "dictation",
                "hands-free", "realtime", "live", "stay available", "leash", "phrases",
                "microphone", "mic", "control center", "talk",
            ]
        case .notificationPermissions:
            return [
                "notifications", "alerts", "push", "permissions", "allow", "lock screen",
                "questions", "face id", "previews", "registration",
            ]
        case .atAGlance:
            return ["widget", "widgets", "live activities", "task progress", "home screen", "glance"]
        case .keyboardSettings:
            return ["keyboard", "language", "skills", "chips", "full access", "typing", "tutorial"]
        case .keyboardReset:
            return ["keyboard", "learned words", "vocabulary", "reset", "forget", "typing"]
        case .machines:
            return ["machine", "machines", "computer", "pair", "paired", "pairing", "switch", "forget", "host"]
        case .selfHostedConnection:
            return [
                "connection", "connect", "qr", "scan", "pair", "pairing", "endpoint", "route",
                "same wi-fi", "wifi", "wi-fi", "remote", "host", "server", "self-hosted",
            ]
        case .services:
            return ["services", "health", "status", "backend", "magicutor", "desktop proxy", "refresh", "version", "accounts", "sign in", "metabase", "higgsfield"]
        case .howToUse:
            return ["help", "guide", "tutorial", "instructions", "how to"]
        case .about:
            return [
                "about", "version", "versions", "build", "app", "ios",
                MagicianAccess.backendServiceLabel.lowercased(), "backend", "api", "magicutor", "desktop",
            ]
        }
    }
}

/// Settings search: a row is shown when every word of the query appears in
/// its title or one of its keywords, ignoring case and accents.
enum SettingsSearch {
    static func trimmed(_ query: String) -> String {
        query.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func isActive(_ query: String) -> Bool {
        !trimmed(query).isEmpty
    }

    static func matches(_ query: String, title: String, keywords: [String]) -> Bool {
        let words = trimmed(query)
            .split(whereSeparator: { $0.isWhitespace })
            .map { fold(String($0)) }
        guard !words.isEmpty else { return true }
        let haystack = ([title] + keywords).map(fold)
        return words.allSatisfy { word in haystack.contains { $0.contains(word) } }
    }

    static func matches(_ query: String, row: SettingsSearchRow) -> Bool {
        matches(query, title: row.title, keywords: row.keywords)
    }

    static func visibleRows(for query: String) -> [SettingsSearchRow] {
        SettingsSearchRow.allCases.filter { matches(query, row: $0) }
    }

    /// The headed sections with at least one matching row, in display order:
    /// the chips the section bar shows. Empty when nothing headed matches.
    static func visibleSections(for query: String) -> [SettingsSearchSection] {
        let rows = visibleRows(for: query)
        return SettingsSearchSection.navigable.filter { section in rows.contains { $0.section == section } }
    }

    /// Scroll-spy: which section the reader is in.
    ///
    /// - `order`: the shown sections, in display order.
    /// - `headerTops`: global top of each heading that is on screen now.
    /// - `threshold`: global y just below the chip bar; a heading at or above
    ///   it has been scrolled to.
    /// - `reachedEnd`: the last shown section's end is on screen. Short last
    ///   sections never reach the top, so the bottom of the list selects
    ///   them — unless the first heading is also on screen (all content fits).
    ///
    /// The answer is the last heading at or above the threshold; else the
    /// section before the first heading on screen (its content fills the top);
    /// else, with no heading on screen (inside a long section), `current`.
    static func activeSection(
        order: [SettingsSearchSection],
        headerTops: [SettingsSearchSection: CGFloat],
        threshold: CGFloat,
        reachedEnd: Bool,
        current: SettingsSearchSection?
    ) -> SettingsSearchSection? {
        guard let first = order.first, let last = order.last else { return nil }
        if reachedEnd, headerTops[first] == nil { return last }
        let onScreen = order.filter { headerTops[$0] != nil }
        if let reached = onScreen.last(where: { (headerTops[$0] ?? .infinity) <= threshold }) {
            return reached
        }
        if let firstOnScreen = onScreen.first, let index = order.firstIndex(of: firstOnScreen) {
            return index > 0 ? order[index - 1] : firstOnScreen
        }
        if let current, order.contains(current) { return current }
        return first
    }

    private static func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil).lowercased()
    }
}

struct SettingsView: View {
    @ObservedObject private var actions = AppActions.shared
    @ObservedObject private var themeManager = ThemeManager.shared
    @ObservedObject private var audio = AudioSettings.shared
    @ObservedObject private var siriIdentity = PrimaryAgentSiriAdvertiser.shared
    @State private var showConnectionScanner = false
    @State private var pendingConnection: MobileEnrollmentLink?
    /// A scanner result is staged until the scanner sheet has fully dismissed.
    /// Presenting the confirmation alert in the same update that dismisses the
    /// sheet is racy on iOS: UIKit may discard the alert, leaving a successful
    /// scan looking like it did nothing.
    @State private var scannedConnection: MobileEnrollmentLink?
    @State private var scannedConnectionError: String?
    @State private var connectionMessage: String?
    @State private var connecting = false
    @State private var selectedConnectionMode: MobileConnectionMode =
        MagicianAccess.connectionProfile.map { MobileConnectionMode.forOrigin($0.publicOrigin) } ?? .remote
    @StateObject private var versions = VersionInfoModel()
    @ObservedObject private var health = HealthViewModel.shared
    @ObservedObject private var ambient = AmbientController.shared
    @ObservedObject private var atAGlance = MobileAtAGlanceUpdates.shared
    /// App Group flag the notification service extension reads before it puts
    /// an ask's question and answer buttons on a notification.
    @AppStorage(AttentionPushAnswer.showQuestionsOnLockScreenKey, store: MagicianAccess.store)
    private var showQuestionsOnLockScreen = false
    /// iOS's Show Previews for this app, re-read on appear and on foreground:
    /// `Always`/`Never` keep questions hidden while the toggle is off.
    @State private var notificationPreviews: UNShowPreviewsSetting?
    /// Persisted in the App Group store, like `SiriPhrasePresentation`'s phrase, and
    /// set when the prompt is RAISED rather than when it is dismissed — see
    /// `AmbientControlCenterHint`.
    @AppStorage(AmbientControlCenterHint.shownKey, store: MagicianAccess.store)
    private var didShowAmbientControlHint = false
    @State private var showAmbientControlHint = false
    /// A Talk tap should not put setup chrome over the first conversation. Earn
    /// the one-time Control Center hint now, but present it only when the call
    /// returns to quiet availability.
    @State private var controlHintPendingUntilAvailable = false
    @State private var showResetLearnedAlert = false
    @State private var learnedCleared = false
    @State private var customSiriPhrase = SiriPhrasePresentation.initialPersonalPhrase(
        stored: MagicianAccess.store.string(forKey: SiriPhrasePresentation.customPhraseKey),
        identity: PrimaryAgentSiriIdentityStore.load()
    )
    @State private var customPhraseFollowsIdentity = MagicianAccess.store
        .string(forKey: SiriPhrasePresentation.customPhraseKey)?
        .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false
    @State private var copiedSiriPhrase = false
    /// Settings search; every row matches on its title and keywords.
    @State private var searchQuery = ""
    /// Whether a machine is paired, so the web Settings row can open.
    @State private var isPaired = MagicianAccess.connectionProfile != nil
    /// Scroll-spy for the section chip bar. Held as plain `@State`, not
    /// `@StateObject`: only the bar observes it, so heading geometry reported
    /// on every scroll frame never re-renders this whole Form.
    @State private var sectionSpy = SettingsSectionSpy()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private func refreshNotificationPreviews() async {
        notificationPreviews = await UNUserNotificationCenter.current().notificationSettings().showPreviewsSetting
    }

    var body: some View {
        NavigationView {
            ScrollViewReader { formProxy in
                Form {
                    if showsRow(.webSettings) {
                        Section {
                            webSettingsRow
                        }
                        .listRowBackground(themeManager.surfaceColor)
                    }

                    if showsSection(.voice) {
                        Section {
                            if showsRow(.deviceVoice) {
                                DeviceSpeechVoicePicker()
                            }
                            if showsRow(.talkToProduct) {
                                siriSetupCard
                                    .listRowInsets(EdgeInsets(top: 10, leading: 12, bottom: 10, trailing: 12))
                            }
                            if showsRow(.talkBehavior) {
                                talkBehaviorRows
                            }
                        } header: {
                            sectionHeading(.voice)
                        } footer: {
                            sectionEndMarker(.voice)
                        }
                        .listRowBackground(themeManager.surfaceColor)
                    }

                    if showsSection(.notifications) {
                        Section {
                            if showsRow(.notificationPermissions) {
                                notificationPermissionRows
                            }
                            if showsRow(.atAGlance) {
                                atAGlanceRows
                            }
                        } header: {
                            sectionHeading(.notifications)
                        } footer: {
                            sectionEndMarker(.notifications)
                        }
                        .listRowBackground(themeManager.surfaceColor)
                    }

                    if showsSection(.keyboard) {
                        Section {
                            if showsRow(.keyboardSettings) {
                                keyboardSettingsLink
                            }
                            if showsRow(.keyboardReset) {
                                keyboardResetRows
                            }
                        } header: {
                            sectionHeading(.keyboard)
                        } footer: {
                            sectionEndMarker(.keyboard)
                        }
                        .listRowBackground(themeManager.surfaceColor)
                    }

                    if showsSection(.connection) {
                        MachinesSection(showsMachines: showsRow(.machines)) {
                            sectionHeading(.connection)
                        } footer: {
                            sectionEndMarker(.connection)
                        } extra: {
                            if showsRow(.selfHostedConnection) {
                                selfHostedConnectionRows
                            }
                            if showsRow(.services) {
                                serviceHealthRows
                            }
                        }
                    }

                    if showsSection(.help) {
                        Section {
                            if showsRow(.howToUse) {
                                howToUseLink
                            }
                            if showsRow(.about) {
                                aboutRows
                            }
                        } header: {
                            sectionHeading(.help)
                        } footer: {
                            sectionEndMarker(.help)
                        }
                        .listRowBackground(themeManager.surfaceColor)
                    }

                    if SettingsSearch.isActive(searchQuery), SettingsSearch.visibleRows(for: searchQuery).isEmpty {
                        Section {
                            noMatchRows
                        }
                        .listRowBackground(themeManager.surfaceColor)
                    }
                }
                // Pinned under the search field; the Form scrolls beneath it.
                .safeAreaInset(edge: .top, spacing: 0) {
                    if !chipSections.isEmpty {
                        SettingsSectionBar(sections: chipSections, spy: sectionSpy) { section in
                            jump(to: section, proxy: formProxy)
                        }
                    }
                }
                .onChange(of: searchQuery, initial: true) { _, query in
                    sectionSpy.setOrder(SettingsSearch.visibleSections(for: query))
                }
            }
            .searchable(
                text: $searchQuery,
                placement: .navigationBarDrawer(displayMode: .always),
                prompt: "Search settings"
            )
            .scrollContentBackground(.hidden)
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle("Settings")
            .toolbarBackground(themeManager.backgroundColor, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbarColorScheme(themeManager.colorScheme, for: .navigationBar)
            .onAppear {
                isPaired = MagicianAccess.connectionProfile != nil
                versions.load()
                health.checkHealth()
                Task { await atAGlance.refreshStatus() }
                Task { await refreshNotificationPreviews() }
                if let link = actions.consumeMobileConnection() {
                    selectedConnectionMode = link.connectionMode
                    pendingConnection = link
                }
            }
            .onReceive(NotificationCenter.default.publisher(for: UIApplication.willEnterForegroundNotification)) { _ in
                Task { await refreshNotificationPreviews() }
            }
            .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
                isPaired = MagicianAccess.connectionProfile != nil
            }
            .sheet(isPresented: $showConnectionScanner, onDismiss: {
                if let link = scannedConnection {
                    scannedConnection = nil
                    pendingConnection = link
                } else if let message = scannedConnectionError {
                    scannedConnectionError = nil
                    connectionMessage = message
                }
            }) {
                MobileConnectionScanner { result in
                    switch result {
                    case .success(let link):
                        if link.connectionMode == selectedConnectionMode {
                            scannedConnection = link
                            scannedConnectionError = nil
                        } else {
                            scannedConnection = nil
                            scannedConnectionError = MobileConnectionError.routeMismatch(
                                selected: selectedConnectionMode,
                                scanned: link.connectionMode
                            ).localizedDescription
                        }
                    case .failure(let error):
                        scannedConnection = nil
                        scannedConnectionError = error.localizedDescription
                    }
                    showConnectionScanner = false
                }
            }
            .alert(item: $pendingConnection) { link in
                Alert(
                    title: Text(link.usesSameWifi ? "Same Wi-Fi · this computer" : "Remote · works anywhere"),
                    message: Text(
                        link.usesSameWifi
                            ? "Connect directly to \(link.publicOrigin.host ?? link.publicOrigin.absoluteString) on this trusted Wi-Fi. The private address is the computer; localhost would mean this iPhone."
                            : "Connect through \(link.publicOrigin.host ?? link.publicOrigin.absoluteString). Choose this when the iPhone is away from the computer or on another network."
                    ),
                    primaryButton: .default(Text("Connect")) {
                        connecting = true
                        Task {
                            do {
                                // Install only after verify succeeded; that also
                                // drops this machine's other-route profile, whose
                                // token the host just revoked.
                                let grant = try await MobileEnrollmentClient.exchange(link)
                                let profile = grant.profile
                                try MagicianAccess.install(profile, sameMachineOrigins: grant.machineOrigins)
                                await MainActor.run {
                                    connecting = false
                                    selectedConnectionMode = link.connectionMode
                                    connectionMessage = "Connected to \(profile.publicOrigin.host ?? profile.publicOrigin.absoluteString)."
                                    versions.load()
                                    health.checkHealth()
                                }
                            } catch {
                                await MainActor.run {
                                    connecting = false
                                    connectionMessage = error.localizedDescription
                                }
                            }
                        }
                    },
                    secondaryButton: .cancel()
                )
            }
            .onChange(of: siriIdentity.identity) { oldIdentity, newIdentity in
                guard customPhraseFollowsIdentity else { return }
                let previousSuggestion = SiriPhrasePresentation.initialPersonalPhrase(
                    stored: nil,
                    identity: oldIdentity
                )
                guard customSiriPhrase == previousSuggestion else { return }
                customSiriPhrase = SiriPhrasePresentation.initialPersonalPhrase(
                    stored: nil,
                    identity: newIdentity
                )
            }
            .onChange(of: ambient.state) { _, state in
                if state == .armed, controlHintPendingUntilAvailable {
                    controlHintPendingUntilAvailable = false
                    raiseControlCenterHintIfEarned()
                } else if !state.windowIsOpen {
                    controlHintPendingUntilAvailable = false
                }
            }
            .alert(AmbientControlCenterHint.title, isPresented: $showAmbientControlHint) {
                Button("Got it", role: .cancel) {}
            } message: {
                Text(AmbientControlCenterHint.message)
            }
            .alert("Reset learned words?", isPresented: $showResetLearnedAlert) {
                Button("Reset", role: .destructive) { resetLearnedVocabulary() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("This forgets every word and phrase the keyboard learned from your typing. It can't be undone.")
            }
        }
    }

    // MARK: Rows

    private func showsRow(_ row: SettingsSearchRow) -> Bool {
        SettingsSearch.matches(searchQuery, row: row)
    }

    private func showsSection(_ section: SettingsSearchSection) -> Bool {
        SettingsSearchRow.allCases.contains { $0.section == section && showsRow($0) }
    }

    // MARK: Section bar

    /// One chip per headed section that has rows for the current query.
    private var chipSections: [SettingsSearchSection] {
        SettingsSearch.visibleSections(for: searchQuery)
    }

    private func sectionHeading(_ section: SettingsSearchSection) -> some View {
        SettingsSectionHeading(section: section, spy: sectionSpy)
    }

    /// An invisible footer on the last shown section: on screen means the
    /// list's end is reached, which selects that section's chip.
    @ViewBuilder
    private func sectionEndMarker(_ section: SettingsSearchSection) -> some View {
        if section == chipSections.last {
            SettingsSectionEndMarker(section: section, spy: sectionSpy)
        }
    }

    /// A chip tap: select it now (the spy holds still while the list moves)
    /// and scroll its heading to the top, without animation under Reduce Motion.
    private func jump(to section: SettingsSearchSection, proxy: ScrollViewProxy) {
        sectionSpy.select(section)
        if reduceMotion {
            proxy.scrollTo(section.anchorID, anchor: .top)
        } else {
            withAnimation(.easeInOut(duration: 0.3)) {
                proxy.scrollTo(section.anchorID, anchor: .top)
            }
        }
    }

    /// The shared web Settings (budgets, preferences, voices, workspaces and
    /// the rest) for the machine in use, embedded full screen. Unpaired, the
    /// row explains instead of navigating.
    @ViewBuilder
    private var webSettingsRow: some View {
        if isPaired {
            NavigationLink(destination: EmbeddedWebSettingsView()) {
                webSettingsLabel
            }
            .accessibilityIdentifier("settings-web-settings")
        } else {
            VStack(alignment: .leading, spacing: 6) {
                webSettingsLabel
                Text("Pair this iPhone with a machine first. Connection below has the QR scanner.")
                    .font(.caption)
                    .foregroundColor(themeManager.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("settings-web-settings")
        }
    }

    private var webSettingsLabel: some View {
        Label {
            VStack(alignment: .leading, spacing: 2) {
                Text(SettingsSearchRow.webSettings.title)
                    .foregroundColor(themeManager.textColor)
                Text("Budgets, preferences, voices, workspaces, models and more")
                    .font(.caption)
                    .foregroundColor(themeManager.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } icon: {
            Image(systemName: "gearshape.2")
                .foregroundColor(themeManager.accentColor)
        }
    }

    @ViewBuilder
    private var noMatchRows: some View {
        Text("No settings match “\(SettingsSearch.trimmed(searchQuery))”.")
            .foregroundColor(themeManager.secondaryTextColor)
            .accessibilityIdentifier("settings-search-no-match")
        if isPaired {
            NavigationLink(destination: EmbeddedWebSettingsView()) {
                Label("Search \(ProductIdentity.productName) settings", systemImage: "magnifyingglass")
                    .foregroundColor(themeManager.accentColor)
            }
            .accessibilityIdentifier("settings-search-web")
        } else {
            Text("Shared \(ProductIdentity.productName) settings open here once this iPhone is paired with a machine.")
                .font(.caption)
                .foregroundColor(themeManager.secondaryTextColor)
        }
    }

    @ViewBuilder
    private var talkBehaviorRows: some View {
        ambientArmControl
        ambientVoiceModePicker
        ambientLeashPicker
        ambientPhraseReadout
    }

    @ViewBuilder
    private var notificationPermissionRows: some View {
        HStack {
            Label("Notifications", systemImage: "bell.badge.fill")
                .foregroundColor(themeManager.textColor)
            Spacer()
            Text(atAGlance.statusLabel)
                .foregroundColor(themeManager.secondaryTextColor)
        }

        if atAGlance.authorizationStatus == .authorized
            || atAGlance.authorizationStatus == .provisional
            || atAGlance.authorizationStatus == .ephemeral {
            Button {
                Task { await atAGlance.enable() }
            } label: {
                Text("Refresh registration")
            }
            .disabled(!atAGlance.remoteRegistrationSupported)
            .foregroundColor(themeManager.accentColor)
        }

        Button {
            if atAGlance.authorizationStatus == .notDetermined {
                Task { await atAGlance.enable() }
            } else if let url = URL(string: UIApplication.openSettingsURLString) {
                UIApplication.shared.open(url)
            }
        } label: {
            Text(atAGlance.authorizationStatus == .notDetermined
                 ? (atAGlance.remoteRegistrationSupported
                    ? "Allow notifications"
                    : "Remote alerts unavailable in this build")
                 : "Open iOS Settings")
        }
        .disabled(!atAGlance.remoteRegistrationSupported && atAGlance.authorizationStatus == .notDetermined)
        .foregroundColor(themeManager.accentColor)

        Text(atAGlance.statusMessage
             ?? "Notifications shows whether alerts are allowed. Allow notifications asks again while iOS will still show the prompt. After a denial, Open iOS Settings is where that choice can change.")
            .font(.caption)
            .foregroundColor(themeManager.secondaryTextColor)

        Toggle("Show questions on lock screen", isOn: $showQuestionsOnLockScreen)
            .foregroundColor(themeManager.textColor)
            .tint(themeManager.accentColor)
            .accessibilityIdentifier("settings.notifications.showQuestionsOnLockScreen")

        Text(showQuestionsOnLockScreen
             ? "Questions and answer buttons show on the lock screen; answering still needs Face ID."
             : "Questions and answer buttons appear after you unlock.")
            .font(.caption)
            .foregroundColor(themeManager.secondaryTextColor)

        if let previews = notificationPreviews,
           let hint = AttentionPushPreviewHint.hint(
               showOnLockScreen: showQuestionsOnLockScreen, previews: previews) {
            Text(hint.message)
                .font(.caption)
                .foregroundColor(themeManager.secondaryTextColor)
                .accessibilityIdentifier("settings.notifications.previewHint")
            Button("Open notification settings") {
                if let url = URL(string: UIApplication.openNotificationSettingsURLString) {
                    UIApplication.shared.open(url)
                }
            }
            .foregroundColor(themeManager.accentColor)
        }
    }

    @ViewBuilder
    private var atAGlanceRows: some View {
        HStack {
            Label("Widget & task progress", systemImage: "rectangle.3.group.fill")
                .foregroundColor(themeManager.textColor)
            Spacer()
            Text(ActivityAuthorizationInfo().areActivitiesEnabled
                 ? "Ready"
                 : "Live Activities off")
                .foregroundColor(themeManager.secondaryTextColor)
        }

        Text("Add “\(ProductIdentity.productName) at a glance” from the widget gallery; it refreshes even when alerts are off. Notification permission is just above. Task progress follows the iPhone’s Live Activities setting. Listen and Talk activities remain private and local to this iPhone.")
            .font(.caption)
            .foregroundColor(themeManager.secondaryTextColor)
    }

    @ViewBuilder
    private var keyboardSettingsLink: some View {
        NavigationLink(destination: KeyboardSettingsView()) {
            Label("\(ProductIdentity.productName) Keyboard", systemImage: "keyboard")
                .foregroundColor(themeManager.textColor)
        }
    }

    @ViewBuilder
    private var keyboardResetRows: some View {
        Button(role: .destructive, action: { showResetLearnedAlert = true }) {
            HStack {
                Image(systemName: "trash")
                Text("Reset learned words")
                if learnedCleared {
                    Spacer()
                    Image(systemName: "checkmark")
                        .foregroundColor(themeManager.secondaryTextColor)
                }
            }
            .foregroundColor(themeManager.dangerColor)
        }
        Text("Forget the words and phrases the \(ProductIdentity.productName) keyboard has picked up from your typing. Takes effect the next time the keyboard loads.")
            .font(.caption)
            .foregroundColor(themeManager.secondaryTextColor)
    }

    @ViewBuilder
    private var selfHostedConnectionRows: some View {
        Picker("Connection route", selection: $selectedConnectionMode) {
            ForEach(MobileConnectionMode.allCases) { mode in
                Text(mode.title).tag(mode)
            }
        }
        .pickerStyle(.segmented)
        .disabled(connecting)

        Text(
            selectedConnectionMode == .sameWifi
                ? "Use when this iPhone and the computer hosting the backend share the same trusted Wi-Fi. Choose Same Wi-Fi on the computer too."
                : "Use from another network or when the backend runs on a remote machine. Choose Remote on the computer too."
        )
        .font(.caption)
        .foregroundColor(themeManager.secondaryTextColor)

        Button(action: {
            connectionMessage = nil
            showConnectionScanner = true
        }) {
            HStack {
                Image(systemName: "qrcode.viewfinder")
                Text(connecting ? "Connecting…" : "Scan \(selectedConnectionMode.title) QR")
            }
            .foregroundColor(themeManager.accentColor)
        }
        .disabled(connecting)

        HStack {
            Text("Current Endpoint")
                .foregroundColor(themeManager.textColor)
            Spacer()
            Text(MagicianAccess.isConfigured ? (MagicianAccess.baseURL.host ?? MagicianAccess.baseURL.absoluteString) : "Not connected")
                .foregroundColor(themeManager.secondaryTextColor)
        }

        if let profile = MagicianAccess.connectionProfile {
            HStack {
                Text("Current Route")
                    .foregroundColor(themeManager.textColor)
                Spacer()
                Text(MobileConnectionMode.forOrigin(profile.publicOrigin).title)
                    .foregroundColor(themeManager.secondaryTextColor)
            }
        }

        if let connectionMessage {
            Text(connectionMessage)
                .font(.caption)
                .foregroundColor(themeManager.secondaryTextColor)
        }
    }

    @ViewBuilder
    private var serviceHealthRows: some View {
        NavigationLink("Service accounts", destination: BrowserServiceAccountsView())
        serviceRow(MagicianAccess.backendServiceLabel, health.magicianState, versions.magician)
        serviceRow("Magicutor", health.magicutorState, versions.magicutor)
        serviceRow("DesktopProxy", health.tauriState, versions.desktop)
        Button(action: health.checkHealth) {
            Label(health.isChecking ? "Checking…" : "Refresh service status", systemImage: "arrow.clockwise")
        }
        .disabled(health.isChecking)
        .foregroundColor(themeManager.accentColor)
    }

    @ViewBuilder
    private var howToUseLink: some View {
        NavigationLink(destination: HowToUseView()) {
            Label("How to Use", systemImage: "questionmark.circle")
                .foregroundColor(themeManager.textColor)
        }
    }

    @ViewBuilder
    private var aboutRows: some View {
        versionRow("\(MagicianAccess.productName) iOS", AppInfo.versionAndBuild)
        versionRow(MagicianAccess.backendServiceLabel, versions.magician)
        if versions.magicianAPI != nil { versionRow("\(MagicianAccess.backendServiceLabel) API", versions.magicianAPI) }
        if versions.magicutor != nil { versionRow("Magicutor", versions.magicutor) }
        if versions.desktop != nil { versionRow("Desktop", versions.desktop) }
        if versions.supervisor != nil { versionRow("Supervisor", versions.supervisor) }
    }

    /// Start and stop a listening window from inside the app.
    ///
    /// **Nothing armed from in-app before this**, so a fresh install had no way into
    /// the feature at all: the only entry was `ArmAmbientIntent`, which lives in
    /// Control Center and Shortcuts. This section already let the user choose a
    /// two-hour leash while giving them no way to start one.
    ///
    /// It goes through `AmbientEntryPoint.talk()` — the same call the intent makes —
    /// so every refusal is identical from either door, and the phrase set and leash
    /// are constructed once rather than twice. A second construction here is exactly
    /// how the two would come to disagree about what the wake word is.
    ///
    /// **The stop affordance is keyed off `AmbientState.windowIsOpen`, never off
    /// `orbPhase`.** That reduction maps `.recoverableError` onto `.armed`, and every
    /// refused arm lands in `.recoverableError` — so a control keyed off it would
    /// offer to stop a window that never opened, which is the inverse lie
    /// `AmbientMiniBar` was written to avoid. The predicate lives on the state so
    /// this and that bar cannot drift.
    ///
    /// The refusal message is rendered HERE rather than left to the orb, and that is
    /// the division of labour: a refused arm has no orb (no window opened, so
    /// nothing was published), and the user is looking at this screen.
    @ViewBuilder
    private var ambientArmControl: some View {
        let phrases = AmbientActivationPhrases.forArming(identity: siriIdentity.identity)
        VStack(alignment: .leading, spacing: 8) {
            if ambient.state.windowIsOpen {
                Button(role: .destructive) {
                    // No reason: this is the user's own doing, and a reasonless end
                    // dismisses the orb immediately rather than lingering with an
                    // explanation nobody needs. Same as `AmbientMiniBar`'s button.
                    Task { await AmbientController.shared.disarm(reason: nil) }
                } label: {
                    Label("Stop listening", systemImage: "stop.fill")
                }
                .accessibilityIdentifier("settings-ambient-stop")
            } else {
                Button {
                    Task {
                        await AmbientEntryPoint.talk()
                        controlHintPendingUntilAvailable = ambient.state.windowIsOpen
                    }
                } label: {
                    Label(
                        ambient.state == .arming ? "Starting…" : "Talk to \(ProductIdentity.productName)",
                        systemImage: "waveform"
                    )
                }
                // An empty phrase set can never wake the spotter, and
                // `ambientPhraseReadout` below already explains why. Disabling
                // beats arming into a guaranteed refusal.
                .disabled(phrases.isEmpty || ambient.state == .arming)
                .accessibilityIdentifier("settings-ambient-start")
            }
            if case .recoverableError(let message) = ambient.state {
                Label(message, systemImage: "exclamationmark.circle")
                    .font(.caption)
                    .foregroundColor(themeManager.warningColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            // The power condition the open window is running in spite of. Carried
            // for the whole window, not reported once — it is the entire
            // justification for having armed anyway.
            if ambient.state.windowIsOpen, let warning = ambient.powerWarning {
                Label(warning.warning, systemImage: "battery.25")
                    .font(.caption)
                    .foregroundColor(themeManager.warningColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityIdentifier("settings-ambient-arm")
    }

    /// Raise the Control Center nudge, at most once ever, and only after a window
    /// actually opened. See `AmbientControlCenterHint`.
    private func raiseControlCenterHintIfEarned() {
        guard AmbientControlCenterHint.shouldShow(
            alreadyShown: didShowAmbientControlHint,
            armSucceeded: ambient.state.windowIsOpen
        ) else { return }
        didShowAmbientControlHint = true
        showAmbientControlHint = true
    }

    /// The leash on a microphone nobody is watching. Every option is finite —
    /// including the one labelled otherwise, which says so in its own caption.
    private var ambientLeashPicker: some View {
        VStack(alignment: .leading, spacing: 8) {
            Picker("Stay available for", selection: $audio.ambientLeash) {
                ForEach(AmbientLeash.allCases) { leash in
                    Text(leash.label).tag(leash)
                }
            }
            .pickerStyle(.segmented)
            Text(audio.ambientLeash.detail)
                .font(.caption)
                .foregroundColor(themeManager.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityIdentifier("settings-ambient-leash")
    }

    /// What arming will actually listen for, shown BEFORE the user arms.
    ///
    /// The phrases are derived from the assistant's name rather than typed here,
    /// so without this the user has no way to know what to say — and no way to
    /// find out what the wake model actually does with the name they picked.
    /// `VoskWakeSpotter.assessment(of:)` is a pure function over the phrase, so
    /// this needs no spotter, no model and no armed window; the same sentence is
    /// still carried on the armed window's `AmbientPhraseSet.notes`, but rendered
    /// nowhere since the chip went single-line — this readout is where it is
    /// read.
    ///
    /// **Every phrase gets a line, not just the bad ones.** The line used to be a
    /// fixed warning shown only to phrases a two-rule check flagged, and it made
    /// two claims the measurements do not support: that the unflagged phrases
    /// were fine (most of them have never been measured at all), and that "a
    /// longer, more distinctive name is heard far more reliably" (`hey samy`, the
    /// best row ever measured at 6%, is shorter than `hey sammie` at 46%). The
    /// note carries the measured numbers, or says plainly that there aren't any.
    ///
    /// **Two numbers, not one, and the wake rate comes first.** A near-miss
    /// false-accept rate on its own is unreadable: a phrase that never fires
    /// scores a perfect 0%, so the sentence would praise a name the user could
    /// not summon. `VoskWakeSpotter.assessment(of:)` has the measurement that
    /// established this and the reason neither number gets a verdict attached.
    @ViewBuilder
    private var ambientPhraseReadout: some View {
        let phrases = AmbientActivationPhrases.forArming(identity: siriIdentity.identity)
        if phrases.isEmpty {
            Label(
                "\(ProductIdentity.productName) doesn't know your assistant's name yet, so ambient listening can't start. It loads the next time \(ProductIdentity.productName) syncs.",
                systemImage: "exclamationmark.circle"
            )
            .font(.caption)
            .foregroundColor(themeManager.secondaryTextColor)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                Text("Wakes on \(phrases.map { "“\($0)”" }.joined(separator: " or ")).")
                    .font(.caption)
                    .foregroundColor(themeManager.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(phrases, id: \.self) { phrase in
                    if let note = VoskWakeSpotter.assessment(of: phrase).note {
                        Label(
                            "“\(phrase)”: \(note)",
                            systemImage: "waveform.badge.magnifyingglass"
                        )
                        .font(.caption)
                        .foregroundColor(themeManager.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    private var automaticSiriPhrases: [String] {
        SiriPhrasePresentation.automaticPhrases(identity: siriIdentity.identity)
    }

    private var personalSiriSuggestions: [String] {
        SiriPhrasePresentation.personalSuggestions(identity: siriIdentity.identity)
    }

    private var effectiveCustomSiriPhrase: String {
        SiriPhrasePresentation.initialPersonalPhrase(
            stored: customSiriPhrase,
            identity: siriIdentity.identity
        )
    }

    private var customSiriPhraseBinding: Binding<String> {
        Binding(
            get: { customSiriPhrase },
            set: { value in
                customPhraseFollowsIdentity = false
                customSiriPhrase = value
                persistCustomSiriPhrase(value)
            }
        )
    }

    private var siriSetupCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 11) {
                ZStack {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(themeManager.accentColor.opacity(0.15))
                        .frame(width: 44, height: 44)
                    Image(systemName: "waveform.and.mic")
                        .font(.system(size: 18, weight: .semibold))
                        .foregroundColor(themeManager.accentColor)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("Talk to \(ProductIdentity.productName) anywhere")
                        .font(.themed(16, weight: .bold))
                        .foregroundColor(themeManager.textColor)
                    Text("Use one Talk action across iOS, with optional Siri phrases. Choose its conversation mode below.")
                        .font(.themed(12))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            readyNowSiriCard
            personalSiriCard
        }
        .padding(.vertical, 2)
        .accessibilityIdentifier("settings-siri-setup-card")
    }

    /// Ambient owns its own three-way, device-local conversation choice. The
    /// Dictation option is a complete turn loop (record → transcribe → agent →
    /// speak → listen again), not the composer's one-shot transcript action.
    private var ambientVoiceModePicker: some View {
        VStack(alignment: .leading, spacing: 8) {
            Picker("Conversation mode", selection: $audio.ambientVoiceMode) {
                ForEach(AmbientVoiceMode.allCases) { mode in
                    Text(mode.label).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            Text(ambientVoiceModeDetail)
                .font(.caption)
                .foregroundColor(themeManager.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityIdentifier("settings-ambient-voice-mode")
    }

    private var ambientVoiceModeDetail: String {
        switch audio.ambientVoiceMode {
        case .dictation:
            return "Talk to \(ProductIdentity.productName) records each turn, transcribes it, speaks the answer, then waits for a wake-word follow-up. Chat keeps its own choice."
        case .handsFree:
            return "Talk to \(ProductIdentity.productName) uses the configured streaming STT → agent → TTS pipeline. Chat keeps its own choice."
        case .realtime:
            return "Talk to \(ProductIdentity.productName) uses the selected low-latency realtime provider. Chat keeps its own choice."
        }
    }

    private var readyNowSiriCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Ready now", systemImage: "checkmark.seal.fill")
                    .font(.themed(13, weight: .semibold))
                    .foregroundColor(themeManager.successColor)
                Spacer()
                Text("NO SETUP")
                    .font(.themedMono(9, weight: .bold))
                    .tracking(0.5)
                    .foregroundColor(themeManager.successColor)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 4)
                    .background(themeManager.successColor.opacity(0.12))
                    .clipShape(Capsule())
            }

            ForEach(automaticSiriPhrases, id: \.self) { phrase in
                HStack(spacing: 8) {
                    Image(systemName: "quote.opening")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundColor(themeManager.accentColor)
                    Text(phrase)
                        .font(.themed(14, weight: .semibold))
                        .foregroundColor(themeManager.textColor)
                    Spacer(minLength: 0)
                }
                .accessibilityLabel("Say \(phrase)")
            }

            Text("Siri then asks for the request and sends it to your current primary assistant.")
                .font(.themed(11))
                .foregroundColor(themeManager.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(13)
        .background(themeManager.softBackgroundColor)
        .clipShape(RoundedRectangle(cornerRadius: 15))
        .overlay(
            RoundedRectangle(cornerRadius: 15)
                .stroke(themeManager.successColor.opacity(0.24), lineWidth: 1)
        )
    }

    private var personalSiriCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                Label("Make it shorter", systemImage: "wand.and.stars")
                    .font(.themed(13, weight: .semibold))
                    .foregroundColor(themeManager.accentColor)
                Spacer()
                Text("ONE-TIME SETUP")
                    .font(.themedMono(9, weight: .bold))
                    .tracking(0.5)
                    .foregroundColor(themeManager.accentColor)
            }

            Text("Choose what you want to say")
                .font(.themed(11, weight: .medium))
                .foregroundColor(themeManager.secondaryTextColor)

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 7) {
                    ForEach(personalSiriSuggestions, id: \.self) { phrase in
                        Button {
                            selectCustomSiriPhrase(phrase)
                        } label: {
                            Text(phrase)
                                .font(.themed(11, weight: .semibold))
                                .foregroundColor(customSiriPhrase == phrase
                                    ? themeManager.onAccentColor
                                    : themeManager.secondaryTextColor)
                                .padding(.horizontal, 10)
                                .frame(height: 30)
                                .background(customSiriPhrase == phrase
                                    ? themeManager.accentFillColor
                                    : themeManager.surfaceColor)
                                .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
                                .overlay(
                                    RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).stroke(
                                        customSiriPhrase == phrase
                                            ? themeManager.accentColor
                                            : themeManager.controlBorderColor,
                                        lineWidth: 1
                                    )
                                )
                        }
                        .buttonStyle(.plain)
                    }
                }
            }

            HStack(spacing: 8) {
                TextField("Shortcut name", text: customSiriPhraseBinding)
                    .textInputAutocapitalization(.words)
                    .autocorrectionDisabled()
                    .font(.themed(14, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                    .padding(.horizontal, 11)
                    .frame(height: 42)
                    .background(themeManager.surfaceColor)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(themeManager.controlBorderColor, lineWidth: 1)
                    )
                    .accessibilityIdentifier("settings-siri-custom-phrase")

                Button {
                    copyCustomSiriPhrase()
                } label: {
                    Image(systemName: copiedSiriPhrase ? "checkmark" : "doc.on.doc")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundColor(copiedSiriPhrase
                            ? themeManager.successColor
                            : themeManager.accentColor)
                        .frame(width: 42, height: 42)
                        .background(themeManager.surfaceColor)
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                        .overlay(
                            RoundedRectangle(cornerRadius: 10)
                                .stroke(themeManager.controlBorderColor, lineWidth: 1)
                        )
                }
                .buttonStyle(.plain)
                .accessibilityLabel(copiedSiriPhrase ? "Phrase copied" : "Copy phrase")
            }

            VStack(alignment: .leading, spacing: 6) {
                setupStep(1, "Open Shortcuts and create a personal shortcut with \(ProductIdentity.productName)’s Ask Assistant action.")
                setupStep(2, "Set Prompt to Ask Each Time, then name the shortcut “\(effectiveCustomSiriPhrase)”.")
                setupStep(3, "Say “Hey Siri, \(effectiveCustomSiriPhrase)”.")
            }

            ShortcutsLink {
                // Put the exact chosen name on the pasteboard before switching
                // apps; this removes the only error-prone part of the setup.
                copyCustomSiriPhrase(resetFeedback: false)
            }
            .shortcutsLinkStyle(.automaticOutline)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityHint("Opens \(ProductIdentity.productName) actions in Apple Shortcuts. The chosen phrase is copied.")
            .accessibilityIdentifier("settings-open-shortcuts")
        }
        .padding(13)
        .background(themeManager.accentColor.opacity(0.07))
        .clipShape(RoundedRectangle(cornerRadius: 15))
        .overlay(
            RoundedRectangle(cornerRadius: 15)
                .stroke(themeManager.accentColor.opacity(0.25), lineWidth: 1)
        )
    }

    private func setupStep(_ number: Int, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text("\(number)")
                .font(.system(size: 10, weight: .bold, design: .rounded))
                .foregroundColor(themeManager.onAccentColor)
                .frame(width: 20, height: 20)
                .background(themeManager.accentFillColor)
                .clipShape(Circle())
            Text(text)
                .font(.themed(11))
                .foregroundColor(themeManager.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func selectCustomSiriPhrase(_ phrase: String) {
        customPhraseFollowsIdentity = false
        customSiriPhrase = phrase
        persistCustomSiriPhrase(phrase)
        copiedSiriPhrase = false
    }

    private func persistCustomSiriPhrase(_ phrase: String) {
        let trimmed = phrase.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            MagicianAccess.store.removeObject(forKey: SiriPhrasePresentation.customPhraseKey)
        } else {
            MagicianAccess.store.set(trimmed, forKey: SiriPhrasePresentation.customPhraseKey)
        }
    }

    private func copyCustomSiriPhrase(resetFeedback: Bool = true) {
        let phrase = effectiveCustomSiriPhrase
        customSiriPhrase = phrase
        customPhraseFollowsIdentity = false
        persistCustomSiriPhrase(phrase)
        UIPasteboard.general.string = phrase
        copiedSiriPhrase = true
        guard resetFeedback else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.8) {
            copiedSiriPhrase = false
        }
    }

    /// Clear the keyboard's learned vocabulary + phrasing. Both stores are backed
    /// by the shared App Group, so removing them here is picked up by the keyboard
    /// extension the next time it loads. Clears the trusted (reject-fed) words too,
    /// since those live in the same learned-words map.
    private func resetLearnedVocabulary() {
        LearnedWordsStore().reset()
        LearnedBigramsStore().reset()
        learnedCleared = true
    }

    /// A label + right-aligned monospaced version value. Required rows show a
    /// loading dot and then "not reported" if unavailable; optional component
    /// rows are omitted unless the health response includes their version.
    private func versionRow(_ label: String, _ value: String?) -> some View {
        HStack {
            Text(label).foregroundColor(themeManager.textColor)
            Spacer()
            Text(value ?? (versions.loaded ? "not reported" : "…"))
                .font(.themedMono(.subheadline))
                .foregroundColor(themeManager.secondaryTextColor)
        }
    }

    private func serviceRow(_ label: String, _ state: ServiceHealthState, _ version: String? = nil) -> some View {
        HStack {
            Circle().fill(state.color).frame(width: 9, height: 9)
            Text(label).foregroundColor(themeManager.textColor)
            if let version, !version.isEmpty {
                Text(version)
                    .font(.themedMono(.caption2))
                    .foregroundColor(themeManager.secondaryTextColor)
            }
            Spacer()
            Text(state.label).foregroundColor(themeManager.secondaryTextColor)
        }
    }

    private func themeModeButton(dark: Bool) -> some View {
        let selected = themeManager.isDark == dark
        let label = dark ? "Dark" : "Light"
        return Button {
            themeManager.setDarkMode(dark)
        } label: {
            Image(systemName: dark ? "moon.fill" : "sun.max.fill")
                .font(.system(size: 14, weight: .semibold))
                .foregroundColor(selected ? themeManager.onAccentColor : themeManager.secondaryTextColor)
                .frame(width: 34, height: 28)
                .background(
                    RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                        .fill(selected ? themeManager.accentFillColor : Color.clear)
                )
                .contentShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Use \(label.lowercased()) appearance")
        .accessibilityValue(selected ? "Selected" : "")
    }
}

/// Scroll-spy state behind the Settings section chip bar. Headings and end
/// markers report here as the Form scrolls; only `active` is published, and
/// only when it changes. The choice itself is `SettingsSearch.activeSection`.
final class SettingsSectionSpy: ObservableObject {
    @Published private(set) var active: SettingsSearchSection?

    /// How long after a chip tap scroll reports are ignored, so the tapped
    /// chip does not flicker through the sections the list passes.
    static let tapSettle: TimeInterval = 0.6
    /// How far below the chip bar a heading still counts as scrolled to.
    static let reachSlack: CGFloat = 32

    private var order: [SettingsSearchSection] = []
    private var headerTops: [SettingsSearchSection: CGFloat] = [:]
    private var endsOnScreen: Set<SettingsSearchSection> = []
    private var barBottom: CGFloat = 0
    private var holdUntil = Date.distantPast

    /// The shown sections changed (search). Re-choose at once.
    func setOrder(_ sections: [SettingsSearchSection]) {
        order = sections
        recompute(force: true)
    }

    func select(_ section: SettingsSearchSection) {
        holdUntil = Date().addingTimeInterval(Self.tapSettle)
        publish(section)
    }

    func reportHeader(_ section: SettingsSearchSection, top: CGFloat) {
        headerTops[section] = top
        recompute()
    }

    func headerLeft(_ section: SettingsSearchSection) {
        headerTops[section] = nil
        recompute()
    }

    func reportEnd(_ section: SettingsSearchSection, onScreen: Bool) {
        if onScreen { endsOnScreen.insert(section) } else { endsOnScreen.remove(section) }
        recompute()
    }

    func reportBarBottom(_ y: CGFloat) {
        barBottom = y
    }

    private func recompute(force: Bool = false) {
        guard force || Date() >= holdUntil else { return }
        publish(SettingsSearch.activeSection(
            order: order,
            headerTops: headerTops,
            threshold: barBottom + Self.reachSlack,
            reachedEnd: order.last.map { endsOnScreen.contains($0) } ?? false,
            current: active
        ))
    }

    private func publish(_ section: SettingsSearchSection?) {
        if active != section { active = section }
    }
}

/// A Settings section heading: left-aligned, bold, larger than the stock
/// grouped header. It is the chip bar's scroll target and reports its
/// on-screen position to the spy.
private struct SettingsSectionHeading: View {
    let section: SettingsSearchSection
    let spy: SettingsSectionSpy
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Text(section.title)
            .font(.title3.weight(.bold))
            .foregroundColor(theme.textColor)
            .textCase(nil)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 10)
            .padding(.bottom, 2)
            .accessibilityAddTraits(.isHeader)
            .accessibilityIdentifier("settings-section-heading-\(section.rawValue)")
            .background(
                GeometryReader { proxy in
                    Color.clear
                        .onChange(of: proxy.frame(in: .global).minY, initial: true) { _, top in
                            spy.reportHeader(section, top: top)
                        }
                }
            )
            .onDisappear { spy.headerLeft(section) }
            .id(section.anchorID)
    }
}

/// Invisible footer on the last shown section; on screen = list end reached.
private struct SettingsSectionEndMarker: View {
    let section: SettingsSearchSection
    let spy: SettingsSectionSpy

    var body: some View {
        Color.clear
            .frame(height: 1)
            .accessibilityHidden(true)
            .onAppear { spy.reportEnd(section, onScreen: true) }
            .onDisappear { spy.reportEnd(section, onScreen: false) }
    }
}

/// The pinned, horizontally scrolling chip bar: one chip per shown section,
/// the section in view highlighted, a tap jumps there.
private struct SettingsSectionBar: View {
    let sections: [SettingsSearchSection]
    @ObservedObject var spy: SettingsSectionSpy
    let onSelect: (SettingsSearchSection) -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(sections, id: \.self) { section in
                        chip(section).id(section)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            }
            .onChange(of: spy.active) { _, active in
                guard let active else { return }
                if reduceMotion {
                    proxy.scrollTo(active, anchor: .center)
                } else {
                    withAnimation(.easeInOut(duration: 0.25)) {
                        proxy.scrollTo(active, anchor: .center)
                    }
                }
            }
        }
        .background(theme.backgroundColor)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(theme.controlBorderColor)
                .frame(height: 0.5)
        }
        .background(
            GeometryReader { proxy in
                Color.clear
                    .onChange(of: proxy.frame(in: .global).maxY, initial: true) { _, bottom in
                        spy.reportBarBottom(bottom)
                    }
            }
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("settings-section-bar")
    }

    private func chip(_ section: SettingsSearchSection) -> some View {
        let selected = spy.active == section
        return Button {
            onSelect(section)
        } label: {
            Text(section.title)
                .font(.themed(13, weight: .semibold))
                .foregroundColor(selected ? theme.onAccentColor : theme.secondaryTextColor)
                .lineLimit(1)
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .frame(minHeight: 32)
                .background(Capsule().fill(selected ? theme.accentFillColor : theme.surfaceColor))
                .overlay(
                    Capsule().stroke(selected ? theme.accentColor : theme.controlBorderColor, lineWidth: 1)
                )
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(section.title) section")
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier("settings-section-chip-\(section.rawValue)")
    }
}

private struct MobileEnrollmentExchangeRequest: Encodable {
    let enrollmentId: String
    let secret: String
    let deviceId: String
    let label: String

    enum CodingKeys: String, CodingKey {
        case enrollmentId = "enrollment_id"
        case secret
        case deviceId = "device_id"
        case label
    }
}

private struct MobileEnrollmentExchangeResponse: Decodable {
    struct CloudflareAccess: Decodable {
        let clientId: String
        let clientSecret: String

        enum CodingKeys: String, CodingKey {
            case clientId = "client_id"
            case clientSecret = "client_secret"
        }
    }

    let token: String
    let principal: String
    let workspace: String
    let publicOrigin: URL
    let clientKind: String
    let capabilities: [String]
    let cloudflareAccess: CloudflareAccess?
    /// The paired machine's registry name; absent from older hosts.
    let machineName: String?
    /// That machine's public origin and current same-Wi-Fi origin; absent
    /// from older hosts.
    let machineOrigins: [String]?

    enum CodingKeys: String, CodingKey {
        case token, principal, workspace, capabilities
        case publicOrigin = "public_origin"
        case clientKind = "client_kind"
        case cloudflareAccess = "cloudflare_access"
        case machineName = "machine_name"
        case machineOrigins = "machine_origins"
    }
}

/// A verified pairing: the profile to install, plus every origin the host
/// reported for the same machine (so its other-route profile is replaced).
struct MobileEnrollmentGrant: Equatable {
    let profile: MobileConnectionProfile
    let machineOrigins: [String]
}

private struct MobileEnrollmentProbeResponse: Decodable {
    let deviceId: String
    let principal: String
    let workspace: String

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case principal, workspace
    }
}

enum MobileEnrollmentClient {
    static func exchange(
        _ link: MobileEnrollmentLink,
        session: URLSession = .shared
    ) async throws -> MobileEnrollmentGrant {
        let endpoint = link.publicOrigin.appendingPathComponent(
            "api/magician/v2/devices/enrollment/exchange"
        )
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let deviceLabel = await MainActor.run { UIDevice.current.name }
        request.httpBody = try JSONEncoder().encode(
            MobileEnrollmentExchangeRequest(
                enrollmentId: link.enrollmentID,
                secret: link.secret,
                deviceId: MagicianAccess.pendingDeviceID,
                label: deviceLabel
            )
        )

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw MobileConnectionError.exchangeFailed("\(ProductIdentity.productName) returned no HTTP response.")
        }
        guard (200..<300).contains(http.statusCode) else {
            let message: String
            switch http.statusCode {
            case 401, 403:
                message = "Cloudflare did not allow this enrollment route. Re-run the mobile Access setup on the \(ProductIdentity.productName) host."
            case 404:
                message = "The running \(ProductIdentity.productName) host does not expose this enrollment route. Rebuild and restart the host, then scan a new code."
            case 410:
                message = "This connection code expired or was already used. Create a new one."
            case 503:
                message = "Device pairing storage is unavailable on the \(ProductIdentity.productName) host. Repair and restart the host before scanning another code."
            default:
                message = "\(ProductIdentity.productName) could not connect this iPhone (HTTP \(http.statusCode))."
            }
            throw MobileConnectionError.exchangeFailed(message)
        }

        let grant = try JSONDecoder().decode(MobileEnrollmentExchangeResponse.self, from: data)
        guard grant.publicOrigin == link.publicOrigin,
              grant.clientKind == "ios",
              grant.capabilities.contains("mobile_client"),
              !grant.capabilities.contains("device_automation") else {
            throw MobileConnectionError.invalidProfile
        }
        let profile = try MobileConnectionProfile(
            publicOrigin: grant.publicOrigin,
            principal: grant.principal,
            workspace: grant.workspace,
            deviceID: MagicianAccess.pendingDeviceID,
            deviceToken: grant.token,
            cloudflareClientID: grant.cloudflareAccess?.clientId ?? "",
            cloudflareClientSecret: grant.cloudflareAccess?.clientSecret ?? "",
            machineName: grant.machineName
        )
        try await verify(profile, session: session)
        return MobileEnrollmentGrant(profile: profile, machineOrigins: grant.machineOrigins ?? [])
    }

    private static func verify(
        _ profile: MobileConnectionProfile,
        session: URLSession
    ) async throws {
        var request = URLRequest(
            url: profile.publicOrigin.appendingPathComponent("api/magician/v2/devices/me")
        )
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        for (name, value) in MagicianAccess.authorizedHeaders(
            for: request.url!,
            profile: profile
        ) {
            request.setValue(value, forHTTPHeaderField: name)
        }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode),
              let probe = try? JSONDecoder().decode(MobileEnrollmentProbeResponse.self, from: data),
              probe.deviceId == profile.deviceID,
              probe.principal == profile.principal,
              probe.workspace == profile.workspace else {
            throw MobileConnectionError.verificationFailed
        }
    }
}

/// One entry of the active machine's registry
/// (`GET /api/magician/v2/remote-machines`). It carries no secrets: pairing a
/// listed machine still needs that machine's own QR.
struct RemoteMachine: Decodable, Equatable, Identifiable {
    let name: String
    let apiOrigin: String
    let uiOrigin: String
    let isSelf: Bool
    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name
        case apiOrigin = "api_origin"
        case uiOrigin = "ui_origin"
        case isSelf = "is_self"
    }
}

struct RemoteMachinesResponse: Decodable, Equatable {
    let selfName: String?
    let machines: [RemoteMachine]

    enum CodingKeys: String, CodingKey {
        case selfName = "self"
        case machines
    }
}

enum RemoteMachinesClient {
    /// Reads the registry from the given (default: active) machine with that
    /// machine's own credential.
    static func fetch(
        profile: MobileConnectionProfile? = MagicianAccess.connectionProfile,
        session: URLSession = .shared
    ) async throws -> RemoteMachinesResponse {
        guard let profile else { throw URLError(.userAuthenticationRequired) }
        var request = URLRequest(
            url: profile.publicOrigin.appendingPathComponent("api/magician/v2/remote-machines"),
            timeoutInterval: 10
        )
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        return try JSONDecoder().decode(RemoteMachinesResponse.self, from: data)
    }
}

/// Rows for Settings → Machines: paired machines (named by their stored
/// `machineName`, else from the registry when their origin matches), then
/// registry machines not yet paired by any route.
struct MachinePickerRows: Equatable {
    struct Paired: Equatable, Identifiable {
        let origin: String
        let name: String
        let host: String
        let isActive: Bool
        var id: String { origin }
    }

    let paired: [Paired]
    let unpaired: [RemoteMachine]

    init(profiles: MobileConnectionProfiles, registry: [RemoteMachine]) {
        var byOrigin: [String: RemoteMachine] = [:]
        for machine in registry {
            if let key = MobileConnectionProfiles.originKey(machine.apiOrigin) { byOrigin[key] = machine }
        }
        paired = profiles.ordered.compactMap { profile in
            guard let key = MobileConnectionProfiles.originKey(profile.publicOrigin) else { return nil }
            let host = profile.publicOrigin.host ?? profile.publicOrigin.absoluteString
            return Paired(
                origin: key,
                name: profile.machineName ?? byOrigin[key]?.name ?? host,
                host: host,
                isActive: key == profiles.activeOrigin
            )
        }
        // A machine paired by its other route (same Wi-Fi) is still paired.
        let pairedNames = Set(profiles.profiles.values.compactMap {
            $0.machineName.map(MobileConnectionProfiles.machineNameKey)
        })
        unpaired = registry.filter { machine in
            guard let key = MobileConnectionProfiles.originKey(machine.apiOrigin) else { return false }
            return profiles.profiles[key] == nil
                && !pairedNames.contains(MobileConnectionProfiles.machineNameKey(machine.name))
        }
    }
}

/// Settings → Connection: switch between paired Magician machines without
/// re-pairing, forget one, or see which registry machines still need a QR.
/// The section's other connection rows (`extra`) follow the machine rows;
/// `header` and `footer` are the Settings section heading and end marker.
private struct MachinesSection<Header: View, Footer: View, Extra: View>: View {
    /// False while a Settings search hides the machine rows.
    let showsMachines: Bool
    let header: Header
    let footer: Footer
    let extra: Extra
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.openURL) private var openURL
    @State private var profiles = MagicianAccess.connectionProfiles
    @State private var registry: [RemoteMachine] = []
    @State private var registryUnavailable = false
    @State private var pendingForget: MachinePickerRows.Paired?

    init(
        showsMachines: Bool = true,
        @ViewBuilder header: () -> Header,
        @ViewBuilder footer: () -> Footer,
        @ViewBuilder extra: () -> Extra
    ) {
        self.showsMachines = showsMachines
        self.header = header()
        self.footer = footer()
        self.extra = extra()
    }

    private var rows: MachinePickerRows { MachinePickerRows(profiles: profiles, registry: registry) }

    var body: some View {
        Section {
            if showsMachines {
                machineRows
            }
            extra
        } header: {
            header
        } footer: {
            footer
        }
        .listRowBackground(theme.surfaceColor)
        .task(id: profiles.activeOrigin) { await loadRegistry() }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            reload()
        }
        .alert(item: $pendingForget) { row in
            Alert(
                title: Text("Forget \(row.name)?"),
                message: Text("This iPhone removes its credential for \(row.host). Pairing it again needs a new QR from that machine."),
                primaryButton: .destructive(Text("Forget")) { forget(row) },
                secondaryButton: .cancel()
            )
        }
    }

    @ViewBuilder
    private var machineRows: some View {
        if rows.paired.isEmpty {
            Text("No machine paired yet. Scan a QR below to pair this iPhone.")
                .font(.caption)
                .foregroundColor(theme.secondaryTextColor)
        }
        ForEach(rows.paired) { row in
            Button {
                guard !row.isActive else { return }
                MagicianAccess.activate(origin: row.origin)
                reload()
            } label: {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.name).foregroundColor(theme.textColor)
                        if row.name != row.host {
                            Text(row.host).font(.caption).foregroundColor(theme.secondaryTextColor)
                        }
                    }
                    Spacer()
                    if row.isActive {
                        Image(systemName: "checkmark")
                            .foregroundColor(theme.accentColor)
                            .accessibilityLabel("In use")
                    }
                }
            }
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                Button("Forget", role: .destructive) { pendingForget = row }
            }
            .contextMenu {
                Button("Forget \(row.name)", role: .destructive) { pendingForget = row }
            }
        }
        if rows.paired.count > 1 {
            Text("Tap a machine to use it. Swipe left to forget one; pairing it again needs a new QR from that machine.")
                .font(.caption)
                .foregroundColor(theme.secondaryTextColor)
        }

        if !rows.unpaired.isEmpty {
            ForEach(rows.unpaired) { machine in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(machine.name).foregroundColor(theme.textColor)
                        Spacer()
                        Text("Not paired").font(.caption).foregroundColor(theme.secondaryTextColor)
                    }
                    Text("To pair, open this machine's web UI → Settings → Mobile devices, create an iPhone code and scan it here.")
                        .font(.caption)
                        .foregroundColor(theme.secondaryTextColor)
                    if let url = URL(string: machine.uiOrigin), url.scheme == "https" {
                        Button {
                            openURL(url)
                        } label: {
                            Label("Open \(machine.name) in Safari", systemImage: "safari")
                        }
                        .buttonStyle(.borderless)
                        .foregroundColor(theme.accentColor)
                    }
                }
            }
        } else if registryUnavailable, !rows.paired.isEmpty {
            Text("The machine list is unavailable from the machine in use.")
                .font(.caption)
                .foregroundColor(theme.secondaryTextColor)
        }
    }

    private func reload() {
        profiles = MagicianAccess.connectionProfiles
    }

    private func forget(_ row: MachinePickerRows.Paired) {
        guard let profile = MagicianAccess.profile(forOrigin: row.origin) else { reload(); return }
        // Its push routes there go best-effort, with the credential captured
        // here; forgetting does not wait for them, so an unreachable machine
        // is forgotten at once. The coding-runs retire keeps its own limits.
        let deletes = [
            MobileAtAGlanceUpdates.shared.retireApplicationRoute(for: profile),
        ].compactMap { $0 }
        MobilePushRegistrationClient.giveUp(on: deletes, after: .seconds(10))
        CodingRunsActivity.shared.retireRoutes(for: profile)
        MagicianAccess.forget(origin: row.origin)
        // Its VibeDev runs must not show again under a new pairing.
        VibeDevCache.forget(origin: row.origin)
        reload()
    }

    private func loadRegistry() async {
        guard let active = profiles.active else {
            registry = []
            registryUnavailable = false
            return
        }
        do {
            let response = try await RemoteMachinesClient.fetch(profile: active)
            guard !Task.isCancelled, MagicianAccess.connectionProfile == active else { return }
            registry = response.machines
            registryUnavailable = false
        } catch {
            guard !Task.isCancelled else { return }
            registry = []
            registryUnavailable = true
        }
    }
}

private struct MobileConnectionScanner: View {
    let completion: (Result<MobileEnrollmentLink, Error>) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            Group {
                if DataScannerViewController.isSupported && DataScannerViewController.isAvailable {
                    MobileDataScanner { raw in
                        do { completion(.success(try MobileEnrollmentLink.parse(raw))) }
                        catch { completion(.failure(error)) }
                    }
                } else {
                    ContentUnavailableView(
                        "QR scanning unavailable",
                        systemImage: "qrcode.viewfinder",
                        description: Text("Use a camera-equipped iPhone running iOS 17 or later.")
                    )
                }
            }
            .navigationTitle("Connect to \(ProductIdentity.productName)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

private struct MobileDataScanner: UIViewControllerRepresentable {
    let onCode: (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(onCode: onCode) }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let controller = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            recognizesMultipleItems: false,
            isHighFrameRateTrackingEnabled: false,
            isHighlightingEnabled: true
        )
        controller.delegate = context.coordinator
        try? controller.startScanning()
        return controller
    }

    func updateUIViewController(_ uiViewController: DataScannerViewController, context: Context) {}

    static func dismantleUIViewController(_ uiViewController: DataScannerViewController, coordinator: Coordinator) {
        uiViewController.stopScanning()
    }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        private let onCode: (String) -> Void
        private var consumed = false

        init(onCode: @escaping (String) -> Void) { self.onCode = onCode }

        func dataScanner(
            _ dataScanner: DataScannerViewController,
            didAdd addedItems: [RecognizedItem],
            allItems: [RecognizedItem]
        ) {
            guard !consumed else { return }
            for item in addedItems {
                guard case .barcode(let barcode) = item,
                      let raw = barcode.payloadStringValue else { continue }
                consumed = true
                dataScanner.stopScanning()
                onCode(raw)
                return
            }
        }
    }
}
