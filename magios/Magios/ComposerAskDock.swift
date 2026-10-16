import SwiftUI
import UIKit

/// The questions this conversation is waiting on, above the composer card
/// (web `ComposerAskDock.svelte` + `HitlPromptFields.svelte`): one at a time
/// with a "1 of N" switcher, every simple answer in place (choices,
/// confirm/deny, plain text, plan approval), and secrets, grants, forms and
/// diff reviews handed to the focused Attention surface.
///
/// The items come from this chat session: its own unresolved escalation
/// cards, plus the Attention feed's pending asks scoped to this session /
/// thread (the hitl scope or the feed row's own `ui_thread_id`) or owned by a
/// task known to belong to this thread. Answering posts through the existing
/// respond paths (`ChatViewModel.respondToEscalation`, `AttentionViewModel`),
/// both of which drop the ask from `PendingHitlTracker`, so it clears from
/// every surface.
///
/// The same card answers in place on the task detail screen
/// (`TaskDetailAskSection`, `placement: .taskPanel`), fed by
/// `ComposerAskModel.taskItems`. An ask whose review link names a Work
/// destination (the Relentless "Review draft") shows a Review button that
/// opens it in-app.

// MARK: - Model

/// One answer button.
struct ComposerAskChoice: Identifiable, Equatable {
    enum Action: Equatable {
        /// A server-authored option (choice, confirmation, external action).
        case option(EscalationOptionData)
        /// A confirmation the feed row names only by its labels.
        case confirm(Bool)
        case plan(PlanApprovalChoice)
    }

    let id: String
    let label: String
    let action: Action
    var prominent = false
    var destructive = false
}

/// How the dock answers an ask.
enum ComposerAskForm: Equatable {
    case choices([ComposerAskChoice])
    /// A plain text answer. `guidance` posts advice rather than text.
    case text(placeholder: String, guidance: Bool)
    /// Open the focused Attention surface; the label says what waits there.
    case handoff(String)
}

/// Where an ask came from, which decides the respond path.
enum ComposerAskSource: Equatable {
    case chat(ChatMessageContentData)
    case attention(AttentionItem)
}

/// What the reader picked.
enum ComposerAskReply: Equatable {
    case choice(ComposerAskChoice)
    case text(String)
}

/// A "Review draft"-style link an ask carries (`input_schema.context` or the
/// feed row's `review_href`). A Work destination (`/work?view=quality&run=…`)
/// opens natively through the Work router; an absolute web page opens in the
/// browser; anything else is not offered.
struct HitlReviewLink: Equatable {
    let label: String
    let href: String

    init?(href: String?, label: String?) {
        guard let href = href?.trimmingCharacters(in: .whitespacesAndNewlines), !href.isEmpty else { return nil }
        let trimmed = label?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.href = href
        self.label = trimmed.isEmpty ? "Review" : trimmed
        guard workTarget != nil || externalURL != nil else { return nil }
    }

    /// The native Work destination the link names, if any.
    var workTarget: WorkNavigationTarget? { WorkNavigationTarget.parse(href) }

    /// An absolute web page, when the link is not a Work destination.
    var externalURL: URL? {
        guard workTarget == nil, let url = URL(string: href),
              ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { return nil }
        return url
    }

    /// Open it: the Work router first, else the browser.
    func open() {
        if let target = workTarget {
            AppActions.shared.requestWork(target)
        } else if let url = externalURL {
            UIApplication.shared.open(url)
        }
    }
}

struct ComposerAskItem: Identifiable, Equatable {
    /// The canonical correlation id: what `/hitl/{id}/respond` posts to and
    /// what dedupes a chat card against its feed row.
    let id: String
    let prompt: String
    let hint: String?
    /// "from <agent>" for a delegated child's ask.
    let from: String?
    let taskId: String?
    let form: ComposerAskForm
    let source: ComposerAskSource
    /// What to review before answering ("Review draft").
    var review: HitlReviewLink? = nil
}

enum ComposerAskModel {
    /// The handoff labels (web `HANDOFF`), by input type.
    static func handoffLabel(inputType: String, sensitive: Bool) -> String? {
        if sensitive { return "Enter it securely →" }
        switch inputType {
        case "password", "otp": return "Enter it securely →"
        case "tool_authorization", "sandbox_override": return "Review and decide →"
        case "diff_approval": return "Review the changes →"
        case "form", "file_path", "multi_choice", "external_action": return "Answer in Attention →"
        default: return nil
        }
    }

