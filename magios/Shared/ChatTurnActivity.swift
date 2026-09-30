import Foundation

// MARK: - Chat turn activity timeline
//
// Swift port of ui/unified-ui/src/lib/magician/components/RequestActivityCard.svelte's
// event→row projection. Accumulates the granular realtime
// events for one assistant turn (llm / reasoning / tool / step / artifact / pause,
// plus tutor & coding lifecycles) into ordered rows, coalesced one-row-per-logical-
// operation, and attaches them to the finished assistant message so it can render a
// collapsible Steps section — matching the web.
//
// The mapping mirrors the web byte-for-byte where it matters (event_type strings,
// coalescing keys, labels). Shapes are confirmed against the live WS during the
// owner-run shakedown; unknown events drop silently (same as the web).

public enum ActivityKind: String, Equatable { case llm, reasoning, tool, step, artifact, pause }
public enum ActivityStatus: String, Equatable { case running, done, failed, waiting }
public enum ActivityTone: String, Equatable { case info, tool, error, reasoning, pause }

/// Canonical lifecycle owner of a materialized complete result. This is not
/// interchangeable with `ActivityRow.taskId`: delegated Chat activity can
/// point at a child task for inspection while its tool result remains owned by
/// the parent Chat session.
public enum ActivityResultOwner: Equatable {
    case chat(sessionId: String)
    case task(taskId: String, executionId: String?)
    case ephemeralVoice(voiceSessionId: String)
}

public struct ActivityFileRef: Identifiable, Equatable {
    public let absolutePath: String
    public let label: String
    public var id: String { absolutePath }
}

public struct ActivityRow: Identifiable, Equatable {
    public let key: String
    public var kind: ActivityKind
    public var label: String
    public var detail: String?
    public var status: ActivityStatus
    public var tone: ActivityTone
    public var durationMs: Double?
    public var agentId: String?
    public var startedAtMs: Double?
    public var taskId: String?
    public var executionId: String?
    public var files: [ActivityFileRef]
    public var resultRef: String?
    public var resultHash: String?
    public var resultSizeBytes: Int?
    public var resultOwner: ActivityResultOwner?
    public var id: String { key }

    public init(
        key: String,
        kind: ActivityKind,
        label: String,
        detail: String?,
        status: ActivityStatus,
        tone: ActivityTone,
        durationMs: Double?,
        agentId: String? = nil,
        startedAtMs: Double? = nil,
        taskId: String? = nil,
        executionId: String? = nil,
        files: [ActivityFileRef] = [],
        resultRef: String? = nil,
        resultHash: String? = nil,
        resultSizeBytes: Int? = nil,
        resultOwner: ActivityResultOwner? = nil
    ) {
        self.key = key
        self.kind = kind
        self.label = label
        self.detail = detail
        self.status = status
        self.tone = tone
        self.durationMs = durationMs
        self.agentId = agentId
        self.startedAtMs = startedAtMs
        self.taskId = taskId
        self.executionId = executionId
        self.files = files
        self.resultRef = resultRef
        self.resultHash = resultHash
        self.resultSizeBytes = resultSizeBytes
        self.resultOwner = resultOwner
    }
}

public struct ActivityInspectTarget: Equatable {
    public let taskId: String
    public let executionId: String
}

/// Accumulates one turn's rows. Reset at turn start; snapshot on the finished
/// assistant message. Not thread-safe — drive it from the main actor (the WS
/// delegate runs on the main queue in ChatViewModel).
public final class ChatTurnActivityAccumulator {
    private var rowsByKey: [String: ActivityRow] = [:]
    private var orderedKeys: [String] = []
    private var terminalTutorRunIds: Set<String> = []
    private var ingestAgentId: String?
    private var ingestStartedAtMs: Double?
    private var ingestTaskId: String?
    private var ingestExecutionId: String?
    private var ingestFiles: [ActivityFileRef] = []
    public private(set) var pauseActive = false

    public init() {}

    public func reset() {
        rowsByKey.removeAll(); orderedKeys.removeAll()
        terminalTutorRunIds.removeAll(); pauseActive = false
    }

    public func snapshot() -> [ActivityRow] {
        var rows = orderedKeys.compactMap { rowsByKey[$0] }
        let taskRows = rows.filter { $0.key.hasPrefix("task::") }
        guard !taskRows.isEmpty, !taskRows.contains(where: { $0.status == .running }) else {
            return rows
        }

        // Durable replay can miss a child tool/LLM terminal event even though
        // the task itself is authoritative and terminal. Match web by settling
        // those stale running leaves so the card cannot spin or offer Stop forever.
        let terminalStatus: ActivityStatus = taskRows.contains { $0.status == .failed }
            ? .failed : .done
        for index in rows.indices where !rows[index].key.hasPrefix("task::")
            && rows[index].status == .running {
            rows[index].status = terminalStatus
            rows[index].tone = terminalStatus == .failed ? .error : rows[index].tone
            if rows[index].detail == nil {
                rows[index].detail = terminalStatus == .failed
                    ? "Stopped when task failed" : "Settled when task finished"
            }
        }
        return rows
    }

