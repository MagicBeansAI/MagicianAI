import SwiftUI
import PhotosUI
import UIKit
import MarkdownUI

struct ChatView: View {
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var concurrentVoice = ConcurrentVoiceCoordinator.shared
    @StateObject private var viewModel = ChatViewModel()
    @StateObject private var threadViewModel = ThreadViewModel()
    @State private var inputText: String = ""
    @StateObject private var themeManager = ThemeManager.shared
    @State private var showThreadPanel: Bool = false

    @State private var photoItem: PhotosPickerItem?
    @State private var showPhotoPicker = false
    @State private var showCamera = false
    @State private var showQueue = false
    /// True while the bottom of the transcript is visible. Drives scroll-lock: a
    /// new message only auto-scrolls when the user is already at the bottom;
    /// otherwise a "jump to latest" pill appears (matches the web behaviour).
    @State private var isAtBottom = true
    @State private var showJumpPill = false
    /// Mic dictation + the web-style auto-send countdown after a transcript lands.
    @StateObject private var dictation = DictationController()
    @ObservedObject private var audioSettings = AudioSettings.shared
    /// Voice-first composer layout: the big tap-or-hold mic is primary by default;
    /// flips to the text composer when the user opts to type.
    @State private var isVoiceMode = true
    @State private var typingDuringCall = false
    @State private var autoSendCountdown: Int? = nil
    @State private var autoSendTimer: Timer?
    /// True when the pending message came from voice dictation — makes the reply
    /// speak back (voice in → voice out), regardless of the auto-speak setting.
    @State private var turnFromVoice = false
    /// Content shared in from other apps (via the Share Extension → App Group).
    @ObservedObject private var shareRouter = ShareRouter.shared
    /// The Live voice call. Owns the transport + audio engine and drives the
    /// floating `VoiceCallPanel`. Presented as an overlay while a call is active;
    /// the composer "Live" pill that *starts* a call is wired in a later task.
    @StateObject private var voiceCall = VoiceCallViewModel(mode: .inApp)
    /// True when the backend reports a configured realtime voice provider. Gates the
    /// composer "Live" pill (mirrors the web `providers.realtime_voice` gate); fetched
    /// best-effort on appear, defaults to disabled.
    @State private var liveAvailable = false
    /// True when the backend reports a COMPLETE cascaded pipeline (VAD +
    /// streaming STT + TTS). Gates the in-call Hands-free engine option;
    /// defaults to unavailable so a failed probe never offers a dead engine.
    @State private var handsFreeAvailable = false

    private var composerTopRows: some View {
        VStack(spacing: 0) {
            if !viewModel.queuedMessages.isEmpty {
                ComposerTopRow(title: "\(viewModel.queuedMessages.count) queued", systemImage: "chevron.right") {
                    showQueue = true
                }.accessibilityIdentifier("chat-queued-messages")
            }
            ConcurrentVoiceStrip(coordinator: concurrentVoice, onReview: { id in
                threadViewModel.resumeLastSession(preferredSessionId: id) { result in
                    if case .restored(let sessionId) = result { viewModel.loadSession(sessionId) }
                }
            }, topCornerRadius: viewModel.queuedMessages.isEmpty ? ComposerView.cornerRadius : 0)
            if let parent = viewModel.currentSessionOrigin?.parentSessionId {
                ComposerTopRow(
                    title: "Concurrent · Started from original conversation",
                    systemImage: "arrow.up.right",
                    topCornerRadius: viewModel.queuedMessages.isEmpty && concurrentVoice.available.isEmpty ? ComposerView.cornerRadius : 0
                ) {
                    threadViewModel.resumeLastSession(preferredSessionId: parent) { result in
                        if case .restored(let id) = result { viewModel.loadSession(id) }
                    }
                }.accessibilityIdentifier("chat-concurrent-origin")
            }
            if voiceCall.isActive && typingDuringCall {
                LiveCallComposerStrip(viewModel: voiceCall) { typingDuringCall = false }
            }
        }
    }

