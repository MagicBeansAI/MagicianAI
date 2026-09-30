import SwiftUI
import AppIntents
import ActivityKit
import UIKit
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
    @State private var showDashboardThemePicker = false
    @State private var customSiriPhrase = SiriPhrasePresentation.initialPersonalPhrase(
        stored: MagicianAccess.store.string(forKey: SiriPhrasePresentation.customPhraseKey),
        identity: PrimaryAgentSiriIdentityStore.load()
    )
    @State private var customPhraseFollowsIdentity = MagicianAccess.store
        .string(forKey: SiriPhrasePresentation.customPhraseKey)?
        .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false
    @State private var copiedSiriPhrase = false

    var body: some View {
        NavigationView {
            Form {
                Section(header: Text("Guide").foregroundColor(themeManager.secondaryTextColor)) {
                    NavigationLink(destination: HowToUseView()) {
                        Label("How to Use", systemImage: "questionmark.circle")
                            .foregroundColor(themeManager.textColor)
                    }
                    NavigationLink(destination: RoadmapView()) {
                        Label("Features & Roadmap", systemImage: "list.star")
                            .foregroundColor(themeManager.textColor)
                    }
                    NavigationLink(destination: KeyboardSettingsView()) {
                        Label("\(ProductIdentity.productName) Keyboard", systemImage: "keyboard")
                            .foregroundColor(themeManager.textColor)
                    }
                }
                .listRowBackground(themeManager.surfaceColor)

                Section(header: Text("Talk to \(ProductIdentity.productName)").foregroundColor(themeManager.secondaryTextColor)) {
                    siriSetupCard
                }
                .listRowInsets(EdgeInsets(top: 10, leading: 12, bottom: 10, trailing: 12))
                .listRowBackground(themeManager.surfaceColor)

                Section(header: Text("Talk behavior").foregroundColor(themeManager.secondaryTextColor)) {
                    ambientArmControl
                    ambientVoiceModePicker
                    ambientLeashPicker
                    ambientPhraseReadout
                }
                .listRowBackground(themeManager.surfaceColor)

                Section(header: Text("At a glance").foregroundColor(themeManager.secondaryTextColor)) {
                    HStack {
                        Label("Widget & task progress", systemImage: "rectangle.3.group.fill")
                            .foregroundColor(themeManager.textColor)
                        Spacer()
                        Text(ActivityAuthorizationInfo().areActivitiesEnabled
                             ? "Ready"
                             : "Live Activities off")
                            .foregroundColor(themeManager.secondaryTextColor)
                    }

                    HStack {
                        Label("Attention alerts", systemImage: "bell.badge.fill")
                            .foregroundColor(themeManager.textColor)
                        Spacer()
                        Text(atAGlance.statusLabel)
                            .foregroundColor(themeManager.secondaryTextColor)
                    }

                    Button {
                        if atAGlance.authorizationStatus == .denied {
                            if let url = URL(string: UIApplication.openSettingsURLString) {
                                UIApplication.shared.open(url)
                            }
                        } else {
                            Task { await atAGlance.enable() }
                        }
                    } label: {
                        Text(!atAGlance.remoteRegistrationSupported
                             ? "Remote alerts unavailable in this build"
                             : atAGlance.authorizationStatus == .denied
                             ? "Open iOS Settings"
                             : atAGlance.authorizationStatus == .notDetermined
                                ? "Enable remote updates"
                                : "Refresh registration")
                    }
                    .disabled(!atAGlance.remoteRegistrationSupported)
                    .foregroundColor(themeManager.accentColor)

                    Text(atAGlance.statusMessage
                         ?? "Add “\(ProductIdentity.productName) at a glance” from the widget gallery; it refreshes even when alerts are off. Task progress follows the iPhone’s Live Activities setting, while this one alert setup covers Needs You. Listen and Talk activities remain private and local to this iPhone.")
                        .font(.caption)
                        .foregroundColor(themeManager.secondaryTextColor)
                }
                .listRowBackground(themeManager.surfaceColor)

                Section(header: Text("Services").foregroundColor(themeManager.secondaryTextColor)) {
                    serviceRow(MagicianAccess.backendServiceLabel, health.magicianState, versions.magician)
                    serviceRow("Magicutor", health.magicutorState, versions.magicutor)
                    serviceRow("DesktopProxy", health.tauriState, versions.tauri)
                    Button(action: health.checkHealth) {
                        Label(health.isChecking ? "Checking…" : "Refresh service status", systemImage: "arrow.clockwise")
                    }
                    .disabled(health.isChecking)
                    .foregroundColor(themeManager.accentColor)
                }
                .listRowBackground(themeManager.surfaceColor)

                StorageMaintenanceSection()

                Section(header: Text("Appearance").foregroundColor(themeManager.secondaryTextColor)) {
                    Button {
                        showDashboardThemePicker = true
                    } label: {
                        HStack(spacing: 10) {
                            Text("Dashboard Theme")
                                .foregroundColor(themeManager.textColor)
                            Spacer()
                            if let family = selectedThemeFamily {
                                Text(family.name)
                                    .foregroundColor(themeManager.secondaryTextColor)
                                    .lineLimit(1)
                                ThemePaletteSwatch(
                                    palette: themeManager.previewPalette(
                                        for: family,
                                        systemDark: ThemeManager.systemIsDark
                                    )
                                )
                            }
                            Image(systemName: "chevron.right")
                                .font(.caption.weight(.semibold))
                                .foregroundColor(themeManager.secondaryTextColor.opacity(0.7))
                        }
                    }
                    .buttonStyle(.plain)

                    Picker("Appearance", selection: Binding(
                        get: { themeManager.appearanceMode },
                        set: { themeManager.setAppearanceMode($0, systemDark: ThemeManager.systemIsDark) }
                    )) {
                        ForEach(ThemeManager.AppearanceMode.allCases) { mode in
                            Text(mode.label).tag(mode)
                        }
                    }
                    .pickerStyle(.segmented)

                    Text("System follows your device's Day/Night. The \(ProductIdentity.productName) keyboard always follows the device appearance, regardless of this.")
                        .font(.caption)
                        .foregroundColor(themeManager.secondaryTextColor)
                }
                .listRowBackground(themeManager.surfaceColor)

                Section(header: Text("Keyboard").foregroundColor(themeManager.secondaryTextColor)) {
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
                .listRowBackground(themeManager.surfaceColor)

                Section(header: Text("Self-hosted connection").foregroundColor(themeManager.secondaryTextColor)) {
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
                .listRowBackground(themeManager.surfaceColor)

                Section(header: Text("About").foregroundColor(themeManager.secondaryTextColor)) {
                    versionRow(MagicianAccess.productName, AppInfo.versionAndBuild)
                    versionRow(MagicianAccess.backendServiceLabel, versions.magician)
                    if versions.magicutor != nil { versionRow("Magicutor", versions.magicutor) }
                    if versions.supervisor != nil { versionRow("Supervisor", versions.supervisor) }
                    if versions.tauri != nil { versionRow("DesktopProxy", versions.tauri) }
                }
                .listRowBackground(themeManager.surfaceColor)
            }
            .scrollContentBackground(.hidden)
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle("Settings")
            .toolbarBackground(themeManager.backgroundColor, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbarColorScheme(themeManager.colorScheme, for: .navigationBar)
            .onAppear {
                versions.load()
                health.checkHealth()
                Task { await atAGlance.refreshStatus() }
                if let link = actions.consumeMobileConnection() {
                    selectedConnectionMode = link.connectionMode
                    pendingConnection = link
                }
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
                                let profile = try await MobileEnrollmentClient.exchange(link)
                                try MagicianAccess.install(profile)
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
            .sheet(isPresented: $showDashboardThemePicker) {
                DashboardThemePickerSheet(themeManager: themeManager)
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

    private var selectedThemeFamily: ThemeManager.ThemeFamily? {
        themeManager.availableThemeFamilies.first { $0.id == themeManager.currentThemeFamilyID }
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
                                    ? themeManager.accentColor
                                    : themeManager.surfaceColor)
                                .clipShape(Capsule())
                                .overlay(
                                    Capsule().stroke(
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
                .background(themeManager.accentColor)
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
                .foregroundColor(selected ? themeManager.elevatedColor : themeManager.secondaryTextColor)
                .frame(width: 34, height: 28)
                .background(
                    Capsule()
                        .fill(selected ? themeManager.accentColor : Color.clear)
                )
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Use \(label.lowercased()) appearance")
        .accessibilityValue(selected ? "Selected" : "")
    }
}

/// A truthful miniature of the palette: its real background, accent and text.
/// Decorative only; the adjacent family name carries the accessible identity.
private struct ThemePaletteSwatch: View {
    let palette: ThemeManager.ThemePalette

    var body: some View {
        HStack(spacing: 4) {
            Capsule()
                .fill(palette.accent)
                .frame(width: 12, height: 4)
            Capsule()
                .fill(palette.text.opacity(0.7))
                .frame(width: 18, height: 4)
        }
        .frame(width: 46, height: 28)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(palette.background)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .stroke(palette.accent.opacity(0.55), lineWidth: 1)
        )
        .accessibilityHidden(true)
    }
}

/// All families with live palette previews. A separate sheet is intentional:
/// system picker menus reduce option labels to text and cannot communicate what
/// names such as “Risograph” or “Longhand” actually look like.
private struct DashboardThemePickerSheet: View {
    @ObservedObject var themeManager: ThemeManager
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            List {
                Section {
                    ForEach(themeManager.availableThemeFamilies) { family in
                        Button {
                            themeManager.applyThemeFamily(
                                family.id,
                                systemDark: ThemeManager.systemIsDark
                            )
                            dismiss()
                        } label: {
                            HStack(spacing: 12) {
                                ThemePaletteSwatch(
                                    palette: themeManager.previewPalette(
                                        for: family,
                                        systemDark: ThemeManager.systemIsDark
                                    )
                                )
                                Text(family.name)
                                    .foregroundColor(themeManager.textColor)
                                Spacer()
                                if family.id == themeManager.currentThemeFamilyID {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundColor(themeManager.accentColor)
                                }
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .listRowBackground(themeManager.surfaceColor)
                        .accessibilityLabel(family.name)
                        .accessibilityValue(
                            family.id == themeManager.currentThemeFamilyID ? "Selected" : ""
                        )
                    }
                } footer: {
                    Text(
                        themeManager.appearanceMode == .system
                            ? "Previews follow your device's current appearance."
                            : "Previews show each theme's \(themeManager.appearanceMode.label.lowercased()) palette."
                    )
                    .foregroundColor(themeManager.secondaryTextColor)
                }
            }
            .scrollContentBackground(.hidden)
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle("Dashboard Theme")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                        .foregroundColor(themeManager.accentColor)
                }
            }
        }
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

    enum CodingKeys: String, CodingKey {
        case token, principal, workspace, capabilities
        case publicOrigin = "public_origin"
        case clientKind = "client_kind"
        case cloudflareAccess = "cloudflare_access"
    }
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
    ) async throws -> MobileConnectionProfile {
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
            cloudflareClientSecret: grant.cloudflareAccess?.clientSecret ?? ""
        )
        try await verify(profile, session: session)
        return profile
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


/// Poll only while this Settings surface is visible and the app is active.
private struct StorageMaintenanceSection: View {
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var theme = ThemeManager.shared
    @State private var rows: [StorageMaintenanceRow] = []
    @State private var unavailable = false

    var body: some View {
        Section(header: Text("Automatic maintenance").foregroundColor(theme.secondaryTextColor)) {
            if unavailable {
                Text("Maintenance status is unavailable. Retrying automatically.")
                    .foregroundColor(theme.secondaryTextColor)
            }
            ForEach(rows) { row in
                VStack(alignment: .leading, spacing: 4) {
                    Text("\(row.database == "channel_assist" ? "Comms intelligence" : "Feed") · \(row.state)")
                        .foregroundColor(theme.textColor)
                    Text(row.message).font(.caption).foregroundColor(theme.secondaryTextColor)
                    if let completed = row.last_success_at_ms {
                        Text("Last completed \(Date(timeIntervalSince1970: completed / 1000).formatted())")
                            .font(.caption).foregroundColor(theme.secondaryTextColor)
                    }
                }
            }
            Text("Storage optimization runs while the service stays online. Related requests may briefly wait.")
                .font(.caption).foregroundColor(theme.secondaryTextColor)
        }
        .listRowBackground(theme.surfaceColor)
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                let profile = MagicianAccess.connectionProfile
                var request = URLRequest(url: MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/storage/maintenance"), timeoutInterval: 8)
                MagicianAccess.authorize(&request, principal: profile?.principal ?? "anonymous", workspace: profile?.workspace ?? "default", profile: profile)
                do {
                    let (data, response) = try await URLSession.shared.data(for: request)
                    guard !Task.isCancelled else { return }
                    guard profile == MagicianAccess.connectionProfile else { rows = []; continue }
                    guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw URLError(.badServerResponse) }
                    rows = try JSONDecoder().decode([StorageMaintenanceRow].self, from: data)
                    unavailable = false
                } catch {
                    guard !Task.isCancelled else { return }
                    rows = []; unavailable = true
                }
                do { try await Task.sleep(nanoseconds: 10_000_000_000) } catch { return }
            }
        }
    }
}

private struct StorageMaintenanceRow: Decodable, Identifiable {
    let database: String
    let state: String
    let message: String
    let last_success_at_ms: Double?
    var id: String { database }
}