    private func upsert(_ incoming: ActivityRow) {
        var row = incoming
        row.agentId = row.agentId ?? ingestAgentId
        row.startedAtMs = row.startedAtMs ?? ingestStartedAtMs
        row.taskId = row.taskId ?? ingestTaskId
        row.executionId = row.executionId ?? ingestExecutionId
        if row.files.isEmpty { row.files = ingestFiles }
        if rowsByKey[row.key] == nil { orderedKeys.append(row.key) }
        rowsByKey[row.key] = row
    }

    private func patch(_ key: String, _ mutate: (inout ActivityRow) -> Void) {
        guard var row = rowsByKey[key] else { return }
        let previousStatus = row.status
        mutate(&row)
        row.agentId = row.agentId ?? ingestAgentId
        row.startedAtMs = row.startedAtMs ?? ingestStartedAtMs
        row.taskId = row.taskId ?? ingestTaskId
        row.executionId = row.executionId ?? ingestExecutionId
        if !ingestFiles.isEmpty { row.files = ingestFiles }
        rowsByKey[key] = row

        // Web moves a row to the bounded tail when its terminal event lands,
        // so a long-running tool cannot finish off-screen behind newer work.
        if [.done, .failed].contains(row.status), row.status != previousStatus,
           let index = orderedKeys.firstIndex(of: key), index != orderedKeys.count - 1 {
            orderedKeys.remove(at: index)
            orderedKeys.append(key)
        }
    }

    // MARK: Ingest one raw WS event

