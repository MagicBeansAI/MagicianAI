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
    @State private var suggestionFocusRequest: UUID?
    @State private var activeDraftKey: String?
    @State private var chatContextSheet: ChatContextSheet?
    /// The pushed task detail screen (chat card Inspect / Review, task links).
    @State private var detailRoute: TaskDetailRoute?
    /// The Plan Inspector a live card's Review / Plan graph opens.
    @State private var planInspectorRoute: PlanInspectorRoute?
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
    /// Ids of the messages currently on screen (LazyVStack appear/disappear):
    /// the floating date pill reads the topmost one's day.
    @State private var visibleMessageIds: Set<String> = []
    /// The floating date pill shows while the reader scrolls history and fades
    /// ~1.2 s after scrolling stops.
    @State private var floatingDateShown = false
    @State private var floatingDateToken = 0
    @State private var transcriptScrolling = false
    /// The top row may load an older page only after it scrolled out of
    /// view since the last load or the open (see `ChatOlderHistoryTrigger`).
    @State private var olderHistoryArmed = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Mic dictation + the web-style auto-send countdown after a transcript lands.
    @StateObject private var dictation = DictationController()
    @ObservedObject private var audioSettings = AudioSettings.shared
    /// Voice-first composer layout: the big tap-or-hold mic is primary by default;
    /// flips to the text composer when the user opts to type.
    @State private var isVoiceMode = true
    @State private var composerVoiceModeBeforeCall: Bool?
    @State private var typingDuringCall = false
    @State private var autoSendCountdown: Int? = nil
    /// When the pending dictation sends: drives the countdown band's draining bar.
    @State private var autoSendDeadline: Date?
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
    /// The Attention feed (owned by the tab view): the ask dock reads this
    /// conversation's pending asks from it and answers through it.
    @ObservedObject private var attention: AttentionViewModel
    /// Re-renders the dock the instant an ask resolves anywhere.
    @ObservedObject private var pendingHitl = PendingHitlTracker.shared
    /// Plan reads for the chat's open tasks (the Planner dock).
    @StateObject private var plannerDock = PlannerDockLoader()
    /// A feed answer posted from the ask dock is optimistic: its failure
    /// arrives later as the feed's `mutationError`, shown in the dock.
    @State private var askAwaitingFeed = false
    @State private var askNotice: String?
    /// A feed ask the dock handed off (a form, secret, grant…): its focused
    /// answer sheet, presented over the chat rather than in the Attention tab.
    @State private var askSheetRow: AttentionItem?

    init(attention: AttentionViewModel = AttentionViewModel()) {
        _attention = ObservedObject(wrappedValue: attention)
    }

    // MARK: Composer stack

    /// The task cards in this conversation, transcript order.
    private var chatTasks: [TaskStatusModel] {
        viewModel.messages.compactMap { message in
            if case .taskStatus(let task) = message.type { return task }
            return nil
        }
    }

    private var currentSession: ChatSession? {
        viewModel.currentSessionIdValue.flatMap { threadViewModel.sessionMetadata(for: $0) }
    }

    private var recentWelcomeSessions: [ChatSession] {
        // History's Sessions page is one thread's while filtered, not the recents.
        guard threadViewModel.sessionThreadFilter == nil else { return [] }
        return Array(threadViewModel.sessions.filter {
            $0.id != viewModel.currentSessionIdValue && $0.internalVoice == nil
        }.sorted { $0.updatedAt > $1.updatedAt }.prefix(3))
    }

    private func applySuggestedPrompt(_ prompt: String) {
        guard !isArchivedSession, !dictation.isRecording, !dictation.isTranscribing else { return }
        cancelAutoSend()
        turnFromVoice = false
        isVoiceMode = false
        if voiceCall.isActive { typingDuringCall = true }
        inputText = prompt
        suggestionFocusRequest = UUID()
    }

    /// An archived conversation opens read-only: no composer, no docks.
    private var isArchivedSession: Bool { currentSession?.status == "archived" }

    /// Every open ask (the ask dock): this conversation's unresolved
    /// escalation cards and the feed's pending asks scoped to it, then every
    /// other chat's or run's. An ask a realtime resolution named is gone
    /// everywhere, here included.
    private var askItems: [ComposerAskItem] {
        let resolvedElsewhere = attention.remotelyResolved
        let escalations = viewModel.messages.compactMap { message -> ChatMessageContentData? in
            guard case .escalation(let content) = message.type else { return nil }
            if let id = content.hitlCorrelationId,
               resolvedElsewhere.contains(id) || attention.isSettled(correlationId: id) { return nil }
            return content
        }
        return ComposerAskModel.items(
            chatEscalations: escalations,
            attention: attention.items("all").filter { !attention.isAnsweredElsewhere($0) },
            sessionId: viewModel.currentSessionIdValue,
            threadId: currentSession?.uiThreadId,
            chatTaskIds: Set(chatTasks.map(\.taskId)),
            plannerTaskIds: Set(plannerDock.dockEntries.map(\.taskId)),
            readOnly: isArchivedSession)
    }

    /// The presence dot beside the session title (web `PresenceOrb`): only
    /// this conversation's own ask means it needs you.
    private var presence: ChatPresenceState {
        let last = viewModel.messages.last
        let streaming = last.map { !$0.isUser && $0.id.hasPrefix("streaming-")
            && !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } ?? false
        let toolsRunning = viewModel.serverRunning || (last.map { !$0.isUser && $0.activityIsLive } ?? false)
        return ChatPresenceState.derive(
            hasPendingAsk: askItems.contains { !$0.elsewhere }, streaming: streaming,
            toolsRunning: toolsRunning, liveTurn: viewModel.isThinking)
    }

    private var composerTopRows: some View {
        VStack(spacing: 0) {
            if !viewModel.queuedMessages.isEmpty {
                ComposerTopRow(
                    title: QueuedMessagesTitle.make(
                        count: viewModel.queuedMessages.count,
                        firstText: viewModel.queuedMessages.first?.text),
                    systemImage: "chevron.right"
                ) {
                    showQueue = true
                }
                .accessibilityLabel("\(viewModel.queuedMessages.count) queued messages")
                .accessibilityHint("Opens the queue")
                .accessibilityIdentifier("chat-queued-messages")
            }
            // A queue failure, in place. The queue sheet's alert covers it
            // while the sheet is open, so it shows in one place at a time.
            if let error = viewModel.queueErrorMessage, !showQueue {
                ComposerErrorBand(message: error) { viewModel.queueErrorMessage = nil }
            }
            if let issue = viewModel.modelSelectionIssue,
               ["magician", "pi"].contains(viewModel.selectedHarnessEngine) {
                Text(issue)
                    .font(.themed(12))
                    .foregroundStyle(themeManager.dangerColor)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .accessibilityIdentifier("chat-model-selection-issue")
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
            if let reply = viewModel.planReply {
                PlanReplyBanner(
                    target: reply,
                    stale: PlannerDockModel.isStale(reply, entries: plannerDock.entries),
                    onCancel: cancelPlanReply)
            }
            if voiceCall.isActive && typingDuringCall {
                LiveCallComposerStrip(viewModel: voiceCall) { typingDuringCall = false }
            }
            // Dictation auto-send, inside the card (never a floating capsule).
            if let remaining = autoSendCountdown {
                AutoSendCountdownBand(remaining: remaining, deadline: autoSendDeadline) { cancelAutoSend() }
                    .transition(reduceMotion ? .identity : .move(edge: .top).combined(with: .opacity))
            }
            if viewModel.composerMode == .plan && viewModel.planReply == nil {
                PlanModeBand()
            }
        }
    }

    /// Ask dock → planner dock → call panel or composer card;
    /// an archived conversation shows its read-only band instead.
    @ViewBuilder
    private var composerStack: some View {
        if isArchivedSession {
            ArchivedSessionBand(onReturn: returnToActiveSession)
        } else {
            ComposerAskDock(
                items: askItems,
                originChatTitles: Dictionary(threadViewModel.sessions.map { ($0.id, $0.title ?? "Chat \($0.id.prefix(8))") }, uniquingKeysWith: { first, _ in first }),
                notice: askNotice,
                answer: answerAsk,
                onHandoff: handOffAsk,
                onReviewPlan: { planInspectorRoute = PlanInspectorRoute(taskId: $0) }
            )
            ComposerPlannerDock(
                loader: plannerDock,
                activeReply: viewModel.planReply,
                onReply: beginPlanReply,
                onOpenPlan: { planInspectorRoute = PlanInspectorRoute(taskId: $0.taskId, title: $0.title) }
            )
            // The call occupies the composer's slot; its top rows stay available.
            if voiceCall.isActive && !typingDuringCall {
                VoiceCallPanel(viewModel: voiceCall, handsFreeAvailable: handsFreeAvailable, onTypeMessage: {
                    isVoiceMode = false
                    typingDuringCall = true
                }, inFlow: true, topRows: AnyView(composerTopRows))
            } else {
                composer
            }
        }
    }

    private var composer: some View {
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
            executionChatSessionID: viewModel.currentSessionIdValue,
            mentionItems: viewModel.mentionItems,
            stagedAttachments: viewModel.stagedAttachments,
            onRemoveAttachment: { viewModel.removeStagedAttachment($0) },
            isRecording: dictation.isRecording, dictationLevel: { dictation.level },
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
            onLive: {
                voiceCall.startCall(
                    uiThreadId: viewModel.currentSessionIdValue ?? "",
                    chatSessionId: viewModel.currentSessionIdValue
                )
            },
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
            queueMutationInFlight: viewModel.queueMutationInFlight,
            commandContext: viewModel.composerCommandContext,
            onCommand: { viewModel.runComposerCommand($0) },
            onCommandOpened: { viewModel.composerCommandOpened($0) },
            onCommandLevelOpened: { viewModel.composerCommandLevelOpened($0) },
            commandNotice: viewModel.commandNotice,
            onCommandNoticeDone: { viewModel.commandNotice = nil },
            focusRequest: suggestionFocusRequest,
            focusOnAppear: voiceCall.isActive && typingDuringCall
        )
    }

    /// Answer an ask from the dock through its own respond path. Nil on
    /// success, else the reason the dock shows inline.
    private func answerAsk(_ item: ComposerAskItem, _ reply: ComposerAskReply) async -> String? {
        switch item.source {
        case .chat(let content):
            guard let submission = ComposerAskModel.submission(for: content, reply: reply) else {
                return "This answer can't be sent from here. Open it in Attention."
            }
            let failure = AskFailureBox()
            let outcome: String? = await withCheckedContinuation { continuation in
                viewModel.respondToEscalation(
                    content: content, submission: submission,
                    onError: { failure.message = $0 }
                ) { ok in
                    continuation.resume(returning: ok ? nil : (failure.message ?? "The answer was not sent."))
                }
            }
            // Answered here: the same ask's feed row (Attention, a task's
            // detail screen) is settled too, so no other surface offers it.
            if outcome == nil { attention.noteRemoteResolution(correlationId: item.id) }
            return outcome
        case .attention(let row):
            askNotice = nil
            askAwaitingFeed = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { askAwaitingFeed = false }
            // The feed answers optimistically: the row leaves at once and
            // comes back (with `mutationError`) only if the post fails.
            return attention.answerInPlace(row, form: item.form, reply: reply)
        }
    }

    /// An ask the dock cannot answer inline (a form, a secret, a grant, a
    /// diff): its focused answer sheet, over the chat. A chat card with no
    /// feed row loaded yet still opens Attention on it.
    private func handOffAsk(_ item: ComposerAskItem) {
        switch item.source {
        case .attention(let row):
            askSheetRow = row
        case .chat:
            if let row = attention.items("all").first(where: { $0.correlationId == item.id }) {
                askSheetRow = row
            } else {
                AppActions.shared.requestAttention(itemID: item.id)
            }
        }
    }

    /// Planner dock → Reply: focus the composer on that plan question.
    private func beginPlanReply(_ entry: PlannerDockEntry, _ question: PlanQuestion) {
        if let current = viewModel.planReply, current.taskId == entry.taskId, current.questionId == question.id { return }
        let returnMode = viewModel.planReply?.returnMode ?? viewModel.composerMode
        viewModel.planReply = PlanReplyTarget(
            taskId: entry.taskId, taskTitle: entry.title,
            questionId: question.id, questionText: question.text, returnMode: returnMode)
        withAnimation(reduceMotion ? nil : .default) {
            viewModel.composerMode = .plan
            isVoiceMode = false
            if voiceCall.isActive { typingDuringCall = true }
        }
    }

    private func cancelPlanReply() {
        guard let reply = viewModel.planReply else { return }
        viewModel.planReply = nil
        viewModel.composerMode = reply.returnMode
    }

    /// Archived → the thread's active conversation, or a fresh one.
    private func returnToActiveSession() {
        let threadId = currentSession?.uiThreadId ?? threadViewModel.activeThreadId
        if let active = threadViewModel.sessions.first(where: { $0.uiThreadId == threadId && $0.status == "active" }) {
            threadViewModel.selectSession(active.id)
            viewModel.loadSession(active.id)
        } else {
            threadViewModel.newSession(uiThreadId: threadId) { viewModel.startNewSession($0) }
        }
    }

    var body: some View {
        NavigationStack {
            ZStack(alignment: .leading) {
                VStack(spacing: 0) {
                ScrollViewReader { proxy in
                    ZStack(alignment: .bottomTrailing) {
                        ScrollView {
                            LazyVStack(spacing: 12) {
                                if let error = viewModel.originalAnswerError {
                                    Text(error).font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                                }
                                if viewModel.messages.isEmpty, viewModel.openingSessionId != nil {
                                    ProgressView()
                                        .frame(maxWidth: .infinity)
                                        .padding(.top, 40)
                                        .accessibilityLabel("Opening conversation")
                                } else if viewModel.messages.isEmpty, viewModel.sessionOpenFailed,
                                          let sessionId = viewModel.currentSessionIdValue {
                                    Button("Try again") { viewModel.loadSession(sessionId) }
                                        .font(.themed(14))
                                        .frame(maxWidth: .infinity)
                                        .padding(.top, 24)
                                } else if viewModel.messages.isEmpty {
                                    ChatWelcomeView(
                                        theme: themeManager,
                                        readOnly: isArchivedSession,
                                        enabled: !dictation.isRecording && !dictation.isTranscribing,
                                        recentSessions: recentWelcomeSessions,
                                        onSuggest: applySuggestedPrompt,
                                        onOpenSession: { id in
                                            cancelAutoSend()
                                            threadViewModel.selectSession(id)
                                            viewModel.loadSession(id)
                                        }
                                    )
                                    .id("chat-welcome")
                                    .onAppear {
                                        isAtBottom = true
                                        showJumpPill = false
                                        DispatchQueue.main.async {
                                            if viewModel.messages.isEmpty {
                                                proxy.scrollTo("chat-welcome", anchor: .top)
                                            }
                                        }
                                    }
                                } else {
                                    olderHistoryRow
                                    let dayLabels = ChatDateDividers.labels(for: viewModel.messages.map(\.createdAt))
                                    ForEach(Array(viewModel.messages.enumerated()), id: \.element.id) { index, message in
                                        if let label = dayLabels[index] {
                                            ChatDayDivider(label: label, theme: themeManager)
                                        }
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
                                        .onDisappear {
                                            visibleMessageIds.remove(message.id)
                                            noteTranscriptScrolled()
                                        }
                                        .onAppear {
                                            visibleMessageIds.insert(message.id)
                                            noteTranscriptScrolled()
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
                                        isAtBottom = viewModel.messages.isEmpty
                                        showJumpPill = !viewModel.messages.isEmpty
                                    }
                            }
                            .padding(.horizontal)
                            .padding(.top)
                            // The composer card sits right under the last item.
                            .padding(.bottom, 4)
                        }
                        // Drag the transcript to dismiss the keyboard without hiding
                        // the viewport-pinned jump-to-latest control.
                        .scrollDismissesKeyboard(.interactively)
                        .modifier(TranscriptScrollPhaseObserver { scrolling in
                            transcriptScrolling = scrolling
                            noteTranscriptScrolled()
                        })
                        .overlay(alignment: .top) { floatingDatePill }
                        // Tap anywhere on the chat to dismiss the keyboard (WhatsApp
                        // style). `simultaneousGesture` so message buttons/links still
                        // fire on the same tap.
                        .simultaneousGesture(TapGesture().onEnded {
                            guard !viewModel.messages.isEmpty else { return }
                            UIApplication.shared.sendAction(
                                #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
                        })

                        if showJumpPill && !viewModel.messages.isEmpty {
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
                                    .background(themeManager.accentFillColor)
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
                    // An older page landed on top: keep the row that was first
                    // where the reader left it (unless they went to the bottom).
                    .onChange(of: viewModel.olderHistoryPrepend) { _, prepend in
                        guard let prepend, !isAtBottom else { return }
                        var transaction = Transaction()
                        transaction.disablesAnimations = true
                        withTransaction(transaction) { proxy.scrollTo(prepend.anchorMessageId, anchor: .top) }
                        DispatchQueue.main.async {
                            withTransaction(transaction) { proxy.scrollTo(prepend.anchorMessageId, anchor: .top) }
                        }
                    }
                    // Also scroll when a task status updates its internal steps.
                    .onChange(of: viewModel.messages) { _, _ in
                        handleTranscriptGrowth(proxy)
                    }
                }
                
                // The composer stack: docks, then the card.
                VStack(spacing: 0) { composerStack }
                    // On the stack, not the dock: the dock empties once the
                    // sheet's answer lands, and must not take the sheet with it.
                    .attentionAskSheet($askSheetRow, viewModel: attention)
                    .followVisibleAsks(attention)
                    .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: askItems.map(\.id))
                    .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: plannerDock.dockEntries.map(\.taskId))
                    .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: autoSendCountdown != nil)
                // Removed padding to allow full width for composer
            }
            // The theme's wash, full bleed behind the transcript (never flat).
            .background(themeManager.washGradient.ignoresSafeArea())
            .navigationBarTitleDisplayMode(.inline)
            // SwiftUI owns this navigation bar, so bind its chrome directly to
            // the live palette instead of relying only on UIKit appearance
            // proxies, which can retain the launch-time color.
            .toolbarBackground(themeManager.backgroundColor, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbarColorScheme(themeManager.colorScheme, for: .navigationBar)
            .sheet(item: $chatContextSheet) { target in
                if target.page == .settings, let threadID = target.threadID {
                    ChatThreadSettings(threadID: threadID, client: target.client, onSaved: threadViewModel.applyThreadUpdate)
                } else {
                    ChatSessionDetails(sessionID: target.sessionID, page: target.page, client: target.client)
                }
            }
            .onChange(of: viewModel.currentSessionIdValue) { _, _ in
                chatContextSheet = nil
                olderHistoryArmed = false
            }
            .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in chatContextSheet = nil }
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
                        presence: presence,
                        onOpenPanel: { withAnimation { showThreadPanel = true } }
                    )
                    .id("chat-navigation-title-\(themeManager.themeRevision)")
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    ChatSessionActionsMenu(
                        chatViewModel: viewModel,
                        threadViewModel: threadViewModel,
                        hasThreadContext: currentSession != nil,
                        onOpenContext: { page in
                            guard let sessionID = viewModel.currentSessionIdValue else { return }
                            let threadID = currentSession?.uiThreadId
                            if page == .tasks {
                                if let threadID { AppActions.shared.requestWork(.init(kind: .thread, recordID: threadID)) }
                            } else {
                                chatContextSheet = ChatContextSheet(sessionID: sessionID, threadID: threadID, page: page, client: try? WorkAPIClient.connected())
                            }
                        }
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
            .taskDetailHost($detailRoute)
            .planInspectorHost($planInspectorRoute)
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
            // A thread or chat link (`/t/<thread>[/chat][?session=…]`,
            // `/chat?session=…`, `magican://thread/…`): the named conversation,
            // else the thread's latest one, as a tap in the thread panel does.
            .onReceive(AppActions.shared.$threadRequestID.dropFirst()) { _ in
                let actions = AppActions.shared
                let threadId = actions.threadTargetID
                let sessionId = actions.threadTargetSessionID
                actions.consumeThreadTarget()
                if let sessionId {
                    cancelAutoSend()
                    threadViewModel.selectSession(sessionId)
                    viewModel.loadSession(sessionId)
                } else if let threadKey = threadId {
                    // A link's key may be the thread's slug (it follows the
                    // name) or an old one; open by the canonical id.
                    ThreadDirectory.shared.resolve(threadKey) { threadId in
                        threadViewModel.selectThread(threadId)
                        let thread = threadViewModel.threadMetadata(for: threadId) ?? UiThreadRecord(
                            principal: "", workspace: "", id: threadId,
                            name: ThreadDirectory.shared.entries[threadId]?.name ?? threadId, archived: false,
                            sortOrder: 0, memorySummary: nil, memoryUpdatedAt: nil, createdAt: 0, updatedAt: 0
                        )
                        threadViewModel.openThread(thread) { opened in
                            guard let opened else { return }
                            cancelAutoSend()
                            viewModel.loadSession(opened)
                        }
                    }
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
        // Preserve the entry mode even if the owner types during the call.
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: voiceCall.isActive)
        .onChange(of: voiceCall.isActive) { _, active in
            if active {
                if composerVoiceModeBeforeCall == nil { composerVoiceModeBeforeCall = isVoiceMode }
            } else {
                if let previous = composerVoiceModeBeforeCall { isVoiceMode = previous }
                composerVoiceModeBeforeCall = nil
                typingDuringCall = false
            }
        }
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
        .onChange(of: attention.mutationError) { _, error in
            guard let error, askAwaitingFeed else { return }
            askAwaitingFeed = false
            askNotice = error
        }
        // Planner dock: re-read the open tasks' plans while the chat is up.
        .task(id: plannerRefreshKey) {
            while !Task.isCancelled {
                await plannerDock.refresh(tasks: isArchivedSession ? [] : chatTasks)
                try? await Task.sleep(for: .seconds(8))
            }
        }
        .onChange(of: viewModel.chatLocalStorageKey, initial: true) { _, key in
            let store = ChatDraftStore(defaults: .standard)
            if let activeDraftKey { store.save(inputText, key: activeDraftKey) }
            activeDraftKey = key
            inputText = store.read(key)
            concurrentVoice.select(nil)
        }
        .onChange(of: inputText) { _, text in
            if let activeDraftKey { ChatDraftStore(defaults: .standard).save(text, key: activeDraftKey) }
        }
        .onChange(of: concurrentVoice.focus?.id, initial: true) { _, id in viewModel.concurrentVoiceTopicSelected = id != nil }
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

    /// Top of the transcript: the older-history trigger, its spinner, or a
    /// retry after a failed page. The wrapper's appear/disappear tracks only
    /// scrolling (not the branch inside), so a landed page cannot re-fire it.
    private var olderHistoryRow: some View {
        VStack(spacing: 0) {
            let state = viewModel.olderHistory
            if state.isLoading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading earlier messages…")
                        .font(.themed(12))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 4)
            } else if state.failed {
                Button {
                    viewModel.loadOlderMessages()
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.clockwise")
                        Text("Couldn't load earlier messages. Retry")
                    }
                    .font(.themed(12, weight: .semibold))
                    .foregroundColor(themeManager.accentColor)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 4)
                }
                .buttonStyle(.plain)
            } else if state.hasMore {
                // Also a tap target: a transcript that fits on screen never
                // scrolls the row out and back, so the auto-trigger cannot arm.
                Button {
                    viewModel.loadOlderMessages()
                } label: {
                    Text("Load earlier messages")
                        .font(.themed(12))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 4)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("chat-load-earlier")
            }
        }
        .id("older-history-row")
        .onAppear {
            guard ChatOlderHistoryTrigger.shouldLoad(
                armed: olderHistoryArmed, isAtBottom: isAtBottom, state: viewModel.olderHistory) else { return }
            olderHistoryArmed = false
            viewModel.loadOlderMessages()
        }
        // Scrolled away from the top of a loaded transcript (the open lands
        // at the bottom): the next return to the top may load one more page.
        // A chat switch resets the state first, so its teardown never arms.
        .onDisappear {
            if viewModel.olderHistory.hasMore && !viewModel.messages.isEmpty { olderHistoryArmed = true }
        }
    }

    /// A new/updated message arrived: if the user is at the bottom, follow it;
    /// otherwise surface the jump pill instead of yanking their scroll position.
    private func handleTranscriptGrowth(_ proxy: ScrollViewProxy) {
        guard !viewModel.messages.isEmpty else {
            isAtBottom = true
            showJumpPill = false
            proxy.scrollTo("chat-welcome", anchor: .top)
            return
        }
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
            dictation.startVoiceNote(chatSessionId: viewModel.currentSessionIdValue)
        } else {
            dictation.startLive(chatSessionId: viewModel.currentSessionIdValue)
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
                chatSessionId: viewModel.currentSessionIdValue,
                engineOverride: .handsFree
            )
        case .realtime:
            guard !voiceCall.isActive else { return }
            voiceCall.startCall(
                uiThreadId: viewModel.currentSessionIdValue ?? "",
                chatSessionId: viewModel.currentSessionIdValue,
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
    // MARK: Floating date

    /// The day under the top of the transcript, while the reader scrolls
    /// history (never pinned to the tail, so streaming never flashes it).
    @ViewBuilder
    private var floatingDatePill: some View {
        let indices = Set(viewModel.messages.indices.filter { visibleMessageIds.contains(viewModel.messages[$0].id) })
        if floatingDateShown, !isAtBottom,
           let label = ChatDateDividers.floatingLabel(
               timestamps: viewModel.messages.map(\.createdAt), visibleIndices: indices) {
            Text(label)
                .font(.themed(12, weight: .semibold))
                .foregroundColor(themeManager.textColor)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(Capsule().fill(.ultraThinMaterial))
                .overlay(Capsule().stroke(themeManager.cardBorderColor, lineWidth: 1))
                .shadow(color: .black.opacity(0.08), radius: 4, y: 1)
                .padding(.top, 8)
                .allowsHitTesting(false)
                .transition(.opacity)
                .accessibilityHidden(true)
        }
    }

    /// Scrolling shows the pill; it fades ~1.2 s after scrolling stops.
    private func noteTranscriptScrolled() {
        guard !isAtBottom else {
            if floatingDateShown { floatingDateShown = false }
            return
        }
        floatingDateToken &+= 1
        if !floatingDateShown {
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.15)) { floatingDateShown = true }
        }
        guard !transcriptScrolling else { return }
        let token = floatingDateToken
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
            guard token == floatingDateToken, !transcriptScrolling else { return }
            withAnimation(reduceMotion ? nil : .easeIn(duration: 0.3)) { floatingDateShown = false }
        }
    }

    private func handleHoldStart() {
        cancelAutoSend()
        concurrentVoice.captureStarted()
        if VoiceCapture.shouldBargeIn(isSpeaking: SpeechSynthesizer.shared.isSpeaking) {
            SpeechSynthesizer.shared.stop()
        }
        if audioSettings.archiveChatDictation {
            dictation.startVoiceNote(chatSessionId: viewModel.currentSessionIdValue)
        } else {
            dictation.startLive(chatSessionId: viewModel.currentSessionIdValue)
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

    /// The open tasks the planner dock reads, and the session they belong to.
    private var plannerRefreshKey: String {
        let ids = PlannerDockModel.candidateTaskIds(chatTasks).map(\.taskId).joined(separator: ",")
        return "\(viewModel.currentSessionIdValue ?? "")|\(ids)|\(isArchivedSession)"
    }

    private func armAutoSend() {
        autoSendTimer?.invalidate()
        autoSendDeadline = Date().addingTimeInterval(TimeInterval(AutoSendCountdown.seconds))
        withAnimation(reduceMotion ? nil : .default) { autoSendCountdown = AutoSendCountdown.seconds }
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
        autoSendDeadline = nil
        withAnimation(reduceMotion ? nil : .default) { autoSendCountdown = nil }
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
/// Carries a dock answer's failure out of the respond callback.
private final class AskFailureBox { var message: String? }

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
                                .buttonBorderShape(.roundedRectangle(radius: Lively.chipRadius))
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
                                .buttonBorderShape(.roundedRectangle(radius: Lively.chipRadius))
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
    @Environment(\.openTaskDetail) private var openTaskDetail
    @Environment(\.openPlanInspector) private var openPlanInspector
    @State private var openedArtifact: ArtifactRef?
    @State private var sharePayload: SharePayload?
    @State private var structuredActionErrorMessage: String?
    @State private var speechPlaybackResult: SpeechPlaybackResult?
    @ObservedObject private var speech = SpeechSynthesizer.shared

    
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
                            plainTextMessageBubble(answerDisplay.body)
                        }
                        if !message.isUser, let notice = message.turnStatusNotice {
                            turnStatusLine(notice)
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
                            HStack(spacing: 4) {
                                speakButton
                                if let note = answerDisplay.verification {
                                    AnswerVerificationInfo(note: note, theme: theme)
                                }
                            }
                        }
                    }

                case .taskStatus(let task):
                    // One live card for the task's whole life (design §2):
                    // planning → plan review → run → result.
                    LiveTaskCardView(
                        messageId: message.id,
                        task: task,
                        sessionId: sessionId,
                        theme: theme,
                        onInspect: { openTaskDetail?(TaskDetailRoute(statusModel: task, inspectRun: true)) },
                        onReviewPlan: { taskId in
                            let target = PlanReviewTarget(taskId: taskId)
                            if let openPlanInspector { openPlanInspector(target.inspectorRoute) }
                            else { openTaskDetail?(target.route) }
                        }
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
            // Links anywhere in the bubble (task cards, escalations, inline
            // Markdown) open through the chat link router, not the system.
            .environment(\.openURL, chatLinkAction)
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

    private var answerDisplay: AnswerVerificationDisplay {
        message.isUser ? AnswerVerificationDisplay(body: message.text, verification: nil)
            : AnswerVerificationDisplay.split(message.verificationSourceText ?? message.text)
    }

    private func answerBody(_ text: String) -> String {
        answerDisplay.verification == nil ? text : AnswerVerificationDisplay.split(text).body
    }

    /// A settled turn's status line in place of a reply: "Stopped" after a
    /// Stop, or a failed / interrupted line (`ChatTurnState.notice`).
    private func turnStatusLine(_ notice: String) -> some View {
        let stopped = notice == ChatTurnState.stoppedNotice
        return Label(notice, systemImage: stopped ? "stop.circle" : "exclamationmark.triangle")
            .font(.themed(12, weight: .medium))
            .foregroundColor(stopped ? theme.secondaryTextColor : theme.warningColor)
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .background(theme.controlColor)
            .clipShape(RoundedRectangle(cornerRadius: Lively.bubbleRadius, style: .continuous))
            .accessibilityIdentifier("chat-turn-status")
    }

    private func plainTextMessageBubble(_ text: String) -> some View {
        Markdown(ChatPathLinkifier.markdown(text))
            .environment(\.openURL, chatLinkAction)
            .markdownTextStyle {
                FontFamily(.custom(theme.fontName))
                FontSize(14)
                ForegroundColor(message.isUser ? theme.onAccentColor : theme.textColor)
            }
            // After the body style, which colours every run: a link keeps its
            // own accent and underline instead of reading as body text.
            .markdownTextStyle(\.link) {
                ForegroundColor(message.isUser ? theme.onAccentColor : theme.accentColor)
                UnderlineStyle(.single)
            }
            // Assistant bubbles are compact and share their leading edge (and
            // inner inset) with the task cards; user bubbles keep more air.
            .padding(.horizontal, message.isUser ? 14 : 12)
            .padding(.vertical, message.isUser ? 12 : 9)
            .background(message.isUser ? theme.accentFillColor : theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: Lively.bubbleRadius, style: .continuous))
    }

    @ViewBuilder
    private func structuredResponseCard(_ response: ChatMessagePresentationData) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let title = trimmed(response.title) {
                Text(title)
                    .font(.themed(15, weight: .semibold))
            }
            if let summary = trimmed(response.summary.map(answerBody)) {
                structuredMarkdownBlock(summary, size: 13, color: theme.secondaryTextColor)
            }
            if response.blocks.isEmpty {
                let fallback = trimmed(answerBody(response.plainText)) ?? trimmed(answerDisplay.body)
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
                        .buttonBorderShape(.roundedRectangle(radius: 10))
                        .controlSize(.small)
                    }
                }
            }
        }
        .padding(14)
        .frame(maxWidth: message.isUser ? 320 : nil, alignment: .leading)
        .background(message.isUser ? theme.accentFillColor : theme.surfaceColor)
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
            if let text = trimmed(block.text.map(answerBody)) {
                structuredMarkdownBlock(text)
            }
        case "text":
            if let text = trimmed(block.text.map(answerBody)) {
                structuredMarkdownBlock(text)
            }
        case "callout":
            if let text = trimmed(block.text.map(answerBody)) {
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
                        structuredMarkdownBlock(text, size: 13)
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

    /// Structured `markdown`, `text`, `callout` and summary copy: Markdown so
    /// links parse, are visible and open through the chat link router.
    private func structuredMarkdownBlock(_ text: String, size: CGFloat = 14, color: Color? = nil) -> some View {
        Markdown(ChatPathLinkifier.markdown(text))
            .environment(\.openURL, chatLinkAction)
            .markdownTextStyle {
                FontFamily(.custom(theme.fontName))
                FontSize(size)
                ForegroundColor(color ?? (message.isUser ? theme.onAccentColor : theme.textColor))
            }
            .markdownTextStyle(\.link) {
                ForegroundColor(message.isUser ? theme.onAccentColor : theme.accentColor)
                UnderlineStyle(.single)
            }
    }

    /// Every link tap in this bubble goes through the one chat link router.
    private var chatLinkRouter: ChatLinkRouter {
        ChatLinkRouter(
            sessionId: sessionId,
            present: { openedArtifact = $0 },
            fail: { structuredActionErrorMessage = $0 },
            openTask: openTaskDetail.map { open -> (String) -> Void in
                { taskId in open(TaskDetailRoute(taskId: taskId, titleHint: taskId)) }
            }
        )
    }

    private var chatLinkAction: OpenURLAction {
        let router = chatLinkRouter
        return OpenURLAction { url in
            router.open(url)
            return .handled
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
        openTaskDetail?(TaskDetailRoute(taskId: taskId, titleHint: title))
    }

    /// Structured `open_url` actions, source rows and artifact hrefs open
    /// through the same router as Markdown links.
    private func openStructuredURL(_ raw: String) async {
        chatLinkRouter.open(raw)
    }

    /// The open target the server recorded for a shortened `artifact_id`
    /// (`meta.artifact_locators`); nil means parse the id as before.
    private func structuredArtifactLocator(for artifactId: String?) -> (taskId: String?, relativePath: String)? {
        guard let artifactId = trimmed(artifactId),
              let locators = message.structuredResponse?.meta?.artifactLocators else { return nil }
        for locator in locators where trimmed(locator["artifact_id"]) == artifactId {
            guard let relativePath = trimmed(locator["relative_path"]) else { return nil }
            return (trimmed(locator["task_id"]), relativePath)
        }
        return nil
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
        if let locator = structuredArtifactLocator(for: artifactId) {
            if let taskId = locator.taskId {
                return ArtifactRef.taskOutput(taskId: taskId, relativePath: locator.relativePath, mime: nil)
            }
            if let sid = sessionId {
                return ArtifactRef.sessionOutput(sessionId: sid, relativePath: locator.relativePath, mime: nil)
            }
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
        if let locator = structuredArtifactLocator(for: trimmedValue) {
            if let taskId = locator.taskId {
                openedArtifact = .taskOutput(taskId: taskId, relativePath: locator.relativePath, mime: nil)
                return
            }
            if let sessionId {
                openedArtifact = .sessionOutput(sessionId: sessionId, relativePath: locator.relativePath, mime: nil)
                return
            }
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
        chatLinkRouter.open(trimmedValue)
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

    /// Per-message "read aloud / stop" affordance (mirrors the web SpeakButton).
    /// One message speaks at a time; tapping another stops the prior.
    private var speakButton: some View {
        let active = speech.isSpeaking(messageId: message.id)
        return Button(action: {
            if active { speech.stop() }
            else {
                speechPlaybackResult = nil
                speech.speak(message.text, messageId: message.id, chatSessionId: sessionId, completion: { result in
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
                RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                    .stroke((active ? theme.accentColor : theme.secondaryTextColor).opacity(0.3), lineWidth: 1)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.top, 2)
        .accessibilityIdentifier("chat-speak-\(message.id)")
        .accessibilityValue(active ? (speech.playbackSource?.label ?? "Preparing voice")
            : speechPlaybackResult == .completed ? "Playback complete"
            : speechPlaybackResult == .failed ? "Playback failed" : "")
    }
}

/// Web's welcome surface, keeping suggestions as editable drafts.
private struct ChatWelcomeView: View {
    @ObservedObject var theme: ThemeManager
    let readOnly: Bool
    let enabled: Bool
    let recentSessions: [ChatSession]
    let onSuggest: (String) -> Void
    let onOpenSession: (String) -> Void
    @Environment(\.dynamicTypeSize) private var typeSize

    private struct Starter: Identifiable {
        let id: String
        let label: String
        let hint: String
        let prompt: String
        let icon: String
    }

    private static let starters = [
        Starter(id: "flights", label: "Research flights", hint: "Compare fares", prompt: "Find the cheapest Kyoto flights for next month and shortlist the three best options.", icon: "airplane"),
        Starter(id: "meeting", label: "Catch up on a meeting", hint: "Decisions & next steps", prompt: "What did I miss in yesterday’s design sync? Decisions, and anything I owe people.", icon: "calendar"),
        Starter(id: "groceries", label: "Restock the groceries", hint: "Stay within budget", prompt: "We’re out of groceries. Order my usual list and stay within my spending limit.", icon: "basket"),
        Starter(id: "screen", label: "Review my screen time", hint: "See where time went", prompt: "Look at my recent screen observations and summarize where my time went today.", icon: "desktopcomputer"),
    ]

    private var greeting: String {
        if readOnly { return "Nothing in this session" }
        switch Calendar.current.component(.hour, from: Date()) {
        case ..<12: return "Good morning"
        case ..<18: return "Good afternoon"
        default: return "Good evening"
        }
    }

    var body: some View {
        VStack(spacing: 18) {
            VStack(spacing: 8) {
                    Text(greeting)
                        .font(.themedDisplay(28, weight: .semibold))
                        .foregroundStyle(theme.textColor)
                        .multilineTextAlignment(.center)
                        .accessibilityAddTraits(.isHeader)
                if !readOnly {
                    Text("Pick a start, or say it your way.")
                        .font(.themed(12)).foregroundStyle(theme.secondaryTextColor)
                }
            }
            .multilineTextAlignment(.center)
            if !readOnly {
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 10), count: typeSize.isAccessibilitySize ? 1 : 2), spacing: 10) {
                    ForEach(Array(Self.starters.enumerated()), id: \.element.id) { index, starter in
                        starterCard(starter, tint: [theme.accentColor, theme.infoColor, theme.successColor, theme.discoveryColor][index])
                    }
                }
            }
            if !recentSessions.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("PICK UP WHERE YOU LEFT OFF")
                        .font(.themed(10, weight: .semibold)).tracking(1)
                        .foregroundStyle(theme.secondaryTextColor)
                        .accessibilityAddTraits(.isHeader)
                        .padding(.bottom, 4)
                    ForEach(recentSessions) { session in
                        Button { onOpenSession(session.id) } label: {
                            HStack(spacing: 10) {
                                Image(systemName: "clock").font(.system(size: 13))
                                Text(session.title.flatMap { $0.isEmpty ? nil : $0 } ?? "Untitled chat")
                                    .font(.themed(12)).foregroundStyle(theme.textColor)
                                    .lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
                                Text(relativeTime(session.updatedAt)).font(.themed(10)).fixedSize()
                                Image(systemName: "chevron.right").font(.system(size: 10))
                            }
                            .foregroundStyle(theme.secondaryTextColor)
                            .padding(.horizontal, 12).padding(.vertical, 9)
                            .frame(minHeight: 44)
                            .background(theme.cardColor.opacity(0.70), in: RoundedRectangle(cornerRadius: 12))
                            .contentShape(RoundedRectangle(cornerRadius: 12))
                        }
                        .buttonStyle(.plain).disabled(!enabled)
                        .accessibilityHint("Opens this conversation")
                    }
                }
            }
        }
        .frame(maxWidth: 560)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 16)
        .accessibilityIdentifier("chat-welcome")
    }

    private func starterCard(_ starter: Starter, tint: Color) -> some View {
        Button { onSuggest(starter.prompt) } label: {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: starter.icon)
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(tint)
                    .frame(width: 28, height: 28)
                    .background(tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
                VStack(alignment: .leading, spacing: 5) {
                    Text(starter.label).font(.themed(14, weight: .semibold))
                        .foregroundStyle(theme.textColor).lineLimit(2)
                    Text(starter.hint).font(.themed(12))
                        .foregroundStyle(theme.secondaryTextColor).lineLimit(2)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxWidth: .infinity, minHeight: 64, maxHeight: .infinity, alignment: .topLeading)
            .padding(12)
            .background(theme.cardColor, in: RoundedRectangle(cornerRadius: 16))
            .overlay(RoundedRectangle(cornerRadius: 16).stroke(theme.cardBorderColor, lineWidth: 1))
            .contentShape(RoundedRectangle(cornerRadius: 16))
        }
        .buttonStyle(.plain).disabled(!enabled)
        .accessibilityLabel(starter.label + ". " + starter.prompt)
        .accessibilityHint("Fills the message box without sending")
        .accessibilityIdentifier("chat-starter-" + starter.id)
    }

    private func relativeTime(_ timestamp: Int) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(for: Date(timeIntervalSince1970: Double(timestamp) / 1000), relativeTo: Date())
    }
}

/// The divider before the first message of each local day.
private struct ChatDayDivider: View {
    let label: String
    @ObservedObject var theme: ThemeManager

    var body: some View {
        HStack(spacing: 10) {
            Rectangle().fill(theme.cardBorderColor).frame(height: 1)
            Text(label)
                .font(.themed(11, weight: .semibold))
                .foregroundColor(theme.secondaryTextColor)
                .padding(.horizontal, 10)
                .padding(.vertical, 3)
                .background(Capsule().fill(theme.surfaceColor.opacity(0.85)))
                .fixedSize()
            Rectangle().fill(theme.cardBorderColor).frame(height: 1)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityAddTraits(.isHeader)
    }
}

/// Reports whether the transcript is being scrolled (iOS 18 scroll phases).
/// On iOS 17 nothing is reported and the floating date leans on messages
/// appearing and disappearing instead.
private struct TranscriptScrollPhaseObserver: ViewModifier {
    let onChange: (Bool) -> Void

    func body(content: Content) -> some View {
        if #available(iOS 18.0, *) {
            content.onScrollPhaseChange { _, phase in
                onChange(phase.isScrolling)
            }
        } else {
            content
        }
    }
}


// Display-only extraction of the generated leading evidence-check caveat.
// Never remove numbers or warnings elsewhere in an answer or edit stored text.
struct AnswerVerificationNote: Equatable {
    let details: String?
    static let explanation = "Some parts of this answer could not be confirmed against the available sources."
}

struct AnswerVerificationDisplay: Equatable {
    let body: String
    let verification: AnswerVerificationNote?
    private static let prefix = try? NSRegularExpression(pattern: #"^\s*(?:>\s*)?⚠\uFE0F?\s*(?:\*\*)?Not fully verified\.(?:\*\*)?[ \t]*"#)

    static func split(_ text: String) -> Self {
        guard let match = prefix?.firstMatch(in: text, range: NSRange(text.startIndex..<text.endIndex, in: text)),
              let marker = Range(match.range, in: text) else {
            return Self(body: text, verification: nil)
        }
        let rest = String(text[marker.upperBound...]).replacingOccurrences(of: "\r\n", with: "\n")
        let diagnostic = #"^(?:Magician|Magican)['’]s evidence check could not confirm part of this answer:[ \t]*"#
        guard let diagnosticRange = rest.range(of: diagnostic, options: .regularExpression) else {
            return Self(body: rest.trimmingCharacters(in: .whitespacesAndNewlines), verification: AnswerVerificationNote(details: nil))
        }
        let separator = rest.range(of: #"\r?\n[ \t]*\r?\n"#, options: .regularExpression)
        let raw = String(rest[diagnosticRange.upperBound..<(separator?.lowerBound ?? rest.endIndex)])
        let details = raw.replacingOccurrences(of: #"\\([\\`*{}\[\]()#+.!_>~-])"#, with: "$1", options: .regularExpression).trimmingCharacters(in: .whitespacesAndNewlines)
        let body = separator.map { String(rest[$0.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines) } ?? ""
        return Self(body: body, verification: AnswerVerificationNote(details: details.isEmpty ? nil : details))
    }
}

struct AnswerVerificationInfo: View {
    let note: AnswerVerificationNote
    @ObservedObject var theme: ThemeManager
    @State private var showing = false

    var body: some View {
        Button { showing.toggle() } label: {
            Image(systemName: "info.circle")
                .font(.themed(15))
                .foregroundColor(theme.warningColor)
                .frame(width: 44, height: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Not fully verified — view details")
        .accessibilityHint("Shows what could not be confirmed without changing the answer.")
        .popover(isPresented: $showing) {
            VStack(alignment: .leading, spacing: 10) {
                Text("Not fully verified").font(.themed(16, weight: .semibold))
                Text(note.details ?? AnswerVerificationNote.explanation)
                    .font(.themed(14)).textSelection(.enabled)
                Button("Done") { showing = false }
                    .buttonStyle(.bordered).buttonBorderShape(.roundedRectangle(radius: 8))
                    .font(.themed(14)).tint(theme.accentColor)
            }
            .foregroundColor(theme.textColor).padding(16)
            .frame(idealWidth: 300, maxWidth: 340)
            .fixedSize(horizontal: false, vertical: true)
            .presentationBackground(theme.cardColor)
            .presentationCompactAdaptation(.popover)
        }
    }
}
