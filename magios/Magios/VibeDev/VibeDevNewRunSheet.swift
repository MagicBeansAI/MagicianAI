import SwiftUI
import PhotosUI
import UniformTypeIdentifiers

/// New run, Follow up, and (on a trusted project) a new VibeDev crew.
struct VibeDevNewRunSheet: View {
    enum Outcome { case run(String), crew(String) }

    let context: VibeDevContext
    let seed: VibeDevNewRunSeed
    var embedded = false
    var showPendingAsks = true
    var workLocation: String? = nil
    var availableProjects: [VibeDevProject]? = nil
    let onDone: (Outcome) -> Void

    @StateObject private var dictation = DictationController()
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var text = ""
    @State private var voiceMode = true
    @State private var voiceModeBeforeCall: Bool?
    @State private var typingDuringCall = false
    @State private var composerFocusRequest: UUID? = nil
    @State private var chatMode: ComposerMode = .ask
    @State private var permission: ComposerDoPermission = .ask
    @StateObject private var voiceCall = VoiceCallViewModel(mode: .inApp)
    @State private var liveAvailable = false
    @State private var handsFreeAvailable = false
    @State private var nightly = false
    @State private var attachmentMenu = false
    @State private var showFiles = false
    @State private var showPhotos = false
    @State private var showCamera = false
    @State private var photoItem: PhotosPickerItem?
    @State private var attachments: [StagedAttachment] = []
    @FocusState private var inputFocused: Bool
    @State private var showModels = false
    @State private var projectID: String
    @State private var mode: VibeDevTurnMode = .build
    @State private var location: VibeDevTrust?
    /// "" is the catalog default.
    @State private var profileID = ""
    @State private var budget = ""
    @State private var crew: Bool
    @State private var crewMode: VibeDevCrewMode = .split
    @State private var attempts = 3
    @State private var review = true
    @State private var projects: [VibeDevProject]
    @State private var profiles: [VibeDevCodingProfile] = []
    @State private var sending = false
    @State private var error: String?
    /// One id per submission, so a retried send returns the same run.
    @State private var clientRunID = UUID().uuidString.lowercased()

    init(context: VibeDevContext, seed: VibeDevNewRunSeed, embedded: Bool = false, showPendingAsks: Bool = true, workLocation: String? = nil, availableProjects: [VibeDevProject]? = nil, onDone: @escaping (Outcome) -> Void) {
        self.context = context
        self.seed = seed
        self.onDone = onDone
        self.embedded = embedded
        self.showPendingAsks = showPendingAsks
        self.workLocation = workLocation
        self.availableProjects = availableProjects
        let list = (availableProjects ?? context.projectList).filter { !$0.archived }
        _projects = State(initialValue: list)
        _projectID = State(initialValue: seed.projectID ?? list.first?.projectId ?? "")
        _crew = State(initialValue: seed.crew)
        _profiles = State(initialValue: context.profiles ?? [])
        _profileID = State(initialValue: seed.codingRouteID ?? "")
        _mode = State(initialValue: seed.mode)
    }

    private var isFollowUp: Bool { seed.followUp != nil }
    private var models: [ChatModelChoice] { profiles.filter { $0.engine == selectedProfile?.engine }.compactMap(\.modelChoice) }
    private var engines: [VibeDevCodingProfile] {
        var seen = Set<String>()
        return profiles.filter { $0.selectable && seen.insert($0.engine ?? $0.id).inserted }
    }
    private func engineLabel(_ profile: VibeDevCodingProfile) -> String {
        let name = ["pi": "Pi", "claude_code": "Claude Code", "codex_app_server": "Codex", "grok_acp": "Grok", "agy_cli": "Antigravity"][profile.engine ?? ""] ?? profile.label
        return HarnessCredentialsLabel.engineName(name, engine: profile.engine, credentials: profile.credentials)
    }
    private var engineSelection: Binding<String> {
        Binding(get: { profileID.isEmpty ? "" : selectedProfile?.engine ?? profileID }, set: { engine in
            profileID = engine.isEmpty ? "" : engines.first(where: { ($0.engine ?? $0.id) == engine })?.id ?? engine
        })
    }
    private var selectedProfile: VibeDevCodingProfile? {
        profiles.first(where: { $0.id == profileID }) ?? (profileID.isEmpty ? profiles.first(where: \.isDefault) : nil)
    }
    private var selection: ChatModelSelection? { selectedProfile?.modelSelection }
    private var selectedModel: ChatModelChoice? { models.first(where: { $0.id == selection?.modelId }) }
    /// Web `CodingModelPicker`: the selected thinking level rides the chip.
    private var selectedThinkingLabel: String? { ChatThinkingControl.chipLabel(model: selectedModel, selection: selection) }
    private var project: VibeDevProject? { projects.first { $0.projectId == projectID } ?? context.knownProjects[projectID] }
    private var canCrew: Bool { !isFollowUp && (location ?? project?.trust) == .trusted }
    private var makesCrew: Bool { canCrew && crew }
    private var budgetValue: Double? {
        Double(budget.replacingOccurrences(of: "$", with: "").replacingOccurrences(of: ",", with: ".")
            .trimmingCharacters(in: .whitespaces))
    }

