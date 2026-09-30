import SwiftUI

/// Compact two-line navigation title for the active chat context. This replaces
/// the floating transcript chip so context remains visible without covering
/// messages.
struct ChatNavigationTitle: View {
    @ObservedObject var chatViewModel: ChatViewModel
    @ObservedObject var threadViewModel: ThreadViewModel
    @ObservedObject private var healthViewModel = HealthViewModel.shared
    @ObservedObject private var themeManager = ThemeManager.shared
    var onOpenPanel: () -> Void

    private var currentSessionId: String? {
        threadViewModel.activeSessionId ?? chatViewModel.currentSessionIdValue
    }

    private var currentSession: ChatSession? {
        guard let currentSessionId else { return nil }
        return threadViewModel.sessionMetadata(for: currentSessionId)
    }

    private var threadId: String {
        currentSession?.uiThreadId ?? threadViewModel.activeThreadId
    }

    private var threadTitle: String {
        if let name = threadViewModel.threadMetadata(for: threadId)?.name,
           !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return name
        }
        return threadId == "general" ? "General" : threadId
    }

    private var sessionTitle: String {
        if let title = currentSession?.title?.trimmingCharacters(in: .whitespacesAndNewlines),
           !title.isEmpty {
            return title
        }
        return currentSessionId == nil ? "New session" : "Untitled session"
    }

    var body: some View {
        Button(action: onOpenPanel) {
            VStack(spacing: 1) {
                Text(threadTitle)
                    .font(.themed(15, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                    .lineLimit(1)

                HStack(spacing: 5) {
                    Circle()
                        .fill(healthViewModel.magicianState.color)
                        .frame(width: 6, height: 6)

                    Text(sessionTitle)
                        .font(.themed(11, weight: .regular))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: 210)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            "\(threadTitle), \(sessionTitle), service \(healthViewModel.magicianState.label)"
        )
        .accessibilityHint("Opens chat history")
    }
}

/// Session actions live in the navigation bar instead of sharing space with
/// the chat-context title.
struct ChatSessionActionsMenu: View {
    @ObservedObject var chatViewModel: ChatViewModel
    @ObservedObject var threadViewModel: ThreadViewModel
    @ObservedObject private var themeManager = ThemeManager.shared

    private var currentSessionId: String? {
        threadViewModel.activeSessionId ?? chatViewModel.currentSessionIdValue
    }

    var body: some View {
        Menu {
            Button {
                chatViewModel.clearMessages()
            } label: {
                Label("Clear chat", systemImage: "trash")
            }

            if let currentSessionId {
                Button {
                    threadViewModel.archiveSession(currentSessionId)
                    chatViewModel.startNewSession(nil)
                } label: {
                    Label("Archive session", systemImage: "archivebox")
                }

                Button(role: .destructive) {
                    threadViewModel.deleteSession(currentSessionId)
                    chatViewModel.startNewSession(nil)
                } label: {
                    Label("Delete session", systemImage: "trash.fill")
                }
            }
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 18, weight: .semibold))
                .foregroundColor(themeManager.textColor)
                .frame(width: 36, height: 36)
                .contentShape(Rectangle())
        }
        .accessibilityLabel("Chat options")
    }
}
