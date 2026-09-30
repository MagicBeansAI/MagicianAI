import SwiftUI
import UIKit

/// One-shot presentation handoff used by the Chat `@brainstorm` feature lane.
/// The map itself remains local-first; only its bounded frontier requests use
/// the dedicated Magician facilitator.
@MainActor
final class ThinkingMapRouter: ObservableObject {
    static let shared = ThinkingMapRouter()

    /// Where a presented seed should land: a NEW map (default — `@brainstorm`
    /// + the share sheet's "Start Thinking Map") or APPENDED to the
    /// most-recent non-archived map (the share sheet's "Add to current
    /// Thinking Map", `dest = thinking_map_append`).
    enum SeedDisposition {
        case newMap
        case appendToRecent
    }

    struct Request: Identifiable {
        let id = UUID()
        let initialThought: String?
        /// Optional detail markdown for the seed node — the Share ingestion
        /// path carries the FULL shared text + a provenance line here while
        /// `initialThought` stays a bounded label.
        let initialDetail: String?
        let disposition: SeedDisposition
    }

    @Published var request: Request?

    private init() {}

    func present(
        initialThought: String,
        detail: String? = nil,
        disposition: SeedDisposition = .newMap
    ) {
        let seed = initialThought.trimmingCharacters(in: .whitespacesAndNewlines)
        // Empty is meaningful: `@brainstorm` with no trailing text opens the
        // zero-ceremony capture instead of dropping the user in the library.
        request = Request(initialThought: seed, initialDetail: detail, disposition: disposition)
    }
}

private enum ThinkingMapSurface {
    case library
    case workspace
}

private enum ThinkingMapLibraryFilter: String, CaseIterable, Identifiable {
    case ideas = "Ideas"
    case pinned = "Pinned"
    case archived = "Archive"
    var id: String { rawValue }
}