    var body: some View {
        NavigationView {
            ZStack(alignment: .leading) {
                VStack(spacing: 0) {
                ScrollViewReader { proxy in
                    ZStack(alignment: .bottomTrailing) {
                        ScrollView {
                            LazyVStack(spacing: 12) {
                                if let error = viewModel.originalAnswerError {
                                    Text(error).font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                                }
                                if viewModel.messages.isEmpty {
                                    VStack(spacing: 16) {
                                        Image(systemName: "sparkles")
                                            .font(.system(size: 40))
                                            .foregroundColor(themeManager.accentColor)
                                            .padding(.top, 40)

                                        Text("How can I help you today?")
                                            .font(.themed(22, weight: .bold))
                                            .foregroundColor(themeManager.textColor)

                                        // Voice-forward prompt: the primary way in is the
                                        // tap-or-hold mic. The @ menu and "type instead"
                                        // in the composer remain the text escape hatch.
                                        HStack(spacing: 8) {
                                            Image(systemName: "mic.fill")
                                                .font(.system(size: 14))
                                                .foregroundColor(themeManager.accentColor)
                                            Text("Tap or hold to dictate")
                                                .font(.themed(15, weight: .medium))
                                                .foregroundColor(themeManager.secondaryTextColor)
                                        }
                                        .padding(.horizontal, 16)
                                        .padding(.vertical, 10)
                                        .background(themeManager.surfaceColor)
                                        .clipShape(Capsule())
                                        .overlay(
                                            Capsule()
                                                .stroke(themeManager.controlBorderColor, lineWidth: 1)
                                        )
                                        .padding(.top, 20)
                                    }
                                } else {
                                    ForEach(viewModel.messages) { message in
                                        MessageBubble(
                                            message: message,
                                            theme: themeManager,
                                            sessionId: viewModel.currentSessionIdValue,
                                            onStructuredResponseAction: { action in
                                                if action.kind == "invoke_server_action",
                                                   let actionRef = action.actionRef,
                                                   let sessionId = viewModel.currentSessionIdValue {
                                                    viewModel.invokeStructuredResponseAction(
                                                        sessionId: sessionId,
                                                        actionRef: actionRef
                                                    )
                                                }
                                            },
                                            onStructuredFollowUp: { prompt in
                                                let trimmed = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
                                                guard !trimmed.isEmpty else { return }
                                                inputText = trimmed
                                                turnFromVoice = false
                                                viewModel.sendMessage(trimmed)
                                            },
                                            onEscalationRespond: { content, value, completion in
                                                viewModel.respondToEscalation(
                                                    content: content,
                                                    submission: value,
                                                    completion: completion
                                                )
                                            },
                                            onDelete: { viewModel.deleteMessage($0) },
                                            onOpenOriginalAnswer: { link in
                                                concurrentVoice.select(nil)
                                                threadViewModel.resumeLastSession(preferredSessionId: link.origin.sessionId) { result in
                                                    if case .restored(let id) = result { viewModel.loadSession(id, target: link) }
                                                    else { viewModel.originalAnswerError = "Could not open the original conversation. It may have been removed." }
                                                }
                                            }
                                        )
                                        .id(message.id)
                                        .onAppear {
                                            if !message.isUser,
                                               case .text = message.type,
                                               let turnId = message.chatTurnId,
                                               !turnId.isEmpty {
                                                viewModel.loadActivityIfNeeded(
                                                    messageId: message.id,
                                                    chatTurnId: turnId
                                                )
                                            }
                                        }
                                    }
                                }

                                Color.clear
                                    .frame(height: 1)
                                    .id("bottom-anchor")
                                    .onAppear {
                                        isAtBottom = true
                                        showJumpPill = false
                                    }
                                    .onDisappear {
                                        isAtBottom = false
                                        showJumpPill = true
                                    }
                            }
                            .padding()
                        }
                        // Drag the transcript to dismiss the keyboard without hiding
                        // the viewport-pinned jump-to-latest control.
                        .scrollDismissesKeyboard(.interactively)
                        // Tap anywhere on the chat to dismiss the keyboard (WhatsApp
                        // style). `simultaneousGesture` so message buttons/links still
                        // fire on the same tap.
                        .simultaneousGesture(TapGesture().onEnded {
                            UIApplication.shared.sendAction(
                                #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
                        })

                        if showJumpPill {
                            Button {
                                viewModel.focusedMessageId = nil
                                withAnimation {
                                    proxy.scrollTo("bottom-anchor", anchor: .bottom)
                                }
                                isAtBottom = true
                                showJumpPill = false
                            } label: {
                                Image(systemName: "arrow.down")
                                    .font(.system(size: 15, weight: .bold))
                                    .frame(width: 42, height: 42)
                                    .foregroundColor(themeManager.onAccentColor)
                                    .background(themeManager.accentColor)
                                    .clipShape(Circle())
                                    .shadow(color: .black.opacity(0.18), radius: 6, y: 2)
                            }
                            .buttonStyle(.plain)
                            .padding(.trailing, 14)
                            .padding(.bottom, 12)
                            .accessibilityLabel("Scroll to latest message")
                            .transition(.move(edge: .bottom).combined(with: .opacity))
                        }
                    }
                    .onChange(of: viewModel.focusedMessageId) { _, id in
                        guard let id else { return }
                        isAtBottom = false
                        DispatchQueue.main.async { proxy.scrollTo(id, anchor: .center) }
                        if let receipt = concurrentVoice.requests.first(where: {
                            $0.branchSessionId == viewModel.currentSessionIdValue && $0.resultMessageId == id && $0.readAt == nil
                        }) {
                            Task { try? await concurrentVoice.markRead(receipt.id) }
                        }
                    }
                    .onChange(of: viewModel.messages.count) { _, _ in
                        handleTranscriptGrowth(proxy)
                    }
                    // Also scroll when a task status updates its internal steps.
                    .onChange(of: viewModel.messages) { _, _ in
                        handleTranscriptGrowth(proxy)
                    }
                }
                
                // Input Bar Container
                VStack(spacing: 0) {
                    if let error = viewModel.queueErrorMessage {
                        Text(error).font(.themed(12)).foregroundColor(themeManager.dangerColor).padding(.horizontal, 12)
                    }
                    // Auto-send countdown after a dictation transcript lands.
                    if let cd = autoSendCountdown {
                        HStack(spacing: 8) {
                            Image(systemName: "waveform").font(.caption)
                            Text("Sending in \(cd)s…").font(.themed(13, weight: .medium))
                            Spacer()
                            Button("Cancel") { cancelAutoSend() }
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundColor(themeManager.accentColor)
                        }
                        .padding(.horizontal, 14).padding(.vertical, 8)
                        .foregroundColor(themeManager.textColor)
                        .background(themeManager.accentColor.opacity(0.12))
                        .clipShape(Capsule())
                        .padding(.horizontal, 12).padding(.bottom, 6)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                    }

                    ComposerView(
                        text: $inputText,
                        mode: $viewModel.composerMode,
                        doPermission: $viewModel.composerDoPermission,
                        isThinking: viewModel.isThinking || viewModel.serverRunning,
                        profiles: viewModel.profiles,
                        selectedProfile: $viewModel.selectedProfile,
                        chatHarnesses: viewModel.chatHarnesses,
                        selectedHarnessEngine: $viewModel.selectedHarnessEngine,
                        selectedHarnessModel: $viewModel.selectedHarnessModel,
                        onSend: {
                            cancelAutoSend(settle: false)
                            viewModel.sendMessage(inputText, viaVoice: turnFromVoice)
                            inputText = ""
                            turnFromVoice = false
                        },
                        onStop: {
                            viewModel.cancelRun()
                        },
                        onAttach: { showPhotoPicker = true },
                        onCamera: { showCamera = true },
                        mentionItems: viewModel.mentionItems,
                        stagedAttachments: viewModel.stagedAttachments,
                        onRemoveAttachment: { viewModel.removeStagedAttachment($0) },
                        isRecording: dictation.isRecording,
                        isTranscribing: dictation.isTranscribing,
                        onTapDictationStart: { handleTapDictationStart() },
                        onTapDictationStop: { handleTapDictationStop() },
                        partialTranscript: dictation.partialTranscript,
                        isVoiceMode: $isVoiceMode,
                        // Hold-to-talk: barge-in → live capture → finalize →
                        // merge into the composer → 3s cancelable auto-send.
                        onHoldStart: { handleHoldStart() },
                        onHoldEnd: { handleHoldEnd() },
                        // Live pill → start the Phase-2 realtime voice call, attached
                        // to the current chat thread so the call and chat share history.
                        onLive: { voiceCall.startCall(uiThreadId: viewModel.currentSessionIdValue ?? "") },
                        liveEnabled: liveAvailable,
                        handsFreeAvailable: handsFreeAvailable,
                        voiceCallActive: voiceCall.isCallLive,
                        voiceQueue: AnyView(composerTopRows),
                        onBackground: {
                            cancelAutoSend()
                            viewModel.sendMessage(inputText, background: true)
                            inputText = ""; turnFromVoice = false
                        },
                        onStopAndSend: {
                            viewModel.sendMessage(inputText, stopAndSend: true)
                            inputText = ""; turnFromVoice = false
                        },
                        queueMutationInFlight: viewModel.queueMutationInFlight
                    )
                }
                // Removed padding to allow full width for composer
            }
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationBarTitleDisplayMode(.inline)
            // SwiftUI owns this navigation bar, so bind its chrome directly to
            // the live palette instead of relying only on UIKit appearance
            // proxies, which can retain the launch-time color.
            .toolbarBackground(themeManager.backgroundColor, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbarColorScheme(themeManager.colorScheme, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    HamburgerButton()
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button {
                        withAnimation { showThreadPanel.toggle() }
                    } label: {
                        Image(systemName: "bubble.left.and.bubble.right")
                            .foregroundColor(themeManager.accentColor)
                    }
                    .accessibilityLabel("Chat history")
                }
                ToolbarItem(placement: .principal) {
                    ChatNavigationTitle(
                        chatViewModel: viewModel,
                        threadViewModel: threadViewModel,
                        onOpenPanel: { withAnimation { showThreadPanel = true } }
                    )
                    .id("chat-navigation-title-\(themeManager.themeRevision)")
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    ChatSessionActionsMenu(
                        chatViewModel: viewModel,
                        threadViewModel: threadViewModel
                    )
                    .id("chat-session-actions-\(themeManager.themeRevision)")
                }
            }
            
            if showThreadPanel {
                Color.black.opacity(0.4)
                    .ignoresSafeArea()
                    .onTapGesture {
                        withAnimation { showThreadPanel = false }
                    }

                // Slides from the RIGHT so it doesn't fight the left hamburger.
                HStack(spacing: 0) {
                    Spacer(minLength: 0)
                    ThreadPanelView(viewModel: threadViewModel, isPresented: $showThreadPanel, chatViewModel: viewModel)
                        .frame(width: 300)
                        .transition(.move(edge: .trailing))
                }
            }
            } // End ZStack
            .photosPicker(isPresented: $showPhotoPicker, selection: $photoItem, matching: .images)
            .onChange(of: photoItem) { _, newItem in
                Task { await loadPhoto(newItem) }
            }
            // Shared-in text/URLs prefill the composer; shared files/images stage
            // as attachments (via the Share Extension → App Group → ShareRouter).
            .onReceive(shareRouter.$pendingText.compactMap { $0 }) { text in
                inputText = inputText.trimmingCharacters(in: .whitespaces).isEmpty ? text : inputText + " " + text
                DispatchQueue.main.async { shareRouter.pendingText = nil }
            }
            .onReceive(shareRouter.$pendingBlobs) { blobs in
                guard !blobs.isEmpty else { return }
                for b in blobs { viewModel.uploadAttachment(b.data, filename: b.filename, mime: b.mime) }
                DispatchQueue.main.async { shareRouter.pendingBlobs = [] }
            }
            // A "New Chat" shortcut/Siri intent starts a fresh session.
            .onReceive(AppActions.shared.$newChatRequestID.dropFirst()) { _ in
                threadViewModel.newSession { viewModel.startNewSession($0) }
            }
            // System widget / Control Center / deep-link voice handoff. The
            // request is latched in AppActions, so this works both while warm
            // and when ChatView mounts after a cold app launch.
            .onReceive(AppActions.shared.$voiceRequestID.dropFirst()) { _ in
                consumeExternalVoiceLaunch()
            }
            .onAppear { consumeExternalVoiceLaunch() }
            .onReceive(AppActions.shared.$threadRequestID.dropFirst()) { _ in
                if let id = AppActions.shared.threadTargetID {
                    threadViewModel.selectThread(id)
                    AppActions.shared.consumeThreadTarget()
                }
            }
            // Mic/speech permission off → offer to jump to Settings.
            .alert(item: $dictation.permissionAlert) { alert in
                Alert(
                    title: Text(alert.title),
                    message: Text(alert.message),
                    primaryButton: .default(Text("Open Settings")) { DictationController.openSettings() },
                    secondaryButton: .cancel()
                )
            }
            .alert("Could not stop run", isPresented: Binding(
                get: { viewModel.cancelRunErrorMessage != nil },
                set: { if !$0 { viewModel.cancelRunErrorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { viewModel.cancelRunErrorMessage = nil }
            } message: {
                Text(viewModel.cancelRunErrorMessage ?? "The run could not be stopped.")
            }
            .alert("Could not send response", isPresented: Binding(
                get: { viewModel.escalationResponseErrorMessage != nil },
                set: { if !$0 { viewModel.escalationResponseErrorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { viewModel.escalationResponseErrorMessage = nil }
            } message: {
                Text(viewModel.escalationResponseErrorMessage ?? "The response could not be sent.")
            }
            .alert("Could not run action", isPresented: Binding(
                get: { viewModel.structuredActionErrorMessage != nil },
                set: { if !$0 { viewModel.structuredActionErrorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { viewModel.structuredActionErrorMessage = nil }
            } message: {
                Text(viewModel.structuredActionErrorMessage ?? "The structured action could not be executed.")
            }
            .fullScreenCover(isPresented: $showCamera) {
                CameraPicker { image in
                    if let data = image.jpegData(compressionQuality: 0.85) {
                        viewModel.uploadAttachment(data, filename: "photo.jpg", mime: "image/jpeg")
                    }
                }
                .ignoresSafeArea()
            }
            .sheet(isPresented: $showQueue) { QueueSheet(viewModel: viewModel) }
        }
        // Floating Live-call card, pinned near the bottom over the chat while a
        // call is active (thread stays visible behind it).
        .overlay(alignment: .bottom) {
            if voiceCall.isActive && !typingDuringCall {
                VoiceCallPanel(viewModel: voiceCall, handsFreeAvailable: handsFreeAvailable, onTypeMessage: {
                    isVoiceMode = false
                    typingDuringCall = true
                })
            }
        }
        .animation(.easeInOut(duration: 0.2), value: voiceCall.isActive)
        .onChange(of: voiceCall.isActive) { _, active in if !active { typingDuringCall = false } }
        // Gate the composer "Live" pill on a configured realtime voice provider.
        .task { await fetchLiveAvailability() }
        .onAppear {
            concurrentVoice.screenActive = scenePhase == .active
            concurrentVoice.foregroundBusy = { viewModel.isThinking || viewModel.serverRunning || !inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || dictation.isRecording || dictation.isTranscribing }
            concurrentVoice.activate()
        }
        .onDisappear { concurrentVoice.screenActive = false; concurrentVoice.deactivate() }
        .onChange(of: scenePhase) { _, phase in
            concurrentVoice.screenActive = phase == .active
            if phase == .active { concurrentVoice.activate() } else { concurrentVoice.deactivate() }
        }
        .task {
            while !Task.isCancelled {
                viewModel.fetchQueue()
                try? await Task.sleep(for: .seconds(2))
            }
        }
        .onChange(of: viewModel.currentSessionIdValue) { _, _ in concurrentVoice.select(nil) }
        .onChange(of: concurrentVoice.requests.last?.id) { _, _ in
            // Admission can name a previously untitled voice-only parent.
            guard let id = viewModel.currentSessionIdValue,
                  concurrentVoice.requests.last?.parentSessionId == id else { return }
            threadViewModel.resumeLastSession(preferredSessionId: id) { _ in }
        }
        .onReceive(viewModel.$failedConcurrentInput) { text in
            if let text, inputText.isEmpty { inputText = text; viewModel.failedConcurrentInput = nil }
        }
        // Reopen the exact surviving session this device last displayed. If an
        // older build never recorded one, fall back to the newest active
        // Personal session. Create only after the server authoritatively says
        // there is no session to restore; offline startup must not duplicate it.
        .task {
            threadViewModel.fetchData()
            guard viewModel.currentSessionIdValue == nil else { return }
            let remembered = viewModel.rememberedSessionId
            threadViewModel.resumeLastSession(preferredSessionId: remembered) { result in
                guard viewModel.currentSessionIdValue == nil else { return }
                switch result {
                case .restored(let sessionId):
                    viewModel.loadSession(sessionId)
                case .missing:
                    viewModel.forgetRememberedSession(remembered)
                    threadViewModel.newSessionIfNothingSelected { sessionId in
                        guard let sessionId, viewModel.currentSessionIdValue == nil else { return }
                        viewModel.startNewSession(sessionId)
                    }
                case .unavailable:
                    break
                }
            }
        }
    }

    /// Best-effort probe of the media-providers endpoint. Native Live Call needs
    /// an available backend-proxied profile; the top-level `realtime_voice`
    /// descriptor may point at the browser-only direct WebRTC default.
    private func fetchLiveAvailability() async {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/providers") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200 else { return }
            guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
            // The cascaded engine is only offered when EVERY stage is configured;
            // the backend computes that (see media_api.rs `hands_free_voice`).
            let cascaded = json["hands_free_voice"] as? Bool ?? false
            let catalog = AudioSettings.realtimeVoiceCatalog(from: json)
            await MainActor.run {
                liveAvailable = catalog.profiles.contains {
                    $0.isSupportedByNativeClient && $0.available
                }
                handsFreeAvailable = cascaded
                AudioSettings.shared.updateRealtimeVoiceProfiles(catalog)
            }
        } catch {
            // Best-effort — leave the pill disabled.
        }
    }

    /// A new/updated message arrived: if the user is at the bottom, follow it;
    /// otherwise surface the jump pill instead of yanking their scroll position.
    private func handleTranscriptGrowth(_ proxy: ScrollViewProxy) {
        if isAtBottom && viewModel.focusedMessageId == nil {
            withAnimation { proxy.scrollTo("bottom-anchor", anchor: .bottom) }
        } else {
            withAnimation { showJumpPill = true }
        }
    }

    /// A quick first tap starts an open dictation capture. Start/stop are explicit
    /// so the second tap cannot race the controller's published recording state.
    private func handleTapDictationStart() {
        cancelAutoSend()
        guard !dictation.isRecording, !dictation.isTranscribing else { return }
        concurrentVoice.captureStarted()
        if audioSettings.archiveChatDictation {
            dictation.startVoiceNote()
        } else {
            dictation.startLive()
        }
    }

    /// The second tap always finalizes the capture that was active when the
    /// finger went down, then enters the same transcript/countdown path as hold.
    private func handleTapDictationStop() {
        guard dictation.isRecording else { return }
        concurrentVoice.captureStopped()
        dictation.finishLive(completion: acceptDictationTranscript)
    }

    /// Consume exactly one externally requested voice launch. Mode-neutral
    /// system surfaces follow the user's saved Dictate / Hands-free / Live
    /// choice; explicit deep links override it for that launch only.
    private func consumeExternalVoiceLaunch() {
        guard let requestedMode = AppActions.shared.consumeVoiceLaunchMode() else { return }
        let mode = requestedMode.resolved(using: AudioSettings.shared.systemVoiceLaunchMode)
        switch mode {
        case .handsFree:
            guard !voiceCall.isActive else { return }
            voiceCall.startCall(
                uiThreadId: viewModel.currentSessionIdValue ?? "",
                engineOverride: .handsFree
            )
        case .realtime:
            guard !voiceCall.isActive else { return }
            voiceCall.startCall(
                uiThreadId: viewModel.currentSessionIdValue ?? "",
                engineOverride: .realtime
            )
        case .dictation:
            guard !dictation.isRecording, !dictation.isTranscribing else { return }
            // A warm app may still be in text mode. Make the externally launched
            // recording and its tap-to-stop state immediately visible.
            withAnimation { isVoiceMode = true }
            handleTapDictationStart()
        }
    }

    /// Hold-to-talk begins: interrupt any speaking reply (barge-in), then use
    /// ordinary ephemeral dictation unless the user explicitly opted into
    /// retaining Chat recordings as Audio Notes.
    private func handleHoldStart() {
        cancelAutoSend()
        concurrentVoice.captureStarted()
        if VoiceCapture.shouldBargeIn(isSpeaking: SpeechSynthesizer.shared.isSpeaking) {
            SpeechSynthesizer.shared.stop()
        }
        if audioSettings.archiveChatDictation {
            dictation.startVoiceNote()
        } else {
            dictation.startLive()
        }
    }

    /// Hold-to-talk ends: finalize the transcript, merge into the composer, arm the
    /// existing 3s cancelable auto-send countdown (which sends viaVoice).
    private func handleHoldEnd() {
        concurrentVoice.captureStopped()
        dictation.finishLive(completion: acceptDictationTranscript)
    }

    /// Both tap-stop and hold-release land here. Keeping one completion path is
    /// what guarantees the visible cancelable countdown and `viaVoice` send.
    private func acceptDictationTranscript(_ transcript: String?) {
        guard let transcript, !transcript.isEmpty else { concurrentVoice.inputSettled(); return }
        inputText = VoiceCapture.merge(existing: inputText, transcript: transcript)
        turnFromVoice = true
        armAutoSend()
    }

    private func armAutoSend() {
        autoSendTimer?.invalidate()
        withAnimation { autoSendCountdown = 3 }
        autoSendTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
            guard let c = autoSendCountdown else { return }
            if c <= 1 {
                cancelAutoSend(settle: false)
                if !inputText.trimmingCharacters(in: .whitespaces).isEmpty {
                    viewModel.sendMessage(inputText, viaVoice: turnFromVoice)
                    inputText = ""
                    turnFromVoice = false
                }
            } else {
                autoSendCountdown = c - 1
            }
        }
    }

    private func cancelAutoSend(settle: Bool = true) {
        let hadPendingDictation = autoSendTimer != nil || autoSendCountdown != nil
        autoSendTimer?.invalidate()
        autoSendTimer = nil
        withAnimation { autoSendCountdown = nil }
        if settle && hadPendingDictation { concurrentVoice.inputSettled() }
    }

    private func loadPhoto(_ item: PhotosPickerItem?) async {
        guard let item = item else { return }
        if let data = try? await item.loadTransferable(type: Data.self) {
            viewModel.uploadAttachment(data, filename: "image.jpg", mime: "image/jpeg")
        }
        await MainActor.run { photoItem = nil }
    }
}

/// Camera capture (falls back to the photo library on devices without a camera,
/// e.g. the simulator).
struct CameraPicker: UIViewControllerRepresentable {
    var onImage: (UIImage) -> Void
    @Environment(\.dismiss) private var dismiss

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = UIImagePickerController.isSourceTypeAvailable(.camera) ? .camera : .photoLibrary
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ uiViewController: UIImagePickerController, context: Context) {}
    func makeCoordinator() -> Coordinator { Coordinator(self) }

    class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let parent: CameraPicker
        init(_ parent: CameraPicker) { self.parent = parent }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            if let image = info[.originalImage] as? UIImage { parent.onImage(image) }
            parent.dismiss()
        }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            parent.dismiss()
        }
    }
}

struct QueueSheet: View {
    @ObservedObject var viewModel: ChatViewModel
    @StateObject private var themeManager = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if viewModel.queuedMessages.isEmpty {
                    VStack(spacing: 10) {
                        Image(systemName: "tray").font(.system(size: 34)).foregroundColor(themeManager.secondaryTextColor)
                        Text("No queued messages").foregroundColor(themeManager.secondaryTextColor)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List {
                        ForEach(Array(viewModel.queuedMessages.enumerated()), id: \.element.id) { index, msg in
                            VStack(alignment: .leading, spacing: 8) {
                                HStack(spacing: 8) {
                                    Text("#\(index + 1)")
                                    if let queuedAt = msg.queuedAt {
                                        Text(Self.relativeTime(queuedAt))
                                    }
                                }
                                .font(.themed(11, weight: .semibold))
                                .foregroundColor(themeManager.secondaryTextColor)

                                Text(msg.text?.isEmpty == false ? msg.text! : "(attachments only)")
                                    .foregroundColor(themeManager.textColor)
                                    .lineLimit(4)
                                    .textSelection(.enabled)

                                HStack(spacing: 8) {
                                    Button("Stop & send") { viewModel.actOnQueue(msg.id, action: "stop_and_send") }
                                        .disabled(viewModel.queueMutationInFlight)
                                    if !(msg.text ?? "").isEmpty && (msg.attachmentIds ?? []).isEmpty {
                                        Button("Run in parallel") { viewModel.actOnQueue(msg.id, action: "parallel") }
                                            .disabled(viewModel.queueMutationInFlight)
                                    }
                                }.font(.themed(12, weight: .medium)).buttonStyle(.bordered).controlSize(.small)
                                HStack(spacing: 8) {
                                    Button {
                                        UIPasteboard.general.string = msg.text ?? ""
                                        UIImpactFeedbackGenerator(style: .light).impactOccurred()
                                    } label: {
                                        Label("Copy", systemImage: "doc.on.doc")
                                    }
                                    .buttonStyle(.bordered)
                                    .controlSize(.small)
                                    Button(role: .destructive) { viewModel.deleteQueued(msg.id) } label: {
                                        Label("Remove", systemImage: "trash")
                                    }
                                    .tint(themeManager.dangerColor)
                                    .buttonStyle(.bordered)
                                    .controlSize(.small)
                                }.font(.themed(12, weight: .medium))
                            }
                            .padding(.vertical, 8)
                            .listRowBackground(themeManager.surfaceColor)
                            .listRowSeparatorTint(themeManager.secondaryTextColor.opacity(0.2))
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                Button(role: .destructive) { viewModel.deleteQueued(msg.id) } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                            .contextMenu {
                                Button {
                                    UIPasteboard.general.string = msg.text ?? ""
                                } label: { Label("Copy", systemImage: "doc.on.doc") }
                                Button(role: .destructive) { viewModel.deleteQueued(msg.id) } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                        }
                    }
                    .listStyle(.plain)
                    .scrollContentBackground(.hidden)
                }
            }
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .safeAreaInset(edge: .top, spacing: 0) {
                if let notice = viewModel.queueNoticeMessage {
                    Label(notice, systemImage: "checkmark.circle.fill")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(themeManager.successColor)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 7)
                        .background(themeManager.successColor.opacity(0.12))
                }
            }
            .navigationTitle("Queued messages")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(themeManager.backgroundColor, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbarColorScheme(themeManager.colorScheme, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Menu {
                        if viewModel.isThinking || viewModel.serverRunning {
                            Button("Stop turn", role: .destructive) { viewModel.cancelRun() }
                        }
                        if !viewModel.queuedMessages.isEmpty {
                            Button("Clear all", role: .destructive) { viewModel.clearQueued() }
                                .disabled(viewModel.queueMutationInFlight)
                        }
                    } label: { Image(systemName: "ellipsis") }
                    .accessibilityLabel("Queue options")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .onAppear { viewModel.fetchQueue() }
            .alert("Queue action failed", isPresented: Binding(
                get: { viewModel.queueErrorMessage != nil },
                set: { if !$0 { viewModel.queueErrorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { viewModel.queueErrorMessage = nil }
            } message: {
                Text(viewModel.queueErrorMessage ?? "The queued messages could not be updated.")
            }
        }
        .tint(themeManager.accentColor)
        .preferredColorScheme(themeManager.colorScheme)
        .presentationBackground(themeManager.backgroundColor)
    }

    private static func relativeTime(_ timestamp: Double) -> String {
        let seconds = timestamp > 1_000_000_000_000 ? timestamp / 1000 : timestamp
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(
            for: Date(timeIntervalSince1970: seconds),
            relativeTo: Date()
        )
    }
}

struct MessageBubble: View {
    let message: ChatMessage
    @ObservedObject var theme: ThemeManager
    let sessionId: String?
    var onStructuredResponseAction: (ChatStructuredAction) -> Void = { _ in }
    var onStructuredFollowUp: (String) -> Void = { _ in }
    var onEscalationRespond: (
        ChatMessageContentData,
        ChatHitlSubmission,
        @escaping (Bool) -> Void
    ) -> Void = { _, _, completion in
        completion(false)
    }
    var onDelete: (String) -> Void = { _ in }
    var onOpenOriginalAnswer: (OriginalAnswerLink) -> Void = { _ in }
    @State private var showDeepPanel = false
    @State private var openedArtifact: ArtifactRef?
    @State private var openedStructuredTask: StructuredTaskTarget?
    @State private var sharePayload: SharePayload?
    @State private var structuredActionErrorMessage: String?
    @State private var speechPlaybackResult: SpeechPlaybackResult?
    @ObservedObject private var speech = SpeechSynthesizer.shared

    private struct StructuredTaskTarget: Identifiable {
        let id: String
        let title: String
    }
    
    var body: some View {
        HStack {
            if message.isUser { Spacer() }
            
            VStack(alignment: message.isUser ? .trailing : .leading, spacing: 6) {
                if let link = message.originalAnswer {
                    Button { onOpenOriginalAnswer(link) } label: {
                        Label("View original answer", systemImage: "link")
                            .font(.themed(11, weight: .medium))
                            .foregroundColor(theme.accentColor)
                            .padding(.vertical, 6)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("original-answer-\(message.id)")
                }
                switch message.type {
                case .text:
                    // Full GFM markdown (tables, code fences, lists) via MarkdownUI,
                    // plus any inline Access-gated image content-blocks.
                    VStack(alignment: message.isUser ? .trailing : .leading, spacing: 8) {
                        if message.voiceOrigin {
                            HStack(spacing: 4) {
                                Image(systemName: "waveform").font(.system(size: 9))
                                Text("Voice").font(.themed(11, weight: .medium))
                            }
                            .foregroundColor(message.isUser ? theme.accentColor : theme.secondaryTextColor)
                            .padding(.horizontal, 4)
                        }
                        if let planCtx = message.planReplyContext {
                            HStack(spacing: 4) {
                                Image(systemName: "arrowshape.turn.up.left.fill").font(.system(size: 9))
                                Text("Replying to Planner").font(.themed(11, weight: .medium))
                                if planCtx != "Planner" {
                                    Text("· \(planCtx)").font(.themed(11)).lineLimit(1)
                                }
                            }
                            .foregroundColor(theme.accentColor)
                            .padding(.horizontal, 4)
                        }
                        if let response = message.structuredResponse {
                            structuredResponseCard(response)
                        } else if !message.text.isEmpty {
                            // Render the message as-is — the `<speech>` tags stay
                            // present in the content (matching web, which does not
                            // strip them for display). Only TTS extracts segments.
                            plainTextMessageBubble(message.text)
                        }
                        ForEach(message.imageURLs, id: \.self) { url in
                            AuthAsyncImage(urlString: url)
                        }
                        ForEach(Array(message.richBlocks.enumerated()), id: \.offset) { _, block in
                            artifactCard(block)
                        }
                        if !message.isUser && (!message.activityRows.isEmpty || message.activityIsLive) {
                            ActivityTimelineView(
                                rows: message.activityRows,
                                isLive: message.activityIsLive,
                                sessionId: sessionId,
                                theme: theme
                            )
                        }
                        if !message.isUser && !message.text.isEmpty {
                            speakButton
                        }
                    }

                case .taskStatus(let task):
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            taskStatusIcon(task)
                            Text("\(task.statusVerb): \(task.title)")
                                .font(.themed(15, weight: .semibold))
                                .foregroundColor(theme.textColor)
                                .fixedSize(horizontal: false, vertical: true)
                        }

                        if let summary = task.summary, !summary.isEmpty {
                            Markdown(summary)
                                .markdownTextStyle {
                                    FontFamily(.custom(theme.fontName))
                                    ForegroundColor(theme.textColor)
                                }
                                .font(.themed(13))
                        } else if let lastStep = task.steps.last {
                            Text(lastStep)
                                .font(.themed(12))
                                .foregroundColor(theme.secondaryTextColor)
                                .fixedSize(horizontal: false, vertical: true)
                        }

                        if !task.outputFiles.isEmpty {
                            VStack(alignment: .leading, spacing: 6) {
                                ForEach(Array(task.outputFiles.enumerated()), id: \.offset) { _, block in
                                    artifactCard(block)
                                }
                            }
                        }

                        if let executionId = task.activeExecutionIdForControls {
                            Divider()
                            ExecutionControlsView(
                                executionId: executionId,
                                refreshToken: task.executionControlRefreshToken
                            )
                                .id(executionId)
                        }

                        Button(action: {
                            showDeepPanel = true
                        }) {
                            HStack {
                                Text("Inspect Run")
                                Spacer()
                                Image(systemName: "chevron.right")
                            }
                            .font(.footnote)
                            .foregroundColor(theme.accentColor)
                        }
                        .sheet(isPresented: $showDeepPanel) {
                            DeepWorkPanel(task: task)
                        }
                    }
                    .padding(12)
                    .background(theme.cardColor)
                    .cornerRadius(8)
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .stroke(taskStatusColor(task).opacity(0.35), lineWidth: 1)
                    )
                    
                case .escalation(let content):
                    // Form, text, and the other typed kinds have no options.
                    // Requiring a non-nil array hid those cards; EscalationCard
                    // already sends option-less form pauses to Attention.
                    if let question = content.question {
                        EscalationCard(
                            messageId: message.id,
                            executionId: content.executionId,
                            title: content.text ?? "Action required",
                            question: question,
                            hint: content.hint,
                            previousAnswer: content.previousAnswer,
                            inputType: content.hitlInputType,
                            inputTypeIsAuthoritative: content.inputType != nil,
                            inputSchema: content.inputSchema,
                            options: content.options ?? [],
                            resolved: content.resolved ?? false,
                            onRespond: { value, completion in
                                onEscalationRespond(content, value, completion)
                            },
                            onOpenCanonical: content.hitlCorrelationId.map { correlationId in
                                { AppActions.shared.requestAttention(itemID: correlationId) }
                            },
                            theme: theme
                        )
                    }
                case .system(let text):
                    if let response = message.structuredResponse {
                        structuredResponseCard(response)
                    } else {
                        Text(text)
                            .font(.themed(12))
                            .padding(.horizontal, 16)
                            .padding(.vertical, 8)
                            .background(theme.controlColor)
                            .cornerRadius(16)
                            .foregroundColor(theme.secondaryTextColor)
                    }
                case .attachment(let filename, let size):
                    if let response = message.structuredResponse {
                        structuredResponseCard(response)
                    } else {
                        AttachmentBubble(
                            filename: filename,
                            size: size,
                            isUser: message.isUser,
                            theme: theme
                        )
                    }
                }
            }
            .padding(message.originalAnswer != nil ? 8 : 0)
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(
                message.originalAnswer != nil ? theme.accentColor.opacity(0.35) : .clear,
                lineWidth: 1))
            .contextMenu {
                Button(role: .destructive, action: { onDelete(message.id) }) {
                    Label("Delete", systemImage: "trash")
                }
            }

            if !message.isUser { Spacer() }
        }
        .sheet(item: $openedArtifact) { ArtifactViewer(artifact: $0) }
        .sheet(item: $openedStructuredTask) { target in
            DeepWorkPanel(
                task: TaskStatusModel(taskId: target.id, title: target.title, status: "running", steps: [])
            )
        }
        .sheet(item: $sharePayload) { ShareSheet(items: $0.items) }
        .alert("Could not perform action", isPresented: Binding(
            get: { structuredActionErrorMessage != nil },
            set: { if !$0 { structuredActionErrorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { structuredActionErrorMessage = nil }
        } message: {
            Text(structuredActionErrorMessage ?? "The action could not be executed.")
        }
    }

    private func plainTextMessageBubble(_ text: String) -> some View {
        Markdown(text)
            .markdownTextStyle {
                FontFamily(.custom(theme.fontName))
                FontSize(14)
                ForegroundColor(message.isUser ? theme.onAccentColor : theme.textColor)
            }
            .padding(14)
            .background(message.isUser ? theme.accentColor : theme.surfaceColor)
            .cornerRadius(18)
    }

    @ViewBuilder
    private func structuredResponseCard(_ response: ChatMessagePresentationData) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let title = trimmed(response.title) {
                Text(title)
                    .font(.themed(15, weight: .semibold))
            }
            if let summary = trimmed(response.summary) {
                Text(summary)
                    .font(.themed(13))
                    .foregroundColor(theme.secondaryTextColor)
            }
            if response.blocks.isEmpty {
                let fallback = trimmed(response.plainText) ?? trimmed(message.text)
                if let fallback {
                    structuredMarkdownBlock(fallback)
                }
            } else {
                ForEach(Array(response.blocks.enumerated()), id: \.offset) { _, block in
                    structuredResponseBlock(block)
                }
            }
            if let actions = response.actions, !actions.isEmpty {
                Divider().overlay(theme.secondaryTextColor.opacity(0.35))
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 130), spacing: 8), GridItem(.adaptive(minimum: 130))], alignment: .leading, spacing: 8) {
                    ForEach(Array(actions.enumerated()), id: \.offset) { _, action in
                        Button(action.label) {
                            Task { await executeStructuredAction(action) }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }
            }
        }
        .padding(14)
        .frame(maxWidth: message.isUser ? 320 : nil, alignment: .leading)
        .background(message.isUser ? theme.accentColor : theme.surfaceColor)
        .overlay(
            RoundedRectangle(cornerRadius: 18)
                .stroke(structuredResponseToneColor(response.tone).opacity(0.4), lineWidth: 1)
        )
        .cornerRadius(18)
    }

    @ViewBuilder
    private func structuredResponseBlock(_ block: ChatStructuredBlock) -> some View {
        switch block.kind {
        case "markdown":
            if let text = trimmed(block.text) {
                structuredMarkdownBlock(text)
            }
        case "text":
            if let text = trimmed(block.text) {
                Text(text)
                    .font(.themed(14))
                    .foregroundColor(message.isUser ? theme.onAccentColor : theme.textColor)
                    .lineLimit(nil)
                    .fixedSize(horizontal: false, vertical: true)
            }
        case "callout":
            if let text = trimmed(block.text) {
                let tone = structuredResponseToneColor(block.tone)
                HStack(alignment: .top, spacing: 8) {
                    Text("💬")
                        .font(.themed(12))
                    VStack(alignment: .leading, spacing: 3) {
                        if let title = trimmed(block.title) {
                            Text(title)
                                .font(.themed(12, weight: .semibold))
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Text(text)
                            .font(.themed(13))
                            .lineLimit(nil)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(tone.opacity(0.12))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(tone.opacity(0.35), lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: 10))
            }
        case "key_values":
            if let items = block.items, !items.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    if let title = trimmed(block.title) {
                        Text(title)
                            .font(.themed(13, weight: .semibold))
                            .foregroundColor(message.isUser ? theme.onAccentColor : theme.textColor)
                    }
                    VStack(spacing: 7) {
                        ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                            if let label = trimmed(item["label"]), let value = trimmed(item["value"]) {
                                VStack(alignment: .leading, spacing: 1) {
                                    HStack {
                                        Text(label)
                                            .font(.themed(12, weight: .medium))
                                            .foregroundColor(theme.secondaryTextColor)
                                        Spacer()
                                        Text(value)
                                            .font(.themed(12, weight: .semibold))
                                            .foregroundColor(message.isUser ? theme.onAccentColor : theme.textColor)
                                            .multilineTextAlignment(.trailing)
                                    }
                                    if let hint = trimmed(item["hint"]) {
                                        Text(hint)
                                            .font(.themed(11))
                                            .foregroundColor(theme.secondaryTextColor)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        case "table":
            let title = trimmed(block.title)
            let columns = block.columns ?? []
            let rows = block.rows ?? []
            if !columns.isEmpty && !rows.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    if let title {
                        Text(title).font(.themed(13, weight: .semibold))
                    }
                    ScrollView(.horizontal, showsIndicators: false) {
                        VStack(spacing: 0) {
                            HStack(alignment: .center, spacing: 12) {
                                ForEach(columns, id: \.key) { column in
                                    Text(column.label)
                                        .font(.themed(12, weight: .semibold))
                                        .frame(minWidth: 110, alignment: .leading)
                                }
                            }
                            .padding(8)
                            .background(theme.cardColor)
                            ForEach(Array(rows.enumerated()), id: \.offset) { rowIndex, row in
                                Divider()
                                HStack(alignment: .top, spacing: 12) {
                                    ForEach(columns, id: \.key) { column in
                                        Text(row[column.key] ?? "")
                                            .font(.themed(12))
                                            .frame(minWidth: 110, alignment: .leading)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                }
                                .padding(.vertical, 6)
                                .background(rowIndex % 2 == 0 ? Color.clear : theme.cardColor.opacity(0.15))
                            }
                        }
                    }
                }
            }
        case "list":
            let title = trimmed(block.title)
            let style = block.style
            let items = block.items ?? []
            if !items.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    if let title {
                        Text(title).font(.themed(13, weight: .semibold))
                    }
                    ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                        if let text = trimmed(item["text"]), !text.isEmpty {
                            HStack(alignment: .top, spacing: 8) {
                                Text(listMarker(style, index: index))
                                    .font(.themed(12))
                                    .foregroundColor(message.isUser ? theme.onAccentColor : theme.textColor)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(text).font(.themed(13))
                                        .fixedSize(horizontal: false, vertical: true)
                                    if let detail = trimmed(item["detail"]) {
                                        Text(detail)
                                            .font(.themed(11))
                                            .foregroundColor(theme.secondaryTextColor)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        case "artifacts":
            let title = trimmed(block.title)
            let items = block.items ?? []
            if !items.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    if let title { Text(title).font(.themed(13, weight: .semibold)) }
                    ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                        let label = trimmed(item["label"]) ?? "Artifact"
                        let hint = trimmed(item["size"]).flatMap { sizeText in
                            if let sizeValue = Int(sizeText), sizeValue > 0 {
                                return "\(sizeValue) B"
                            }
                            return sizeText
                        }
                        let meta = [hint, trimmed(item["mime_type"])].compactMap { $0 }.joined(separator: " · ")
                        let target = structuredArtifactTarget(
                            label: label,
                            href: item["href"],
                            artifactId: item["artifact_id"],
                            relativePath: item["relative_path"],
                            absolutePath: item["absolute_path"],
                            source: item["source"],
                            mimeType: item["mime_type"]
                        )
                        Button(action: {
                            Task { await openStructuredArtifactTarget(item: item.displayValues) }
                        }) {
                            HStack(spacing: 10) {
                                Image(systemName: "doc")
                                    .font(.system(size: 16))
                                    .foregroundColor(theme.accentColor)
                                    .frame(width: 30, height: 30)
                                    .background(theme.accentColor.opacity(0.12))
                                    .clipShape(RoundedRectangle(cornerRadius: 7))
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(label)
                                        .font(.themed(13, weight: .medium))
                                        .lineLimit(1)
                                        .foregroundColor(theme.textColor)
                                    if !meta.isEmpty {
                                        Text(meta)
                                            .font(.themed(10))
                                            .foregroundColor(theme.secondaryTextColor)
                                    }
                                }
                                Spacer()
                                if target != nil { Image(systemName: "chevron.up.right").font(.system(size: 12)) }
                            }
                            .padding(.vertical, 7)
                            .padding(.horizontal, 8)
                        }
                        .buttonStyle(.plain)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(theme.cardColor)
                        .cornerRadius(10)
                    }
                }
            }
        case "sources":
            let title = trimmed(block.title)
            let items = block.items ?? []
            if !items.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    if let title { Text(title).font(.themed(13, weight: .semibold)) }
                    ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                        let label = trimmed(item["label"]) ?? "Source"
                        let href = trimmed(item["href"]) ?? trimmed(item["url"]) ?? ""
                        if !href.isEmpty {
                            Button(action: { Task { await openStructuredURL(href) } }) {
                                HStack(spacing: 8) {
                                    Text(label)
                                        .font(.themed(12))
                                        .underline()
                                        .foregroundColor(theme.accentColor)
                                        .multilineTextAlignment(.leading)
                                    Spacer()
                                    Image(systemName: "arrow.up.right.square")
                                }
                                .padding(.vertical, 6)
                            }
                            .buttonStyle(.plain)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            }
        case "metrics":
            let title = trimmed(block.title)
            let items = block.items ?? []
            if !items.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    if let title { Text(title).font(.themed(13, weight: .semibold)) }
                    VStack(spacing: 7) {
                        ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                            let label = trimmed(item["label"]) ?? "Metric"
                            if let value = trimmed(item["value"]), !value.isEmpty {
                                let unit = trimmed(item["unit"]).map { " \($0)" } ?? ""
                                HStack {
                                    Text(label)
                                        .font(.themed(12))
                                        .foregroundColor(theme.secondaryTextColor)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                    HStack(spacing: 4) {
                                        Text("\(value)\(unit)").font(.themed(13, weight: .semibold))
                                        if let trend = trimmed(item["trend"]) {
                                            Image(systemName: trendIcon(trend))
                                                .font(.system(size: 11))
                                        }
                                    }
                                    .foregroundColor(theme.textColor)
                                }
                            }
                        }
                    }
                }
            }
        default:
            EmptyView()
        }
    }

    private func structuredMarkdownBlock(_ text: String) -> some View {
        Markdown(text)
            .markdownTextStyle {
                FontFamily(.custom(theme.fontName))
                FontSize(14)
                ForegroundColor(message.isUser ? theme.onAccentColor : theme.textColor)
            }
    }

    private func structuredResponseToneColor(_ tone: String?) -> Color {
        switch tone?.lowercased() {
        case "success":
            return theme.successColor
        case "warning":
            return theme.warningColor
        case "danger":
            return theme.dangerColor
        case "info":
            return theme.accentColor
        default:
            return theme.secondaryTextColor
        }
    }

    private func listMarker(_ style: String?, index: Int) -> String {
        switch style {
        case "checks":
            return "☐"
        case "steps":
            return "\(index + 1)."
        default:
            return "•"
        }
    }

    private func trendIcon(_ value: String) -> String {
        switch value {
        case "up":
            return "arrow.up.right"
        case "down":
            return "arrow.down.right"
        case "flat":
            return "arrow.right"
        default:
            return "dash"
        }
    }

    private func trimmed(_ value: String?) -> String? {
        let value = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return value.isEmpty ? nil : value
    }

    private func executeStructuredAction(_ action: ChatStructuredAction) async {
        switch action.kind {
        case "copy_text":
            guard let text = trimmed(action.text) else {
                structuredActionErrorMessage = "Copy action is missing text."
                return
            }
            UIPasteboard.general.string = text
        case "open_url":
            guard let url = trimmed(action.url) else {
                structuredActionErrorMessage = "Open URL action is missing a URL."
                return
            }
            await openStructuredURL(url)
        case "open_task":
            guard let taskId = trimmed(action.taskId) else {
                structuredActionErrorMessage = "Open-task action is missing a task id."
                return
            }
            openStructuredTask(taskId, title: trimmed(action.label) ?? taskId)
        case "open_artifact":
            guard let artifactId = trimmed(action.artifactId) else {
                structuredActionErrorMessage = "Open-artifact action is missing an artifact id."
                return
            }
            await openStructuredArtifactTarget(
                item: [
                    "label": action.label,
                    "artifact_id": artifactId
                ]
            )
        case "send_follow_up":
            guard let prompt = trimmed(action.prompt) else {
                structuredActionErrorMessage = "Follow-up action is missing a prompt."
                return
            }
            onStructuredFollowUp(prompt)
        case "invoke_server_action":
            guard trimmed(action.actionRef) != nil else {
                structuredActionErrorMessage = "Action reference is missing for invoke_server_action."
                return
            }
            onStructuredResponseAction(action)
        default:
            structuredActionErrorMessage = "Unsupported structured action: \(action.kind)."
        }
    }

    private func openStructuredTask(_ taskId: String, title: String) {
        openedStructuredTask = StructuredTaskTarget(id: taskId, title: title)
    }

    private func openStructuredURL(_ raw: String) async {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            structuredActionErrorMessage = "Cannot open an empty destination."
            return
        }
        let destination = classifyActivityLink(trimmed)
        await openResolvedDestination(destination, raw: trimmed)
    }

    private func openResolvedDestination(_ destination: ActivityLinkDestination, raw: String) async {
        switch destination {
        case .external(let urlString):
            guard let url = safeStructuredExternalURL(urlString) else {
                structuredActionErrorMessage = "Could not open link: \(urlString)"
                return
            }
            await UIApplication.shared.open(url)
        case .task(let taskId):
            guard let taskId else {
                structuredActionErrorMessage = "Task identifier is missing."
                return
            }
            openStructuredTask(taskId, title: taskId)
        case .taskOutput(let taskId, let relativePath):
            openedArtifact = .taskOutput(taskId: taskId, relativePath: relativePath, mime: nil)
        case .artifact(let path):
            if let url = URL(string: path) {
                openedArtifact = .direct(url: url.absoluteString, mime: nil, name: URL(string: path)?.lastPathComponent)
            } else {
                structuredActionErrorMessage = "Cannot resolve artifact path."
            }
        case .webRoute(let path):
            if let url = tunnelURL(path) {
                openedArtifact = .direct(url: url.absoluteString, mime: "text/html", name: taskOrRouteTitle(from: path))
            } else {
                structuredActionErrorMessage = "Cannot resolve web route: \(path)"
            }
        case .file(let filePath):
            guard let sid = sessionId, !sid.isEmpty else {
                structuredActionErrorMessage = "Cannot open files from artifact references in this chat."
                return
            }
            openedArtifact = .sessionOutput(sessionId: sid, relativePath: filePath, mime: nil)
        case .thread(let threadId):
            let path = "/t/\(threadId)"
            if let url = tunnelURL(path) {
                openedArtifact = .direct(url: url.absoluteString, mime: "text/html", name: "Thread")
            } else {
                structuredActionErrorMessage = "Cannot open thread: \(threadId)"
            }
        case .attention(let attentionId):
            let path = "/attention\(attentionId.map { "?selected=\($0)" } ?? "")"
            if let url = tunnelURL(path) { openedArtifact = .direct(url: url.absoluteString, mime: "text/html", name: "Attention") }
            else { structuredActionErrorMessage = "Cannot open attention target." }
        case .today(let tab):
            let path = "/today\(tab.map { "?tab=\($0)" } ?? "")"
            if let url = tunnelURL(path) { openedArtifact = .direct(url: url.absoluteString, mime: "text/html", name: "Today") }
            else { structuredActionErrorMessage = "Cannot open Today surface." }
        case .settings:
            if let url = tunnelURL("/settings") { openedArtifact = .direct(url: url.absoluteString, mime: "text/html", name: "Settings") }
            else { structuredActionErrorMessage = "Cannot open settings." }
        case .observe:
            if let url = tunnelURL("/observe") { openedArtifact = .direct(url: url.absoluteString, mime: "text/html", name: "Observe") }
            else { structuredActionErrorMessage = "Cannot open observe surface." }
        case .execution(let executionId):
            if executionId.isEmpty {
                structuredActionErrorMessage = "Task execution identifier is missing."
            } else if let path = tunnelURL("/debug?execution_id=\(executionId)") {
                openedArtifact = .direct(url: path.absoluteString, mime: "text/plain", name: "Execution")
            } else {
                structuredActionErrorMessage = "Cannot open execution \(executionId)."
            }
        case .unknown:
            structuredActionErrorMessage = "Cannot resolve destination."
        }
    }

    private func safeStructuredExternalURL(_ raw: String) -> URL? {
        guard let url = URL(string: raw), let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https" else { return nil }
        return url
    }

    private func tunnelURL(_ path: String) -> URL? {
        if let parsed = URL(string: path), parsed.scheme != nil {
            return parsed
        }
        let prefixed = path.hasPrefix("/") ? path : "/\(path)"
        return URL(string: "\(MagicianAccess.baseURL.absoluteString)\(prefixed)")
    }

    private func taskOrRouteTitle(from path: String) -> String {
        if let idx = path.lastIndex(of: "/"), idx < path.endIndex {
            let suffix = String(path[path.index(after: idx)...])
            return trimmed(suffix) ?? "Open"
        }
        return trimmed(path) ?? "Open"
    }

    private func structuredArtifactTarget(
        label _: String,
        href: String?,
        artifactId: String?,
        relativePath: String?,
        absolutePath: String?,
        source: String?,
        mimeType _: String?
    ) -> ArtifactRef? {
        if let explicitHref = trimmed(href) {
            if let route = URL(string: explicitHref), route.scheme != nil {
                return ArtifactRef.direct(url: explicitHref, mime: nil, name: explicitHref)
            }
            let normalized = explicitHref.hasPrefix("/") ? explicitHref : "/\(explicitHref)"
            return ArtifactRef.direct(
                url: "\(MagicianAccess.baseURL.absoluteString)\(normalized)",
                mime: nil,
                name: explicitHref
            )
        }
        if let absPath = trimmed(absolutePath) {
            if let url = URL(string: absPath), url.scheme != nil { return ArtifactRef.direct(url: absPath, mime: nil, name: absPath) }
            return ArtifactRef.sessionOutput(sessionId: sessionId ?? "", relativePath: absPath, mime: nil)
        }
        if let artifactId, artifactId.hasPrefix("magician-artifact:task:") {
            let target = String(artifactId.dropFirst("magician-artifact:task:".count))
            let parts = target.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
            if parts.count == 2, !parts[0].isEmpty, !parts[1].isEmpty {
                return ArtifactRef.taskOutput(taskId: String(parts[0]), relativePath: String(parts[1]), mime: nil)
            }
        }
        if let artifactId, let sid = sessionId, artifactId.hasPrefix("magician-artifact:session:") {
            let relativePath = String(artifactId.dropFirst("magician-artifact:session:".count))
            if !relativePath.isEmpty {
                return ArtifactRef.sessionOutput(sessionId: sid, relativePath: relativePath, mime: nil)
            }
        }
        if let sessionId, let relPath = trimmed(relativePath) {
            return ArtifactRef.sessionOutput(sessionId: sessionId, relativePath: relPath, mime: nil)
        }
        if let artifactId, let sid = sessionId {
            let sourceId = trimmed(source)
            if sourceId == "task_output" {
                return ArtifactRef.taskOutput(taskId: artifactId, relativePath: trimmed(relativePath) ?? "", mime: nil)
            }
            return ArtifactRef.sessionOutput(sessionId: sid, relativePath: artifactId, mime: nil)
        }
        return nil
    }

    private func openStructuredArtifactTarget(item: [String: String]) async {
        if item.isEmpty {
            structuredActionErrorMessage = "Artifact item is empty."
            return
        }
        if let href = trimmed(item["href"]) {
            await openStructuredURL(href)
            return
        }
        if let path = trimmed(item["absolute_path"]) {
            await openStructuredArtifactTargetString(path)
            return
        }
        if let path = trimmed(item["relative_path"]) {
            await openStructuredArtifactTargetString(path)
            return
        }
        if let artifactId = trimmed(item["artifact_id"]) {
            await openStructuredArtifactTargetString(artifactId)
            return
        }
        if let label = trimmed(item["label"]) {
            await openStructuredArtifactTargetString(label)
            return
        }
        structuredActionErrorMessage = "Artifact action is missing destination."
    }

    private func openStructuredArtifactTargetString(_ raw: String) async {
        let trimmedValue = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedValue.isEmpty else {
            structuredActionErrorMessage = "Artifact action is missing destination."
            return
        }
        if trimmedValue.hasPrefix("magician-artifact:task:") {
            let target = String(trimmedValue.dropFirst("magician-artifact:task:".count))
            let parts = target.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
            if parts.count == 2, !parts[0].isEmpty, !parts[1].isEmpty {
                openedArtifact = .taskOutput(taskId: String(parts[0]), relativePath: String(parts[1]), mime: nil)
                return
            }
        }
        if trimmedValue.hasPrefix("magician-artifact:session:") {
            let relativePath = String(trimmedValue.dropFirst("magician-artifact:session:".count))
            if let sessionId, !relativePath.isEmpty {
                openedArtifact = .sessionOutput(sessionId: sessionId, relativePath: relativePath, mime: nil)
                return
            }
        }
        let destination = classifyActivityLink(trimmedValue)
        switch destination {
        case .taskOutput(let taskId, let relativePath):
            openedArtifact = .taskOutput(taskId: taskId, relativePath: relativePath, mime: nil)
        case .task(let taskId):
            if let taskId {
                openStructuredTask(taskId, title: "Task")
            } else {
                structuredActionErrorMessage = "Task id is missing from artifact action."
            }
        case .artifact(let path):
            if let url = URL(string: path) {
                openedArtifact = .direct(url: url.absoluteString, mime: nil, name: URL(string: path)?.lastPathComponent)
            } else {
                structuredActionErrorMessage = "Artifact path is invalid: \(path)"
            }
        case .external(let path):
            if let url = safeStructuredExternalURL(path) {
                openedArtifact = .direct(url: url.absoluteString, mime: nil, name: URL(string: path)?.lastPathComponent)
            } else {
                structuredActionErrorMessage = "Artifact destination is invalid."
            }
        case .webRoute(let path):
            if let route = tunnelURL(path) {
                openedArtifact = .direct(url: route.absoluteString, mime: nil, name: taskOrRouteTitle(from: path))
            } else {
                structuredActionErrorMessage = "Could not open artifact route."
            }
        case .file(let path):
            if let sid = sessionId, !sid.isEmpty {
                openedArtifact = .sessionOutput(sessionId: sid, relativePath: path, mime: nil)
            } else {
                structuredActionErrorMessage = "Cannot open artifact file for this chat."
            }
        case .thread(let threadId):
            if let route = tunnelURL("/t/\(threadId)") {
                openedArtifact = .direct(url: route.absoluteString, mime: "text/html", name: "Thread")
            } else {
                structuredActionErrorMessage = "Could not open thread artifact."
            }
        case .attention(let attentionId):
            let path = "/attention\(attentionId.map { "?selected=\($0)" } ?? "")"
            if let route = tunnelURL(path) {
                openedArtifact = .direct(url: route.absoluteString, mime: "text/html", name: "Attention")
            } else {
                structuredActionErrorMessage = "Could not open attention."
            }
        case .today(let tab):
            let path = "/today\(tab.map { "?tab=\($0)" } ?? "")"
            if let route = tunnelURL(path) {
                openedArtifact = .direct(url: route.absoluteString, mime: "text/html", name: "Today")
            } else {
                structuredActionErrorMessage = "Could not open today."
            }
        case .settings:
            if let route = tunnelURL("/settings") {
                openedArtifact = .direct(url: route.absoluteString, mime: "text/html", name: "Settings")
            } else {
                structuredActionErrorMessage = "Could not open settings."
            }
        case .observe:
            if let route = tunnelURL("/observe") {
                openedArtifact = .direct(url: route.absoluteString, mime: "text/html", name: "Observe")
            } else {
                structuredActionErrorMessage = "Could not open observe."
            }
        case .execution(let executionId):
            if let route = tunnelURL("/api/magician/v3/tasks/unknown/outputs/\(executionId)") {
                openedArtifact = .direct(url: route.absoluteString, mime: nil, name: "Execution output")
            } else {
                structuredActionErrorMessage = "Could not open execution output."
            }
        case .unknown:
            structuredActionErrorMessage = "Could not open artifact."
        }
    }

    /// A per-type "open" card for a file/url content block (mirrors the web
    /// ChatContentBlocks open card). Tapping opens the typed ArtifactViewer.
    @ViewBuilder
    private func artifactCard(_ block: ContentBlock) -> some View {
        let ref = artifactReference(block)
        HStack(spacing: 10) {
            Button(action: { if let ref { openedArtifact = ref } }) {
                HStack(spacing: 10) {
                Image(systemName: ref?.kind.systemIcon ?? "doc")
                    .font(.system(size: 16)).foregroundColor(theme.accentColor)
                    .frame(width: 32, height: 32)
                    .background(theme.accentColor.opacity(0.12)).clipShape(RoundedRectangle(cornerRadius: 8))
                VStack(alignment: .leading, spacing: 1) {
                    Text(ref?.filename ?? block.displayName ?? block.label ?? block.filename ?? "File")
                        .font(.themed(14, weight: .medium)).foregroundColor(theme.textColor).lineLimit(1)
                    Text(ref?.kind.label ?? "File").font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                }
                Spacer()
                }
            }
            .buttonStyle(.plain)
            .disabled(ref == nil)

            if let ref, let url = ref.url {
                Menu {
                    Button { openedArtifact = ref } label: {
                        Label("Open preview", systemImage: "doc.text.magnifyingglass")
                    }
                    if !ref.isMagicianOwned {
                        Button { UIApplication.shared.open(url) } label: {
                            Label("Open externally", systemImage: "arrow.up.forward.app")
                        }
                        Button { UIPasteboard.general.string = url.absoluteString } label: {
                            Label("Copy link", systemImage: "link")
                        }
                        Button { sharePayload = SharePayload(items: [url]) } label: {
                            Label("Share or save…", systemImage: "square.and.arrow.up")
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                        .font(.system(size: 17))
                        .foregroundColor(theme.secondaryTextColor)
                        .frame(width: 34, height: 34)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("Artifact actions")
            }
        }
        .padding(10)
        .background(theme.cardColor)
        .cornerRadius(12)
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(theme.cardBorderColor, lineWidth: 1))
        .contextMenu {
            if let ref {
                Button { openedArtifact = ref } label: {
                    Label("Open preview", systemImage: "doc.text.magnifyingglass")
                }
                if !ref.isMagicianOwned, let u = ref.url {
                    Button { UIApplication.shared.open(u) } label: {
                        Label("Open in Browser", systemImage: "safari")
                    }
                    Button { UIPasteboard.general.string = u.absoluteString } label: {
                        Label("Copy Link", systemImage: "link")
                    }
                    Button { sharePayload = SharePayload(items: [u]) } label: {
                        Label("Share…", systemImage: "square.and.arrow.up")
                    }
                }
            }
        }
    }

    private func artifactReference(_ block: ContentBlock) -> ArtifactRef? {
        let name = block.displayName ?? block.label ?? block.filename
        if let url = block.url, !url.isEmpty {
            return .direct(url: url, mime: block.mimeType, name: name)
        }
        guard let relativePath = block.relativePath, !relativePath.isEmpty else { return nil }
        if block.source?.type == "task_output", let taskId = block.source?.taskId {
            return .taskOutput(taskId: taskId, relativePath: relativePath, mime: block.mimeType)
        }
        guard let sessionId else { return nil }
        return .sessionOutput(sessionId: sessionId, relativePath: relativePath, mime: block.mimeType)
    }

    @ViewBuilder
    private func taskStatusIcon(_ task: TaskStatusModel) -> some View {
        if task.showsProgress {
            ProgressView().controlSize(.small).tint(taskStatusColor(task))
        } else {
            Image(systemName: taskStatusSystemImage(task))
                .foregroundColor(taskStatusColor(task))
        }
    }

    private func taskStatusSystemImage(_ task: TaskStatusModel) -> String {
        switch task.normalizedStatus {
        case "complete", "completed", "done": return "checkmark.circle.fill"
        case "failed", "error": return "exclamationmark.triangle.fill"
        case "paused": return "pause.circle.fill"
        case "cancelled", "canceled": return "xmark.circle.fill"
        case "created": return "sparkles"
        default: return "clock.fill"
        }
    }

    private func taskStatusColor(_ task: TaskStatusModel) -> Color {
        switch task.normalizedStatus {
        case "complete", "completed", "done": return theme.successColor
        case "failed", "error": return theme.dangerColor
        case "paused", "cancelled", "canceled": return theme.warningColor
        default: return theme.accentColor
        }
    }

    /// Per-message "read aloud / stop" affordance (mirrors the web SpeakButton).
    /// One message speaks at a time; tapping another stops the prior.
    private var speakButton: some View {
        let active = speech.isSpeaking(messageId: message.id)
        return Button(action: {
            if active { speech.stop() }
            else {
                speechPlaybackResult = nil
                speech.speak(message.text, messageId: message.id, completion: { result in
                    speechPlaybackResult = result
                })
            }
        }) {
            HStack(spacing: 4) {
                Image(systemName: active ? "stop.circle.fill" : "speaker.wave.2")
                    .font(.system(size: 12))
                Text(active ? "Stop" : "Speak").font(.themed(11, weight: .medium))
            }
            .foregroundColor(active ? theme.accentColor : theme.secondaryTextColor)
            .padding(.horizontal, 8).padding(.vertical, 4)
            .overlay(
                Capsule().stroke((active ? theme.accentColor : theme.secondaryTextColor).opacity(0.3), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .padding(.top, 2)
        .accessibilityIdentifier("chat-speak-\(message.id)")
        .accessibilityValue(active ? (speech.playbackSource?.label ?? "Preparing voice")
            : speechPlaybackResult == .completed ? "Playback complete"
            : speechPlaybackResult == .failed ? "Playback failed" : "")
    }
}