    // MARK: Builders

    /// A chat escalation card that still waits on the reader.
    static func item(chat content: ChatMessageContentData) -> ComposerAskItem? {
        guard content.resolved != true,
              let question = nonEmpty(content.question),
              let correlationId = content.hitlCorrelationId else { return nil }
        let inputType = content.hitlInputType
        let options = content.options ?? []
        let sensitive = content.inputSchema?.sensitive != nil
        let form: ComposerAskForm
        if let handoff = handoffLabel(inputType: inputType, sensitive: sensitive) {
            form = .handoff(handoff)
        } else if ChatHitlResponseComposer.needsCanonicalAttentionFallback(
            inputType: inputType, options: options,
            allowsOther: content.inputSchema?.allowOther == true,
            inputTypeIsAuthoritative: content.inputType != nil) {
            form = .handoff("Answer in Attention →")
        } else if inputType == "text" || inputType == "guidance" {
            form = .text(placeholder: content.inputSchema?.placeholder ?? "Type your answer…",
                         guidance: inputType == "guidance")
        } else if options.contains(where: { $0.requiresInput == true }) || content.inputSchema?.allowOther == true {
            // An option that needs words of its own: the full card has room.
            form = .handoff("Answer in Attention →")
        } else {
            let choices = options.compactMap { option -> ComposerAskChoice? in
                guard ChatHitlResponseComposer.compose(inputType: inputType, option: option) != nil else { return nil }
                return ComposerAskChoice(
                    id: option.id, label: option.label, action: .option(option),
                    prominent: option.action?.confirmed == true || isApprove(option.id),
                    destructive: option.action?.confirmed == false || isReject(option.id))
            }
            form = choices.isEmpty ? .handoff("Answer in Attention →") : .choices(choices)
        }
        let context = content.inputSchema?.context
        return ComposerAskItem(
            id: correlationId, prompt: question, hint: nonEmpty(content.hint), from: nil,
            taskId: content.hitlTarget?.taskId ?? nonEmpty(content.taskId),
            form: form, source: .chat(content),
            review: HitlReviewLink(href: context?.reviewHref, label: context?.reviewLabel))
    }

    /// An option that approves / rejects (the Relentless review's
    /// `approve` / `reject`) is styled as the decision it is.
    static func isApprove(_ id: String) -> Bool { id == "approve" }
    static func isReject(_ id: String) -> Bool { id == "reject" }

    /// A pending Attention feed ask.
    static func item(attention item: AttentionItem) -> ComposerAskItem? {
        guard item.isActionable, !item.isStalledWait else { return nil }
        let inputType = item.inputType
        let form: ComposerAskForm
        if PlanApprovalActions.isPlanApproval(source: item.source, inputType: inputType) {
            form = .choices(PlanApprovalActions.choices(hasTask: item.owningTaskId != nil).map { choice in
                ComposerAskChoice(
                    id: choice.rawValue,
                    label: choice.label(confirmLabel: item.schema?.confirmLabel, denyLabel: item.schema?.denyLabel),
                    action: .plan(choice),
                    prominent: choice == .approveAndRun,
                    destructive: choice == .reject)
            })
        } else if item.isMcpOAuth {
            form = .handoff("Sign in →")
        } else if item.alignment != nil {
            // A preference alignment hold is answered where its reason shows
            // (`AlignmentAskView`): a bare confirm here would consent unseen.
            form = .handoff("Review in Attention →")
        } else if let handoff = handoffLabel(inputType: inputType, sensitive: item.isSensitive) {
            form = .handoff(handoff)
        } else {
            switch inputType {
            case "text", "guidance":
                form = .text(placeholder: item.placeholder, guidance: inputType == "guidance")
            case "confirmation":
                form = .choices([
                    ComposerAskChoice(id: "deny", label: item.denyLabel, action: .confirm(false),
                                      destructive: item.isDestructive),
                    ComposerAskChoice(id: "confirm", label: item.confirmLabel, action: .confirm(true),
                                      prominent: true),
                ])
            case "choice":
                let options = item.options
                if options.isEmpty || options.contains(where: { $0.requiresInput == true }) || item.schema?.allowOther == true {
                    form = .handoff("Answer in Attention →")
                } else {
                    form = .choices(options.map {
                        ComposerAskChoice(id: $0.id, label: $0.label, action: .option($0),
                                          prominent: isApprove($0.id), destructive: isReject($0.id))
                    })
                }
            default:
                form = .handoff("Answer in Attention →")
            }
        }
        return ComposerAskItem(
            id: item.correlationId, prompt: item.prompt, hint: nonEmpty(item.metadata?.hint),
            from: item.delegatedFromLabel, taskId: item.owningTaskId, form: form, source: .attention(item),
            review: item.reviewLink)
    }