    public func ingest(_ parsed: [String: Any]) {
        let outerType = parsed["event_type"] as? String ?? ""
        if outerType.isEmpty || outerType.hasPrefix("__events_") { return }

        // Unwrap the AgentEvent envelope so we classify on the inner event_type.
        var eventType = outerType
        var payload: [String: Any] = [:]
        var envelopeAgentId: String?
        if outerType == "AgentEvent", let data = parsed["data"] as? [String: Any],
           let inner = data["event"] as? [String: Any], let it = inner["event_type"] as? String {
            eventType = it
            payload = inner["payload"] as? [String: Any] ?? [:]
            envelopeAgentId = inner["agent_id"] as? String
        } else if let data = parsed["data"] as? [String: Any] {
            payload = data
            envelopeAgentId = data["agent_id"] as? String
        }

        // Flat-loop executions use typed transport events rather than the
        // AgentEvent-enveloped tool lifecycle. Normalize the completed action
        // into the same tool branch used by web's RequestActivityCard.
        if outerType == "AgenticActionExecuted" {
            eventType = (payload["success"] as? Bool) == false
                ? "tool.call.failed" : "tool.call.finished"
            payload["tool_name"] = nonEmptyString(payload["target"])
                ?? nonEmptyString(payload["action_type"]) ?? "tool"
            payload["call_id"] = [
                scalarActivityValue(payload["step_id"]) ?? "step",
                scalarActivityValue(payload["iteration"]) ?? "0",
                scalarActivityValue(payload["timestamp"]) ?? "0"
            ].joined(separator: "-")
            payload["duration_ms"] = payload["latency_ms"]
        }

        // Agent prefix for delegated sub-agent events (prefer origin_agent_id).
        let originAgentId = payload["origin_agent_id"] as? String
        let effectiveAgentId = originAgentId ?? envelopeAgentId
        let skipPrefix = effectiveAgentId == nil
            || effectiveAgentId == "__system__"
            || (effectiveAgentId?.hasPrefix("task_") ?? false)
            || (effectiveAgentId?.hasPrefix("exec_") ?? false)
            || (effectiveAgentId?.hasPrefix("cycle_") ?? false)
        let agentPrefix = skipPrefix ? "" : "[\(effectiveAgentId!)] "

        let normalizedAgentId: String = {
            guard let effectiveAgentId, effectiveAgentId != "__system__" else { return "system" }
            if effectiveAgentId.hasPrefix("task_") || effectiveAgentId.hasPrefix("exec_")
                || effectiveAgentId.hasPrefix("cycle_") {
                return "execution"
            }
            return effectiveAgentId
        }()
        ingestAgentId = normalizedAgentId
        ingestStartedAtMs = activityTimestampMs(parsed)
        ingestTaskId = nonEmptyString(payload["task_id"])
        ingestExecutionId = nonEmptyString(payload["execution_id"])
            ?? nonEmptyString(payload["root_execution_id"])
        ingestFiles = extractActivityFiles(payload)
        defer {
            ingestAgentId = nil
            ingestStartedAtMs = nil
            ingestTaskId = nil
            ingestExecutionId = nil
            ingestFiles = []
        }

        // ─── HITL pause family ───
        if isPauseEvent(outer: outerType, event: eventType) {
            pauseActive = true
            let cid = (payload["correlation_id"] as? String)
                ?? (payload["pause_state_id"] as? String)
                ?? (payload["request_id"] as? String) ?? "pause"
            let prompt = (payload["prompt"] as? String) ?? (payload["question"] as? String)
            upsert(ActivityRow(key: "pause:\(cid)", kind: .pause, label: "Waiting on your input",
                               detail: prompt.map { trimTo($0, 140) }, status: .waiting, tone: .pause, durationMs: nil))
            return
        }

        // Typed flat-loop LLM completions do not carry a stable call id that
        // can pair request/response events, so each response is one terminal
        // row, keyed by its timestamp exactly as on web.
        if eventType == "LLMResponseReceived" {
            let capability = (payload["capability"] as? String) ?? "model"
            let succeeded = (payload["success"] as? Bool) != false
            let duration = doubleVal(payload["latency_ms"])
            let summary = (payload["decision_summary"] as? String)
                ?? (payload["error"] as? String)
            let timestampKey = scalarActivityValue(payload["timestamp"])
                ?? scalarActivityValue(ingestStartedAtMs) ?? String(orderedKeys.count)
            upsert(ActivityRow(
                key: "\(normalizedAgentId)::llm::\(timestampKey)",
                kind: .llm,
                label: "\(agentPrefix)Thinking with \(capability)",
                detail: duration.map(formatLatency) ?? summary.map { trimTo($0, 180) },
                status: succeeded ? .done : .failed,
                tone: succeeded ? .info : .error,
                durationMs: duration
            ))
            return
        }
        if eventType == "LLMRequestSent" { return }

        // ─── LLM lifecycle (coalesced by trace_id or iteration) ───
        if eventType.hasPrefix("llm.") {
            let trace = (payload["trace_id"] as? String)
                ?? (payload["iteration"] as? Int).map(String.init) ?? "llm"
            let key = "\(normalizedAgentId)::llm::\(trace)"
            switch eventType {
            case "llm.requested":
                if rowsByKey[key] != nil { return }
                let model = (payload["model"] as? String) ?? (payload["capability"] as? String) ?? "model"
                upsert(ActivityRow(key: key, kind: .llm, label: "\(agentPrefix)Thinking with \(model)",
                                   detail: nil, status: .running, tone: .info, durationMs: nil))
            case "llm.first_token":
                if let ttft = payload["duration_ms"] as? Double ?? (payload["duration_ms"] as? Int).map(Double.init) {
                    patch(key) { $0.detail = formatLatencyPair(ttft, nil) }
                }
            case "llm.succeeded":
                let ms = doubleVal(payload["duration_ms"]); let ttft = doubleVal(payload["ttft_ms"])
                if rowsByKey[key] != nil {
                    patch(key) { $0.status = .done; $0.tone = .info
                        $0.detail = formatLatencyPair(ttft, ms); $0.durationMs = ms }
                } else {
                    let model = (payload["model"] as? String)
                        ?? (payload["capability"] as? String) ?? "model"
                    upsert(ActivityRow(key: key, kind: .llm,
                                       label: "\(agentPrefix)Thinking with \(model)",
                                       detail: formatLatencyPair(ttft, ms), status: .done,
                                       tone: .info, durationMs: ms))
                }
            case "llm.failed":
                let err = (payload["error"] as? String) ?? "unknown error"; let ms = doubleVal(payload["duration_ms"])
                let model = (payload["model"] as? String)
                    ?? (payload["capability"] as? String) ?? "model"
                if rowsByKey[key] != nil {
                    patch(key) { $0.label = "\(agentPrefix)Thinking with \(model)"; $0.status = .failed; $0.tone = .error
                        $0.detail = trimTo(err, 200); $0.durationMs = ms }
                } else {
                    upsert(ActivityRow(key: key, kind: .llm,
                                       label: "\(agentPrefix)Thinking with \(model)",
                                       detail: trimTo(err, 200), status: .failed,
                                       tone: .error, durationMs: ms))
                }
            default: break
            }
            return
        }

        // ─── Reasoning (coalesced by trace_id) ───
        if eventType.hasPrefix("reasoning.") {
            let key = "\(normalizedAgentId)::reasoning::\(payload["trace_id"] as? String ?? "reasoning")"
            switch eventType {
            case "reasoning.start":
                if rowsByKey[key] == nil {
                    upsert(ActivityRow(key: key, kind: .reasoning, label: "Reasoning", detail: nil,
                                       status: .running, tone: .reasoning, durationMs: nil))
                }
            case "reasoning.content":
                let delta = (payload["delta"] as? String) ?? (payload["content"] as? String) ?? ""
                if let existing = rowsByKey[key] {
                    let merged = existing.detail.map { "\($0) \(delta)".trimmingCharacters(in: .whitespaces) } ?? delta
                    patch(key) { $0.detail = trimTo(merged, 200) }
                } else {
                    upsert(ActivityRow(key: key, kind: .reasoning, label: "Reasoning", detail: trimTo(delta, 200),
                                       status: .running, tone: .reasoning, durationMs: nil))
                }
            case "reasoning.end":
                patch(key) { $0.status = .done; $0.durationMs = doubleVal(payload["duration_ms"]) }
            default: break
            }
            return
        }

        // ─── Tool lifecycle (coalesced by call_id) ───
        if eventType.hasPrefix("tool.") {
            let callId = (payload["origin_call_id"] as? String)
                ?? (payload["call_id"] as? String)
                ?? (payload["tool_call_id"] as? String) ?? "anon"
            let tool = (payload["tool_name"] as? String) ?? (payload["tool"] as? String)
                ?? (payload["name"] as? String) ?? "tool"
            let key = "\(normalizedAgentId)::tool::\(callId)"
            if eventType == "tool.result.projected",
               let resultRef = nonEmptyString(payload["result_ref"]) {
                let resultHash = nonEmptyString(payload["content_hash"])
                let resultSizeBytes = (payload["size_bytes"] as? NSNumber)?.intValue
                let resultOwner = Self.resultOwner(payload["result_owner"])
                let resultTaskId = nonEmptyString(payload["task_id"])
                let resultExecutionId = nonEmptyString(payload["execution_id"])
                if rowsByKey[key] != nil {
                    patch(key) {
                        $0.resultRef = resultRef
                        $0.resultHash = resultHash
                        $0.resultSizeBytes = resultSizeBytes
                        $0.resultOwner = resultOwner
                        if let resultTaskId { $0.taskId = resultTaskId }
                        if let resultExecutionId { $0.executionId = resultExecutionId }
                    }
                } else {
                    upsert(ActivityRow(
                        key: key,
                        kind: .tool,
                        label: "\(agentPrefix)\(tool) result available",
                        detail: nil,
                        status: .done,
                        tone: .tool,
                        durationMs: nil,
                        taskId: resultTaskId,
                        executionId: resultExecutionId,
                        resultRef: resultRef,
                        resultHash: resultHash,
                        resultSizeBytes: resultSizeBytes,
                        resultOwner: resultOwner
                    ))
                }
            } else if eventType.hasSuffix(".started") || eventType.hasSuffix(".args") {
                if rowsByKey[key] != nil { return }
                var label = "\(agentPrefix)Calling \(tool)"
                var detail: String?
                if tool == "delegate_to_agent", eventType.hasSuffix(".started"),
                   let args = payload["args"] as? [String: Any],
                   let targets = args["delegation_targets"] as? [[String: Any]] {
                    let ids = targets.compactMap { $0["target_agent_id"] as? String }
                    if ids.count >= 2 { label = "\(agentPrefix)Decomposing into \(ids.count) parallel agents"; detail = ids.joined(separator: ", ") }
                    else if ids.count == 1 { label = "\(agentPrefix)Delegating to \(ids[0])" }
                }
                upsert(ActivityRow(key: key, kind: .tool, label: label, detail: detail,
                                   status: .running, tone: .tool, durationMs: nil))
            } else if eventType.hasSuffix(".succeeded") || eventType.hasSuffix(".finished") {
                let ms = doubleVal(payload["duration_ms"])
                let preview = (payload["content_preview"] as? String).map { trimTo($0, 200) }
                if rowsByKey[key] != nil {
                    patch(key) { $0.label = "\(agentPrefix)\(tool) returned"; $0.status = .done; $0.tone = .tool
                        $0.detail = preview; $0.durationMs = ms }
                } else {
                    upsert(ActivityRow(key: key, kind: .tool,
                                       label: "\(agentPrefix)\(tool) returned", detail: preview,
                                       status: .done, tone: .tool, durationMs: ms))
                }
            } else if eventType.hasSuffix(".failed") {
                let err = (payload["error"] as? String) ?? "tool failed"
                if rowsByKey[key] != nil {
                    patch(key) { $0.label = "\(agentPrefix)\(tool) failed"; $0.status = .failed; $0.tone = .error; $0.detail = trimTo(err, 200) }
                } else {
                    upsert(ActivityRow(key: key, kind: .tool,
                                       label: "\(agentPrefix)\(tool) failed",
                                       detail: trimTo(err, 200), status: .failed,
                                       tone: .error, durationMs: nil))
                }
            }
            return
        }

        // ─── Delegated task lifecycle ───
        if eventType == "task.status_changed" || eventType == "chat.delegate.status_changed" {
            let taskId = nonEmptyString(payload["task_id"])
                ?? "task-\(scalarActivityValue(ingestStartedAtMs) ?? String(orderedKeys.count))"
            let targetAgent = nonEmptyString(payload["chat_inline_delegate_agent_id"])
                ?? nonEmptyString(payload["target_agent_id"])
                ?? nonEmptyString(payload["origin_agent_id"])
                ?? normalizedAgentId
            let displayLabel = nonEmptyString(payload["display_label"]) ?? targetAgent
            let status = (nonEmptyString(payload["status"]) ?? "running").lowercased()
            let synthesisPending = (payload["synthesis_pending"] as? Bool) == true
            let rowStatus: ActivityStatus = synthesisPending
                ? .running
                : (["completed", "succeeded"].contains(status)
                    ? .done : (["failed", "cancelled", "canceled"].contains(status) ? .failed : .running))
            let label = synthesisPending
                ? "\(displayLabel) preparing final result"
                : "\(displayLabel) \(status)"
            let summary = nonEmptyString(payload["summary"])
                ?? (synthesisPending ? "Task completed. Preparing final result..." : nil)
            let key = "task::\(taskId)"
            if rowsByKey[key] != nil {
                patch(key) {
                    $0.label = label
                    $0.detail = summary.map { trimTo($0, 240) } ?? $0.detail
                    $0.status = rowStatus
                    $0.tone = rowStatus == .failed ? .error : .tool
                    $0.agentId = targetAgent
                    $0.taskId = taskId
                }
            } else {
                upsert(ActivityRow(
                    key: key,
                    kind: .step,
                    label: label,
                    detail: summary.map { trimTo($0, 240) },
                    status: rowStatus,
                    tone: rowStatus == .failed ? .error : .tool,
                    durationMs: nil,
                    agentId: targetAgent,
                    taskId: taskId
                ))
            }
            return
        }

        if eventType == "chat.delegate.output_ready" {
            guard let taskId = nonEmptyString(payload["task_id"]) else { return }
            let key = "task::\(taskId)"
            let terminalStatus = nonEmptyString(payload["terminal_task_status"])?.lowercased()
            let failed = ["failed", "cancelled", "canceled"].contains(terminalStatus ?? "")
                || rowsByKey[key]?.status == .failed
            let summary = nonEmptyString(payload["summary_preview"])
            if rowsByKey[key] != nil {
                patch(key) {
                    $0.label = failed ? "\($0.agentId ?? "Task") output ready after failure" : "\($0.agentId ?? "Task") output ready"
                    $0.detail = summary.map { trimTo($0, 240) } ?? $0.detail
                    $0.status = failed ? .failed : .done
                    $0.tone = failed ? .error : .tool
                    $0.taskId = taskId
                }
            } else {
                upsert(ActivityRow(
                    key: key, kind: .step,
                    label: failed ? "Task output ready after failure" : "Task output ready",
                    detail: summary.map { trimTo($0, 240) },
                    status: failed ? .failed : .done,
                    tone: failed ? .error : .tool,
                    durationMs: nil,
                    taskId: taskId
                ))
            }
            return
        }

        if eventType == "chat.delegate.output_failed" {
            guard let taskId = nonEmptyString(payload["task_id"]) else { return }
            let key = "task::\(taskId)"
            let stage = nonEmptyString(payload["stage"]) ?? "synthesis"
            let error = nonEmptyString(payload["last_error"]) ?? "unknown error"
            let detail = "Output synthesis failed (\(stage)): \(error)"
            if rowsByKey[key] != nil {
                patch(key) {
                    $0.label = "\($0.agentId ?? "Task") output failed"
                    $0.detail = trimTo(detail, 240)
                    $0.status = .failed
                    $0.tone = .error
                    $0.taskId = taskId
                }
            } else {
                upsert(ActivityRow(key: key, kind: .step, label: "Task output failed",
                                   detail: trimTo(detail, 240), status: .failed,
                                   tone: .error, durationMs: nil, taskId: taskId))
            }
            return
        }

        // ─── Step lifecycle (coalesced by step_id) ───
        if eventType.hasPrefix("step.") {
            let stepId = (payload["step_id"] as? String) ?? (payload["label"] as? String) ?? "step"
            let key = "step:\(stepId)"
            switch eventType {
            case "step.started":
                upsert(ActivityRow(key: key, kind: .step, label: "Step: \(stepId)", detail: nil,
                                   status: .running, tone: .info, durationMs: nil))
            case "step.completed":
                patch(key) { $0.status = .done; $0.label = "Step complete: \(stepId)" }
            case "step.failed":
                patch(key) { $0.status = .failed; $0.label = "Step failed: \(stepId)"; $0.tone = .error
                    $0.detail = payload["error"] as? String }
            default: break
            }
            return
        }

        // ─── Agentic iteration markers ───
        if eventType == "agentic.iteration_started" {
            let iter = payload["iteration"] as? Int
            upsert(ActivityRow(key: "iter:\(iter.map(String.init) ?? "x")", kind: .step,
                               label: iter.map { "Iteration \($0)" } ?? "Iteration", detail: nil,
                               status: .running, tone: .info, durationMs: nil))
            return
        }

        // ─── Artifact / output ───
        if eventType == "artifact.created" || eventType == "output.created" {
            if !ingestFiles.isEmpty {
                for file in ingestFiles {
                    upsert(ActivityRow(
                        key: "\(normalizedAgentId)::file::\(file.absolutePath)",
                        kind: .artifact,
                        label: "\(agentPrefix)Produced \(file.label)",
                        detail: nil,
                        status: .done,
                        tone: .tool,
                        durationMs: nil,
                        files: [file]
                    ))
                }
            } else {
                let name = (payload["display_name"] as? String)
                    ?? (payload["name"] as? String)
                    ?? (payload["title"] as? String) ?? "file"
                upsert(ActivityRow(key: "artifact:\(name):\(orderedKeys.count)", kind: .artifact,
                                   label: "\(agentPrefix)Produced \(name)", detail: nil,
                                   status: .done, tone: .tool, durationMs: nil))
            }
            return
        }

        // ─── Tutor run lifecycle (lightweight) ───
        if eventType.hasPrefix("tutor.") {
            let runId = (payload["run_id"] as? String) ?? "active"
            let runKey = "tutor:\(runId)"
            switch eventType {
            case "tutor.run.started":
                terminalTutorRunIds.remove(runId)
                upsert(ActivityRow(key: runKey, kind: .step, label: "\(agentPrefix)Tutor started",
                                   detail: (payload["goal"] as? String).map { trimTo($0, 180) }, status: .running, tone: .info, durationMs: nil))
            case "tutor.run.completed":
                terminalTutorRunIds.insert(runId)
                if rowsByKey[runKey] != nil { patch(runKey) { $0.label = "\(agentPrefix)Tutor completed"; $0.status = .done; $0.tone = .info } }
                else { upsert(ActivityRow(key: runKey, kind: .step, label: "\(agentPrefix)Tutor completed", detail: nil, status: .done, tone: .info, durationMs: nil)) }
            case "tutor.run.failed":
                terminalTutorRunIds.insert(runId)
                let note = payload["note"] as? String
                if rowsByKey[runKey] != nil { patch(runKey) { $0.label = "\(agentPrefix)Tutor failed"; $0.status = .failed; $0.tone = .error; $0.detail = note.map { trimTo($0, 180) } } }
                else { upsert(ActivityRow(key: runKey, kind: .step, label: "\(agentPrefix)Tutor failed", detail: note.map { trimTo($0, 180) }, status: .failed, tone: .error, durationMs: nil)) }
            default:
                let label = tutorStepLabel(eventType)
                guard let l = label else { return }
                let stepKey = "\(runKey):\(payload["step_count"] as? Int ?? orderedKeys.count)"
                let detail = (payload["step_label"] as? String) ?? (payload["target"] as? String)
                upsert(ActivityRow(key: stepKey, kind: .step, label: "\(agentPrefix)\(l.0)", detail: detail,
                                   status: l.1, tone: l.2, durationMs: nil))
            }
            return
        }

        // ─── Coding-engine lifecycle (lightweight) ───
        if eventType.hasPrefix("coding.") {
            let shadow = (payload["shadow_workspace_id"] as? String) ?? "coding"
            let key = "coding:\(shadow)"
            switch eventType {
            case "coding.started":
                let profile = payload["coding_profile"] as? [String: Any]
                let label = (profile?["label"] as? String) ?? (profile?["id"] as? String) ?? "Coding agent"
                upsert(ActivityRow(key: key, kind: .step, label: "\(agentPrefix)Coding with \(label)", detail: nil, status: .running, tone: .info, durationMs: nil))
            case "coding.completed":
                patch(key) { $0.label = "\(agentPrefix)Coding finished"; $0.status = .done; $0.tone = .info }
            case "coding.failed":
                let err = (payload["error"] as? String) ?? "coding failed"
                if rowsByKey[key] != nil { patch(key) { $0.label = "\(agentPrefix)Coding failed"; $0.status = .failed; $0.tone = .error; $0.detail = trimTo(err, 180) } }
                else { upsert(ActivityRow(key: key, kind: .step, label: "\(agentPrefix)Coding failed", detail: trimTo(err, 180), status: .failed, tone: .error, durationMs: nil)) }
            default: break
            }
            return
        }
        // Anything else drops silently.
    }