/// A voice-first, local thinking canvas. Magician adds graph-aware frontier
/// suggestions when reachable; capture, navigation, and persistence keep working
/// locally in every app configuration.
struct ThinkingMapView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var theme = ThemeManager.shared
    @StateObject private var model = ThinkingMapModel.shared
    @StateObject private var dictation = DictationController()
    @StateObject private var intelligence = ThinkingMapIntelligenceController()
    /// Ambient "Listen" mode drives a hands-free realtime voice call; its media
    /// session id is attached to the open map so spoken turns auto-map. Owned by
    /// the view (audio lifecycle); the model only gets the session id.
    @StateObject private var listenVoice = RealtimeVoiceClient()

    @State private var mode: ThinkingMapMode = .map
    @State private var composer = ""
    @State private var composerKind: ThinkingNodeKind = .idea
    @State private var selectedNode: ThinkingNodeSelection?
    @State private var showHarvest = false
    @State private var surface: ThinkingMapSurface = .library
    @State private var libraryFilter: ThinkingMapLibraryFilter = .ideas
    @State private var librarySearch = ""
    @State private var selectedLibraryMap: ThinkingMapRecord?
    @State private var didApplyLaunchControl = false
    @State private var isReorganizing = false
    /// True while the hands-free voice call is connecting (before it goes live
    /// and the map is attached) — drives the "Starting…" affordance.
    @State private var isActivatingListen = false
    @FocusState private var composerFocused: Bool

    private let initialThought: String?
    private let initialDetail: String?
    private let seedDisposition: ThinkingMapRouter.SeedDisposition

    init(
        initialThought: String? = nil,
        initialDetail: String? = nil,
        seedDisposition: ThinkingMapRouter.SeedDisposition = .newMap
    ) {
        self.initialThought = initialThought
        self.initialDetail = initialDetail
        self.seedDisposition = seedDisposition
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if surface == .library {
                    libraryView
                } else if model.hasMap {
                    mapHeader
                    modePicker
                    Group {
                        switch mode {
                        case .map: graphView
                        case .focus: focusView
                        case .outline: outlineView
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    listenBanner
                    reorganizeBanner
                    thinkingComposer
                } else {
                    welcomeView
                }
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .foregroundColor(theme.textColor)
            .navigationTitle(surface == .library ? "Ideas" : (model.hasMap ? model.title : "New idea"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { navigationToolbar }
            .sheet(item: $selectedNode) { selection in
                ThinkingNodeDetailSheet(model: model, nodeID: selection.id)
            }
            .sheet(isPresented: $showHarvest) {
                ThinkingHarvestSheet(model: model)
            }
            .sheet(item: $selectedLibraryMap) { record in
                ThinkingMapLibraryActionsSheet(
                    model: model,
                    recordID: record.id,
                    onOpen: { openMap($0) },
                    onDuplicate: { openMap($0) },
                    onDeleted: { surface = .library }
                )
            }
            .alert(item: $dictation.permissionAlert) { alert in
                Alert(
                    title: Text(alert.title),
                    message: Text(alert.message),
                    primaryButton: .default(Text("Open Settings")) { DictationController.openSettings() },
                    secondaryButton: .cancel()
                )
            }
            .onAppear { applyLaunchControlOnce() }
            // Entering Focus with nothing selected (e.g. a freshly-opened map)
            // would render an empty view — its content is gated on an active
            // node. Default the focus to a sensible node so Focus is never blank.
            .onChange(of: mode) { _, newMode in
                if newMode == .focus { model.focusDefaultIfNeeded() }
            }
            .onDisappear {
                finishDictationIfNeeded(submit: false)
                // Leave ambient Listen mode + hang up the voice call when the
                // map view goes away, so nothing keeps mapping / holding the mic.
                if model.isListening || isActivatingListen { stopListening() }
            }
            // If the voice call drops (failed / ended by the server) while we
            // think we're listening, leave Listen mode so the UI stays honest.
            .onChange(of: listenVoice.phase) { _, phase in
                if (phase == .failed || phase == .ended), model.isListening {
                    stopListening()
                }
            }
        }
        .preferredColorScheme(theme.colorScheme)
    }

    @ToolbarContentBuilder
    private var navigationToolbar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            if surface == .library {
                Button("Close") { finishDictationIfNeeded(submit: false); dismiss() }
            } else {
                Button {
                    finishDictationIfNeeded(submit: false)
                    composer = ""
                    surface = .library
                } label: {
                    Label("Ideas", systemImage: "chevron.left")
                }
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            if surface == .library {
                Button { startNewMap() } label: {
                    Image(systemName: "plus")
                }
                .accessibilityLabel("New idea")
                .accessibilityIdentifier("thinking-map-new")
            } else {
                Menu {
                    if model.hasMap {
                        Button { toggleListening() } label: {
                            Label(
                                model.isListening ? "Stop listening" : "Listen (build as you talk)",
                                systemImage: model.isListening ? "stop.circle" : "dot.radiowaves.left.and.right")
                        }
                        .disabled(isActivatingListen)
                        .accessibilityIdentifier("thinking-map-listen-toggle")
                        Button { reorganize() } label: {
                            Label("Reorganize", systemImage: "arrow.triangle.2.circlepath")
                        }
                        .disabled(isReorganizing)
                        Button { showHarvest = true } label: {
                            Label("Harvest", systemImage: "square.and.arrow.up")
                        }
                        if let record = model.openRecord {
                            Button { selectedLibraryMap = record } label: {
                                Label("Manage idea", systemImage: "slider.horizontal.3")
                            }
                        }
                        Divider()
                    }
                    Button { startNewMap() } label: {
                        Label("New idea", systemImage: "plus.rectangle.on.rectangle")
                    }
                    Button { loadExample() } label: {
                        Label("Load guided example", systemImage: "sparkles")
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel("Thinking Map actions")
            }
        }
    }

    private var filteredLibraryMaps: [ThinkingMapRecord] {
        model.libraryMaps.filter { record in
            let belongs: Bool
            switch libraryFilter {
            case .ideas: belongs = !record.isArchived
            case .pinned: belongs = record.isPinned && !record.isArchived
            case .archived: belongs = record.isArchived
            }
            return belongs && record.matches(librarySearch)
        }
    }

    private var mostRecentMap: ThinkingMapRecord? {
        model.libraryMaps.first { !$0.isArchived }
    }

    private var libraryView: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 20) {
                libraryCaptureCard

                if librarySearch.isEmpty, libraryFilter == .ideas, let recent = mostRecentMap {
                    continueCard(recent)
                }

                VStack(spacing: 12) {
                    HStack(spacing: 9) {
                        Image(systemName: "magnifyingglass")
                            .foregroundColor(theme.secondaryTextColor)
                        TextField("Search every thought", text: $librarySearch)
                            .font(.themed(14))
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled(false)
                            .accessibilityIdentifier("thinking-map-search")
                        if !librarySearch.isEmpty {
                            Button { librarySearch = "" } label: {
                                Image(systemName: "xmark.circle.fill")
                            }
                            .buttonStyle(.plain)
                            .foregroundColor(theme.secondaryTextColor)
                            .accessibilityLabel("Clear search")
                        }
                    }
                    .padding(.horizontal, 14)
                    .frame(height: 46)
                    .background(theme.surfaceColor)
                    .clipShape(RoundedRectangle(cornerRadius: 15))

                    HStack(spacing: 8) {
                        ForEach(ThinkingMapLibraryFilter.allCases) { filter in
                            Button {
                                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) {
                                    libraryFilter = filter
                                }
                            } label: {
                                HStack(spacing: 6) {
                                    if filter == .pinned { Image(systemName: "pin.fill") }
                                    if filter == .archived { Image(systemName: "archivebox.fill") }
                                    Text(filter.rawValue)
                                }
                                .font(.themed(11, weight: .semibold))
                                .foregroundColor(libraryFilter == filter ? theme.onAccentColor : theme.secondaryTextColor)
                                .padding(.horizontal, 13)
                                .padding(.vertical, 8)
                                .background(libraryFilter == filter ? theme.accentColor : theme.elevatedColor)
                                .clipShape(Capsule())
                            }
                            .buttonStyle(.plain)
                        }
                        Spacer()
                        Text("\(filteredLibraryMaps.count)")
                            .font(.themed(11, weight: .bold))
                            .foregroundColor(theme.secondaryTextColor)
                    }
                    .accessibilityIdentifier("thinking-map-library-filters")
                }

                if filteredLibraryMaps.isEmpty {
                    libraryEmptyState
                } else {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(libraryFilter == .archived ? "PUT AWAY, NOT LOST" : "YOUR THINKING")
                            .thinkingEyebrow(theme)
                        ForEach(filteredLibraryMaps) { libraryCard($0) }
                    }
                }

                Label("Private on this iPhone · every change saves automatically", systemImage: "lock.shield")
                    .font(.themed(10, weight: .medium))
                    .foregroundColor(theme.secondaryTextColor)
                    .frame(maxWidth: .infinity)
                    .padding(.top, 2)
                    .padding(.bottom, 28)
            }
            .padding(.horizontal, 18)
            .padding(.top, 8)
        }
        .accessibilityIdentifier("thinking-map-library")
    }

    private var libraryCaptureCard: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Label("QUICK CAPTURE", systemImage: "waveform")
                    .font(.themed(9, weight: .bold))
                    .tracking(1)
                    .foregroundColor(theme.accentColor)
                Spacer()
                Text(model.maps.isEmpty ? "YOUR FIRST MAP" : "A FRESH BRANCH OF THOUGHT")
                    .font(.themed(8, weight: .bold))
                    .foregroundColor(theme.secondaryTextColor)
            }

            Text(dictation.isRecording ? "Keep going. It can be messy." : "Catch the thought before it behaves.")
                .font(.themed(26, weight: .bold))
                .fixedSize(horizontal: false, vertical: true)

            Text(dictation.isRecording
                 ? "Tap stop when the thought is out. A new map will take shape around it."
                 : "Speak or type one unfinished thought. You can decide where it goes after it exists.")
                .font(.themed(13))
                .foregroundColor(theme.secondaryTextColor)

            if dictation.isRecording || !dictation.partialTranscript.isEmpty {
                ghostSpeech(text: dictation.partialTranscript.isEmpty ? "Listening…" : dictation.partialTranscript)
            }

            HStack(alignment: .bottom, spacing: 10) {
                Button { toggleDictation() } label: {
                    Image(systemName: dictation.isRecording ? "stop.fill" : "mic.fill")
                        .font(.system(size: 18, weight: .bold))
                        .foregroundColor(theme.contrastingTextColor(
                            for: dictation.isRecording ? theme.dangerColor : theme.accentColor
                        ))
                        .frame(width: 52, height: 52)
                        .background(dictation.isRecording ? theme.dangerColor : theme.accentColor)
                        .clipShape(Circle())
                        .shadow(color: (dictation.isRecording ? theme.dangerColor : theme.accentColor).opacity(0.22), radius: 12, y: 5)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("thinking-map-library-microphone")
                .accessibilityLabel(dictation.isRecording ? "Finish new idea" : "Speak a new idea")

                TextField("Say the messy version…", text: $composer, axis: .vertical)
                    .lineLimit(1...4)
                    .font(.themed(14))
                    .padding(.horizontal, 13)
                    .padding(.vertical, 14)
                    .background(theme.elevatedColor)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                    .focused($composerFocused)
                    .submitLabel(.done)
                    .onSubmit { beginMapFromComposer() }
                    .accessibilityIdentifier("thinking-map-new-composer")

                if !composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Button { beginMapFromComposer() } label: {
                        Image(systemName: "arrow.up.right")
                            .font(.system(size: 15, weight: .bold))
                            .foregroundColor(theme.onAccentColor)
                            .frame(width: 48, height: 48)
                            .background(theme.accentColor)
                            .clipShape(RoundedRectangle(cornerRadius: 16))
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("thinking-map-create")
                }
            }
        }
        .padding(18)
        .background {
            RoundedRectangle(cornerRadius: 24)
                .fill(theme.surfaceColor)
                .overlay(alignment: .topTrailing) {
                    Circle()
                        .fill(theme.accentColor.opacity(0.09))
                        .frame(width: 150, height: 150)
                        .offset(x: 48, y: -58)
                }
        }
        .clipShape(RoundedRectangle(cornerRadius: 24))
        .overlay(RoundedRectangle(cornerRadius: 24).stroke(theme.accentColor.opacity(0.18)))
    }

    private func continueCard(_ record: ThinkingMapRecord) -> some View {
        Button { openMap(record.id) } label: {
            HStack(spacing: 14) {
                ThinkingMapMiniGraph(record: record)
                    .frame(width: 74, height: 74)
                    .background(theme.elevatedColor)
                    .clipShape(RoundedRectangle(cornerRadius: 18))

                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        Image(systemName: "scope")
                        Text("CONTINUE WHERE YOU LEFT OFF")
                    }
                    .font(.themed(8, weight: .bold))
                    .tracking(0.7)
                    .foregroundColor(theme.accentColor)
                    Text(record.title)
                        .font(.themed(17, weight: .bold))
                        .foregroundColor(theme.textColor)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                    Text(record.activeThought)
                        .font(.themed(11))
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                Image(systemName: "arrow.right.circle.fill")
                    .font(.title2)
                    .foregroundColor(theme.accentColor)
            }
            .padding(14)
            .background(theme.accentColor.opacity(0.08))
            .clipShape(RoundedRectangle(cornerRadius: 20))
            .overlay(RoundedRectangle(cornerRadius: 20).stroke(theme.accentColor.opacity(0.2)))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("thinking-map-continue")
    }

    private func libraryCard(_ record: ThinkingMapRecord) -> some View {
        VStack(spacing: 0) {
            Button { openMap(record.id) } label: {
                HStack(alignment: .top, spacing: 14) {
                    ThinkingMapMiniGraph(record: record)
                        .frame(width: 84, height: 84)
                        .background(theme.elevatedColor)
                        .clipShape(RoundedRectangle(cornerRadius: 19))

                    VStack(alignment: .leading, spacing: 7) {
                        HStack(spacing: 6) {
                            if record.isPinned {
                                Image(systemName: "pin.fill")
                                    .foregroundColor(theme.accentColor)
                            }
                            if record.isArchived {
                                Text("ARCHIVED")
                                    .font(.themed(8, weight: .bold))
                                    .foregroundColor(theme.secondaryTextColor)
                            }
                            Spacer()
                            Text(record.updatedAt.formatted(.relative(presentation: .named)))
                                .font(.themed(9))
                                .foregroundColor(theme.secondaryTextColor)
                        }
                        Text(record.title)
                            .font(.themed(17, weight: .bold))
                            .foregroundColor(theme.textColor)
                            .multilineTextAlignment(.leading)
                            .lineLimit(2)
                        Text(record.activeThought)
                            .font(.themed(11))
                            .foregroundColor(theme.secondaryTextColor)
                            .multilineTextAlignment(.leading)
                            .lineLimit(2)
                    }
                    Spacer(minLength: 0)
                }
                .padding(14)
            }
            .buttonStyle(.plain)

            Divider().opacity(0.45)

            HStack(spacing: 14) {
                Label("\(record.nodeCount) thoughts", systemImage: "point.3.connected.trianglepath.dotted")
                if record.questionCount > 0 {
                    Label("\(record.questionCount)", systemImage: ThinkingNodeKind.question.icon)
                }
                if record.actionCount > 0 {
                    Label("\(record.actionCount)", systemImage: ThinkingNodeKind.action.icon)
                }
                Spacer()
                Button { model.togglePinned(record.id) } label: {
                    Image(systemName: record.isPinned ? "pin.slash.fill" : "pin.fill")
                        .frame(width: 30, height: 30)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(record.isPinned ? "Unpin \(record.title)" : "Pin \(record.title)")
                Button { selectedLibraryMap = record } label: {
                    Image(systemName: "ellipsis")
                        .frame(width: 30, height: 30)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Manage \(record.title)")
            }
            .font(.themed(9, weight: .semibold))
            .foregroundColor(theme.secondaryTextColor)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
        }
        .background(theme.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 20))
        .overlay(RoundedRectangle(cornerRadius: 20).stroke(theme.secondaryTextColor.opacity(0.1)))
        .accessibilityIdentifier("thinking-map-library-card-\(record.id.uuidString)")
        .contextMenu {
            Button { openMap(record.id) } label: { Label("Continue", systemImage: "scope") }
            Button { model.togglePinned(record.id) } label: {
                Label(record.isPinned ? "Unpin" : "Pin", systemImage: record.isPinned ? "pin.slash" : "pin")
            }
            Button { selectedLibraryMap = record } label: { Label("More actions", systemImage: "ellipsis.circle") }
        }
    }

    private var libraryEmptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: libraryFilter == .archived ? "archivebox" : (libraryFilter == .pinned ? "pin" : "sparkles"))
                .font(.system(size: 28, weight: .light))
                .foregroundColor(theme.accentColor)
            Text(librarySearch.isEmpty
                 ? (libraryFilter == .archived ? "Nothing archived" : (libraryFilter == .pinned ? "Pin the ideas you keep returning to" : "Your next idea starts above"))
                 : "No thought matches that search")
                .font(.themed(16, weight: .bold))
                .multilineTextAlignment(.center)
            if !librarySearch.isEmpty {
                Button("Clear search") { librarySearch = "" }
                    .buttonStyle(.bordered)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 38)
        .padding(.horizontal, 20)
        .background(theme.surfaceColor.opacity(0.55))
        .clipShape(RoundedRectangle(cornerRadius: 20))
    }

    private var mapHeader: some View {
        VStack(spacing: 9) {
            HStack(spacing: 8) {
                Circle()
                    .fill(dictation.isRecording ? theme.dangerColor : theme.accentColor)
                    .frame(width: 7, height: 7)
                Text(dictation.isRecording ? "LISTENING" : "LIVE MAP")
                    .font(.themed(10, weight: .bold))
                    .tracking(1.2)
                Text("ON THIS IPHONE")
                    .font(.themed(9, weight: .bold))
                    .tracking(0.8)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                    .background(theme.elevatedColor)
                    .clipShape(Capsule())
                Spacer()
                Text("\(model.nodes.count) thoughts")
                    .font(.themed(11, weight: .semibold))
            }
            .foregroundColor(theme.secondaryTextColor)

            HStack {
                metric(.question)
                metric(.risk)
                metric(.decision)
                metric(.action)
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 9)
    }

    private func metric(_ kind: ThinkingNodeKind) -> some View {
        VStack(spacing: 2) {
            Text("\(model.nodes.filter { $0.kind == kind }.count)")
                .font(.themed(16, weight: .bold))
                .foregroundColor(kind.color)
            Text(kind.rawValue)
                .font(.themed(9))
                .foregroundColor(theme.secondaryTextColor)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(model.nodes.filter { $0.kind == kind }.count) \(kind.rawValue.lowercased())")
    }

    private var modePicker: some View {
        Picker("View", selection: Binding(
            get: { mode },
            set: { newMode in
                mode = newMode
                model.setPreferredMode(newMode)
            }
        )) {
            ForEach(ThinkingMapMode.allCases) { mode in
                Label(mode.label, systemImage: mode.icon).tag(mode)
            }
        }
        .pickerStyle(.segmented)
        .accessibilityIdentifier("thinking-map-mode-picker")
        .padding(.horizontal, 18)
        .padding(.bottom, 10)
    }

    private var welcomeView: some View {
        ScrollView {
            VStack(spacing: 24) {
                Spacer(minLength: 28)
                ZStack {
                    Circle()
                        .fill(theme.accentColor.opacity(dictation.isRecording ? 0.18 : 0.08))
                        .frame(width: 150, height: 150)
                    Circle()
                        .stroke(theme.accentColor.opacity(dictation.isRecording ? 0.72 : 0.24), lineWidth: 2)
                        .frame(width: dictation.isRecording ? 118 : 104, height: dictation.isRecording ? 118 : 104)
                    Button { toggleDictation() } label: {
                        Image(systemName: dictation.isRecording ? "stop.fill" : "waveform.and.mic")
                            .font(.system(size: 36, weight: .semibold))
                            .foregroundColor(dictation.isRecording ? theme.contrastingTextColor(for: theme.dangerColor) : theme.accentColor)
                            .frame(width: 84, height: 84)
                            .background(dictation.isRecording ? theme.dangerColor : theme.surfaceColor)
                            .clipShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(dictation.isRecording ? "Finish thought" : "Speak an idea")
                }
                .animation(reduceMotion ? nil : .spring(response: 0.35), value: dictation.isRecording)

                VStack(spacing: 9) {
                    Text(dictation.isRecording ? "I’m listening" : "Start with the messy version")
                        .font(.themed(29, weight: .bold))
                        .multilineTextAlignment(.center)
                    Text(dictation.isRecording
                         ? "Tap stop when the thought is out. It does not need to be polished."
                         : "Say what is on your mind. Choose a branch, keep talking, and watch the idea find its shape.")
                        .font(.themed(15))
                        .foregroundColor(theme.secondaryTextColor)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 18)
                }

                if dictation.isRecording || !dictation.partialTranscript.isEmpty {
                    ghostSpeech(text: dictation.partialTranscript.isEmpty ? "Listening…" : dictation.partialTranscript)
                }

                VStack(spacing: 11) {
                    TextField("Or type the thought here…", text: $composer, axis: .vertical)
                        .lineLimit(2...5)
                        .font(.themed(15))
                        .padding(15)
                        .background(theme.surfaceColor)
                        .clipShape(RoundedRectangle(cornerRadius: 17))
                        .overlay(RoundedRectangle(cornerRadius: 17).stroke(theme.secondaryTextColor.opacity(0.15)))
                        .focused($composerFocused)

                    Button { beginMapFromComposer() } label: {
                        Label("Begin the map", systemImage: "arrow.up.forward.circle.fill")
                            .font(.themed(15, weight: .bold))
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 13)
                    }
                    .buttonStyle(.borderedProminent)
                    .foregroundColor(theme.onAccentColor)
                    .accessibilityIdentifier("thinking-map-begin")
                    .disabled(composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }

                VStack(alignment: .leading, spacing: 10) {
                    Text("A PLACE TO BEGIN")
                        .thinkingEyebrow(theme)
                    starter("Shape a product idea", icon: "lightbulb.max.fill")
                    starter("Think through a difficult decision", icon: "arrow.triangle.branch")
                    starter("Turn a vague ambition into an experiment", icon: "flask.fill")
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                Button { loadExample() } label: {
                    Label("Try an interactive example", systemImage: "sparkles")
                }
                .buttonStyle(.bordered)

                Label("The map is saved locally on this iPhone", systemImage: "iphone.gen3")
                    .font(.themed(10, weight: .medium))
                    .foregroundColor(theme.secondaryTextColor)
                Spacer(minLength: 30)
            }
            .padding(.horizontal, 20)
        }
        .accessibilityIdentifier("thinking-map-welcome")
    }

    private func starter(_ text: String, icon: String) -> some View {
        Button {
            composer = text
            composerFocused = true
        } label: {
            HStack(spacing: 12) {
                Image(systemName: icon)
                    .foregroundColor(theme.accentColor)
                    .frame(width: 24)
                Text(text)
                    .font(.themed(13, weight: .medium))
                    .foregroundColor(theme.textColor)
                Spacer()
                Image(systemName: "arrow.up.left")
                    .font(.caption)
                    .foregroundColor(theme.secondaryTextColor)
            }
            .padding(13)
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: 14))
        }
        .buttonStyle(.plain)
    }

    private var focusView: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 18) {
                if let suggestion = model.pendingConnection {
                    connectionSuggestion(suggestion)
                }

                if !model.clarifications.isEmpty {
                    clarificationsSection
                }

                if dictation.isRecording || !dictation.partialTranscript.isEmpty {
                    ghostSpeech(text: dictation.partialTranscript.isEmpty ? "Listening from this branch…" : dictation.partialTranscript)
                }

                if model.ancestry.count > 1 {
                    breadcrumbTrail
                }

                // Resolve to a sensible default when nothing is explicitly
                // active (e.g. a map whose nodes just loaded before a selection
                // publishes) so Focus renders content instead of a blank view.
                if let active = model.defaultFocusNode {
                    Text(active.suggested ? "A DIRECTION TO EXPLORE" : "YOU ARE EXPLORING")
                        .thinkingEyebrow(theme)
                    activeNodeCard(active)

                    let children = model.children(of: active.id)
                    if !children.isEmpty {
                        HStack {
                            Text("CHOOSE WHERE TO GO NEXT")
                                .thinkingEyebrow(theme)
                            Spacer()
                            Text("\(children.count) branches")
                                .font(.themed(10))
                                .foregroundColor(theme.secondaryTextColor)
                        }
                        ForEach(children) { branchCard($0) }
                    }

                    Text("OPEN ANOTHER ANGLE")
                        .thinkingEyebrow(theme)
                    promptPalette

                    let related = model.relatedNodes(to: active.id)
                    if !related.isEmpty {
                        Text("CONNECTED ELSEWHERE")
                            .thinkingEyebrow(theme)
                        ForEach(related) { relatedCard($0) }
                    }
                }
            }
            .id(model.activeNodeID)
            .padding(.horizontal, 18)
            .padding(.bottom, 28)
        }
    }

    private var breadcrumbTrail: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 7) {
                ForEach(Array(model.ancestry.enumerated()), id: \.element.id) { index, node in
                    if index > 0 {
                        Image(systemName: "chevron.right")
                            .font(.caption2)
                            .foregroundColor(theme.secondaryTextColor)
                    }
                    Button {
                        model.select(node.id)
                    } label: {
                        Text(node.title)
                            .font(.themed(11, weight: node.id == model.activeNodeID ? .bold : .medium))
                            .lineLimit(1)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 7)
                            .background(node.id == model.activeNodeID ? node.kind.color.opacity(0.14) : theme.elevatedColor)
                            .clipShape(Capsule())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .defaultScrollAnchor(.trailing)
        .accessibilityLabel("Current branch path")
    }

    private func activeNodeCard(_ node: ThinkingNode) -> some View {
        Button { selectedNode = .init(id: node.id) } label: {
            VStack(alignment: .leading, spacing: 13) {
                HStack {
                    Label(node.kind.rawValue.uppercased(), systemImage: node.kind.icon)
                        .font(.themed(9, weight: .bold))
                        .foregroundColor(node.kind.color)
                    if hasClarification(node.id) {
                        clarificationChip
                    }
                    Spacer()
                    Text(node.suggested ? "SUGGESTED" : "CAPTURED")
                        .font(.themed(9, weight: .bold))
                        .foregroundColor(node.kind.color)
                }
                Text(node.title)
                    .font(.themed(23, weight: .bold))
                    .fixedSize(horizontal: false, vertical: true)
                    .foregroundColor(theme.textColor)
                if !node.detail.isEmpty, node.detail != node.title {
                    Text(node.detail)
                        .font(.themed(14))
                        .foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
                HStack {
                    Label(node.suggested ? "Answer or reshape this branch" : "Continue speaking from here", systemImage: "waveform")
                        .font(.themed(10, weight: .semibold))
                        .foregroundColor(theme.accentColor)
                    Spacer()
                    Image(systemName: "info.circle")
                        .foregroundColor(theme.secondaryTextColor)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(18)
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: 22))
            .overlay {
                RoundedRectangle(cornerRadius: 22)
                    .stroke(node.kind.color.opacity(0.5), style: StrokeStyle(lineWidth: 1.4, dash: node.suggested ? [6, 5] : []))
            }
            .shadow(color: node.kind.color.opacity(0.08), radius: 14, y: 4)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("thinking-map-active-node")
        .accessibilityLabel("Active \(node.kind.rawValue): \(node.title)")
        .accessibilityHint("Shows details and connection controls")
    }

    private func branchCard(_ node: ThinkingNode) -> some View {
        Button {
            model.select(node.id)
            impact(.light)
        } label: {
            HStack(spacing: 13) {
                RoundedRectangle(cornerRadius: 3)
                    .fill(node.kind.color)
                    .frame(width: 4, height: 56)
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 6) {
                        Label(node.kind.rawValue.uppercased(), systemImage: node.kind.icon)
                            .font(.themed(9, weight: .bold))
                            .foregroundColor(node.kind.color)
                        if node.suggested {
                            Text("SUGGESTED")
                                .font(.themed(8, weight: .bold))
                                .foregroundColor(theme.secondaryTextColor)
                        }
                    }
                    Text(node.title)
                        .font(.themed(15, weight: .semibold))
                        .foregroundColor(theme.textColor)
                        .multilineTextAlignment(.leading)
                        .lineLimit(3)
                }
                Spacer(minLength: 8)
                Image(systemName: "arrow.up.forward")
                    .font(.caption.weight(.bold))
                    .foregroundColor(node.kind.color)
            }
            .padding(14)
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: 17))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("thinking-map-branch-\(node.id.uuidString)")
        .contextMenu {
            Button { model.select(node.id) } label: { Label("Explore here", systemImage: "scope") }
            Button { selectedNode = .init(id: node.id) } label: { Label("Thought details", systemImage: "info.circle") }
        }
    }

    private var promptPalette: some View {
        let fallback = intelligence.fallback
        return VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 7) {
                Image(systemName: intelligence.isThinking ? "sparkles" : (fallback?.icon ?? "wand.and.stars"))
                Text(intelligence.isThinking ? intelligence.progress.label.uppercased() : (fallback?.paletteLabel ?? "NEXT USEFUL MOVES"))
                    .thinkingEyebrow(theme)
                Spacer()
                if fallback?.canRetry == true {
                    Button("Retry") { requestFrontier(.continueThinking, force: true) }
                        .font(.themed(10, weight: .semibold))
                        .accessibilityIdentifier("thinking-map-ai-retry")
                } else {
                    Button("Break this open") { requestFrontier(.breakOpen, force: true) }
                        .font(.themed(10, weight: .semibold))
                        .disabled(intelligence.isThinking)
                }
            }
            .foregroundColor(fallback == nil ? theme.accentColor : theme.warningColor)

            if intelligence.isThinking {
                VStack(alignment: .leading, spacing: 7) {
                    HStack(spacing: 10) {
                        ProgressView().controlSize(.small)
                        Text(intelligence.progress.detail)
                            .font(.themed(11))
                            .foregroundColor(theme.secondaryTextColor)
                    }
                    ForEach(intelligence.activityRows.suffix(3)) { row in
                        Label(row.label, systemImage: row.status == .done ? "checkmark.circle.fill" : "circle.dotted")
                            .font(.themed(10, weight: .medium))
                            .foregroundColor(row.kind == .tool ? theme.warningColor : theme.secondaryTextColor)
                            .lineLimit(1)
                    }
                }
                .frame(minHeight: 48)
            } else {
                if let message = fallback?.message {
                    Label(message, systemImage: "info.circle")
                        .font(.themed(10, weight: .medium))
                        .foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if intelligence.frontier.isEmpty {
                    Text("Keep talking, or break this thought open when you want another perspective.")
                        .font(.themed(11))
                        .foregroundColor(theme.secondaryTextColor)
                        .padding(.vertical, 10)
                } else {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 10) {
                            ForEach(intelligence.frontier) { prompt in
                                Button { chooseFrontier(prompt) } label: {
                                    VStack(alignment: .leading, spacing: 7) {
                                        Image(systemName: prompt.icon)
                                            .foregroundColor(prompt.kind.color)
                                        Text(prompt.title)
                                            .font(.themed(12, weight: .semibold))
                                            .foregroundColor(theme.textColor)
                                            .multilineTextAlignment(.leading)
                                            .lineLimit(2)
                                    }
                                    .frame(width: 136, height: 78, alignment: .topLeading)
                                    .padding(13)
                                    .background(theme.elevatedColor)
                                    .clipShape(RoundedRectangle(cornerRadius: 16))
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                }
            }
        }
    }

    private func relatedCard(_ node: ThinkingNode) -> some View {
        Button { model.select(node.id) } label: {
            HStack(spacing: 11) {
                Image(systemName: "link")
                    .foregroundColor(node.kind.color)
                VStack(alignment: .leading, spacing: 2) {
                    Text(node.title)
                        .font(.themed(13, weight: .semibold))
                        .foregroundColor(theme.textColor)
                    Text("Connected across branches")
                        .font(.themed(9))
                        .foregroundColor(theme.secondaryTextColor)
                }
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.caption)
                    .foregroundColor(theme.secondaryTextColor)
            }
            .padding(13)
            .background(theme.elevatedColor)
            .clipShape(RoundedRectangle(cornerRadius: 14))
        }
        .buttonStyle(.plain)
    }

    private func connectionSuggestion(_ suggestion: ThinkingConnectionSuggestion) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                Label("POSSIBLE CONNECTION", systemImage: "point.3.connected.trianglepath.dotted")
                    .font(.themed(9, weight: .bold))
                    .tracking(0.8)
                    .foregroundColor(theme.accentColor)
                Spacer()
                Button { model.dismissConnectionSuggestion() } label: {
                    Image(systemName: "xmark")
                }
                .buttonStyle(.plain)
                .foregroundColor(theme.secondaryTextColor)
            }
            HStack {
                Text(model.node(suggestion.from)?.title ?? "Thought")
                    .lineLimit(1)
                Image(systemName: "arrow.left.arrow.right")
                Text(model.node(suggestion.to)?.title ?? "Thought")
                    .lineLimit(1)
            }
            .font(.themed(11, weight: .semibold))
            Text(suggestion.reason)
                .font(.themed(11))
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(2)
            HStack(spacing: 10) {
                Button("Connect") { model.acceptConnectionSuggestion(); impact(.medium) }
                    .buttonStyle(.borderedProminent)
                    .foregroundColor(theme.onAccentColor)
                    .controlSize(.small)
                    .accessibilityIdentifier("thinking-map-connect-suggestion")
                Button("Not now") { model.dismissConnectionSuggestion() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
        .padding(14)
        .background(theme.accentColor.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 17))
        .overlay(RoundedRectangle(cornerRadius: 17).stroke(theme.accentColor.opacity(0.24)))
    }

    /// True when the model has an open clarification attached to `nodeID`.
    private func hasClarification(_ nodeID: UUID) -> Bool {
        model.clarifications.contains { $0.nodeID == nodeID }
    }

    /// A small accent "?" chip flagging that the AI is asking about a node.
    private var clarificationChip: some View {
        Label("Question", systemImage: "questionmark.circle.fill")
            .labelStyle(.iconOnly)
            .font(.themed(11, weight: .bold))
            .foregroundColor(theme.accentColor)
            .padding(4)
            .background(theme.accentColor.opacity(0.14))
            .clipShape(Circle())
            .accessibilityLabel("The assistant is asking about this thought")
    }

    /// The open-clarifications list surfaced in the focus view. Each row jumps to
    /// the referenced node's detail sheet (where the answer UI lives).
    private var clarificationsSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("YOUR FACILITATOR IS ASKING", systemImage: "questionmark.bubble.fill")
                    .font(.themed(9, weight: .bold))
                    .tracking(0.8)
                    .foregroundColor(theme.accentColor)
                Spacer()
                Text("\(model.clarifications.count)")
                    .font(.themed(10, weight: .bold))
                    .foregroundColor(theme.secondaryTextColor)
            }
            ForEach(model.clarifications) { clarification in
                Button {
                    if let nodeID = clarification.nodeID, model.node(nodeID) != nil {
                        selectedNode = .init(id: nodeID)
                    }
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "questionmark.circle.fill")
                            .foregroundColor(theme.accentColor)
                        Text(clarification.question)
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(theme.textColor)
                            .multilineTextAlignment(.leading)
                            .lineLimit(3)
                        Spacer(minLength: 6)
                        if clarification.nodeID.flatMap(model.node) != nil {
                            Image(systemName: "chevron.right")
                                .font(.caption)
                                .foregroundColor(theme.secondaryTextColor)
                        }
                    }
                    .padding(13)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(theme.elevatedColor)
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                }
                .buttonStyle(.plain)
                .disabled(clarification.nodeID.flatMap(model.node) == nil)
                .accessibilityIdentifier("thinking-map-clarification-\(clarification.id)")
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.accentColor.opacity(0.06))
        .clipShape(RoundedRectangle(cornerRadius: 17))
        .overlay(RoundedRectangle(cornerRadius: 17).stroke(theme.accentColor.opacity(0.2)))
    }

    private func ghostSpeech(text: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "waveform")
                .foregroundColor(dictation.isRecording ? theme.dangerColor : theme.accentColor)
            Text(text)
                .font(.themed(14, weight: .medium))
                .italic()
                .foregroundColor(theme.textColor)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(15)
        .background(theme.surfaceColor.opacity(0.7))
        .clipShape(RoundedRectangle(cornerRadius: 17))
        .overlay {
            RoundedRectangle(cornerRadius: 17)
                .stroke(theme.accentColor.opacity(0.45), style: StrokeStyle(dash: [4, 5]))
        }
    }

    private var outlineView: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 9) {
                ForEach(model.orderedNodes) { node in
                    Button {
                        model.select(node.id)
                        mode = .focus
                    } label: {
                        HStack(alignment: .top, spacing: 10) {
                            Color.clear.frame(width: CGFloat(model.depth(of: node.id)) * 16)
                            Image(systemName: node.kind.icon)
                                .font(.caption)
                                .foregroundColor(node.kind.color)
                                .frame(width: 20)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(node.title)
                                    .font(.themed(14, weight: node.id == model.activeNodeID ? .bold : .medium))
                                    .foregroundColor(theme.textColor)
                                    .multilineTextAlignment(.leading)
                                Text("\(node.kind.rawValue) · \(node.suggested ? "Suggested" : "Captured")")
                                    .font(.themed(9))
                                    .foregroundColor(theme.secondaryTextColor)
                            }
                            Spacer()
                            if node.id == model.activeNodeID {
                                Image(systemName: "scope")
                                    .foregroundColor(theme.accentColor)
                            }
                        }
                        .padding(12)
                        .background(node.id == model.activeNodeID ? theme.accentColor.opacity(0.08) : theme.surfaceColor)
                        .clipShape(RoundedRectangle(cornerRadius: 14))
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 18)
            .padding(.bottom, 24)
        }
    }

    private var graphView: some View {
        ThinkingGraphView(
            model: model,
            frontier: intelligence.frontier,
            isThinking: intelligence.isThinking,
            progress: intelligence.progress,
            activityRows: intelligence.activityRows,
            fallback: intelligence.fallback,
            onInspect: { selectedNode = .init(id: $0) },
            onChooseFrontier: chooseFrontier,
            onBreakOpen: { requestFrontier(.breakOpen, force: true) },
            onRetry: { requestFrontier(.continueThinking, force: true) }
        )
    }

    /// Ambient "Listen" status strip, shown just above the composer while a
    /// hands-free voice call is connecting or live. Renders the live caption
    /// (from the voice client) so the user can see they're being heard, plus a
    /// Stop control; also surfaces the soft "couldn't start" notice.
    @ViewBuilder
    private var listenBanner: some View {
        if isActivatingListen || model.isListening {
            HStack(spacing: 9) {
                if isActivatingListen && !model.isListening {
                    ProgressView().controlSize(.small)
                    Text("Starting live listening…")
                        .font(.themed(11, weight: .semibold))
                        .foregroundColor(theme.secondaryTextColor)
                } else {
                    Image(systemName: "dot.radiowaves.left.and.right")
                        .foregroundColor(theme.accentColor)
                        .symbolEffect(.variableColor.iterative, options: reduceMotion ? .nonRepeating : .repeating)
                    Text(listenCaption)
                        .font(.themed(11, weight: .medium))
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(2)
                }
                Spacer(minLength: 8)
                Button { stopListening() } label: {
                    Text("Stop").font(.themed(11, weight: .bold))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier("thinking-map-listen-stop")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity)
            .background(theme.accentColor.opacity(0.08))
            .overlay(alignment: .top) { Divider() }
            .accessibilityIdentifier("thinking-map-listen-banner")
        } else if model.listeningUnavailable {
            Label("Couldn't start live listening — needs a connection.", systemImage: "wifi.slash")
                .font(.themed(10, weight: .medium))
                .foregroundColor(theme.warningColor)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .background(theme.surfaceColor)
                .overlay(alignment: .top) { Divider() }
        }
    }

    /// The live status line inside `listenBanner`: the most recent voice caption
    /// if one has arrived, else an invitational prompt.
    private var listenCaption: String {
        if let last = listenVoice.captions.last, !last.text.isEmpty {
            return last.text
        }
        return "Listening — speak and the map builds itself."
    }

    /// Reorganize progress / AI-unavailable note + the pending restructure
    /// proposal card, stacked just above the composer. All state comes from the
    /// model (`pendingProposals` / `aiUnavailable`) plus the local `isReorganizing`
    /// in-progress flag.
    @ViewBuilder
    private var reorganizeBanner: some View {
        if isReorganizing {
            HStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text("Reorganizing this map…")
                    .font(.themed(11, weight: .semibold))
                    .foregroundColor(theme.secondaryTextColor)
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity)
            .background(theme.surfaceColor)
            .overlay(alignment: .top) { Divider() }
        } else if model.aiUnavailable, model.pendingProposals.isEmpty {
            Label("AI unavailable — reorganize needs a connection.", systemImage: "wifi.slash")
                .font(.themed(10, weight: .medium))
                .foregroundColor(theme.warningColor)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .background(theme.surfaceColor)
                .overlay(alignment: .top) { Divider() }
        }

        if let proposal = model.pendingProposals.first {
            proposalCard(proposal)
                .padding(.horizontal, 14)
                .padding(.top, 10)
        }
    }

    /// A dashed, provisional-styled card for a pending restructure proposal — reads
    /// as "proposed, not yet applied" (mirrors the dashed suggested-node border).
    /// Wired only to `model.decideProposal(_:confirm:)`.
    private func proposalCard(_ proposal: ThinkingProposal) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("SUGGESTED REORGANIZATION", systemImage: "arrow.triangle.2.circlepath")
                    .font(.themed(9, weight: .bold))
                    .tracking(0.8)
                    .foregroundColor(theme.accentColor)
                Spacer()
                Text("PROPOSED")
                    .font(.themed(8, weight: .bold))
                    .foregroundColor(theme.secondaryTextColor)
            }

            Text(proposal.rationale.isEmpty ? "Your facilitator suggests reshaping this map." : proposal.rationale)
                .font(.themed(14, weight: .semibold))
                .foregroundColor(theme.textColor)
                .fixedSize(horizontal: false, vertical: true)

            Text("Affects \(proposal.affectedNodeIDs.count) nodes · \(proposal.operationCount) changes")
                .font(.themed(10, weight: .medium))
                .foregroundColor(theme.secondaryTextColor)

            HStack(spacing: 10) {
                Button {
                    Task { await model.decideProposal(proposal.id, confirm: true) }
                    impact(.medium)
                } label: {
                    Label("Confirm", systemImage: "checkmark")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .foregroundColor(theme.onAccentColor)
                .controlSize(.small)
                .accessibilityIdentifier("thinking-map-proposal-confirm")

                Button {
                    Task { await model.decideProposal(proposal.id, confirm: false) }
                } label: {
                    Label("Reject", systemImage: "xmark")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier("thinking-map-proposal-reject")
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.accentColor.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 17))
        .overlay {
            RoundedRectangle(cornerRadius: 17)
                .stroke(theme.accentColor.opacity(0.5), style: StrokeStyle(lineWidth: 1.4, dash: [6, 5]))
        }
        .accessibilityIdentifier("thinking-map-proposal-card")
    }

    private var thinkingComposer: some View {
        VStack(spacing: 9) {
            if let active = model.activeNode {
                HStack(spacing: 6) {
                    Image(systemName: "scope")
                        .foregroundColor(active.kind.color)
                    Text("Continue from")
                        .foregroundColor(theme.secondaryTextColor)
                    Text(active.title)
                        .fontWeight(.semibold)
                        .lineLimit(1)
                    Spacer()
                }
                .font(.themed(10))
            }

            if dictation.isRecording {
                HStack(spacing: 8) {
                    Circle().fill(theme.dangerColor).frame(width: 7, height: 7)
                    Text(dictation.partialTranscript.isEmpty ? "Listening… tap stop when finished" : dictation.partialTranscript)
                        .font(.themed(11, weight: .medium))
                        .lineLimit(2)
                    Spacer()
                }
                .foregroundColor(theme.secondaryTextColor)
            }

            HStack(alignment: .bottom, spacing: 9) {
                Menu {
                    ForEach(ThinkingNodeKind.allCases) { kind in
                        Button { composerKind = kind } label: {
                            Label(kind.rawValue, systemImage: kind.icon)
                        }
                    }
                } label: {
                    Image(systemName: composerKind.icon)
                        .foregroundColor(composerKind.color)
                        .frame(width: 36, height: 42)
                        .background(theme.elevatedColor)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                }

                TextField(composerPlaceholder, text: $composer, axis: .vertical)
                    .lineLimit(1...4)
                    .font(.themed(14))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 11)
                    .background(theme.elevatedColor)
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                    .focused($composerFocused)
                    .accessibilityIdentifier("thinking-map-composer")
                    .submitLabel(.send)
                    .onSubmit { submitComposer() }

                Button { toggleDictation() } label: {
                    Image(systemName: dictation.isRecording ? "stop.fill" : "mic.fill")
                        .font(.system(size: 15, weight: .bold))
                        .foregroundColor(theme.contrastingTextColor(
                            for: dictation.isRecording ? theme.dangerColor : theme.accentColor
                        ))
                        .frame(width: 42, height: 42)
                        .background(dictation.isRecording ? theme.dangerColor : theme.accentColor)
                        .clipShape(Circle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("thinking-map-microphone")

                if !composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Button { submitComposer() } label: {
                        Image(systemName: "arrow.up")
                            .font(.system(size: 15, weight: .bold))
                            .foregroundColor(theme.onAccentColor)
                            .frame(width: 42, height: 42)
                            .background(theme.accentColor)
                            .clipShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("thinking-map-send")
                    .transition(.scale.combined(with: .opacity))
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(theme.surfaceColor)
        .overlay(alignment: .top) { Divider() }
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.18), value: composer.isEmpty)
    }

    private var composerPlaceholder: String {
        guard let active = model.activeNode else { return "Add a thought…" }
        if active.suggested { return active.kind == .question ? "Answer this question…" : "Explore this direction…" }
        return "Keep going, correct it, or branch…"
    }

    private func beginMapFromComposer() {
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        withAnimation(reduceMotion ? nil : .spring(response: 0.45, dampingFraction: 0.86)) {
            // A composer "begin" from the library/welcome creates a fresh map
            // seeded with the text (E0's begin() semantics). `model.begin` alone
            // would seed the CURRENTLY-open map instead — post-migration those
            // are different things.
            model.beginNewMap(with: text)
            composer = ""
            composerKind = .idea
            mode = .map
            surface = .workspace
        }
        requestFrontier(.continueThinking, force: true)
        impact(.medium)
    }

    private func submitComposer() {
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        withAnimation(reduceMotion ? nil : .spring(response: 0.45, dampingFraction: 0.86)) {
            model.addThought(text, preferredKind: composerKind)
            composer = ""
            composerKind = .idea
        }
        requestFrontier(.continueThinking, force: true)
        impact(.light)
    }

    private func toggleDictation() {
        if dictation.isRecording {
            finishDictationIfNeeded(submit: true)
        } else {
            composerFocused = false
            dictation.startLive()
            impact(.medium)
        }
    }

    private func finishDictationIfNeeded(submit: Bool) {
        guard dictation.isRecording else { return }
        dictation.finishLive { transcript in
            guard submit, let transcript, !transcript.isEmpty else { return }
            withAnimation(reduceMotion ? nil : .spring(response: 0.45, dampingFraction: 0.86)) {
                if surface == .workspace, model.hasMap {
                    model.addThought(transcript, preferredKind: nil)
                } else {
                    model.begin(with: transcript)
                    mode = .map
                    surface = .workspace
                }
            }
            requestFrontier(.continueThinking, force: true)
            impact(.medium)
        }
    }

    private func applyLaunchControlOnce() {
        guard !didApplyLaunchControl else { return }
        didApplyLaunchControl = true
        if let initialThought {
            let seed = initialThought.trimmingCharacters(in: .whitespacesAndNewlines)
            if seed.isEmpty {
                startNewMap()
                return
            }
            // One sequential create→seed flow — the old startNewMap()+begin()
            // pair raced the seed onto the previous map (two independent async
            // Tasks against a re-pointing backend). Share ingestion passes a
            // provenance-prefixed detail for the seed node; its "Add to
            // current Thinking Map" variant appends to the most-recent
            // non-archived map instead of creating one.
            if seedDisposition == .appendToRecent {
                model.appendToMostRecentMap(seed, detail: initialDetail)
            } else {
                model.beginNewMap(with: seed, detail: initialDetail)
            }
            composer = ""
            composerKind = .idea
            mode = .map
            surface = .workspace
            selectedLibraryMap = nil
            requestFrontier(.continueThinking, force: true)
            return
        }
        let arguments = ProcessInfo.processInfo.arguments
        if arguments.contains("--thinking-map-empty") {
            startNewMap()
        } else if arguments.contains("--thinking-map-demo")
            || arguments.contains("--thinking-map-prototype-autoplay")
            || arguments.contains(where: { $0.hasPrefix("--thinking-map-prototype-step=") }) {
            loadExample()
        }
    }

    private func openMap(_ id: UUID) {
        model.openMap(id)
        mode = model.preferredMode
        composer = ""
        composerKind = .idea
        surface = .workspace
        selectedLibraryMap = nil
        requestFrontier(.continueThinking, force: false)
        impact(.light)
    }

    private func startNewMap() {
        finishDictationIfNeeded(submit: false)
        model.startNewMap()
        composer = ""
        composerKind = .idea
        mode = .map
        surface = .workspace
        selectedLibraryMap = nil
        intelligence.clear()
    }

    private func loadExample() {
        let seeding = model.loadExample()
        mode = .map
        surface = .workspace
        selectedLibraryMap = nil
        // Refresh the frontier AFTER the async seed lands — before it, there is
        // no active node and the refresh guard just clears. (E0's seed was
        // synchronous, which made the immediate refresh work by accident.)
        Task { @MainActor in
            await seeding.value
            requestFrontier(.continueThinking, force: true)
        }
    }

    private func chooseFrontier(_ prompt: ThinkingPrompt) {
        model.addSuggestedBranch(
            prompt,
            source: intelligence.fallback?.provenance
                ?? "Suggested by your facilitator after reading this map"
        )
        intelligence.consume(prompt)
        impact(.soft)
    }

    private func requestFrontier(_ intent: ThinkingMapFrontierIntent, force: Bool) {
        intelligence.refresh(model: model, intent: intent, force: force)
    }

    /// Ask the model to STAGE a restructure proposal. On success a pending
    /// proposal surfaces via `model.pendingProposals`; on failure the model sets
    /// `aiUnavailable`, which the banner renders. Wired only to `model.consolidate`.
    private func reorganize() {
        guard !isReorganizing else { return }
        isReorganizing = true
        impact(.light)
        Task {
            await model.consolidate()
            isReorganizing = false
        }
    }

    // MARK: - Ambient "Listen" mode
    //
    // Toggles a hands-free realtime voice call bound to this map. When it goes
    // live we hand its media session id to the model, which attaches it so the
    // server ambient coordinator auto-maps every spoken turn (and a live-refresh
    // surfaces the new nodes). Stopping detaches + hangs up.

    private func toggleListening() {
        if model.isListening || isActivatingListen {
            stopListening()
        } else {
            startListening()
        }
    }

    private func startListening() {
        guard !isActivatingListen, !model.isListening else { return }
        // Any local dictation would fight the realtime call for the mic.
        finishDictationIfNeeded(submit: false)
        isActivatingListen = true
        impact(.light)
        // A stable ui-thread id keeps the voice call's server-side chat/session
        // mapping consistent for THIS map across start/stop toggles.
        let threadID = "thinking-map-\(model.openRecord?.id.uuidString ?? "solo")"
        // Map listening is continuous by nature: default engine, PTT off.
        listenVoice.startCall(uiThreadId: threadID, engine: .realtime, pttOn: false)
        Task {
            // Wait for the call to go live, then attach its media session id.
            if let sessionID = await listenVoice.awaitReadySessionID() {
                await model.startListening(sessionID: sessionID)
            } else {
                // Never went live. Only surface the "couldn't start" notice for a
                // GENUINE failure — if the user hit Stop mid-activation,
                // `stopListening()` already flipped `isActivatingListen` off, so
                // suppress the misleading banner and just ensure the call is torn down.
                if isActivatingListen {
                    model.markListeningUnavailable()
                }
                listenVoice.end()
            }
            isActivatingListen = false
        }
    }

    private func stopListening() {
        isActivatingListen = false
        listenVoice.end()
        Task { await model.stopListening() }
    }

    private func impact(_ style: UIImpactFeedbackGenerator.FeedbackStyle) {
        UIImpactFeedbackGenerator(style: style).impactOccurred()
    }
}

private struct ThinkingMapMiniGraph: View {
    let record: ThinkingMapRecord
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Canvas { context, size in
            let nodes = Array(record.snapshot.nodes.prefix(10))
            guard !nodes.isEmpty else { return }

            func depth(_ node: ThinkingNode) -> Int {
                var result = 0
                var cursor = node.parentID
                var visited = Set<UUID>()
                while let id = cursor,
                      !visited.contains(id),
                      let parent = record.snapshot.nodes.first(where: { $0.id == id }) {
                    visited.insert(id)
                    result += 1
                    cursor = parent.parentID
                }
                return result
            }

            let grouped = Dictionary(grouping: nodes, by: depth)
            let maximumDepth = max(1, grouped.keys.max() ?? 1)
            var positions: [UUID: CGPoint] = [:]
            for level in grouped.keys.sorted() {
                let levelNodes = grouped[level] ?? []
                for (index, node) in levelNodes.enumerated() {
                    positions[node.id] = CGPoint(
                        x: 10 + (size.width - 20) * CGFloat(level) / CGFloat(maximumDepth),
                        y: (size.height / CGFloat(levelNodes.count + 1)) * CGFloat(index + 1)
                    )
                }
            }

            for edge in record.snapshot.edges where edge.kind == .branch {
                guard let from = positions[edge.from], let to = positions[edge.to] else { continue }
                var path = Path()
                path.move(to: from)
                let midpoint = (from.x + to.x) / 2
                path.addCurve(
                    to: to,
                    control1: CGPoint(x: midpoint, y: from.y),
                    control2: CGPoint(x: midpoint, y: to.y)
                )
                context.stroke(path, with: .color(theme.secondaryTextColor.opacity(0.24)), lineWidth: 1)
            }

            for node in nodes {
                guard let point = positions[node.id] else { continue }
                let diameter: CGFloat = node.id == record.snapshot.activeNodeID ? 9 : 6
                let rect = CGRect(x: point.x - diameter / 2, y: point.y - diameter / 2, width: diameter, height: diameter)
                context.fill(Path(ellipseIn: rect), with: .color(node.kind.color))
                if node.id == record.snapshot.activeNodeID {
                    context.stroke(Path(ellipseIn: rect.insetBy(dx: -3, dy: -3)), with: .color(node.kind.color.opacity(0.35)), lineWidth: 2)
                }
            }
        }
        .padding(10)
        .accessibilityHidden(true)
    }
}

private struct ThinkingMapLibraryActionsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: ThinkingMapModel
    @ObservedObject private var theme = ThemeManager.shared
    let recordID: UUID
    let onOpen: (UUID) -> Void
    let onDuplicate: (UUID) -> Void
    let onDeleted: () -> Void

    @State private var editedTitle = ""
    @State private var showDeleteConfirmation = false

    private var record: ThinkingMapRecord? { model.maps.first { $0.id == recordID } }

    var body: some View {
        NavigationStack {
            ScrollView {
                if let record {
                    VStack(alignment: .leading, spacing: 18) {
                        HStack(spacing: 16) {
                            ThinkingMapMiniGraph(record: record)
                                .frame(width: 88, height: 88)
                                .background(theme.elevatedColor)
                                .clipShape(RoundedRectangle(cornerRadius: 20))
                            VStack(alignment: .leading, spacing: 6) {
                                Text(record.isArchived ? "ARCHIVED IDEA" : "IDEA MAP")
                                    .thinkingEyebrow(theme)
                                Text("\(record.nodeCount) thoughts · \(record.capturedNodeCount) captured")
                                    .font(.themed(12, weight: .semibold))
                                Text("Updated \(record.updatedAt.formatted(.relative(presentation: .named)))")
                                    .font(.themed(10))
                                    .foregroundColor(theme.secondaryTextColor)
                            }
                        }

                        VStack(alignment: .leading, spacing: 9) {
                            Text("NAME")
                                .thinkingEyebrow(theme)
                            TextField("Idea name", text: $editedTitle)
                                .font(.themed(18, weight: .bold))
                                .padding(14)
                                .background(theme.elevatedColor)
                                .clipShape(RoundedRectangle(cornerRadius: 15))
                                .accessibilityIdentifier("thinking-map-rename-field")
                            Button {
                                model.renameMap(recordID, to: editedTitle)
                            } label: {
                                Label("Save name", systemImage: "checkmark")
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.bordered)
                            .disabled(editedTitle.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || editedTitle == record.title)
                        }
                        .padding(16)
                        .background(theme.surfaceColor)
                        .clipShape(RoundedRectangle(cornerRadius: 19))

                        Button {
                            dismiss()
                            DispatchQueue.main.async { onOpen(recordID) }
                        } label: {
                            Label("Continue this idea", systemImage: "scope")
                                .font(.themed(15, weight: .bold))
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 5)
                        }
                        .buttonStyle(.borderedProminent)
                        .foregroundColor(theme.onAccentColor)

                        HStack(spacing: 10) {
                            managementButton(
                                record.isPinned ? "Unpin" : "Pin",
                                icon: record.isPinned ? "pin.slash.fill" : "pin.fill"
                            ) {
                                model.togglePinned(recordID)
                            }
                            managementButton("Duplicate", icon: "plus.square.on.square") {
                                dismiss()
                                // The copy's id arrives asynchronously (canonical
                                // create); open it the moment it lands.
                                model.duplicateMap(recordID) { duplicate in
                                    guard let duplicate else { return }
                                    onDuplicate(duplicate)
                                }
                            }
                        }

                        HStack(spacing: 10) {
                            managementButton(
                                record.isArchived ? "Restore" : "Archive",
                                icon: record.isArchived ? "arrow.uturn.backward.circle" : "archivebox.fill"
                            ) {
                                model.setArchived(!record.isArchived, for: recordID)
                                dismiss()
                                DispatchQueue.main.async { onDeleted() }
                            }

                            ShareLink(item: model.exportMarkdown(for: recordID)) {
                                VStack(spacing: 7) {
                                    Image(systemName: "square.and.arrow.up")
                                        .font(.title3)
                                    Text("Share")
                                        .font(.themed(11, weight: .semibold))
                                }
                                .foregroundColor(theme.textColor)
                                .frame(maxWidth: .infinity)
                                .frame(height: 68)
                                .background(theme.surfaceColor)
                                .clipShape(RoundedRectangle(cornerRadius: 17))
                            }
                        }

                        Button(role: .destructive) { showDeleteConfirmation = true } label: {
                            Label("Delete idea permanently", systemImage: "trash")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                        .padding(.top, 4)
                    }
                    .padding(20)
                }
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Manage idea")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .onAppear { editedTitle = record?.title ?? "" }
            .confirmationDialog("Delete this idea and all of its branches?", isPresented: $showDeleteConfirmation, titleVisibility: .visible) {
                Button("Delete permanently", role: .destructive) {
                    model.deleteMap(recordID)
                    dismiss()
                    DispatchQueue.main.async { onDeleted() }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("This cannot be undone. Archive it instead if you may want it later.")
            }
        }
        .preferredColorScheme(theme.colorScheme)
        .presentationDetents([.large])
    }

    private func managementButton(_ title: String, icon: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 7) {
                Image(systemName: icon)
                    .font(.title3)
                Text(title)
                    .font(.themed(11, weight: .semibold))
            }
            .foregroundColor(theme.textColor)
            .frame(maxWidth: .infinity)
            .frame(height: 68)
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: 17))
        }
        .buttonStyle(.plain)
    }
}

