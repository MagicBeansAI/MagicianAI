import SwiftUI

struct ThreadPanelView: View {
    @ObservedObject var viewModel: ThreadViewModel
    @StateObject private var themeManager = ThemeManager.shared
    @Binding var isPresented: Bool
    @ObservedObject var chatViewModel: ChatViewModel
    @State private var showNewThreadPrompt = false
    @State private var newThreadName = ""

    private var currentCollectionIsEmpty: Bool {
        if viewModel.isSearchActive { return viewModel.searchResults.isEmpty }
        return viewModel.activeTab == "sessions" ? viewModel.sessions.isEmpty : viewModel.threads.isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("History")
                        .font(.themed(22, weight: .bold))
                        .foregroundColor(themeManager.textColor)
                    Text("\(viewModel.total) \(viewModel.isSearchActive ? "results" : viewModel.activeTab)")
                        .font(.themed(11))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
                
                Spacer()
                
                Button(action: {
                    withAnimation {
                        isPresented = false
                    }
                }) {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundColor(themeManager.secondaryTextColor)
                        .font(.title3)
                }
            }
            .padding()
            .background(themeManager.surfaceColor)
            
            if viewModel.isSearchActive {
                HStack(spacing: 8) {
                    Image(systemName: "rectangle.stack")
                        .foregroundColor(themeManager.secondaryTextColor)
                    Text("All sessions and threads")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(themeManager.textColor)
                    Spacer()
                    Text("Personal + Automated")
                        .font(.themed(10))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
                .padding(.horizontal, 12)
                .frame(height: 42)
                .background(themeManager.backgroundColor)
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(themeManager.secondaryTextColor.opacity(0.16), lineWidth: 1)
                )
                .padding(.horizontal)
                .padding(.vertical, 8)
                .background(themeManager.surfaceColor)
            } else {
                HStack {
                    TabButton(title: "Sessions", isActive: viewModel.activeTab == "sessions", themeManager: themeManager) {
                        viewModel.selectTab("sessions")
                    }
                    TabButton(title: "Threads", isActive: viewModel.activeTab == "threads", themeManager: themeManager) {
                        viewModel.selectTab("threads")
                    }
                }
                .padding(.horizontal)
                .padding(.vertical, 8)
                .background(themeManager.surfaceColor)

                HStack(spacing: 4) {
                    ForEach(ChatHistoryLane.allCases) { lane in
                        HistoryLaneButton(
                            lane: lane,
                            isActive: viewModel.historyLane == lane,
                            themeManager: themeManager
                        ) {
                            viewModel.selectHistoryLane(lane)
                        }
                    }
                }
                .padding(3)
                .background(themeManager.backgroundColor)
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(themeManager.secondaryTextColor.opacity(0.16), lineWidth: 1)
                )
                .padding(.horizontal)
                .padding(.bottom, 8)
                .background(themeManager.surfaceColor)
            }

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundColor(themeManager.secondaryTextColor)
                TextField(
                    "Search all history",
                    text: Binding(
                        get: { viewModel.searchText },
                        set: { viewModel.updateSearchText($0) }
                    )
                )
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.search)
                .onSubmit { viewModel.submitSearch() }
                .font(.themed(14))
                .foregroundColor(themeManager.textColor)
                if !viewModel.searchText.isEmpty {
                    Button(action: viewModel.clearSearch) {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundColor(themeManager.secondaryTextColor)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Clear history search")
                }
            }
            .padding(.horizontal, 11)
            .frame(height: 38)
            .background(themeManager.backgroundColor)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(themeManager.secondaryTextColor.opacity(0.2), lineWidth: 1)
            )
            .padding(.horizontal)
            .padding(.bottom, 8)
            .background(themeManager.surfaceColor)

            if viewModel.historyLane == .personal && !viewModel.isSearchActive {
                Button(action: {
                    if viewModel.activeTab == "sessions" {
                        viewModel.newSession { sid in
                            chatViewModel.startNewSession(sid)
                            withAnimation { isPresented = false }
                        }
                    } else {
                        newThreadName = ""
                        showNewThreadPrompt = true
                    }
                }) {
                    HStack(spacing: 6) {
                        Image(systemName: "plus")
                        Text(viewModel.activeTab == "sessions" ? "New Session" : "New Thread")
                    }
                    .font(.themed(14, weight: .semibold))
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 9)
                    .background(themeManager.accentColor.opacity(0.12))
                    .foregroundColor(themeManager.accentColor)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                }
                .buttonStyle(.plain)
                .padding(.horizontal)
                .padding(.bottom, 8)
                .background(themeManager.surfaceColor)
            }

            Divider().background(themeManager.secondaryTextColor.opacity(0.3))

            if viewModel.isLoading && !currentCollectionIsEmpty {
                ProgressView()
                    .progressViewStyle(.linear)
                    .tint(themeManager.accentColor)
                    .accessibilityLabel("Loading history")
            }

            if let error = viewModel.errorMessage, !currentCollectionIsEmpty {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundColor(themeManager.warningColor)
                    Text(error)
                        .font(.themed(12))
                        .foregroundColor(themeManager.textColor)
                        .lineLimit(2)
                    Spacer()
                    Button(action: viewModel.dismissError) {
                        Image(systemName: "xmark")
                            .foregroundColor(themeManager.secondaryTextColor)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Dismiss history error")
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(themeManager.warningColor.opacity(0.12))
            }

            if let error = viewModel.errorMessage, currentCollectionIsEmpty {
                VStack(spacing: 12) {
                    Spacer()
                    Image(systemName: "exclamationmark.triangle")
                        .foregroundColor(themeManager.secondaryTextColor)
                    Text(error)
                        .font(.themed(13))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .multilineTextAlignment(.center)
                    Button("Retry", action: viewModel.fetchData)
                        .font(.themed(13, weight: .semibold))
                        .foregroundColor(themeManager.accentColor)
                    Spacer()
                }
                .padding()
                .frame(maxWidth: .infinity)
            } else if viewModel.isLoading && currentCollectionIsEmpty {
                VStack(spacing: 8) {
                    ForEach(0..<6, id: \.self) { _ in
                        SessionRowSkeleton(themeManager: themeManager)
                    }
                }
                .padding()
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            } else {
                ScrollView {
                    VStack(spacing: 8) {
                        if viewModel.isSearchActive {
                            if viewModel.searchResults.isEmpty {
                                Text("No matching history.")
                                    .foregroundColor(themeManager.secondaryTextColor)
                                    .padding()
                            } else {
                                ForEach(viewModel.searchResults) { result in
                                    searchResultRow(result)
                                }
                            }
                        } else if viewModel.activeTab == "sessions" {
                            if viewModel.sessions.isEmpty {
                                Text("No sessions yet.")
                                    .foregroundColor(themeManager.secondaryTextColor)
                                    .padding()
                            } else {
                                ForEach(viewModel.sessions) { session in
                                    SessionRowView(
                                        session: session,
                                        isActive: viewModel.activeSessionId == session.id,
                                        themeManager: themeManager,
                                        historyLane: nil
                                    )
                                    .onTapGesture {
                                        viewModel.selectSession(session.id)
                                        chatViewModel.loadSession(session.id)
                                        withAnimation {
                                            isPresented = false
                                        }
                                    }
                                    .contextMenu {
                                        if session.isDefaultSession != true && session.internalVoice == nil {
                                            if session.status == "archived" {
                                                Button(action: { viewModel.unarchiveSession(session.id) }) {
                                                    Label("Restore", systemImage: "arrow.uturn.backward")
                                                }
                                            } else {
                                                Button(action: { viewModel.archiveSession(session.id) }) {
                                                    Label("Archive", systemImage: "archivebox")
                                                }
                                            }
                                            Button(role: .destructive, action: {
                                                viewModel.deleteSession(session.id)
                                                if chatViewModel.currentSessionIdValue == session.id {
                                                    chatViewModel.startNewSession(nil)
                                                }
                                            }) {
                                                Label("Delete", systemImage: "trash")
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            if viewModel.threads.isEmpty {
                                Text("No threads yet.")
                                    .foregroundColor(themeManager.secondaryTextColor)
                                    .padding()
                            } else {
                                ForEach(viewModel.threads) { thread in
                                    ThreadRowView(
                                        thread: thread,
                                        isActive: viewModel.activeThreadId == thread.id,
                                        themeManager: themeManager,
                                        historyLane: nil
                                    )
                                    .onTapGesture {
                                        viewModel.openThread(thread) { sessionId in
                                            guard let sessionId else { return }
                                            chatViewModel.loadSession(sessionId)
                                            withAnimation {
                                                isPresented = false
                                            }
                                        }
                                    }
                                    .contextMenu {
                                        if thread.id != "general" {
                                            if thread.archived {
                                                Button(action: { viewModel.unarchiveThread(thread.id) }) {
                                                    Label("Restore", systemImage: "arrow.uturn.backward")
                                                }
                                            } else {
                                                Button(action: { viewModel.archiveThread(thread.id) }) {
                                                    Label("Archive", systemImage: "archivebox")
                                                }
                                            }
                                            Button(role: .destructive, action: { viewModel.deleteThread(thread.id) }) {
                                                Label("Delete", systemImage: "trash")
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    .padding()
                }
            }

            Divider().background(themeManager.secondaryTextColor.opacity(0.3))
            HStack {
                Button(action: viewModel.loadPreviousPage) {
                    Image(systemName: "chevron.left")
                        .frame(width: 30, height: 30)
                }
                .disabled(!viewModel.canLoadPrevious)
                .accessibilityLabel("Previous history page")

                Spacer()
                Text("\(viewModel.pageStart)-\(viewModel.pageEnd) of \(viewModel.total)")
                    .font(.themed(11))
                    .foregroundColor(themeManager.secondaryTextColor)
                Spacer()

                Button(action: viewModel.loadNextPage) {
                    Image(systemName: "chevron.right")
                        .frame(width: 30, height: 30)
                }
                .disabled(!viewModel.canLoadNext)
                .accessibilityLabel("Next history page")
            }
            .buttonStyle(.plain)
            .foregroundColor(themeManager.accentColor)
            .padding(.horizontal)
            .frame(height: 48)
            .background(themeManager.surfaceColor)
        }
        .background(themeManager.backgroundColor.ignoresSafeArea())
        .onAppear {
            viewModel.fetchData()
        }
        .alert("New Thread", isPresented: $showNewThreadPrompt) {
            TextField("Thread name", text: $newThreadName)
            Button("Cancel", role: .cancel) {}
            Button("Create") {
                viewModel.newThread(name: newThreadName) { _ in
                    withAnimation { isPresented = false }
                }
            }
        } message: {
            Text("Name your new thread.")
        }
    }

    @ViewBuilder
    private func searchResultRow(_ result: HistorySearchItem) -> some View {
        if let session = result.session {
            SessionRowView(
                session: session,
                isActive: viewModel.activeSessionId == session.id,
                themeManager: themeManager,
                historyLane: result.historyLane
            )
            .onTapGesture {
                viewModel.selectSession(session.id)
                chatViewModel.loadSession(session.id)
                withAnimation { isPresented = false }
            }
            .contextMenu {
                if session.isDefaultSession != true && session.internalVoice == nil {
                    if session.status == "archived" {
                        Button(action: { viewModel.unarchiveSession(session.id) }) {
                            Label("Restore", systemImage: "arrow.uturn.backward")
                        }
                    } else {
                        Button(action: { viewModel.archiveSession(session.id) }) {
                            Label("Archive", systemImage: "archivebox")
                        }
                    }
                    Button(role: .destructive, action: {
                        viewModel.deleteSession(session.id)
                        if chatViewModel.currentSessionIdValue == session.id {
                            chatViewModel.startNewSession(nil)
                        }
                    }) {
                        Label("Delete", systemImage: "trash")
                    }
                }
            }
        } else if let thread = result.thread {
            ThreadRowView(
                thread: thread,
                isActive: viewModel.activeThreadId == thread.id,
                themeManager: themeManager,
                historyLane: result.historyLane
            )
            .onTapGesture {
                viewModel.openThread(thread) { sessionId in
                    guard let sessionId else { return }
                    chatViewModel.loadSession(sessionId)
                    withAnimation { isPresented = false }
                }
            }
            .contextMenu {
                if thread.id != "general" {
                    if thread.archived {
                        Button(action: { viewModel.unarchiveThread(thread.id) }) {
                            Label("Restore", systemImage: "arrow.uturn.backward")
                        }
                    } else {
                        Button(action: { viewModel.archiveThread(thread.id) }) {
                            Label("Archive", systemImage: "archivebox")
                        }
                    }
                    Button(role: .destructive, action: { viewModel.deleteThread(thread.id) }) {
                        Label("Delete", systemImage: "trash")
                    }
                }
            }
        }
    }
}

struct HistoryLaneButton: View {
    let lane: ChatHistoryLane
    let isActive: Bool
    @ObservedObject var themeManager: ThemeManager
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(lane.title)
                .font(.themed(12, weight: isActive ? .semibold : .regular))
                .foregroundColor(isActive ? themeManager.textColor : themeManager.secondaryTextColor)
                .frame(maxWidth: .infinity)
                .frame(height: 30)
                .background(isActive ? themeManager.surfaceColor : Color.clear)
                .clipShape(RoundedRectangle(cornerRadius: 6))
                .shadow(color: isActive ? Color.black.opacity(0.08) : .clear, radius: 2, y: 1)
        }
        .buttonStyle(.plain)
    }
}

struct SessionRowSkeleton: View {
    @ObservedObject var themeManager: ThemeManager

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 8) {
                RoundedRectangle(cornerRadius: 3)
                    .fill(themeManager.secondaryTextColor.opacity(0.14))
                    .frame(width: 190, height: 13)
                RoundedRectangle(cornerRadius: 3)
                    .fill(themeManager.secondaryTextColor.opacity(0.1))
                    .frame(width: 110, height: 10)
            }
            Spacer()
        }
        .padding(12)
        .background(themeManager.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .accessibilityHidden(true)
    }
}

struct HistoryResultBadge: View {
    let title: String
    let emphasized: Bool
    @ObservedObject var themeManager: ThemeManager

    var body: some View {
        Text(title)
            .font(.themed(10, weight: emphasized ? .semibold : .regular))
            .foregroundColor(emphasized ? themeManager.accentColor : themeManager.secondaryTextColor)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(emphasized ? themeManager.accentColor.opacity(0.1) : themeManager.backgroundColor)
            .clipShape(RoundedRectangle(cornerRadius: 4))
            .overlay(
                RoundedRectangle(cornerRadius: 4)
                    .stroke(
                        emphasized ? themeManager.accentColor.opacity(0.22) : themeManager.secondaryTextColor.opacity(0.14),
                        lineWidth: 1
                    )
            )
    }
}

struct TabButton: View {
    let title: String
    let isActive: Bool
    @ObservedObject var themeManager: ThemeManager
    let action: () -> Void
    
    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.themed(15))
                .bold(isActive)
                .foregroundColor(isActive ? themeManager.accentColor : themeManager.secondaryTextColor)
                .padding(.vertical, 10)
                .padding(.horizontal, 24)
                .frame(maxWidth: .infinity)   // wider tabs: split the bar evenly
                .background(isActive ? themeManager.accentColor.opacity(0.1) : Color.clear)
                .cornerRadius(8)
        }
        .buttonStyle(.plain)
    }
}

struct SessionRowView: View {
    let session: ChatSession
    let isActive: Bool
    @ObservedObject var themeManager: ThemeManager
    let historyLane: ChatHistoryLane?
    
    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text(session.title ?? "Untitled session")
                    .font(.themed(17, weight: .semibold))
                    .foregroundColor(isActive ? themeManager.accentColor : themeManager.textColor)
                    .lineLimit(1)
                
                HStack {
                    Text("#\(session.uiThreadId)")
                        .font(.themed(12))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(themeManager.secondaryTextColor.opacity(0.2))
                        .cornerRadius(4)
                        .foregroundColor(themeManager.textColor)

                    if session.internalVoice?.kind == "branch" {
                        HistoryResultBadge(title: "Concurrent", emphasized: false, themeManager: themeManager)
                    }
                    if let historyLane {
                        HistoryResultBadge(title: "Session", emphasized: false, themeManager: themeManager)
                        HistoryResultBadge(
                            title: historyLane.title,
                            emphasized: historyLane == .personal,
                            themeManager: themeManager
                        )
                    }
                    
                    Text(formatTimestamp(session.updatedAt))
                        .font(.themed(12))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
            }
            
            Spacer()

            if session.isDefaultSession == true {
                Image(systemName: "lock.fill")
                    .foregroundColor(themeManager.secondaryTextColor)
                    .font(.system(size: 10))
                    .accessibilityLabel("Default session")
            } else if isActive {
                Image(systemName: "circle.fill")
                    .foregroundColor(themeManager.accentColor)
                    .font(.system(size: 8))
            }
        }
        .padding(12)
        .background(isActive ? themeManager.accentColor.opacity(0.1) : themeManager.surfaceColor)
        .cornerRadius(12)
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(isActive ? themeManager.accentColor.opacity(0.3) : Color.clear, lineWidth: 1)
        )
    }
    
    private func formatTimestamp(_ ts: Int) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ts / 1000))
        let formatter = DateFormatter()
        formatter.dateStyle = .short
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}