    private static func resultOwner(_ value: Any?) -> ActivityResultOwner? {
        guard let wire = value as? [String: Any],
              let kind = wire["kind"] as? String else { return nil }
        switch kind {
        case "chat":
            guard let sessionId = nonEmptyString(wire["session_id"]) else { return nil }
            return .chat(sessionId: sessionId)
        case "task":
            guard let taskId = nonEmptyString(wire["task_id"]) else { return nil }
            return .task(taskId: taskId, executionId: nonEmptyString(wire["execution_id"]))
        case "ephemeral_voice":
            guard let voiceSessionId = nonEmptyString(wire["voice_session_id"]) else { return nil }
            return .ephemeralVoice(voiceSessionId: voiceSessionId)
        default:
            return nil
        }
    }

    private func tutorStepLabel(_ eventType: String) -> (String, ActivityStatus, ActivityTone)? {
        switch eventType {
        case "tutor.step.observed": return ("Observed screen", .done, .info)
        case "tutor.step.target_resolved": return ("Resolved target", .done, .info)
        case "tutor.step.drawing": return ("Drawing marker", .done, .tool)
        case "tutor.step.action_delegated": return ("Action delegated", .running, .tool)
        case "tutor.step.verifying": return ("Verifying action", .running, .info)
        case "tutor.step.verified": return ("Verified action", .done, .info)
        case "tutor.step.failed": return ("Tutor step failed", .failed, .error)
        case "tutor.step.recovering": return ("Recovering tutor flow", .running, .pause)
        case "tutor.step.clearing": return ("Clearing tutor marks", .done, .tool)
        default: return nil
        }
    }
}