    /// Whether a feed ask belongs to this conversation: scoped to the session
    /// or thread (the hitl scope, or the feed row's own `ui_thread_id` — an
    /// ask from Relentless / quality work carries only that and its task), or
    /// owned by a task known to belong to this thread.
    static func belongs(_ item: AttentionItem, sessionId: String?, threadId: String?, chatTaskIds: Set<String>) -> Bool {
        if belongsToThread(item, sessionId: sessionId, threadId: threadId) { return true }
        if let task = item.owningTaskId, chatTaskIds.contains(task) { return true }
        return false
    }

    /// Scoped to the session or thread by the ask's own stamps.
    static func belongsToThread(_ item: AttentionItem, sessionId: String?, threadId: String?) -> Bool {
        let scope = item.metadata?.hitlRequest?.scope ?? [:]
        if let sessionId, !sessionId.isEmpty, (scope["session_id"] ?? nil) == sessionId { return true }
        guard let threadId, !threadId.isEmpty else { return false }
        return (scope["thread_id"] ?? nil) == threadId
            || (scope["ui_thread_id"] ?? nil) == threadId
            || item.uiThreadId == threadId
    }

    /// The tasks known to belong to this conversation: the chat's task cards,
    /// the tasks its own escalation cards name, and the owning task of every
    /// feed ask stamped with this session / thread — so a sibling ask of the
    /// same task that carries only `task_id` still belongs.
    static func threadTaskIds(
        chatTaskIds: Set<String>,
        chatItems: [ComposerAskItem],
        attention: [AttentionItem],
        sessionId: String?,
        threadId: String?
    ) -> Set<String> {
        var ids = chatTaskIds
        for item in chatItems { if let task = item.taskId, !task.isEmpty { ids.insert(task) } }
        for row in attention where belongsToThread(row, sessionId: sessionId, threadId: threadId) {
            if let task = row.owningTaskId { ids.insert(task) }
        }
        return ids
    }

    /// What one task waits on (the task detail screen's ask card): the
    /// Attention feed's pending asks owned by the task, then the execution
    /// panel's own `needs_attention` asks the feed's loaded pages do not
    /// carry, deduped by correlation id. `feed` is expected already filtered
    /// of answered-elsewhere rows; `isSettled` drops a panel row answered on
    /// any surface since the panel was read.
    static func taskItems(
        taskId: String,
        panelRows: [AttentionItem],
        feed: [AttentionItem],
        isSettled: (AttentionItem) -> Bool = { _ in false }
    ) -> [ComposerAskItem] {
        var seen = Set<String>()
        var out: [ComposerAskItem] = []
        for row in feed where row.owningTaskId == taskId {
            guard let item = item(attention: row), seen.insert(item.id).inserted else { continue }
            out.append(item)
        }
        for row in panelRows where row.metadata?.hitlRequest != nil && !isSettled(row) {
            guard let item = item(attention: row), seen.insert(item.id).inserted else { continue }
            out.append(item)
        }
        return out
    }

    /// Everything this conversation waits on, chat cards first (transcript
    /// order), then feed asks; deduped by correlation id. A plan approval the
    /// planner dock already offers is left to it, and an archived (read-only)
    /// conversation asks nothing.
    static func items(
        chatEscalations: [ChatMessageContentData],
        attention: [AttentionItem],
        sessionId: String?,
        threadId: String?,
        chatTaskIds: Set<String>,
        plannerTaskIds: Set<String>,
        readOnly: Bool
    ) -> [ComposerAskItem] {
        guard !readOnly else { return [] }
        var seen = Set<String>()
        var out: [ComposerAskItem] = []
        for content in chatEscalations {
            guard let item = item(chat: content), seen.insert(item.id).inserted else { continue }
            out.append(item)
        }
        let threadTasks = threadTaskIds(chatTaskIds: chatTaskIds, chatItems: out, attention: attention,
                                        sessionId: sessionId, threadId: threadId)
        for row in attention where belongs(row, sessionId: sessionId, threadId: threadId, chatTaskIds: threadTasks) {
            guard let item = item(attention: row) else { continue }
            if case .choices(let choices) = item.form,
               choices.contains(where: { if case .plan = $0.action { return true } else { return false } }),
               let task = item.taskId, plannerTaskIds.contains(task) { continue }
            guard seen.insert(item.id).inserted else { continue }
            out.append(item)
        }
        return out
    }