private struct ThinkingGraphView: View {
    @ObservedObject var model: ThinkingMapModel
    @ObservedObject private var theme = ThemeManager.shared
    @State private var zoom: CGFloat = 1
    @State private var settledZoom: CGFloat = 1
    @State private var panOffset: CGSize = .zero
    @State private var settledPanOffset: CGSize = .zero
    /// Whether the one-time "fit the whole map, centered" pass has run for the
    /// currently-open map. Reset when a different map loads (node identity
    /// changes) so a freshly-opened map fits again, but a user's manual
    /// zoom/pan afterwards is never overridden.
    @State private var didFitInitial = false
    @State private var fittedMapSignature: Int?

    /// Zoom/pan bounds shared by every zoom path (gesture, buttons, fit).
    private static let minZoom: CGFloat = 0.46
    private static let maxZoom: CGFloat = 1.8
    let frontier: [ThinkingPrompt]
    let isThinking: Bool
    let progress: ThinkingMapIntelligenceProgress
    let activityRows: [ActivityRow]
    let fallback: ThinkingMapFrontierFallback?
    var onInspect: (UUID) -> Void
    var onChooseFrontier: (ThinkingPrompt) -> Void
    var onBreakOpen: () -> Void
    var onRetry: () -> Void

    var body: some View {
        GeometryReader { proxy in
            let layout = ThinkingGraphLayout(nodes: model.nodes, viewport: proxy.size)
            let frontierPositions = layout.frontierPositions(from: model.activeNodeID, count: frontier.count)
            ZStack(alignment: .topLeading) {
                ZStack {
                        canvasGrid

                        Canvas { context, _ in
                            for edge in model.edges {
                                guard let start = layout.positions[edge.from], let end = layout.positions[edge.to] else { continue }
                                drawEdge(
                                    from: start,
                                    to: end,
                                    color: edge.kind == .branch
                                        ? theme.secondaryTextColor.opacity(0.24)
                                        : theme.accentColor.opacity(0.55),
                                    dashed: edge.kind == .related,
                                    in: &context
                                )
                            }
                            if let activeID = model.activeNodeID,
                               let start = layout.positions[activeID] {
                                for end in frontierPositions {
                                    drawEdge(
                                        from: start,
                                        to: end,
                                        color: theme.accentColor.opacity(0.42),
                                        dashed: true,
                                        in: &context
                                    )
                                }
                            }
                        }

                        ForEach(model.nodes) { node in
                            if let point = layout.positions[node.id] {
                                Button {
                                    if model.activeNodeID == node.id { onInspect(node.id) }
                                    else { model.select(node.id) }
                                } label: {
                                    VStack(alignment: .leading, spacing: 6) {
                                        HStack {
                                            Image(systemName: node.kind.icon)
                                            Text(node.kind.rawValue.uppercased())
                                            Spacer()
                                            if model.activeNodeID == node.id { Image(systemName: "scope") }
                                        }
                                        .font(.themed(8, weight: .bold))
                                        .foregroundColor(node.kind.color)
                                        Text(node.title)
                                            .font(.themed(11, weight: .semibold))
                                            .foregroundColor(theme.textColor)
                                            .lineLimit(3)
                                            .multilineTextAlignment(.leading)
                                    }
                                    .padding(11)
                                    .frame(width: 158, height: 96, alignment: .topLeading)
                                    .background(theme.surfaceColor)
                                    .clipShape(RoundedRectangle(cornerRadius: 15))
                                    .overlay {
                                        RoundedRectangle(cornerRadius: 15)
                                            .stroke(
                                                node.kind.color.opacity(model.activeNodeID == node.id ? 0.9 : 0.38),
                                                style: StrokeStyle(
                                                    lineWidth: model.activeNodeID == node.id ? 2.2 : 1,
                                                    dash: node.suggested ? [5, 4] : []
                                                )
                                            )
                                    }
                                    .shadow(
                                        color: node.kind.color.opacity(model.activeNodeID == node.id ? 0.14 : 0.04),
                                        radius: 9,
                                        y: 3
                                    )
                                }
                                .buttonStyle(.plain)
                                .position(point)
                                .id(node.id.uuidString)
                                .accessibilityIdentifier("thinking-map-canvas-node-\(node.id.uuidString)")
                            }
                        }

                        ForEach(Array(frontier.enumerated()), id: \.element.id) { index, prompt in
                            if frontierPositions.indices.contains(index) {
                                Button { onChooseFrontier(prompt) } label: {
                                    VStack(alignment: .leading, spacing: 6) {
                                        HStack {
                                            Image(systemName: "sparkles")
                                            Text(fallback?.cardLabel ?? "YOUR FACILITATOR SEES")
                                        }
                                        .font(.themed(8, weight: .bold))
                                        .foregroundColor(fallback == nil ? theme.accentColor : theme.warningColor)
                                        Text(prompt.title)
                                            .font(.themed(11, weight: .semibold))
                                            .foregroundColor(theme.textColor)
                                            .multilineTextAlignment(.leading)
                                            .lineLimit(3)
                                    }
                                    .padding(11)
                                    .frame(width: 158, height: 96, alignment: .topLeading)
                                    .background(theme.elevatedColor.opacity(0.96))
                                    .clipShape(RoundedRectangle(cornerRadius: 15))
                                    .overlay {
                                        RoundedRectangle(cornerRadius: 15)
                                            .stroke(
                                                fallback == nil ? theme.accentColor.opacity(0.7) : theme.warningColor.opacity(0.65),
                                                style: StrokeStyle(lineWidth: 1.4, dash: [5, 4])
                                            )
                                    }
                                    .shadow(color: theme.accentColor.opacity(0.08), radius: 9, y: 3)
                                }
                                .buttonStyle(.plain)
                                .position(frontierPositions[index])
                                .accessibilityIdentifier("thinking-map-ai-frontier-\(index)")
                            }
                        }
                }
                .frame(width: layout.width, height: layout.height)
                .scaleEffect(zoom, anchor: .topLeading)
                .offset(panOffset)
            }
            .frame(width: proxy.size.width, height: proxy.size.height, alignment: .topLeading)
            .clipped()
            .contentShape(Rectangle())
            .simultaneousGesture(
                DragGesture(minimumDistance: 8)
                    .onChanged { value in
                        panOffset = CGSize(
                            width: settledPanOffset.width + value.translation.width,
                            height: settledPanOffset.height + value.translation.height
                        )
                    }
                    .onEnded { _ in settledPanOffset = panOffset }
            )
            .simultaneousGesture(
                MagnificationGesture()
                    .onChanged { value in
                        let nextZoom = min(Self.maxZoom, max(Self.minZoom, settledZoom * value))
                        let graphCenter = CGPoint(
                            x: (proxy.size.width / 2 - settledPanOffset.width) / settledZoom,
                            y: (proxy.size.height / 2 - settledPanOffset.height) / settledZoom
                        )
                        zoom = nextZoom
                        panOffset = CGSize(
                            width: proxy.size.width / 2 - graphCenter.x * nextZoom,
                            height: proxy.size.height / 2 - graphCenter.y * nextZoom
                        )
                    }
                    .onEnded { _ in
                        settledZoom = zoom
                        settledPanOffset = panOffset
                    }
            )
            .onAppear { fitOrRecenter(viewport: proxy.size, animated: false) }
            .onChange(of: model.activeNodeID) { _, _ in
                // Only follow the active node once the initial fit-to-view has
                // run — otherwise the launch selection would recenter (at 100%)
                // before the whole-map fit gets a chance to glance-frame it.
                if didFitInitial {
                    recenterAfterLayout(viewport: proxy.size, animated: true)
                }
            }
            .onChange(of: model.nodes.count) { _, _ in
                // A fresh map (or one whose nodes just loaded) re-fits so the
                // whole graph is glanceable; a growing open map recenters on the
                // active node without fighting the user's manual zoom/pan.
                fitOrRecenter(viewport: proxy.size, animated: true)
            }
            .onChange(of: frontier.count) { _, _ in
                recenterAfterLayout(viewport: proxy.size, animated: true)
            }
            .onChange(of: proxy.size) { _, _ in
                recenterAfterLayout(viewport: proxy.size, animated: false)
            }
                .overlay(alignment: .topLeading) {
                    VStack(alignment: .leading, spacing: 7) {
                        Button {
                            if fallback?.canRetry == true { onRetry() }
                        } label: {
                            HStack(spacing: 7) {
                                if isThinking { ProgressView().controlSize(.mini) }
                                Image(systemName: isThinking ? "sparkles" : (fallback?.icon ?? "wand.and.stars"))
                                Text(isThinking ? progress.label.uppercased() : (fallback?.statusLabel ?? "AI FRONTIER"))
                                    .font(.themed(8, weight: .bold))
                                    .tracking(0.8)
                                if fallback?.canRetry == true {
                                    Image(systemName: "arrow.clockwise")
                                        .font(.caption2.weight(.bold))
                                }
                            }
                            .foregroundColor(fallback == nil ? theme.accentColor : theme.warningColor)
                            .padding(.horizontal, 11)
                            .padding(.vertical, 8)
                            .background(theme.surfaceColor.opacity(0.94))
                            .clipShape(Capsule())
                        }
                        .buttonStyle(.plain)
                        .allowsHitTesting(fallback?.canRetry == true)
                        .accessibilityLabel(canvasStatusAccessibilityLabel)
                        .accessibilityHint(fallback?.canRetry == true ? "Retries facilitator intelligence" : "")
                        .accessibilityIdentifier("thinking-map-ai-status")

                        if isThinking {
                            VStack(alignment: .leading, spacing: 5) {
                                Text(progress.detail)
                                    .font(.themed(9, weight: .medium))
                                    .foregroundColor(theme.secondaryTextColor)
                                    .lineLimit(2)
                                ForEach(activityRows.suffix(2)) { row in
                                    HStack(spacing: 5) {
                                        Image(systemName: row.status == .done ? "checkmark.circle.fill" : "circle.dotted")
                                        Text(row.label).lineLimit(1)
                                    }
                                    .font(.themed(8, weight: .semibold))
                                    .foregroundColor(row.kind == .tool ? theme.warningColor : theme.secondaryTextColor)
                                }
                            }
                            .padding(10)
                            .frame(width: 190, alignment: .leading)
                            .background(theme.surfaceColor.opacity(0.94))
                            .clipShape(RoundedRectangle(cornerRadius: 13))
                            .transition(.opacity.combined(with: .move(edge: .top)))
                        }
                    }
                    .padding(12)
                }
                .overlay(alignment: .topTrailing) {
                    HStack(spacing: 0) {
                        Button { changeZoom(by: -0.14, viewport: proxy.size) } label: { Image(systemName: "minus") }
                        Button {
                            zoom = 1
                            settledZoom = 1
                            centerActive(viewport: proxy.size, animated: true)
                        } label: {
                            Text("\(Int(zoom * 100))%")
                                .font(.themed(9, weight: .bold))
                                .frame(width: 40)
                        }
                        .accessibilityIdentifier("thinking-map-zoom-reset")
                        Button { changeZoom(by: 0.14, viewport: proxy.size) } label: { Image(systemName: "plus") }
                        Divider().frame(height: 18)
                        Button { centerActive(viewport: proxy.size, animated: true) } label: { Image(systemName: "scope") }
                            .accessibilityLabel("Center active thought")
                    }
                    .buttonStyle(.plain)
                    .foregroundColor(theme.textColor)
                    .padding(9)
                    .background(theme.surfaceColor.opacity(0.94))
                    .clipShape(Capsule())
                    .shadow(color: .black.opacity(0.08), radius: 7, y: 2)
                    .padding(12)
                }
                .overlay(alignment: .bottomTrailing) {
                    Button(action: onBreakOpen) {
                        Label("Break open", systemImage: "sparkles.rectangle.stack")
                            .font(.themed(11, weight: .bold))
                            .padding(.horizontal, 14)
                            .padding(.vertical, 11)
                    }
                    .buttonStyle(.borderedProminent)
                    .foregroundColor(theme.onAccentColor)
                    .disabled(isThinking)
                    .padding(12)
                    .accessibilityIdentifier("thinking-map-break-open")
                }
        }
    }