// MARK: - Pure helpers

func isPauseEvent(outer: String, event: String) -> Bool {
    if outer == "HitlRequested" { return true }
    return event == "waiting_for_confirmation"
        || event == "execution.waiting_for_user"
        || event == "input.requested"
        || event == "hitl.requested"
        || event == "clarification.queued"
}

func trimTo(_ value: String, _ max: Int) -> String {
    let trimmed = value.replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression).trimmingCharacters(in: .whitespaces)
    if trimmed.count <= max { return trimmed }
    return String(trimmed.prefix(max - 1)) + "…"
}

private func doubleVal(_ any: Any?) -> Double? {
    if let d = any as? Double { return d }
    if let i = any as? Int { return Double(i) }
    if let number = any as? NSNumber { return number.doubleValue }
    return nil
}

private func nonEmptyString(_ any: Any?) -> String? {
    guard let value = any as? String else { return nil }
    let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.isEmpty ? nil : trimmed
}

private func scalarActivityValue(_ any: Any?) -> String? {
    if let value = nonEmptyString(any) { return value }
    if let number = any as? NSNumber { return number.stringValue }
    return nil
}

private func activityTimestampMs(_ parsed: [String: Any]) -> Double? {
    let data = parsed["data"] as? [String: Any]
    let inner = data?["event"] as? [String: Any]
    let payload = inner?["payload"] as? [String: Any]
    let candidates: [Any?] = [
        payload?["timestamp_ms"], payload?["timestamp"],
        inner?["timestamp_ms"], inner?["timestamp"],
        data?["timestamp_ms"], data?["timestamp"], parsed["timestamp_ms"]
    ]
    for candidate in candidates {
        guard let value = doubleVal(candidate), value.isFinite else { continue }
        return value < 10_000_000_000 ? value * 1000 : value
    }
    return nil
}