    // MARK: Switcher

    /// The active question survives a new arrival; it moves on only when it leaves.
    static func activeId(current: String?, in items: [ComposerAskItem]) -> String? {
        if let current, items.contains(where: { $0.id == current }) { return current }
        return items.first?.id
    }

    /// Previous / next, wrapping.
    static func step(from current: String?, by delta: Int, in items: [ComposerAskItem]) -> String? {
        guard !items.isEmpty else { return nil }
        let index = items.firstIndex(where: { $0.id == current }) ?? 0
        let next = ((index + delta) % items.count + items.count) % items.count
        return items[next].id
    }

    /// "1 of 3", or nil for a single question.
    static func position(of current: String?, in items: [ComposerAskItem]) -> String? {
        guard items.count > 1, let index = items.firstIndex(where: { $0.id == current }) else { return nil }
        return "\(index + 1) of \(items.count)"
    }

    /// The chat respond path's submission for a reply, or nil when the reply
    /// cannot be expressed for this card.
    static func submission(for content: ChatMessageContentData, reply: ComposerAskReply) -> ChatHitlSubmission? {
        let inputType = content.hitlInputType
        switch reply {
        case .text(let text):
            return ChatHitlResponseComposer.compose(inputType: inputType, text: text,
                                                    sensitive: content.inputSchema?.sensitive != nil)
        case .choice(let choice):
            guard case .option(let option) = choice.action else { return nil }
            return ChatHitlResponseComposer.compose(inputType: inputType, option: option)
        }
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines), !value.isEmpty else { return nil }
        return value
    }
}

// MARK: - Answering

extension AttentionViewModel {
    /// Answer a feed ask from an in-place card (the composer ask dock, the
    /// task detail screen) through the same optimistic respond path the
    /// Attention sheet uses: the row leaves every surface at once and comes
    /// back, with `mutationError`, only if the post fails. Nil when posted,
    /// else why it cannot be sent.
    ///
    /// `vouched`: the task panel read this ask from the execution panel and
    /// the feed's loaded pages do not carry it — its absence there is not an
    /// answer given elsewhere, so the stale-answer guard must not refuse it.
    func answerInPlace(_ row: AttentionItem, form: ComposerAskForm, reply: ComposerAskReply,
                       vouched: Bool = false) -> String? {
        if vouched { vouchPending(row) }
        switch reply {
        case .text(let text):
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { return "Type an answer first." }
            if case .text(_, true) = form { submitGuidance(row, trimmed) } else { submitText(row, trimmed) }
        case .choice(let choice):
            switch choice.action {
            case .option(let option): submitChoice(row, optionId: option.id)
            case .confirm(let confirmed): submitConfirmation(row, confirmed: confirmed)
            case .plan(let decision): submitPlanApproval(row, choice: decision)
            }
        }
        return nil
    }
}

/// Presents an ask's focused answer sheet (`AttentionDetailModal` — forms,
/// secrets, grants, diffs) in place over the current screen, and closes it
/// the moment the ask is settled on another surface, exactly as the
/// Attention list does, so nothing stale can be submitted from it.
struct AttentionAskSheet: ViewModifier {
    @Binding var row: AttentionItem?
    @ObservedObject var viewModel: AttentionViewModel

    func body(content: Content) -> some View {
        content
            .sheet(item: $row) { AttentionDetailModal(item: $0, viewModel: viewModel) }
            .onReceive(viewModel.$lanes.dropFirst()) { close(lanes: $0) }
            .onReceive(viewModel.$remotelyResolved.dropFirst()) { close(remotelyResolved: $0) }
    }