    private var canvasStatusAccessibilityLabel: String {
        if isThinking { return "\(progress.label). \(progress.detail)" }
        if let fallback {
            return fallback.message.map { "\(fallback.statusLabel.capitalized). \($0)" }
                ?? fallback.statusLabel.capitalized
        }
        return "AI frontier ready"
    }

    private var canvasGrid: some View {
        Canvas { context, size in
            let spacing: CGFloat = 28
            var x: CGFloat = spacing
            while x < size.width {
                var y: CGFloat = spacing
                while y < size.height {
                    context.fill(
                        Path(ellipseIn: CGRect(x: x, y: y, width: 1.5, height: 1.5)),
                        with: .color(theme.secondaryTextColor.opacity(0.12))
                    )
                    y += spacing
                }
                x += spacing
            }
        }
    }

    private func drawEdge(
        from start: CGPoint,
        to end: CGPoint,
        color: Color,
        dashed: Bool,
        in context: inout GraphicsContext
    ) {
        var path = Path()
        path.move(to: start)
        path.addCurve(
            to: end,
            control1: CGPoint(x: start.x, y: start.y + 62),
            control2: CGPoint(x: end.x, y: end.y - 62)
        )
        context.stroke(
            path,
            with: .color(color),
            style: StrokeStyle(lineWidth: dashed ? 2 : 1.5, dash: dashed ? [6, 5] : [])
        )
    }