private func extractActivityFiles(_ payload: [String: Any]) -> [ActivityFileRef] {
    var output: [ActivityFileRef] = []
    var seen: Set<String> = []
    for field in ["output_files", "files", "content_blocks"] {
        guard let entries = payload[field] as? [Any] else { continue }
        for entry in entries {
            guard let object = entry as? [String: Any],
                  let path = nonEmptyString(object["absolute_path"])
                    ?? nonEmptyString(object["path"]),
                  path.hasPrefix("/"), seen.insert(path).inserted else { continue }
            let label = nonEmptyString(object["label"])
                ?? nonEmptyString(object["display_name"])
                ?? nonEmptyString(object["relative_path"])
                ?? URL(fileURLWithPath: path).lastPathComponent
            output.append(ActivityFileRef(absolutePath: path, label: label))
        }
    }
    if output.isEmpty,
       let path = nonEmptyString(payload["absolute_path"]), path.hasPrefix("/") {
        let label = nonEmptyString(payload["display_name"])
            ?? nonEmptyString(payload["name"])
            ?? URL(fileURLWithPath: path).lastPathComponent
        output.append(ActivityFileRef(absolutePath: path, label: label))
    }
    return output
}

func formatLatency(_ ms: Double) -> String {
    if !ms.isFinite || ms < 0 { return "—" }
    if ms < 1000 { return "\(Int(ms.rounded()))ms" }
    return String(format: "%.1fs", ms / 1000)
}