    /// The publishers fire in `willSet`, so the emitted value is passed through.
    private func close(lanes: [String: [AttentionItem]]? = nil, remotelyResolved: Set<String>? = nil) {
        guard let open = row,
              viewModel.isAnsweredElsewhere(open, lanes: lanes, remotelyResolved: remotelyResolved) else { return }
        row = nil
        viewModel.showAnsweredElsewhereNotice()
    }
}

extension View {
    func attentionAskSheet(_ row: Binding<AttentionItem?>, viewModel: AttentionViewModel) -> some View {
        modifier(AttentionAskSheet(row: row, viewModel: viewModel))
    }
}

private struct AttentionFeedKey: EnvironmentKey {
    static let defaultValue: AttentionViewModel? = nil
}

extension EnvironmentValues {
    /// The app's shared Attention feed (set by `AppTabView`), so a pushed or
    /// presented screen answers through the same view model the Attention tab,
    /// its badge and the composer dock read. Nil outside the tab shell.
    var attentionFeed: AttentionViewModel? {
        get { self[AttentionFeedKey.self] }
        set { self[AttentionFeedKey.self] = newValue }
    }
}

// MARK: - View

/// Where the ask card sits: above the chat composer, or on the task detail
/// screen. Same card, same controls, same answer paths.
enum ComposerAskPlacement {
    case composer
    case taskPanel
}

struct ComposerAskDock: View {
    let items: [ComposerAskItem]
    var disabled = false
    /// A failure reported by the feed after an optimistic answer (the ask
    /// comes back with it).
    var notice: String? = nil
    /// Post the reply. Nil on success, else the reason, shown inline.
    var answer: (ComposerAskItem, ComposerAskReply) async -> String?
    /// Open the focused Attention surface for this ask.
    var onHandoff: (ComposerAskItem) -> Void
    /// A plan approval's Review: open the plan.
    var onReviewPlan: (String) -> Void
    var placement: ComposerAskPlacement = .composer
    /// The ask's review link ("Review draft"): the Work router by default.
    var onReview: (HitlReviewLink) -> Void = { $0.open() }

    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var activeId: String?
    @State private var text = ""
    @State private var busyChoice: String?
    @State private var error: (id: String, message: String)?
    /// Answered here: hidden at once, whatever the feed says next.
    @State private var answered: Set<String> = []
    /// The brief "Answered" acknowledgement before the dock moves on.
    @State private var acknowledged: ComposerAskItem?

    private var visible: [ComposerAskItem] { items.filter { !answered.contains($0.id) } }
    private var active: ComposerAskItem? {
        let id = ComposerAskModel.activeId(current: activeId, in: visible)
        return visible.first { $0.id == id }
    }