    private func changeZoom(by delta: CGFloat, viewport: CGSize) {
        let nextZoom = min(Self.maxZoom, max(Self.minZoom, zoom + delta))
        let graphCenter = CGPoint(
            x: (viewport.width / 2 - panOffset.width) / zoom,
            y: (viewport.height / 2 - panOffset.height) / zoom
        )
        zoom = nextZoom
        settledZoom = nextZoom
        panOffset = CGSize(
            width: viewport.width / 2 - graphCenter.x * nextZoom,
            height: viewport.height / 2 - graphCenter.y * nextZoom
        )
        settledPanOffset = panOffset
    }

    private func centerActive(viewport: CGSize, animated: Bool) {
        let layout = ThinkingGraphLayout(nodes: model.nodes, viewport: viewport)
        guard let activeNodeID = model.activeNodeID,
              let point = layout.positions[activeNodeID] else { return }
        let target = CGSize(
            width: viewport.width / 2 - point.x * zoom,
            height: viewport.height / 2 - point.y * zoom
        )
        let apply = {
            panOffset = target
            settledPanOffset = target
        }
        if animated {
            withAnimation(.easeOut(duration: 0.28), apply)
        } else {
            DispatchQueue.main.async(execute: apply)
        }
    }

    private func recenterAfterLayout(viewport: CGSize, animated: Bool) {
        // The map and its restored selection can arrive in the same render pass. Recompute
        // after SwiftUI has committed that layout so we center the selected node, not the
        // previously active node from the launch snapshot.
        DispatchQueue.main.async {
            centerActive(viewport: viewport, animated: animated)
        }
    }

    /// Identifies WHICH map is currently loaded, so the one-time fit runs afresh
    /// for a newly-opened map but not for every append to the same map. Keyed on
    /// the root node's id (stable per map); a fresh 0→N node load re-arms the fit
    /// because the signature goes nil→value while `fittedMapSignature` was nil.
    private var mapSignature: Int? {
        guard !model.nodes.isEmpty else { return nil }
        let root = model.nodes.first(where: { $0.parentID == nil }) ?? model.nodes[0]
        var hasher = Hasher()
        hasher.combine(root.id)
        return hasher.finalize()
    }

    /// On the FIRST layout of a given map, frame the whole graph (fit-to-view,
    /// centered). Afterwards defer to `recenterAfterLayout` so the active node is
    /// followed without overriding the user's manual zoom/pan. A different map
    /// (new root id) re-arms the one-time fit.
    private func fitOrRecenter(viewport: CGSize, animated: Bool) {
        let signature = mapSignature
        if signature != fittedMapSignature {
            didFitInitial = false
        }
        guard signature != nil else { return }
        if !didFitInitial {
            didFitInitial = true
            fittedMapSignature = signature
            // Defer to the next runloop turn so the layout for the just-loaded
            // nodes is committed (positions resolve) before we measure them.
            DispatchQueue.main.async {
                fitToView(viewport: viewport, animated: animated)
            }
        } else {
            recenterAfterLayout(viewport: viewport, animated: animated)
        }
    }

    /// Fit the entire graph centered in `viewport` with padding: compute the
    /// bounding box of all rendered node centers (padded by half a node card so
    /// edges of the cards are included), pick the largest scale that fits inside
    /// the padded viewport clamped to the zoom bounds, then offset so the box is
    /// centered. Establishes both the live and settled zoom/pan so subsequent
    /// gestures continue from the fitted frame.
    private func fitToView(viewport: CGSize, animated: Bool) {
        let layout = ThinkingGraphLayout(nodes: model.nodes, viewport: viewport)
        let points = Array(layout.positions.values)
        guard !points.isEmpty, viewport.width > 0, viewport.height > 0 else { return }

        // Node cards are 158×96, positioned by their CENTER, so pad the bbox by
        // half a card on every side to keep whole cards on-screen.
        let halfNodeW: CGFloat = 158 / 2
        let halfNodeH: CGFloat = 96 / 2
        let minX = (points.map(\.x).min() ?? 0) - halfNodeW
        let maxX = (points.map(\.x).max() ?? 0) + halfNodeW
        let minY = (points.map(\.y).min() ?? 0) - halfNodeH
        let maxY = (points.map(\.y).max() ?? 0) + halfNodeH
        let contentWidth = max(1, maxX - minX)
        let contentHeight = max(1, maxY - minY)

        // Breathing room around the graph inside the viewport.
        let padding: CGFloat = 48
        let usableWidth = max(1, viewport.width - padding * 2)
        let usableHeight = max(1, viewport.height - padding * 2)

        let rawScale = min(usableWidth / contentWidth, usableHeight / contentHeight)
        let targetZoom = min(Self.maxZoom, max(Self.minZoom, rawScale))

        // Center the bbox center in the viewport at the chosen zoom.
        let boxCenter = CGPoint(x: (minX + maxX) / 2, y: (minY + maxY) / 2)
        let target = CGSize(
            width: viewport.width / 2 - boxCenter.x * targetZoom,
            height: viewport.height / 2 - boxCenter.y * targetZoom
        )
        let apply = {
            zoom = targetZoom
            settledZoom = targetZoom
            panOffset = target
            settledPanOffset = target
        }
        if animated {
            withAnimation(.easeOut(duration: 0.28), apply)
        } else {
            apply()
        }
    }
}