/// `TTFT (total)` — e.g. "700ms (2.1s)". Total renders as `…` until known.
func formatLatencyPair(_ ttft: Double?, _ total: Double?) -> String {
    let t = ttft.map(formatLatency) ?? "…"
    let all = total.map(formatLatency) ?? "…"
    return "\(t) (\(all))"
}

/// Inline chat activity shows only the newest rows while the header retains the
/// authoritative total count. Keeping this pure makes the five-row contract
/// independently testable from SwiftUI.
func activityPreviewRows(_ rows: [ActivityRow], limit: Int = 5) -> [ActivityRow] {
    guard limit > 0 else { return [] }
    return Array(rows.suffix(limit))
}

/// Most recent execution id + most recent task id are captured independently,
/// matching web: transport events do not guarantee they appear together. Both
/// are required because inline model turns also carry telemetry execution ids,
/// but do not have a durable task run that can be inspected or controlled.
func activityInspectTarget(_ rows: [ActivityRow]) -> ActivityInspectTarget? {
    var taskId: String?
    var executionId: String?
    for row in rows.reversed() {
        if executionId == nil { executionId = nonEmptyString(row.executionId) }
        if taskId == nil { taskId = nonEmptyString(row.taskId) }
        if executionId != nil, taskId != nil { break }
    }
    guard let taskId, let executionId else { return nil }
    return ActivityInspectTarget(taskId: taskId, executionId: executionId)
}