struct ThreadRowView: View {
    let thread: UiThreadRecord
    let isActive: Bool
    @ObservedObject var themeManager: ThemeManager
    let historyLane: ChatHistoryLane?
    
    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text(thread.name)
                    .font(.themed(17, weight: .semibold))
                    .foregroundColor(isActive ? themeManager.accentColor : themeManager.textColor)
                    .lineLimit(1)

                if let historyLane {
                    HStack(spacing: 5) {
                        HistoryResultBadge(title: "Thread", emphasized: false, themeManager: themeManager)
                        HistoryResultBadge(
                            title: historyLane.title,
                            emphasized: historyLane == .personal,
                            themeManager: themeManager
                        )
                    }
                }
                
                if let summary = thread.memorySummary, !summary.isEmpty {
                    Text(summary)
                        .font(.themed(12))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .lineLimit(2)
                }
            }
            
            Spacer()

            if thread.id == "general" {
                Image(systemName: "lock.fill")
                    .foregroundColor(themeManager.secondaryTextColor)
                    .font(.system(size: 10))
                    .accessibilityLabel("Default thread")
            } else if isActive {
                Image(systemName: "circle.fill")
                    .foregroundColor(themeManager.accentColor)
                    .font(.system(size: 8))
            }
        }
        .padding(12)
        .background(isActive ? themeManager.accentColor.opacity(0.1) : themeManager.surfaceColor)
        .cornerRadius(12)
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(isActive ? themeManager.accentColor.opacity(0.3) : Color.clear, lineWidth: 1)
        )
    }
}