    private var title: String {
        if isFollowUp { return "Follow up" }
        return makesCrew ? "New VibeDev crew" : "New run"
    }

    private var submitLabel: String {
        if makesCrew { return crewMode == .race ? "Set up race" : "Plan lanes" }
        return isFollowUp ? "Send follow-up" : "Start run"
    }

    var body: some View {
        Group {
            if seed.crew { crewForm }
            else if embedded { compactComposer }
            else {
                VStack(alignment: .leading, spacing: 0) {
                    GeometryReader { geometry in
                        ScrollView(showsIndicators: false) {
                            newRunWelcome(compact: geometry.size.height < 260)
                                .padding(.horizontal, 24).padding(.vertical, 20)
                                .frame(maxWidth: .infinity).frame(minHeight: geometry.size.height)
                        }.scrollDismissesKeyboard(.interactively)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .safeAreaInset(edge: .bottom, spacing: 0) { compactComposer }
                .background(theme.backgroundColor)
            }
        }
        .task { await loadPickers(); await loadVoiceCatalog() }
        .confirmationDialog("Attach to this run", isPresented: $attachmentMenu) {
            Button("Choose file") { showFiles = true }
            Button("Photo library") { showPhotos = true }
        }
        .photosPicker(isPresented: $showPhotos, selection: $photoItem, matching: .images)
        .onChange(of: photoItem) { _, item in
            Task {
                if let data = try? await item?.loadTransferable(type: Data.self) {
                    let type = item?.supportedContentTypes.first ?? .image
                    stageAttachment(data, filename: "image.\(type.preferredFilenameExtension ?? "jpg")", mime: type.preferredMIMEType ?? "image/jpeg")
                }
                photoItem = nil
            }
        }
        .fileImporter(isPresented: $showFiles, allowedContentTypes: [.item]) { result in
            do {
                let url = try result.get()
                let access = url.startAccessingSecurityScopedResource()
                defer { if access { url.stopAccessingSecurityScopedResource() } }
                let type = try? url.resourceValues(forKeys: [.contentTypeKey]).contentType
                stageAttachment(try Data(contentsOf: url), filename: url.lastPathComponent, mime: type?.preferredMIMEType ?? "application/octet-stream")
            } catch { self.error = error.localizedDescription }
        }
        .fullScreenCover(isPresented: $showCamera) {
            CameraPicker { image in
                if let data = image.jpegData(compressionQuality: 0.85) { stageAttachment(data, filename: "photo.jpg", mime: "image/jpeg") }
            }.ignoresSafeArea()
        }
        .onChange(of: projectID) { _, _ in attachments = [] }
        .onChange(of: availableProjects) { _, updated in
            guard let updated else { return }
            projects = updated.filter { !$0.archived }
            if projectID.isEmpty { projectID = projects.first?.projectId ?? "" }
        }
        .sheet(isPresented: $showModels) {
            modelPicker.presentationDetents([.medium, .large]).presentationContentInteraction(.scrolls)
                .presentationDragIndicator(.visible).presentationBackground(theme.backgroundColor)
        }
        .toolbar { ComposerKeyboardToolbar() }
        .onDisappear { if dictation.isRecording { dictation.finishLive { _ in } }; voiceCall.hangUp() }
        .onChange(of: voiceCall.isActive) { _, active in
            if active {
                if voiceModeBeforeCall == nil { voiceModeBeforeCall = voiceMode }
            } else {
                if let previous = voiceModeBeforeCall { voiceMode = previous }
                voiceModeBeforeCall = nil
                typingDuringCall = false
            }
        }
    }

    private func newRunWelcome(compact: Bool) -> some View {
        VStack(spacing: 18) {
            if !compact {
                ZStack {
                    Circle().fill(RadialGradient(colors: [theme.accentColor.opacity(0.12), .clear], center: .center, startRadius: 0, endRadius: 56))
                    RoundedRectangle(cornerRadius: 14).fill(theme.accentColor.opacity(0.09))
                        .overlay(RoundedRectangle(cornerRadius: 14).stroke(theme.accentColor.opacity(0.12), lineWidth: 1))
                        .frame(width: 76, height: 66).rotationEffect(.degrees(-12))
                    RoundedRectangle(cornerRadius: 14).fill(theme.surfaceColor)
                        .overlay(RoundedRectangle(cornerRadius: 14).stroke(theme.accentColor.opacity(0.25), lineWidth: 1))
                        .overlay(Text("</>").font(.themedMono(27, weight: .semibold)).foregroundStyle(theme.accentColor))
                        .frame(width: 76, height: 66).shadow(color: theme.accentColor.opacity(0.1), radius: 10, y: 5)
                    Image(systemName: "sparkles").font(.system(size: 17, weight: .medium)).foregroundStyle(theme.accentColor)
                        .padding(6).background(theme.cardColor, in: RoundedRectangle(cornerRadius: 7))
                        .overlay(RoundedRectangle(cornerRadius: 7).stroke(theme.accentColor.opacity(0.2), lineWidth: 1))
                        .offset(x: 35, y: -35)
                }.frame(width: 112, height: 112).accessibilityHidden(true)
            }
            VStack(spacing: 10) {
                Text(title.uppercased()).font(.themedMono(9, weight: .semibold)).tracking(1.2).foregroundStyle(theme.accentColor)
                Text(isFollowUp ? "What’s your next step?" : "What will you build?")
                    .font(.themedDisplay(compact ? 23 : 28, weight: .semibold)).foregroundStyle(theme.textColor)
                Text("Build a feature, fix a bug, or get to know your code. Describe your next step below.")
                    .font(.themed(14)).foregroundStyle(theme.secondaryTextColor).lineSpacing(4).frame(maxWidth: 300)
            }.multilineTextAlignment(.center)
            if !compact {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { welcomeStarters }
                    VStack(spacing: 8) {
                        HStack(spacing: 8) {
                            welcomeStarter("Build a feature", icon: "plus", mode: .build, prompt: "Build a feature in this project: ")
                            welcomeStarter("Fix a bug", icon: "ladybug", mode: .build, prompt: "Help me diagnose and fix this issue: ")
                        }
                        welcomeStarter("Explore the code", icon: "magnifyingglass", mode: .discuss,
                                       prompt: "Explain what this project does and how its main parts fit together.")
                    }
                }
            }
        }
    }

    private var welcomeStarters: some View {
        Group {
            welcomeStarter("Build a feature", icon: "plus", mode: .build, prompt: "Build a feature in this project: ")
            welcomeStarter("Fix a bug", icon: "ladybug", mode: .build, prompt: "Help me diagnose and fix this issue: ")
            welcomeStarter("Explore the code", icon: "magnifyingglass", mode: .discuss,
                           prompt: "Explain what this project does and how its main parts fit together.")
        }
    }

    private func welcomeStarter(_ title: String, icon: String, mode: VibeDevTurnMode, prompt: String) -> some View {
        Button {
            self.mode = mode; text = prompt; voiceMode = false; composerFocusRequest = UUID()
            if voiceCall.isActive { typingDuringCall = true }
        } label: {
            Label(title, systemImage: icon).font(.themed(12, weight: .medium)).fixedSize()
                .foregroundStyle(theme.textColor).padding(.horizontal, 11).padding(.vertical, 10)
                .background(theme.accentColor.opacity(0.045), in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.secondaryTextColor.opacity(0.16), lineWidth: 1))
        }.buttonStyle(.plain).accessibilityHint("Fills the composer without starting a run")
    }