    var body: some View {
        Group {
            if let acknowledged {
                card(acknowledged, answeredState: true)
            } else if let active {
                card(active, answeredState: false)
            }
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: active?.id)
        .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: acknowledged?.id)
        .onChange(of: active?.id) { _, _ in text = "" }
        // An answered ask that left the list is forgotten, so one that comes
        // back (a failed optimistic post) shows again.
        .onChange(of: items.map(\.id)) { _, ids in answered.formIntersection(ids) }
    }

    private func card(_ item: ComposerAskItem, answeredState: Bool) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            header(item, answeredState: answeredState)
            Text(item.prompt)
                .font(.themed(14))
                .foregroundColor(answeredState ? theme.secondaryTextColor : theme.textColor)
                .fixedSize(horizontal: false, vertical: true)
                .lineLimit(answeredState ? 2 : 6)
            if !answeredState {
                if let hint = item.hint {
                    Text(hint).font(.themed(12)).foregroundColor(theme.secondaryTextColor).lineLimit(3)
                }
                if let review = item.review {
                    Button { onReview(review) } label: {
                        Label(review.label, systemImage: "arrow.up.right.square")
                    }
                    .buttonStyle(ChipStyle())
                    .frame(minHeight: Lively.tapMin)
                    .accessibilityIdentifier("composer-ask-review")
                }
                controls(item)
                if let message = (error?.id == item.id ? error?.message : nil) ?? notice {
                    Text(message)
                        .font(.themed(12))
                        .foregroundColor(theme.dangerColor)
                        .accessibilityIdentifier("composer-ask-error")
                }
            }
        }
        .padding(.horizontal, placement == .composer ? 12 : 14)
        .padding(.vertical, placement == .composer ? 10 : 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(placement == .composer ? theme.surfaceColor : theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                .stroke((answeredState ? theme.successColor : theme.warningColor).opacity(0.45), lineWidth: 1)
        )
        .padding(.horizontal, placement == .composer ? ComposerView.horizontalInset : 0)
        .padding(.bottom, placement == .composer ? 6 : 0)
        .transition(reduceMotion ? .identity : .move(edge: .bottom).combined(with: .opacity))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(answeredState ? "Answered" : "Waiting on you")
        .accessibilityIdentifier(placement == .composer ? "composer-ask-dock" : "task-ask-card")
    }

    private var cornerRadius: CGFloat {
        placement == .composer ? Lively.composerRadius : Lively.cardRadius
    }

    private func header(_ item: ComposerAskItem, answeredState: Bool) -> some View {
        HStack(spacing: 8) {
            Text(answeredState ? "Answered" : "Waiting on you")
                .font(.themed(11, weight: .semibold))
                .foregroundColor(theme.textColor)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
                .background(
                    RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                        .fill((answeredState ? theme.successColor : theme.warningColor).opacity(0.3))
                )
            if let from = item.from, !answeredState {
                Text(from).font(.themed(12)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
            }
            Spacer(minLength: 4)
            if !answeredState, let position = ComposerAskModel.position(of: item.id, in: visible) {
                HStack(spacing: 2) {
                    switchButton("chevron.left", label: "Previous question", delta: -1)
                    Text(position)
                        .font(.themed(11))
                        .monospacedDigit()
                        .foregroundColor(theme.secondaryTextColor)
                    switchButton("chevron.right", label: "Next question", delta: 1)
                }
                .accessibilityElement(children: .contain)
            }
        }
    }

    private func switchButton(_ icon: String, label: String, delta: Int) -> some View {
        Button {
            activeId = ComposerAskModel.step(from: active?.id, by: delta, in: visible)
        } label: {
            Image(systemName: icon).font(.system(size: 11, weight: .semibold))
                .foregroundColor(theme.textColor)
                .frame(width: 26, height: 22)
                .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                    .fill(theme.controlColor))
                .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                    .strokeBorder(theme.controlBorderColor, lineWidth: 1))
                .contentShape(Rectangle().inset(by: -10))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    @ViewBuilder
    private func controls(_ item: ComposerAskItem) -> some View {
        switch item.form {
        case .handoff(let label):
            Button(label) { onHandoff(item) }
                .buttonStyle(ChipStyle())
                .disabled(disabled)
                .frame(minHeight: Lively.tapMin)
                .accessibilityIdentifier("composer-ask-handoff")
        case .text(let placeholder, _):
            HStack(spacing: 8) {
                TextField(placeholder, text: $text, axis: .vertical)
                    .lineLimit(1...3)
                    .font(.themed(14))
                    .foregroundColor(theme.textColor)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                        .fill(theme.backgroundColor.opacity(0.6)))
                    .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                        .strokeBorder(theme.controlBorderColor, lineWidth: 1))
                    .accessibilityIdentifier("composer-ask-text")
                Button {
                    send(item, .text(text), key: "text")
                } label: {
                    if busyChoice == "text" { ProgressView().controlSize(.mini) } else { Text("Send") }
                }
                .buttonStyle(ChipStyle(prominent: true))
                .disabled(disabled || busyChoice != nil || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        case .choices(let choices):
            ComposerAskFlow(spacing: 6) {
                ForEach(choices) { choice in
                    Button {
                        if case .plan(.review) = choice.action {
                            if let task = item.taskId { onReviewPlan(task) }
                        } else {
                            send(item, .choice(choice), key: choice.id)
                        }
                    } label: {
                        HStack(spacing: 4) {
                            if busyChoice == choice.id { ProgressView().controlSize(.mini) }
                            Text(choice.label)
                        }
                        .foregroundColor(choice.destructive && !choice.prominent ? theme.dangerColor : nil)
                    }
                    .buttonStyle(ChipStyle(prominent: choice.prominent))
                    .disabled(disabled || busyChoice != nil)
                    .accessibilityIdentifier("composer-ask-choice-\(choice.id)")
                }
            }
        }
    }

    private func send(_ item: ComposerAskItem, _ reply: ComposerAskReply, key: String) {
        guard busyChoice == nil else { return }
        busyChoice = key
        error = nil
        Task { @MainActor in
            let failure = await answer(item, reply)
            busyChoice = nil
            if let failure {
                error = (item.id, failure)
                return
            }
            answered.insert(item.id)
            text = ""
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.2)) { acknowledged = item }
            try? await Task.sleep(nanoseconds: 1_200_000_000)
            withAnimation(reduceMotion ? nil : .easeIn(duration: 0.2)) {
                if acknowledged?.id == item.id { acknowledged = nil }
            }
        }
    }
}