enum ActivityLinkDestination: Equatable {
    case external(String)
    case task(String?)
    case execution(String)
    case thread(String)
    case attention(String?)
    case today(String?)
    case settings
    case observe
    case taskOutput(taskId: String, relativePath: String)
    case artifact(String)
    case file(String)
    case webRoute(String)
    case unknown
}

/// Swift counterpart of web `classifyRef`: markdown links in activity details
/// must open the same logical destination instead of falling through to an
/// unusable relative URL on iOS.
func classifyActivityLink(_ rawValue: String) -> ActivityLinkDestination {
    let raw = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !raw.isEmpty else { return .unknown }
    if raw.lowercased().hasPrefix("file:") {
        guard let url = URL(string: raw), url.isFileURL else { return .unknown }
        return .file(url.path)
    }
    if let scheme = URL(string: raw)?.scheme?.lowercased(),
       ["http", "https", "mailto", "tel", "ftp"].contains(scheme) {
        return .external(raw)
    }
    if matchesActivityIdentifier(raw, prefix: "task_") { return .task(raw) }
    if matchesActivityIdentifier(raw, prefix: "exec_") { return .execution(raw) }

    if let match = raw.range(
        of: #"(?:^|/)tasks/(task_[0-9a-fA-F]{32})/(?:.*?/)?outputs/(.+)$"#,
        options: .regularExpression
    ) {
        let matched = String(raw[match])
        let components = matched.split(separator: "/", omittingEmptySubsequences: true)
        if let taskIndex = components.firstIndex(where: { matchesActivityIdentifier(String($0), prefix: "task_") }),
           let outputIndex = components.firstIndex(of: "outputs"), outputIndex + 1 < components.count {
            return .taskOutput(
                taskId: String(components[taskIndex]),
                relativePath: components[(outputIndex + 1)...].joined(separator: "/")
            )
        }
    }

    guard let components = URLComponents(string: raw) else { return .unknown }
    let path = components.path
    let queryValue: (String) -> String? = { name in
        components.queryItems?.last(where: { $0.name == name })?.value
    }
    if path == "/tasks" || path.hasPrefix("/tasks/") {
        let pathId = path.split(separator: "/").dropFirst().first.map(String.init)
        return .task(queryValue("selected") ?? queryValue("task_id") ?? pathId)
    }
    if path.hasPrefix("/t/"), let thread = path.split(separator: "/").dropFirst().first {
        return .thread(String(thread))
    }
    if path == "/attention" || path.hasPrefix("/attention/") {
        return .attention(queryValue("item_id") ?? queryValue("attention_id") ?? queryValue("selected"))
    }
    if path == "/today" { return .today(queryValue("tab")) }
    if path == "/settings" || path.hasPrefix("/settings/") { return .settings }
    if path == "/observe" || path.hasPrefix("/observe/") { return .observe }
    if path.hasPrefix("/api/") { return .artifact(raw) }
    if path.hasPrefix("/Users/") || path.hasPrefix("/tmp/")
        || path.hasPrefix("/private/tmp/") || path.contains("/magician_data_v3/") {
        return .file(path)
    }
    if path.hasPrefix("/debug"), let executionId = queryValue("execution_id") {
        return .execution(executionId)
    }
    if path.hasPrefix("/"), !path.hasPrefix("//"), !raw.contains("://") {
        return .webRoute(raw)
    }
    return .unknown
}

private func matchesActivityIdentifier(_ value: String, prefix: String) -> Bool {
    guard value.lowercased().hasPrefix(prefix), value.count == prefix.count + 32 else { return false }
    return value.dropFirst(prefix.count).allSatisfy { $0.isHexDigit }
}