    private var pendingAsks: some View {
        TaskDetailAskSection(taskId: seed.followUp?.parentTaskID ?? "", panelRows: [],
            vibeDevTaskIds: Set([seed.followUp?.parentTaskID].compactMap { $0 }), chatSessionId: project?.chatSessionId)
    }

    private var compactComposer: some View {
        VStack(spacing: 0) {
            if showPendingAsks {
                ScrollView { pendingAsks }.fixedSize(horizontal: false, vertical: true).frame(maxHeight: 260)
            }
            if voiceCall.isActive && !typingDuringCall {
                VoiceCallPanel(viewModel: voiceCall, handsFreeAvailable: handsFreeAvailable,
                               onTypeMessage: { voiceMode = false; typingDuringCall = true },
                               inFlow: true, topRows: AnyView(modePanel))
            } else {
            ComposerView(text: $text, mode: $chatMode, doPermission: $permission,
                isThinking: sending, profiles: [], selectedProfile: $profileID,
                chatHarnesses: [], selectedHarnessEngine: .constant("pi"), selectedHarnessModel: .constant("default"),
                onSend: submit, onStop: {}, onAttach: { attachmentMenu = true }, onCamera: { showCamera = true },
                showChatGodMode: false,
                stagedAttachments: attachments, onRemoveAttachment: { id in attachments.removeAll { $0.id == id } },
                isRecording: dictation.isRecording, dictationLevel: { dictation.level }, isTranscribing: dictation.isTranscribing,
                onTapDictationStart: { dictation.startLive() }, onTapDictationStop: finishDictation,
                partialTranscript: dictation.partialTranscript, isVoiceMode: $voiceMode,
                onHoldStart: { dictation.startLive() }, onHoldEnd: finishDictation,
                onLive: {
                    if let session = project?.chatSessionId {
                        voiceCall.startCall(uiThreadId: session, chatSessionId: session)
                    }
                },
                liveEnabled: liveAvailable && project?.chatSessionId != nil,
                handsFreeAvailable: handsFreeAvailable && project?.chatSessionId != nil,
                voiceCallActive: voiceCall.isCallLive, voiceQueue: AnyView(VStack(spacing: 0) {
                    modePanel
                    if voiceCall.isActive && typingDuringCall {
                        LiveCallComposerStrip(viewModel: voiceCall) { typingDuringCall = false }
                    }
                }.clipped()),
                topPanel: AnyView(composerTopPanel), modeControl: AnyView(composerModeToggle),
                modelControl: AnyView(Button { showModels = true } label: {
                    ComposerSelectorLabel(title: "\(selectedProfile.map(engineLabel) ?? "Engine") · \(selectedModel?.label ?? "Choose model")",
                        thinking: selectedThinkingLabel)
                }.buttonStyle(.plain).accessibilityLabel("Engine, model and thinking")
                    // The label stays the stable UI-test handle; the value speaks the selection.
                    .accessibilityValue(ChatThinkingControl.chipAccessibilityLabel(
                        "\(selectedProfile.map(engineLabel) ?? "Engine"), \(selectedModel?.label ?? "Choose model")", thinking: selectedThinkingLabel))),
                inputPlaceholder: isFollowUp ? "What next?" : "What should VibeDev do?",
                sendEnabled: !sending && !projectID.isEmpty && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && (makesCrew || selectedModel != nil), holdToSend: false, supportsChatCommands: false, focusRequest: composerFocusRequest,
                focusOnAppear: voiceCall.isActive && typingDuringCall)
            }
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: mode)
    }