// MARK: - Task detail

/// What a task waits on, answered in place on its detail screen: the same
/// card, controls and respond paths as the composer ask dock. Reads the
/// app's shared Attention feed when the tab shell provides it (so an answer
/// here clears the dock, the badge and the Attention list at once), else a
/// feed of its own.
struct TaskDetailAskSection: View {
    let taskId: String
    /// The execution panel's own asks for this task (`TaskDetailSnapshot.askRows`).
    let panelRows: [AttentionItem]
    /// A plan approval's Review: open the plan.
    var onReviewPlan: (String) -> Void = { _ in }
    /// After an answer posts: re-read the panel so its row settles too.
    var onAnswered: () -> Void = {}

    @Environment(\.attentionFeed) private var sharedFeed

    var body: some View {
        if let sharedFeed {
            TaskAskCard(taskId: taskId, panelRows: panelRows, attention: sharedFeed,
                        onReviewPlan: onReviewPlan, onAnswered: onAnswered)
        } else {
            OwnedFeedTaskAskCard(taskId: taskId, panelRows: panelRows,
                                 onReviewPlan: onReviewPlan, onAnswered: onAnswered)
        }
    }
}

private struct OwnedFeedTaskAskCard: View {
    let taskId: String
    let panelRows: [AttentionItem]
    let onReviewPlan: (String) -> Void
    let onAnswered: () -> Void
    @StateObject private var attention = AttentionViewModel()

    var body: some View {
        TaskAskCard(taskId: taskId, panelRows: panelRows, attention: attention,
                    onReviewPlan: onReviewPlan, onAnswered: onAnswered)
    }
}

private struct TaskAskCard: View {
    let taskId: String
    let panelRows: [AttentionItem]
    @ObservedObject var attention: AttentionViewModel
    let onReviewPlan: (String) -> Void
    let onAnswered: () -> Void
    @State private var sheetRow: AttentionItem?
    @State private var awaitingFeed = false

    private var feedRows: [AttentionItem] {
        attention.items("all").filter { !attention.isAnsweredElsewhere($0) }
    }

    private var items: [ComposerAskItem] {
        ComposerAskModel.taskItems(taskId: taskId, panelRows: panelRows, feed: feedRows,
                                   isSettled: { attention.isSettled($0) })
    }

    /// The panel read this ask but the feed's loaded pages do not carry it.
    private func panelOnly(_ row: AttentionItem) -> Bool {
        !attention.items("all").contains { $0.correlationId == row.correlationId }
    }

    var body: some View {
        VStack(spacing: 0) {
            ComposerAskDock(
                items: items,
                notice: awaitingFeed ? attention.mutationError : nil,
                answer: answer,
                onHandoff: { item in
                    guard case .attention(let row) = item.source else { return }
                    if panelOnly(row) { attention.vouchPending(row) }
                    sheetRow = row
                },
                onReviewPlan: onReviewPlan,
                placement: .taskPanel)
        }
        .attentionAskSheet($sheetRow, viewModel: attention)
        .onAppear { attention.fetch() }
        .onChange(of: sheetRow?.id) { old, new in
            // The focused sheet closed: if it answered, the panel catches up.
            if old != nil, new == nil { onAnswered() }
        }
    }

    private func answer(_ item: ComposerAskItem, _ reply: ComposerAskReply) async -> String? {
        guard case .attention(let row) = item.source else { return "Open it in Attention to answer." }
        awaitingFeed = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 15) { awaitingFeed = false }
        let failure = attention.answerInPlace(row, form: item.form, reply: reply, vouched: panelOnly(row))
        if failure == nil { onAnswered() }
        return failure
    }
}

/// Wraps chips onto as many rows as they need.
struct ComposerAskFlow: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0, widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width { x = 0; y += rowHeight + spacing; rowHeight = 0 }
            x += size.width + spacing
            widest = max(widest, x - spacing)
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: min(widest, width), height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, rowHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX { x = bounds.minX; y += rowHeight + spacing; rowHeight = 0 }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}