private struct ThinkingNodeDetailSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: ThinkingMapModel
    @ObservedObject private var theme = ThemeManager.shared
    let nodeID: UUID
    @State private var title: String
    @State private var detail: String
    @State private var clarificationAnswer = ""
    @State private var showDeleteConfirmation = false
    /// Promote flow state: the target currently in flight, the target awaiting
    /// the inline "AI-suggested — promote anyway?" confirmation (409), the
    /// last success (brief checkmark flash), and a soft failure notice.
    @State private var promoteBusyTarget: String?
    @State private var promoteConfirmTarget: String?
    @State private var promoteSuccess: ThinkingMapModel.ThinkingPromotionResult?
    @State private var promoteFailed = false

    init(model: ThinkingMapModel, nodeID: UUID) {
        self.model = model
        self.nodeID = nodeID
        let node = model.node(nodeID)
        _title = State(initialValue: node?.title ?? "")
        _detail = State(initialValue: node?.detail ?? "")
    }

    /// The open clarification the model asked about THIS node, if any.
    private var openClarification: ThinkingClarification? {
        model.clarifications.first { $0.nodeID == nodeID }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                if let node = model.node(nodeID) {
                    VStack(alignment: .leading, spacing: 20) {
                        HStack {
                            Label(node.kind.rawValue.uppercased(), systemImage: node.kind.icon)
                                .font(.themed(10, weight: .bold))
                                .foregroundColor(node.kind.color)
                            Spacer()
                            Text(node.suggested ? "SUGGESTED" : "CAPTURED")
                                .font(.themed(9, weight: .bold))
                                .foregroundColor(node.kind.color)
                        }

                        VStack(alignment: .leading, spacing: 8) {
                            Text("THOUGHT")
                                .thinkingEyebrow(theme)
                            TextField("Thought", text: $title, axis: .vertical)
                                .font(.themed(21, weight: .bold))
                                .lineLimit(2...5)
                            TextField("Context or notes", text: $detail, axis: .vertical)
                                .font(.themed(14))
                                .foregroundColor(theme.secondaryTextColor)
                                .lineLimit(2...8)
                        }
                        .padding(16)
                        .background(theme.surfaceColor)
                        .clipShape(RoundedRectangle(cornerRadius: 17))

                        if !node.source.isEmpty {
                            detailSection("SOURCE", icon: "quote.opening") {
                                Text(node.source)
                                    .font(.themed(14, weight: .medium))
                                    .italic()
                            }
                        }

                        if let clarification = openClarification {
                            clarificationSection(clarification)
                        }

                        HStack(spacing: 10) {
                            Button {
                                model.updateNode(nodeID, title: title, detail: detail)
                                dismiss()
                            } label: {
                                Label("Save", systemImage: "checkmark")
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.bordered)

                            Button {
                                model.updateNode(nodeID, title: title, detail: detail)
                                model.select(nodeID)
                                dismiss()
                            } label: {
                                Label("Explore here", systemImage: "scope")
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.borderedProminent)
                            .foregroundColor(theme.onAccentColor)
                        }

                        promoteSection(node)

                        detailSection("CONNECTIONS", icon: "point.3.connected.trianglepath.dotted") {
                            let related = model.relatedNodes(to: nodeID)
                            if related.isEmpty {
                                Text("No cross-branch connections yet.")
                                    .font(.themed(13))
                                    .foregroundColor(theme.secondaryTextColor)
                            } else {
                                ForEach(related) { relatedNode in
                                    HStack {
                                        Image(systemName: "link")
                                            .foregroundColor(relatedNode.kind.color)
                                        Text(relatedNode.title)
                                            .font(.themed(12, weight: .semibold))
                                        Spacer()
                                        Button { model.disconnect(nodeID, from: relatedNode.id) } label: {
                                            Image(systemName: "xmark.circle")
                                        }
                                        .buttonStyle(.plain)
                                    }
                                }
                            }

                            Menu {
                                ForEach(model.nodes.filter { $0.id != nodeID }) { candidate in
                                    Button { model.connect(nodeID, to: candidate.id) } label: {
                                        Label(candidate.title, systemImage: candidate.kind.icon)
                                    }
                                }
                            } label: {
                                Label("Connect another thought", systemImage: "link.badge.plus")
                            }
                            .buttonStyle(.bordered)
                        }

                        if node.parentID != nil {
                            Button(role: .destructive) { showDeleteConfirmation = true } label: {
                                Label("Remove this branch", systemImage: "trash")
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.bordered)
                        }
                    }
                    .padding(20)
                }
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Thought details")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .confirmationDialog("Remove this branch and everything beneath it?", isPresented: $showDeleteConfirmation, titleVisibility: .visible) {
                Button("Remove branch", role: .destructive) {
                    model.removeBranch(nodeID)
                    dismiss()
                }
                Button("Cancel", role: .cancel) {}
            }
        }
        .preferredColorScheme(theme.colorScheme)
        .presentationDetents([.large])
    }

    private func detailSection<Content: View>(_ title: String, icon: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(title, systemImage: icon)
                .font(.themed(10, weight: .bold))
                .tracking(1)
                .foregroundColor(theme.secondaryTextColor)
            content()
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 17))
    }

    /// The "send onward" section: Promote to Task / Save to Memory via the
    /// governed backend promote endpoint. Already-promoted kinds (from the
    /// node's projected `promotedKinds`) render as badges instead of buttons;
    /// a 409 `confirmation_required` (AI-suggested node) surfaces an inline
    /// confirm that retries with `confirm: true`; a success flashes a brief
    /// checkmark with the created object's kind.
    private func promoteSection(_ node: ThinkingNode) -> some View {
        detailSection("SEND ONWARD", icon: "arrow.up.forward.square") {
            HStack(spacing: 10) {
                promoteControl(
                    node, target: "task",
                    buttonLabel: "Promote to Task", badgeLabel: "Task created",
                    icon: "checklist")
                promoteControl(
                    node, target: "memory",
                    buttonLabel: "Save to Memory", badgeLabel: "In Memory",
                    icon: "brain")
            }

            if let confirmTarget = promoteConfirmTarget {
                VStack(alignment: .leading, spacing: 10) {
                    Text("This thought is AI-suggested — promote anyway? Confirming records it as yours.")
                        .font(.themed(13))
                        .foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 10) {
                        Button {
                            promote(target: confirmTarget, confirm: true)
                        } label: {
                            Label("Promote anyway", systemImage: "checkmark")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.borderedProminent)
                        .foregroundColor(theme.onAccentColor)
                        .controlSize(.small)
                        .accessibilityIdentifier("thinking-map-promote-confirm")

                        Button {
                            promoteConfirmTarget = nil
                        } label: {
                            Label("Cancel", systemImage: "xmark")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }
                .padding(12)
                .background(theme.accentColor.opacity(0.08))
                .clipShape(RoundedRectangle(cornerRadius: 13))
                .overlay {
                    RoundedRectangle(cornerRadius: 13)
                        .stroke(theme.accentColor.opacity(0.5), style: StrokeStyle(lineWidth: 1.4, dash: [6, 5]))
                }
            }

            if let success = promoteSuccess {
                Label(
                    success.objectKind == "task"
                        ? (success.promoted ? "Task created" : "Already a task")
                        : (success.promoted ? "Saved to Memory (review-gated)" : "Already in Memory"),
                    systemImage: "checkmark.circle.fill")
                    .font(.themed(13, weight: .semibold))
                    .foregroundColor(theme.successColor)
                    .accessibilityIdentifier("thinking-map-promote-success")
            }

            if promoteFailed {
                Label("Couldn't promote — check the connection and try again.", systemImage: "exclamationmark.triangle")
                    .font(.themed(13))
                    .foregroundColor(theme.warningColor)
            }
        }
    }

    /// One promote control: a badge when the node already carries a promotion
    /// link of `target`'s kind, else the action button (spinner while in flight).
    @ViewBuilder
    private func promoteControl(
        _ node: ThinkingNode, target: String,
        buttonLabel: String, badgeLabel: String, icon: String
    ) -> some View {
        if node.promotedKinds.contains(target) {
            Label(badgeLabel, systemImage: "checkmark.seal.fill")
                .font(.themed(13, weight: .semibold))
                .foregroundColor(theme.successColor)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
                .background(theme.successColor.opacity(0.1))
                .clipShape(RoundedRectangle(cornerRadius: 11))
                .accessibilityIdentifier("thinking-map-promoted-\(target)")
        } else {
            Button {
                promote(target: target, confirm: false)
            } label: {
                if promoteBusyTarget == target {
                    ProgressView()
                        .controlSize(.small)
                        .frame(maxWidth: .infinity)
                } else {
                    Label(buttonLabel, systemImage: icon)
                        .frame(maxWidth: .infinity)
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(promoteBusyTarget != nil)
            .accessibilityIdentifier("thinking-map-promote-\(target)")
        }
    }

    /// Run one promotion attempt. A 409 `confirmation_required` opens the
    /// inline confirm (retried with `confirm: true`); any other failure shows
    /// the soft notice. On success the model's backend already refreshed, so
    /// the node re-projects with the new promoted kind (badge replaces button).
    private func promote(target: String, confirm: Bool) {
        promoteConfirmTarget = nil
        promoteFailed = false
        promoteSuccess = nil
        promoteBusyTarget = target
        Task {
            do {
                let result = try await model.promoteNode(nodeID, target: target, confirm: confirm)
                promoteSuccess = result
            } catch LTM.APIError.confirmationRequired {
                promoteConfirmTarget = target
            } catch {
                promoteFailed = true
            }
            promoteBusyTarget = nil
        }
    }

    /// The AI clarification answer surface for this node. The question text plus a
    /// bound answer field, an Answer button, and a Dismiss button. Wired only to
    /// `model.answerClarification(_:answer:)` (nil answer = dismiss). Accent tint +
    /// dashed border read the card as a provisional AI ask.
    private func clarificationSection(_ clarification: ThinkingClarification) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("YOUR FACILITATOR IS ASKING", systemImage: "questionmark.bubble.fill")
                .font(.themed(10, weight: .bold))
                .tracking(1)
                .foregroundColor(theme.accentColor)

            Text(clarification.question)
                .font(.themed(15, weight: .semibold))
                .foregroundColor(theme.textColor)
                .fixedSize(horizontal: false, vertical: true)

            TextField("Your answer…", text: $clarificationAnswer, axis: .vertical)
                .font(.themed(14))
                .lineLimit(1...5)
                .padding(12)
                .background(theme.elevatedColor)
                .clipShape(RoundedRectangle(cornerRadius: 13))
                .accessibilityIdentifier("thinking-map-clarification-answer-field")

            HStack(spacing: 10) {
                Button {
                    let text = clarificationAnswer.trimmingCharacters(in: .whitespacesAndNewlines)
                    Task { await model.answerClarification(clarification.id, answer: text) }
                    clarificationAnswer = ""
                } label: {
                    Label("Answer", systemImage: "checkmark")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .foregroundColor(theme.onAccentColor)
                .controlSize(.small)
                .disabled(clarificationAnswer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("thinking-map-clarification-answer")

                Button {
                    Task { await model.answerClarification(clarification.id, answer: nil) }
                    clarificationAnswer = ""
                } label: {
                    Label("Dismiss", systemImage: "xmark")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier("thinking-map-clarification-dismiss")
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.accentColor.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 17))
        .overlay {
            RoundedRectangle(cornerRadius: 17)
                .stroke(theme.accentColor.opacity(0.5), style: StrokeStyle(lineWidth: 1.4, dash: [6, 5]))
        }
        .accessibilityIdentifier("thinking-map-clarification-section")
    }
}

private struct ThinkingHarvestSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: ThinkingMapModel
    @ObservedObject private var theme = ThemeManager.shared
    @State private var copied = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    Text("Take the useful shape with you")
                        .font(.themed(28, weight: .bold))
                    Text("Harvest does not end the map. Copy or share what is useful, then keep exploring any branch.")
                        .font(.themed(15))
                        .foregroundColor(theme.secondaryTextColor)

                    harvestGroup("DECISIONS", kind: .decision)
                    harvestGroup("NEXT ACTIONS", kind: .action)
                    harvestGroup("OPEN QUESTIONS", kind: .question)

                    Button {
                        UIPasteboard.general.string = model.exportMarkdown
                        copied = true
                    } label: {
                        Label(copied ? "Copied" : "Copy structured map", systemImage: copied ? "checkmark" : "doc.on.doc")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .foregroundColor(theme.onAccentColor)

                    ShareLink(item: model.exportMarkdown) {
                        Label("Share map", systemImage: "square.and.arrow.up")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)

                    Label("Saved locally · you can return and continue from any node", systemImage: "checkmark.icloud")
                        .font(.themed(10))
                        .foregroundColor(theme.secondaryTextColor)
                        .frame(maxWidth: .infinity)
                }
                .padding(20)
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Harvest")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Keep thinking") { dismiss() } } }
        }
        .preferredColorScheme(theme.colorScheme)
        .presentationDetents([.large])
    }

    @ViewBuilder
    private func harvestGroup(_ title: String, kind: ThinkingNodeKind) -> some View {
        let nodes = model.nodes.filter { $0.kind == kind && !$0.suggested }
        if !nodes.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Label(title, systemImage: kind.icon)
                    .font(.themed(10, weight: .bold))
                    .foregroundColor(kind.color)
                ForEach(nodes) { node in
                    Text(node.title)
                        .font(.themed(14, weight: .semibold))
                        .padding(13)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(theme.surfaceColor)
                        .clipShape(RoundedRectangle(cornerRadius: 14))
                }
            }
        }
    }
}

struct ThinkingPrompt: Identifiable, Equatable {
    let id: String
    let title: String
    let detail: String
    let icon: String
    let kind: ThinkingNodeKind
}

enum ThinkingMapFrontierIntent: String {
    case continueThinking = "continue_thinking"
    case breakOpen = "break_open"

    var maximumMoves: Int {
        switch self {
        case .continueThinking: 2
        case .breakOpen: 4
        }
    }

    var diversityPolicy: String {
        switch self {
        case .continueThinking:
            "smallest_useful_frontier"
        case .breakOpen:
            "complementary_directions_prefer_one_per_kind"
        }
    }

