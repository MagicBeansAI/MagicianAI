import Foundation

/// Plan approval — **Review plan · Reject · Approve · Approve & Run**.
///
/// iOS port of the web `planReview` prompt (`ui/unified-ui/src/lib/hitl/promptFor.ts`,
/// `HitlPromptFields.svelte`) and `startRunAfterPlanApproval`
/// (`respondToHitl.ts`). Approving a plan does not start a run, so the two
/// affirmatives are spelled out: Approve keeps it for later, Approve & Run
/// starts it now. Approve & Run posts the same answer as Approve
/// (`{type: confirmation, confirmed: true}`), then `POST /v3/tasks/{id}/execute`.
/// **The approval stands if the start fails**: the outcome says so and the
/// task's Run is still there.
///
/// Every surface that answers a plan approval (the Attention popup, the chat's
/// live task card) goes through this type, so the two-step order and the
/// wording live in one place.

/// One button of a plan-approval prompt, in display order.
enum PlanApprovalChoice: String, CaseIterable, Equatable, Identifiable {
    case review, reject, approve, approveAndRun

    var id: String { rawValue }

    /// The fixed labels. Reject and Approve take the backend's own labels when
    /// it named them (`deny_label` / `confirm_label`).
    func label(confirmLabel: String? = nil, denyLabel: String? = nil) -> String {
        switch self {
        case .review: return "Review plan"
        case .reject: return Self.nonEmpty(denyLabel) ?? "Reject"
        case .approve: return Self.nonEmpty(confirmLabel) ?? "Approve"
        case .approveAndRun: return "Approve & Run"
        }
    }

    /// The `confirmed` value posted, or nil for Review (which posts nothing).
    var confirmed: Bool? {
        switch self {
        case .review: return nil
        case .reject: return false
        case .approve, .approveAndRun: return true
        }
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines), !value.isEmpty else { return nil }
        return value
    }
}

/// The answer to send: who asked, and the task the plan belongs to.
struct PlanApprovalRequest: Equatable {
    /// `/hitl/{correlationId}/respond`.
    let correlationId: String
    /// The task the plan belongs to; Approve & Run starts its run.
    let taskId: String?
    /// The run the answer resumes, when the ask names one.
    var executionId: String? = nil
    var source: String = "plan_approval"
    var channel: String = "ios"
}

/// The approval POST's result.
enum PlanApprovalPost: Equatable {
    case ok
    case failed(String)
}

/// What happened, and the one line the surface shows for it.
enum PlanApprovalOutcome: Equatable {
    /// Approve alone: approved, no run started.
    case approved
    /// Approve & Run: approved and the run started.
    case approvedAndStarted
    /// Approve & Run: approved, but the run did not start. The approval stands.
    case approvedRunNotStarted(String)
    case rejected
    /// The approval (or rejection) itself did not land; nothing changed.
    case failed(String)

    /// The plan is approved, whether or not a run started.
    var isApproved: Bool {
        switch self {
        case .approved, .approvedAndStarted, .approvedRunNotStarted: return true
        case .rejected, .failed: return false
        }
    }

    var message: String {
        switch self {
        case .approved: return "Plan approved. Run it when you're ready."
        case .approvedAndStarted: return "Plan approved. The run has started."
        case .approvedRunNotStarted(let reason):
            let detail = reason.trimmingCharacters(in: .whitespacesAndNewlines)
            return detail.isEmpty
                ? "Approved — the run did not start."
                : "Approved — the run did not start. \(detail)"
        case .rejected: return "Plan rejected. It will not run."
        case .failed(let reason): return reason
        }
    }
}

/// The two calls Approve & Run makes. Live in the app, stubbed in tests.
protocol PlanApprovalClient {
    /// POST the confirmation to `/api/magician/v2/hitl/{id}/respond`.
    func respond(_ request: PlanApprovalRequest, confirmed: Bool) async -> PlanApprovalPost
    /// `POST /api/magician/v3/tasks/{id}/execute`. Throws when the run did not start.
    func startRun(taskId: String) async throws
}

enum PlanApprovalActions {
    /// The buttons, in order. Review only when there is a task to open.
    static func choices(hasTask: Bool) -> [PlanApprovalChoice] {
        hasTask ? PlanApprovalChoice.allCases : PlanApprovalChoice.allCases.filter { $0 != .review }
    }

    /// A plan approval is a `plan_approval` confirmation (web `planReviewTaskId`).
    static func isPlanApproval(source: String?, inputType: String?) -> Bool {
        source == "plan_approval" && inputType == "confirmation"
    }

