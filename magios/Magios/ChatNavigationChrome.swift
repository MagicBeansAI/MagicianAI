import SwiftUI

/// Compact two-line navigation title for the active chat context. This replaces
/// the floating transcript chip so context remains visible without covering
/// messages.
struct ChatNavigationTitle: View {
    @ObservedObject var chatViewModel: ChatViewModel
    @ObservedObject var threadViewModel: ThreadViewModel
    @ObservedObject private var healthViewModel = HealthViewModel.shared
    @ObservedObject private var themeManager = ThemeManager.shared
    @ObservedObject private var primaryAgent = PrimaryAgentSiriAdvertiser.shared
    /// The agent's live state (web `PresenceOrb`): idle / thinking / working /
    /// needs you. Shown while the service is online; otherwise the dot keeps
    /// the service-health colour.
    var presence: ChatPresenceState = .idle
    var onOpenPanel: () -> Void

    private var assistantName: String { primaryAgent.preferredName ?? MagicianAccess.productName }

    private var presenceLabel: String {
        healthViewModel.magicianState == .online
            ? presence.label(assistant: assistantName)
            : "service \(healthViewModel.magicianState.label)"
    }

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
                    if healthViewModel.magicianState == .online {
                        ChatPresenceDot(state: presence, size: 6)
                            .accessibilityIdentifier("chat-presence-dot")
                    } else {
                        Circle()
                            .fill(healthViewModel.magicianState.color)
                            .frame(width: 6, height: 6)
                    }

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
        .accessibilityLabel("\(threadTitle), \(sessionTitle), \(presenceLabel)")
        .accessibilityHint("Opens chat history")
    }
}

/// Session actions live in the navigation bar instead of sharing space with
/// the chat-context title.
struct ChatSessionActionsMenu: View {
    @ObservedObject var chatViewModel: ChatViewModel
    @ObservedObject var threadViewModel: ThreadViewModel
    var hasThreadContext: Bool
    var onOpenContext: (ChatContextPage) -> Void
    @ObservedObject private var themeManager = ThemeManager.shared

    private var currentSessionId: String? {
        threadViewModel.activeSessionId ?? chatViewModel.currentSessionIdValue
    }

    var body: some View {
        Menu {
            Section {
                ForEach(ChatContextPage.allCases, id: \.self) { page in
                    Button(page.title) { onOpenContext(page) }
                        .disabled(chatViewModel.currentSessionIdValue == nil || ([.tasks, .settings].contains(page) && !hasThreadContext))
                }
            }
            Divider()
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

                // Leave the chat only once the server confirmed the delete.
                Button(role: .destructive) {
                    threadViewModel.deleteSession(currentSessionId) { deleted in
                        if chatViewModel.currentSessionIdValue == deleted {
                            chatViewModel.startNewSession(nil)
                        }
                    }
                } label: {
                    Label(
                        threadViewModel.deletingSessionId == currentSessionId ? "Deleting session…" : "Delete session",
                        systemImage: "trash.fill"
                    )
                }
                .disabled(threadViewModel.deletingSessionId != nil)
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