    /// The canonical `/interpret` steering intent for this E0 frontier intent.
    /// Ungated + pure so the S2d canonical AI path (and its test) can map
    /// continue-vs-break-open to `LTM.InterpretIntent` without a live backend.
    var interpretIntent: LTM.InterpretIntent {
        switch self {
        case .continueThinking: return .continueThinking
        case .breakOpen: return .breakOpen
        }
    }
}

enum ThinkingMapFrontierFallback: Equatable {
    case demo
    case unavailable(String)

    var statusLabel: String {
        switch self {
        case .demo: "DEMO"
        case .unavailable: "AI UNAVAILABLE"
        }
    }

    var paletteLabel: String {
        switch self {
        case .demo: "DEMO STARTERS"
        case .unavailable: "FALLBACK STARTERS"
        }
    }

    var cardLabel: String {
        switch self {
        case .demo: "DEMO STARTER"
        case .unavailable: "FALLBACK STARTER"
        }
    }

    var icon: String {
        switch self {
        case .demo: "hammer.fill"
        case .unavailable: "exclamationmark.icloud.fill"
        }
    }

    var message: String? {
        guard case .unavailable(let message) = self else { return nil }
        return message
    }

    var canRetry: Bool {
        if case .unavailable = self { return true }
        return false
    }

    var provenance: String {
        switch self {
        case .demo: "Demo starter — deterministic UI-test suggestion"
        case .unavailable: "Fallback starter — facilitator intelligence was unavailable"
        }
    }

    static func classify(_ error: Error) -> ThinkingMapFrontierFallback {
        guard let error = error as? ContextualAssistClientError else {
            return .unavailable(error.localizedDescription)
        }
        switch error {
        case .offline:
            return .unavailable("The backend could not be reached. Check the connection and try again.")
        case .timedOut:
            return .unavailable("The facilitator took too long to read this map. Try again.")
        case .cancelled:
            return .unavailable("The request was cancelled before the facilitator finished reading the map.")
        case .authentication(let message), .validation(let message), .staleSession(let message),
             .server(let message):
            return .unavailable(message)
        case .malformedResponse:
            return .unavailable("The facilitator replied, but the map could not read the response.")
        }
    }
}

/// What the facilitator is doing, driven by the server's
/// `ThinkingMapInterpretProgress` stage events.
///
/// Every case here is bound to a real step of the backend's `interpret()` —
/// the server narrates it, this enum displays it. `openingThread` and
/// `grounding` used to be declared beside these and were assigned nowhere,
/// because no server step ever backed them; they sat unset for a year as
/// labels for work that does not happen. Deleted rather than left unset
/// again — a stage earns its case by being emitted, not by sounding likely.
enum ThinkingMapIntelligenceProgress: Equatable {
    case idle
    /// The count is the live thoughts the facilitator is reading, from the
    /// stage event; nil when the event raced the socket or carried none.
    case preparing(Int?)
    case loadingContext
    case facilitating
    case parsing
    case shaping

    var label: String {
        switch self {
        case .idle: "Ready"
        case .preparing: "Reading map"
        case .loadingContext: "Loading context"
        case .facilitating: "Exploring"
        case .parsing: "Reading back"
        case .shaping: "Shaping moves"
        }
    }

    var detail: String {
        switch self {
        case .idle:
            "Ready for another direction."
        case .preparing(let count):
            if let count, count > 0 {
                "Reading \(count) thought\(count == 1 ? "" : "s") and the active branch…"
            } else {
                // No count (or an empty board): the generic line. "Reading 0
                // thoughts" reads as a bug, not a board.
                "Reading the board and the active branch…"
            }
        case .loadingContext:
            "Giving the facilitator the graph slice and the move budget…"
        case .facilitating:
            "Finding the few directions that would change the next minute of thinking…"
        case .parsing:
            "Reading the facilitator's answer back…"
        case .shaping:
            "Removing repeats and shaping the strongest cards…"
        }
    }

    /// The server's wire spelling → a display state. Nil for a stage this
    /// build has never heard of: the strip keeps its current line rather than
    /// guessing, which is what lets the vocabulary grow server-first.
    static func fromWire(_ stage: String, nodeCount: Int?) -> ThinkingMapIntelligenceProgress? {
        switch stage {
        case "idle": .idle
        case "preparing": .preparing(nodeCount)
        case "loading_context": .loadingContext
        case "facilitating": .facilitating
        case "parsing": .parsing
        case "shaping": .shaping
        default: nil
        }
    }
}

@MainActor
private final class ThinkingMapIntelligenceController: ObservableObject {
    @Published private(set) var frontier: [ThinkingPrompt] = []
    @Published private(set) var isThinking = false
    @Published private(set) var fallback: ThinkingMapFrontierFallback?
    @Published private(set) var progress: ThinkingMapIntelligenceProgress = .idle
    @Published private(set) var activityRows: [ActivityRow] = []

    private var requestTask: Task<Void, Never>?

    /// The narration channel for the run in flight. Subscribed when a run
    /// starts and torn down when it settles — a few seconds of socket, scoped
    /// to exactly the window in which stage events can exist.
    private let progressRealtime = ThinkingMapRealtime()

    /// The utterance id minted for the run in flight, sent with `/interpret`
    /// and matched against incoming stage events. It is the only thing that
    /// separates this run's narration from an ambient auto-map — or another
    /// device — thinking on the same board.
    private var inFlightUtteranceID: String?

    /// Route one narrated stage onto the strip. The terminal `idle` is
    /// deliberately not applied here: settling is the HTTP response's job (it
    /// also owns failure classification), and a realtime idle can outrun the
    /// response body — clearing early would show "Ready" over a run still
    /// being applied.
    private func applyProgress(utteranceID: String, stage: String, nodeCount: Int?) {
        guard utteranceID == inFlightUtteranceID,
              let mapped = ThinkingMapIntelligenceProgress.fromWire(stage, nodeCount: nodeCount),
              mapped != .idle
        else { return }
        progress = mapped
    }

    /// Settle the narration for ONE run: forget it and close its channel —
    /// but only if it still owns the strip.
    ///
    /// `refresh()` cancels the previous task and immediately starts a new run;
    /// the cancelled task's catch block executes *after* the new run has
    /// claimed `inFlightUtteranceID` and opened its channel. An unconditional
    /// teardown here let the superseded run kill its replacement's narration.
    /// Returns whether this run was still the owner, so callers can gate the
    /// rest of their settle work (spinner, progress) the same way.
    @discardableResult
    private func endProgressRun(_ runID: String) -> Bool {
        guard inFlightUtteranceID == runID else { return false }
        inFlightUtteranceID = nil
        progressRealtime.stop()
        return true
    }

    func refresh(
        model: ThinkingMapModel,
        intent: ThinkingMapFrontierIntent,
        force: Bool
    ) {
        guard let activeNodeID = model.activeNodeID else {
            clear()
            return
        }

        requestTask?.cancel()
        if ProcessInfo.processInfo.arguments.contains("--ui-test") {
            useFallback(.demo, model: model)
            return
        }

        // Canonical AI frontier: the request goes through `/interpret`, which
        // APPLIES the model's operations server-side. The moves therefore arrive
        // as provisional nodes ON the map (dashed, via the projection) rather than
        // as a separate prompt list, so `frontier` stays EMPTY here.
        refreshCanonical(model: model, intent: intent, activeNodeID: activeNodeID)
    }

    /// The canonical frontier path. Maps the intent to `LTM.InterpretIntent`,
    /// passes the active node's title (or "") as the utterance text — the frontier
    /// is triggered on the active node without new speech; the interpreter uses the
    /// map's focus context either way — and calls `model.canonicalInterpret`. On
    /// success the store/backend refresh the map → the model re-projects → the new
    /// provisional nodes appear dashed, so `frontier` stays EMPTY (moves are nodes,
    /// not prompts). On error (offline / interpret throws) it surfaces the
    /// "AI unavailable" fallback (the local `branchPrompts` palette).
    private func refreshCanonical(
        model: ThinkingMapModel,
        intent: ThinkingMapFrontierIntent,
        activeNodeID: UUID
    ) {
        isThinking = true
        fallback = nil
        frontier = []
        progress = .facilitating
        activityRows = []
        let interpretIntent = intent.interpretIntent
        let text = model.node(activeNodeID)?.title ?? ""

        // Minted per run and sent with the request; the server narrates each
        // stage back tagged with it. Subscribed BEFORE the request goes out so
        // the early stages (preparing lands in the first ~100ms) are not lost
        // to a race with the socket opening. `.facilitating` above remains the
        // whole story when the socket is down — the honest summary of a run we
        // cannot hear, and exactly what this strip showed before stages existed.
        let utteranceID = UUID().uuidString
        inFlightUtteranceID = utteranceID
        progressRealtime.start(
            mapID: model.currentCanonicalMapID,
            onNotice: {},
            onInterpretProgress: { [weak self] utterance, stage, nodeCount in
                self?.applyProgress(utteranceID: utterance, stage: stage, nodeCount: nodeCount)
            })

        requestTask = Task { [weak self] in
            guard let self else { return }
            do {
                try await model.canonicalInterpret(
                    text: text,
                    intent: interpretIntent,
                    focusNodeID: activeNodeID,
                    utteranceID: utteranceID)
                endProgressRun(utteranceID)
                guard !Task.isCancelled, model.activeNodeID == activeNodeID else { return }
                // The moves are now provisional nodes on the re-projected map.
                frontier = []
                isThinking = false
                fallback = nil
                progress = .idle
            } catch is CancellationError {
                // Cancelled usually means superseded: refresh() cancelled this
                // task and a newer run now owns the strip. Only reset the
                // spinner if this run was still the owner — resetting
                // unconditionally let a dead run wipe its replacement's
                // isThinking/progress mid-flight.
                if endProgressRun(utteranceID) {
                    isThinking = false
                    progress = .idle
                }
            } catch {
                endProgressRun(utteranceID)
                guard !Task.isCancelled, model.activeNodeID == activeNodeID else { return }
                let fallback = ThinkingMapFrontierFallback.classify(error)
                NSLog("[ThinkingMap] canonical interpret unavailable: %@", fallback.message ?? error.localizedDescription)
                useFallback(fallback, model: model)
            }
        }
    }

    func consume(_ prompt: ThinkingPrompt) {
        frontier.removeAll { $0.id == prompt.id }
    }

    func clear() {
        requestTask?.cancel()
        requestTask = nil
        // Unconditional teardown, unlike the per-run settle: clear() means
        // "whatever is running, stop it", so ownership is not in question.
        inFlightUtteranceID = nil
        progressRealtime.stop()
        frontier = []
        isThinking = false
        fallback = nil
        progress = .idle
        activityRows = []
    }

    private func useFallback(_ fallback: ThinkingMapFrontierFallback, model: ThinkingMapModel) {
        frontier = Array(model.branchPrompts.prefix(2))
        isThinking = false
        self.fallback = fallback
        progress = .idle
        activityRows = []
    }
}

private struct ThinkingNodeSelection: Identifiable {
    let id: UUID
}

private struct ThinkingGraphLayout {
    let width: CGFloat
    let height: CGFloat
    let positions: [UUID: CGPoint]

    init(nodes: [ThinkingNode], viewport: CGSize) {
        func depth(of node: ThinkingNode) -> Int {
            var result = 0
            var cursor = node.parentID
            var visited = Set<UUID>()
            while let id = cursor, !visited.contains(id), let parent = nodes.first(where: { $0.id == id }) {
                visited.insert(id)
                result += 1
                cursor = parent.parentID
            }
            return result
        }

        let grouped = Dictionary(grouping: nodes, by: depth)
        let largestLevel = max(1, grouped.values.map(\.count).max() ?? 1)
        width = max(viewport.width * 2.6, CGFloat(largestLevel) * 190 + 620)
        height = max(viewport.height * 2.4, CGFloat((grouped.keys.max() ?? 0) + 1) * 178 + 760)
        var result: [UUID: CGPoint] = [:]
        let firstRowY = max(330, viewport.height * 0.72)
        for level in grouped.keys.sorted() {
            let levelNodes = (grouped[level] ?? []).sorted { $0.createdAt < $1.createdAt }
            let rowWidth = min(width - 360, CGFloat(max(0, levelNodes.count - 1)) * 190)
            let startX = (width - rowWidth) / 2
            for (index, node) in levelNodes.enumerated() {
                let x = levelNodes.count == 1 ? width / 2 : startX + CGFloat(index) * 190
                result[node.id] = CGPoint(x: x, y: firstRowY + CGFloat(level) * 178)
            }
        }
        positions = result
    }

    func frontierPositions(from activeNodeID: UUID?, count: Int) -> [CGPoint] {
        guard let activeNodeID,
              let active = positions[activeNodeID],
              count > 0 else { return [] }
        let rowWidth = CGFloat(max(0, count - 1)) * 182
        let startX = min(max(100, active.x - rowWidth / 2), width - rowWidth - 100)
        var placed: [CGPoint] = []
        for index in 0..<count {
            let x = startX + CGFloat(index) * 182
            var y = active.y + 176
            while positions.values.contains(where: { abs($0.x - x) < 168 && abs($0.y - y) < 112 })
                || placed.contains(where: { abs($0.x - x) < 168 && abs($0.y - y) < 112 }) {
                y += 132
            }
            placed.append(CGPoint(x: x, y: min(height - 110, y)))
        }
        return placed
    }
}

private extension Text {
    func thinkingEyebrow(_ theme: ThemeManager) -> some View {
        font(.themed(10, weight: .bold))
            .tracking(1.25)
            .foregroundColor(theme.secondaryTextColor)
    }
}