    /// Approve & Run's core: run `approve`, and only once it landed start the
    /// task's run. A failed start keeps the approval and says so.
    static func approveAndRun(
        taskId: String?,
        approve: () async -> PlanApprovalPost,
        startRun: (String) async throws -> Void
    ) async -> PlanApprovalOutcome {
        let posted = await approve()
        guard case .ok = posted else {
            if case .failed(let reason) = posted { return .failed(reason) }
            return .failed("The approval was not sent.")
        }
        guard let taskId = taskId?.trimmingCharacters(in: .whitespacesAndNewlines), !taskId.isEmpty else {
            return .approvedRunNotStarted("This plan names no task to run.")
        }
        do {
            try await startRun(taskId)
            return .approvedAndStarted
        } catch {
            return .approvedRunNotStarted(error.localizedDescription)
        }
    }

    /// Approve & Run through a client: post the confirmation, then start the run.
    static func approveAndRun(
        _ request: PlanApprovalRequest,
        client: PlanApprovalClient = LivePlanApprovalClient()
    ) async -> PlanApprovalOutcome {
        await approveAndRun(
            taskId: request.taskId,
            approve: { await client.respond(request, confirmed: true) },
            startRun: { try await client.startRun(taskId: $0) }
        )
    }

    /// Approve alone: post the confirmation; no run starts.
    static func approve(
        _ request: PlanApprovalRequest,
        client: PlanApprovalClient = LivePlanApprovalClient()
    ) async -> PlanApprovalOutcome {
        switch await client.respond(request, confirmed: true) {
        case .ok: return .approved
        case .failed(let reason): return .failed(reason)
        }
    }

    static func reject(
        _ request: PlanApprovalRequest,
        client: PlanApprovalClient = LivePlanApprovalClient()
    ) async -> PlanApprovalOutcome {
        switch await client.respond(request, confirmed: false) {
        case .ok: return .rejected
        case .failed(let reason): return .failed(reason)
        }
    }

    /// Any of the three answering choices. Review posts nothing: nil.
    static func perform(
        _ choice: PlanApprovalChoice,
        _ request: PlanApprovalRequest,
        client: PlanApprovalClient = LivePlanApprovalClient()
    ) async -> PlanApprovalOutcome? {
        switch choice {
        case .review: return nil
        case .reject: return await reject(request, client: client)
        case .approve: return await approve(request, client: client)
        case .approveAndRun: return await approveAndRun(request, client: client)
        }
    }

    /// The respond body (web `resolveHitlCall` parity: planning sources name the task).
    static func respondPayload(_ request: PlanApprovalRequest, confirmed: Bool) -> [String: Any] {
        var payload: [String: Any] = [
            "source": request.source,
            "channel": request.channel,
            "value": ["type": "confirmation", "confirmed": confirmed],
            "input_type": "confirmation",
            "correlation_id": request.correlationId,
            "pause_state_id": request.correlationId
        ]
        if let executionId = request.executionId, !executionId.isEmpty { payload["execution_id"] = executionId }
        if let taskId = request.taskId, !taskId.isEmpty { payload["task_id"] = taskId }
        return payload
    }
}

/// The app's client: the same endpoints the rest of the app posts to.
struct LivePlanApprovalClient: PlanApprovalClient {
    var session: URLSession = .shared

    private static let pathSegment = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))

    func respond(_ request: PlanApprovalRequest, confirmed: Bool) async -> PlanApprovalPost {
        guard let id = request.correlationId.addingPercentEncoding(withAllowedCharacters: Self.pathSegment),
              let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/hitl/\(id)/respond")
        else { return .failed("The response URL is invalid.") }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = "POST"
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&urlRequest)
        urlRequest.httpBody = try? JSONSerialization.data(
            withJSONObject: PlanApprovalActions.respondPayload(request, confirmed: confirmed))
        do {
            let (data, response) = try await session.data(for: urlRequest)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            let message = (body?["message"] as? String) ?? (body?["reason"] as? String)
            let posted: PlanApprovalPost
            if (200..<300).contains(status) {
                posted = body?["accepted"] as? Bool == false || body?["resumed"] as? Bool == false
                    ? .failed(message ?? "The answer was not accepted.") : .ok
            } else if status == 409, body?["reason"] as? String == "already_resolved" {
                posted = .ok
            } else {
                posted = .failed(message ?? "The answer failed (HTTP \(status)).")
            }
            if posted == .ok {
                await MainActor.run { PendingHitlTracker.shared.drop(request.correlationId) }
            }
            return posted
        } catch {
            return .failed(error.localizedDescription)
        }
    }

    func startRun(taskId: String) async throws {
        guard let id = taskId.addingPercentEncoding(withAllowedCharacters: Self.pathSegment),
              let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v3/tasks/\(id)/execute")
        else { throw URLError(.badURL) }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data("{}".utf8)
        MagicianAccess.authorize(&request)
        let (data, response) = try await session.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            let message = (body?["message"] as? String) ?? (body?["error"] as? String)
                ?? "The server answered HTTP \(status)."
            throw NSError(domain: "PlanApproval", code: status, userInfo: [NSLocalizedDescriptionKey: message])
        }
    }
}