    private var composerTopPanel: some View {
        VStack(alignment: .leading, spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    Menu {
                        ForEach(projects) { candidate in Button(candidate.name) { projectID = candidate.projectId; location = nil } }
                    } label: { ComposerSelectorLabel(title: project?.name ?? "Choose project", icon: "folder", maxWidth: 96, showsCaret: !isFollowUp, truncationMode: .tail) }
                        .disabled(isFollowUp)
                    if isFollowUp {
                        ComposerSelectorLabel(title: locationLabel(workLocation ?? "sandboxed"), icon: "lock", showsCaret: false)
                    } else {
                        Menu {
                            Button("Original folder") { location = .original }
                            Button("Isolated workspace") { location = .sandboxed }
                            Button("Isolated branch") { location = .trusted }
                        } label: { ComposerSelectorLabel(title: locationLabel((location ?? project?.trust ?? .sandboxed).rawValue), icon: "externaldrive") }
                    }
                }.buttonStyle(.plain)
            }
            if let error { Text(error).font(.themed(12)).foregroundStyle(theme.dangerColor) }
        }.padding(.bottom, 2)
    }

    @ViewBuilder
    private var modePanel: some View {
        if mode == .discuss {
            ComposerModeBand(title: "Discuss", message: "Read and plan, without code edits.", maxLines: 1)
                .transition(reduceMotion ? .identity : .modifier(active: ComposerPanelReveal(fraction: 0), identity: ComposerPanelReveal(fraction: 1)).combined(with: .opacity))
        } else if mode == .autopilot {
            ComposerModeBand(title: "Autopilot", message: "Runs unattended; notifies you.", options: AnyView(
                HStack(spacing: 12) {
                    TextField("Budget USD (optional)", text: $budget)
                        .keyboardType(.decimalPad).font(.themed(12)).foregroundStyle(theme.textColor).frame(maxWidth: .infinity)
                        .padding(.horizontal, 2).frame(height: 34)
                        .overlay(alignment: .bottom) { Rectangle().fill(theme.controlBorderColor).frame(height: 1) }
                    if !isFollowUp {
                        Toggle("Run nightly", isOn: $nightly).font(.themed(12)).fixedSize()
                            .tint(theme.accentFillColor).foregroundStyle(theme.textColor)
                            .accessibilityHint("Runs each night at 02:00 UTC")
                    }
                }
            ), maxLines: 1).transition(reduceMotion ? .identity : .modifier(active: ComposerPanelReveal(fraction: 0), identity: ComposerPanelReveal(fraction: 1)).combined(with: .opacity))
        }
    }

    private func stageAttachment(_ data: Data, filename: String, mime: String) {
        guard data.count <= 20 * 1024 * 1024 else { error = "Choose an attachment smaller than 20 MB."; return }
        guard let session = project?.chatSessionId,
              let request = ComposerAttachmentUpload.request(data, filename: filename, mime: mime, sessionID: session) else {
            error = "This project's attachment session is unavailable. Refresh and try again."; return
        }
        let staged = StagedAttachment(id: UUID(), remoteId: nil, filename: filename, mime: mime,
                                      thumbnail: mime.hasPrefix("image/") ? data : nil, uploading: true)
        attachments.append(staged)
        Task { @MainActor in
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode),
                      let remote = ComposerAttachmentUpload.attachmentID(data) else { throw URLError(.badServerResponse) }
                guard let index = attachments.firstIndex(where: { $0.id == staged.id }) else { return }
                attachments[index].remoteId = remote; attachments[index].uploading = false
            } catch {
                guard let index = attachments.firstIndex(where: { $0.id == staged.id }) else { return }
                attachments[index].uploading = false; attachments[index].failed = true
                self.error = "Attachment upload failed. Remove it and choose it again."
            }
        }
    }

    private func loadVoiceCatalog() async {
        let url = MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/media/providers")
        var request = URLRequest(url: url); MagicianAccess.authorize(&request)
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200,
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        let catalog = AudioSettings.realtimeVoiceCatalog(from: json)
        liveAvailable = catalog.profiles.contains { $0.isSupportedByNativeClient && $0.available }
        handsFreeAvailable = json["hands_free_voice"] as? Bool ?? false
        AudioSettings.shared.updateRealtimeVoiceProfiles(catalog)
    }

    private func finishDictation() {
        dictation.finishLive { transcript in
            guard let transcript, !transcript.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
            text = text.isEmpty ? transcript : text + " " + transcript
        }
    }

    private var composerModeToggle: some View {
        HStack(spacing: 2) {
            ForEach([VibeDevTurnMode.build, .discuss, .autopilot], id: \.self) { choice in
                ComposerModeSegment(label: choice.rawValue.uppercased(), selected: mode == choice) { mode = choice }
            }
        }
        .padding(.horizontal, 2)
        .background(theme.softBackgroundColor, in: RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
        .accessibilityLabel("Run mode")
    }

    private func locationLabel(_ location: String) -> String {
        location == "original" ? "Original folder" : location == "trusted" ? "Isolated branch" : "Isolated workspace"
    }

    private var modelPicker: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ModelCatalogPicker(title: "Engine", selected: engineSelection.wrappedValue,
                        options: engines.map { VoiceSelectionOption(id: $0.engine ?? $0.id, label: engineLabel($0)) }) {
                        engineSelection.wrappedValue = $0
                    }
                    ComposerModelCatalogControls(models: models, selection: selection, theme: theme) { id, level in
                        selectModel(id, thinking: level)
                    }
                }.padding(.horizontal, 16).padding(.vertical, 12)
            }
            .background(theme.backgroundColor).navigationTitle("VibeDev engine and model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showModels = false }.foregroundStyle(theme.accentColor) } }
        }.preferredColorScheme(theme.forcedColorScheme).sensoryFeedback(.selection, trigger: profileID)
    }

    private var crewForm: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(title)
                    .font(.themedDisplay(18, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                    .padding(.top, 18)
                VibeDevPromptField(
                    placeholder: makesCrew ? "What should the crew build?" : (isFollowUp ? "What next?" : "What should it build?"),
                    text: $text, dictation: dictation)
                if !isFollowUp && projects.count > 1 {
                    VibeDevField(label: "Project") {
                        VibeDevPills(options: projects.map {
                                         VibeDevPillOption(value: $0.projectId, label: $0.name, health: $0.health)
                                     },
                                     selection: $projectID)
                    }
                }
                if let project {
                    VibeDevProjectHealthBanner(health: project.health, projectID: project.projectId, context: context) {
                        // `gitInit` re-read the list into the context.
                        projects = context.projectList.filter { !$0.archived }
                    }
                }
                if showPendingAsks { pendingAsks }
                if let project, !isFollowUp {
                    VibeDevField(label: "Work location") {
                        VibeDevPills(options: [
                            VibeDevPillOption(value: VibeDevTrust.original, label: "Original folder"),
                            VibeDevPillOption(value: VibeDevTrust.sandboxed, label: "Isolated workspace"),
                            VibeDevPillOption(value: VibeDevTrust.trusted, label: "Isolated branch")
                        ], selection: Binding(get: { location ?? project.trust }, set: { location = $0 }))
                        Text("Original folder: exact diff approval before edits. Existing runs keep their location.")
                            .font(.themed(12)).foregroundStyle(theme.secondaryTextColor)
                    }
                }
                if !isFollowUp && !makesCrew {
                    VibeDevField(label: "Mode") {
                        VibeDevPills(options: [VibeDevPillOption(value: VibeDevTurnMode.build, label: "Build"),
                                               VibeDevPillOption(value: VibeDevTurnMode.discuss, label: "Discuss"),
                                               VibeDevPillOption(value: VibeDevTurnMode.autopilot, label: "Autopilot")],
                                     selection: $mode)
                    }
                }
                if !makesCrew {
                    VibeDevField(label: "Engine") {
                        ModelCatalogPicker(title: "Engine", selected: engineSelection.wrappedValue,
                            options: [VoiceSelectionOption(id: "", label: "Default")] + engines.map {
                                VoiceSelectionOption(id: $0.engine ?? $0.id, label: engineLabel($0))
                            }, showsLabel: false) { engineSelection.wrappedValue = $0 }
                    }
                    if !models.isEmpty {
                        VibeDevField(label: "Model") {
                            ModelCatalogPicker(title: "Model", selected: selectedModel?.id ?? "",
                                options: models.map { VoiceSelectionOption(id: $0.id, label: $0.label, detail: $0.providerDisplayName) },
                                showsLabel: false) { selectModel($0) }
                            if let model = selectedModel {
                                if let limits = model.limits {
                                    Text("\(limits.contextWindowTokens.formatted()) token context")
                                        .font(.themed(12)).foregroundStyle(theme.secondaryTextColor)
                                }
                                ModelThinkingControl(control: model.thinking, level: selection?.thinking ?? "auto", theme: theme) {
                                    selectModel(model.id, thinking: $0)
                                }
                            }
                        }
                    }
                }
                if canCrew {
                    VibeDevToggleRow(title: "VibeDev crew",
                                     detail: "Split the goal into lanes that run at once, or race engines at it.",
                                     isOn: $crew)
                }
                if makesCrew {
                    VibeDevField(label: "Crew") {
                        VibeDevPills(options: [VibeDevPillOption(value: VibeDevCrewMode.split, label: "Split the goal"),
                                               VibeDevPillOption(value: VibeDevCrewMode.race, label: "Race engines")],
                                     selection: $crewMode)
                    }
                    if crewMode == .race {
                        Stepper(value: $attempts, in: 2...8) {
                            Text("\(attempts) attempts")
                                .font(.themed(14, weight: .semibold))
                                .foregroundStyle(theme.textColor)
                        }
                    }
                    VibeDevToggleRow(title: "Review first", detail: "Review the merged branch before the pull request.",
                                     isOn: $review)
                }
                VibeDevField(label: makesCrew ? "Crew budget (optional)" : "Budget (optional)") {
                    TextField("$ no limit", text: $budget)
                        .keyboardType(.decimalPad)
                        .font(.themed(15))
                        .foregroundStyle(theme.textColor)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .background(RoundedRectangle(cornerRadius: 12).fill(theme.surfaceColor))
                        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(theme.controlBorderColor, lineWidth: 1))
                }
                if let error {
                    Text(error)
                        .font(.themed(13))
                        .foregroundStyle(theme.dangerColor)
                }
                Button(action: submit) {
                    if sending { ProgressView().tint(theme.onAccentColor) } else { Text(submitLabel) }
                }
                .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true, large: true))
                .disabled(sending || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || projectID.isEmpty
                    || (!makesCrew && (selectedProfile?.modelSelection == nil || selectedModel == nil)))
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 6)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(theme.cardColor.ignoresSafeArea())

    }

    private func selectModel(_ id: String, thinking: String? = nil) {
        guard let model = models.first(where: { $0.id == id }) else { return }
        profileID = ComposerModelCatalogControls.route(for: model, selection: selection, thinking: thinking) ?? profileID
    }

    private func loadPickers() async {
        if projects.isEmpty, let list = try? await context.api.projects() {
            context.remember(list.projects)
            projects = list.projects.filter { !$0.archived }
            if projectID.isEmpty { projectID = projects.first?.projectId ?? "" }
        }
        if context.profiles == nil {
            do {
                let loaded = try await context.api.codingProfiles()
                context.profiles = loaded
                profiles = loaded
                if !loaded.contains(where: { $0.selectable && $0.modelSelection != nil }) {
                    error = "No coding model is available. Check the engine connection and try again."
                }
            } catch {
                self.error = "Coding models could not be loaded. Reconnect and try again."
            }
        }
        if profileID.isEmpty, let engine = seed.engine,
           let previousEngine = profiles.first(where: { $0.engine == engine && $0.modelSelection?.thinking == "auto" }) {
            profileID = previousEngine.id
        }
    }

    private func submit() {
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !prompt.isEmpty, !projectID.isEmpty,
              !attachments.contains(where: { $0.uploading || $0.failed }) else { return }
        sending = true
        error = nil
        let api = context.api
        let undo = context.undo
        Task { @MainActor in
            defer { sending = false }
            do {
                if !isFollowUp, let location, location != project?.trust {
                    let updated = try await api.updateProject(id: projectID, trust: location, baseBranch: nil)
                    context.remember(project: updated)
                }
                if makesCrew {
                    let request = VibeDevCreateCrewRequest(goal: prompt, mode: crewMode,
                                                           racers: crewMode == .race ? attempts : nil,
                                                           review: review, budgetUsd: budgetValue)
                    _ = try await api.createCrew(projectID: projectID, request: request)
                    undo.show(crewMode == .race ? "Race set up; start it from the project" : "Lanes planned; start the crew from the project")
                    text = ""
                    onDone(.crew(projectID))
                } else {
                    var request = VibeDevStartRunRequest(prompt: prompt, projectId: projectID, mode: mode)
                    request.clientRunId = clientRunID
                    if let followUp = seed.followUp {
                        request.parentTaskId = followUp.parentTaskID
                        request.threaded = true
                    }
                    guard let profile = selectedProfile, let selection = profile.modelSelection,
                          selectedModel?.choices[selection.thinking] == profile.id else {
                        error = "Choose an available coding engine and model to continue."
                        return
                    }
                    request.codingChoice = VibeDevCodingSelection(profile: profile)
                    request.costBudgetUsd = budgetValue
                    request.attachments = attachments
                    request.attachmentSessionId = project?.chatSessionId
                    request.nightly = nightly
                    let response = try await api.startRun(request)
                    undo.show(response.replayed ? "Already started" : (isFollowUp ? "Follow-up sent" : "Run started"))
                    text = ""
                    attachments = []
                    clientRunID = UUID().uuidString.lowercased()
                    onDone(.run(response.taskId))
                }
            } catch {
                self.error = VibeDevActions.describe(error)
            }
        }
    }
}
