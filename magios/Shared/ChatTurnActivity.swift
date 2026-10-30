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

/// What one finished model call reported, read off its event (web
/// `deriveCacheSummary` / `llmUsageSuffix`). Every figure is separately
/// optional: absence is not zero.
public struct ActivityCallUsage: Equatable {
    /// Stable logical call id, when two transports deliver the same call.
    public var callId: String?
    /// Input and output tokens, `nil` when not reported or when the event's
    /// `usage_availability.tokens` is false.
    public var inputTokens: Int?
    public var outputTokens: Int?
    /// Any of input, output, cache-read or cache-write tokens was reported.
    public var reportedTokens: Bool
    /// Input + output as the event states them, for the step's own suffix.
    public var stepTokens: Int?
    /// `cost_usd` (chat calls), else `cost` (task-run calls).
    public var costUsd: Double?

    public init(callId: String?, inputTokens: Int?, outputTokens: Int?, reportedTokens: Bool,
                stepTokens: Int?, costUsd: Double?) {
        self.callId = callId
        self.inputTokens = inputTokens
        self.outputTokens = outputTokens
        self.reportedTokens = reportedTokens
        self.stepTokens = stepTokens
        self.costUsd = costUsd
    }

    public static func read(_ payload: [String: Any]) -> ActivityCallUsage {
        let correlation = payload["correlation"] as? [String: Any]
        let availability = (payload["usage_availability"] ?? correlation?["usage_availability"]) as? [String: Any]
        let tokensAvailable = (availability?["tokens"] as? Bool) != false
            && (payload["usage_reported"] as? Bool) != false
        let rawInput = usageNumber(payload["input_tokens"])
        let rawOutput = usageNumber(payload["output_tokens"])
        let input = tokensAvailable ? rawInput : nil
        let output = tokensAvailable ? rawOutput : nil
        let cacheRead = (availability?["cache_read"] as? Bool) == false ? nil : usageNumber(payload["cache_read_tokens"])
        let cacheWrite = (availability?["cache_write"] as? Bool) == false ? nil : usageNumber(payload["cache_creation_tokens"])
        let callId = nonEmptyString(payload["llm_call_id"]) ?? nonEmptyString(correlation?["llm_call_id"])
        return ActivityCallUsage(
            callId: callId,
            inputTokens: input.map { Int($0) },
            outputTokens: output.map { Int($0) },
            reportedTokens: input != nil || output != nil || cacheRead != nil || cacheWrite != nil,
            stepTokens: input == nil && output == nil ? nil : Int((input ?? 0) + (output ?? 0)),
            costUsd: (availability?["cost"] as? Bool) == false ? nil
                : (usageNumber(payload["cost_usd"]) ?? usageNumber(payload["cost"]))
        )
    }
}

/// Tools whose result is a task the turn started. Delegation is not here: a
/// delegate's calls already fan into the turn's own events.
let activitySpawningTools: Set<String> = ["create_task", "run_task"]

/// Ids of the tasks a `tool.call.finished` payload started: its
/// `spawned_task_ids`, else (turns recorded before that field) the task ids
/// in a spawning tool's result preview.
func activitySpawnedTaskIds(_ payload: [String: Any], toolName: String) -> [String] {
    if let stated = payload["spawned_task_ids"] as? [Any] {
        return stated.compactMap { nonEmptyString($0) }
    }
    guard activitySpawningTools.contains(toolName),
          let preview = payload["content_preview"] as? String,
          let regex = try? NSRegularExpression(pattern: "task_[0-9a-f]{32}") else { return [] }
    var ids: [String] = []
    for match in regex.matches(in: preview, range: NSRange(preview.startIndex..., in: preview)) {
        guard let range = Range(match.range, in: preview) else { continue }
        let id = String(preview[range])
        if !ids.contains(id) { ids.append(id) }
    }
    return ids
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
    /// A finished model call's usage (LLM rows only).
    public var callUsage: ActivityCallUsage?
    /// Tasks a finished tool call started (`create_task` / `run_task`).
    public var spawnedTaskIds: [String] = []
    /// The tool a tool row calls; keys the row's dot tint (web `StageRow.tool`).
    public var toolName: String?
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
    /// Tool rows' display name and target (`activityToolDisplay`), by row key.
    private var toolDisplayByKey: [String: ActivityToolDisplay] = [:]
    /// Typed LLM rows (`LLMRequestSent` / `LLMRouted`) still waiting for their
    /// `LLMResponseReceived`, oldest first.
    private var openTypedLLMKeys: [String] = []
    /// The model `LLMRouted` named for a typed LLM row, by row key.
    private var routedModelByKey: [String: String] = [:]
    /// The row each `llm_call_id` already has (web `llmKeyByCallId`): the
    /// chat-side `llm.*` events key by trace and the typed copies by call id,
    /// so one model call would otherwise render as two rows.
    private var llmKeyByCallId: [String: String] = [:]
    public private(set) var pauseActive = false

    public init() {}

    public func reset() {
        rowsByKey.removeAll(); orderedKeys.removeAll()
        terminalTutorRunIds.removeAll(); pauseActive = false
        toolDisplayByKey.removeAll()
        openTypedLLMKeys.removeAll(); routedModelByKey.removeAll()
        llmKeyByCallId.removeAll()
    }

    public func snapshot() -> [ActivityRow] {
        settleLeavesOfTerminalTasks(orderedKeys.compactMap { rowsByKey[$0] })
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

    private func dropRow(_ key: String) {
        guard rowsByKey.removeValue(forKey: key) != nil else { return }
        orderedKeys.removeAll { $0 == key }
        openTypedLLMKeys.removeAll { $0 == key }
        routedModelByKey.removeValue(forKey: key)
    }

    /// The row key for a typed LLM event's call: the row that call already
    /// has (a chat-side `llm.*` event may have opened it), else its own.
    private func typedLLMKey(callId: String, agentId: String) -> String {
        let key = llmKeyByCallId[callId] ?? "\(agentId)::llm::call::\(callId)"
        llmKeyByCallId[callId] = key
        return key
    }

    /// The row key for a chat-side `llm.*` event (web `chatLlmKey`): the
    /// trace/iteration key, or, when the event names an `llm_call_id`, the
    /// row that call already has. A typed copy that opened its own row
    /// before the id-less `llm.requested`'s settle named the id is folded
    /// into the trace row.
    private func chatLLMKey(_ eventType: String, payload: [String: Any], traceKey: String, agentId: String) -> String {
        guard let callId = activityLLMCallId(payload) else { return traceKey }
        let aliased = llmKeyByCallId[callId]
        let key: String
        if eventType == "llm.requested" {
            key = aliased ?? "\(agentId)::llm::call::\(callId)"
        } else if let aliased, aliased != traceKey, rowsByKey[traceKey]?.status == .running {
            dropRow(aliased)
            key = traceKey
        } else {
            key = aliased ?? traceKey
        }
        // Only a key that holds (or is about to hold) this call's row: a
        // first token that patches nothing must not send the typed copy onto
        // a shared key, while a settle opens the row it names (Android
        // `chatLlmKey`).
        if eventType != "llm.first_token" || rowsByKey[key] != nil { llmKeyByCallId[callId] = key }
        return key
    }

    /// Settle the pause rows an answer resolves: the row keyed on its
    /// correlation id, plus the id-less `pause:pause` row a payload without ids
    /// files under. An answer that names no id settles every waiting pause row.
    /// The turn stops reading "Waiting on you" once no pause row is left
    /// waiting. Mirrors web `ChatTurnProgress.settlePauseRows`.
    private func settlePauseRows(correlationId: String?) {
        for key in orderedKeys {
            guard var row = rowsByKey[key], row.kind == .pause, row.status == .waiting else { continue }
            let matches = correlationId == nil
                || key == "pause:\(correlationId!)" || key == "pause:pause"
            guard matches else { continue }
            row.label = "Answered"
            row.status = .done
            row.tone = .info
            rowsByKey[key] = row
        }
        pauseActive = rowsByKey.values.contains { $0.kind == .pause && $0.status == .waiting }
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
        // AgentEvent-enveloped tool lifecycle. Normalize them into the same
        // tool branch used by web's RequestActivityCard: AgenticActionStarted
        // opens a running row the moment the action starts, and
        // AgenticActionExecuted settles that same row. Both carry the server's
        // `call_id`; only a legacy executed event without one falls back to
        // the step-iteration-timestamp composite (a row of its own).
        if outerType == "AgenticActionStarted" || outerType == "AgenticActionExecuted" {
            if outerType == "AgenticActionStarted" {
                eventType = "tool.call.started"
            } else {
                eventType = (payload["success"] as? Bool) == false
                    ? "tool.call.failed" : "tool.call.finished"
                payload["duration_ms"] = payload["latency_ms"]
            }
            payload["tool_name"] = nonEmptyString(payload["target"])
                ?? nonEmptyString(payload["action_type"]) ?? "tool"
            payload["call_id"] = nonEmptyString(payload["call_id"]) ?? [
                scalarActivityValue(payload["step_id"]) ?? "step",
                scalarActivityValue(payload["iteration"]) ?? "0",
                scalarActivityValue(payload["timestamp"]) ?? "0"
            ].joined(separator: "-")
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

        // ─── HITL answer: settle the waiting pause rows it resolves ───
        if eventType == "context.retrieval.started" || eventType == "context.retrieval.completed" {
            guard let phase = nonEmptyString(payload["phase_id"]) else { return }
            let key = "context:\(phase)"
            let running = eventType.hasSuffix(".started")
            let status = payload["status"] as? String
            // A phase may name itself ("Compacting conversation") and report
            // exact progress as step of steps; a finished one keeps its result.
            let named = nonEmptyString(payload["label"])
            var progress = ""
            if let step = (payload["step"] as? NSNumber)?.intValue,
               let steps = (payload["steps"] as? NSNumber)?.intValue, steps > 0, (0...steps).contains(step) {
                progress = " · \(step)/\(steps)"
            }
            if running, rowsByKey[key] != nil {
                if rowsByKey[key]?.status == .running, let named {
                    patch(key) { row in row.label = "\(agentPrefix)\(named)\(progress)" }
                }
                return
            }
            let label = running ? "\(named ?? "Preparing context")\(progress)"
                : nonEmptyString(payload["done_label"]) ?? (status == "cancelled" ? "Context preparation cancelled"
                    : status == "partial" ? "Context prepared (partial)" : "Context prepared")
            let finished: ActivityStatus = status == "failed" ? .failed : .done
            let duration = (payload["duration_ms"] as? NSNumber)?.doubleValue
            let measured = duration.flatMap { $0.isFinite && $0 >= 0 ? $0 : nil }
            if rowsByKey[key] != nil {
                patch(key) { row in
                    row.label = "\(agentPrefix)\(label)"; row.detail = payload["summary"] as? String
                    row.status = finished; row.tone = .info; row.durationMs = measured
                }
            } else {
                upsert(ActivityRow(key: key, kind: .step, label: "\(agentPrefix)\(label)",
                    detail: payload["summary"] as? String, status: running ? .running : finished,
                    tone: .info, durationMs: measured))
            }
            return
        }

        if isPauseResolvedEvent(outer: outerType, event: eventType) {
            settlePauseRows(correlationId: (payload["correlation_id"] as? String)
                ?? (payload["pause_state_id"] as? String)
                ?? (payload["request_id"] as? String))
            return
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

        // Typed flat-loop LLM calls. A current server stamps one call's
        // `LLMRequestSent`, `LLMRouted` (once the model is chosen, again on a
        // fallback) and `LLMResponseReceived` with the same
        // `correlation.llm_call_id`: the request opens a running "Thinking…"
        // row, the route names the model, and the response settles that row
        // in place. An older server's request has no id and is skipped; its
        // response is one terminal row keyed by its timestamp, as on web.
        if eventType == "LLMRequestSent" {
            guard let callId = activityLLMCallId(payload) else { return }
            let key = typedLLMKey(callId: callId, agentId: normalizedAgentId)
            if rowsByKey[key] != nil { return }
            upsert(ActivityRow(key: key, kind: .llm, label: "\(agentPrefix)Thinking…",
                               detail: nil, status: .running, tone: .info, durationMs: nil))
            openTypedLLMKeys.append(key)
            return
        }
        if eventType == "LLMRouted" {
            guard let callId = activityLLMCallId(payload),
                  let model = activityAnsweredModel(payload) else { return }
            let key = typedLLMKey(callId: callId, agentId: normalizedAgentId)
            routedModelByKey[key] = model
            let label = "\(agentPrefix)Thinking with \(model)"
            if let existing = rowsByKey[key] {
                // A settled row already names the model that answered.
                if existing.status == .running { patch(key) { $0.label = label } }
            } else {
                upsert(ActivityRow(key: key, kind: .llm, label: label, detail: nil,
                                   status: .running, tone: .info, durationMs: nil))
                openTypedLLMKeys.append(key)
            }
            return
        }
        if eventType == "LLMResponseReceived" {
            let succeeded = (payload["success"] as? Bool) != false
            let duration = doubleVal(payload["latency_ms"])
            let summary = (payload["decision_summary"] as? String)
                ?? (payload["error"] as? String)
            let usage = ActivityCallUsage.read(payload)
            let key: String
            if let callId = usage.callId {
                key = typedLLMKey(callId: callId, agentId: normalizedAgentId)
                // A row the chat-side settle already closed keeps what it
                // wrote (TTFT, latency, usage).
                if let existing = rowsByKey[key], existing.status != .running {
                    openTypedLLMKeys.removeAll { $0 == key }
                    return
                }
            } else if let open = openTypedLLMKeys.first(where: {
                ingestExecutionId == nil || rowsByKey[$0]?.executionId == ingestExecutionId
            }) {
                // An id-less response settles the oldest id-keyed request still
                // open, so a server that drops the id on one side never
                // strands a running row.
                key = open
            } else {
                let timestampKey = scalarActivityValue(payload["timestamp"])
                    ?? scalarActivityValue(ingestStartedAtMs) ?? String(orderedKeys.count)
                key = "\(normalizedAgentId)::llm::\(timestampKey)"
            }
            openTypedLLMKeys.removeAll { $0 == key }
            // The model that answered, else the routed model, else the
            // capability asked for.
            let model = activityAnsweredModel(payload) ?? routedModelByKey[key]
                ?? nonEmptyString(payload["capability"]) ?? "model"
            let label = "\(agentPrefix)Thinking with \(model)"
            let detail = duration.map(formatLatency) ?? summary.map { trimTo($0, 180) }
            if rowsByKey[key] != nil {
                patch(key) {
                    $0.label = label
                    $0.detail = detail
                    $0.status = succeeded ? .done : .failed
                    $0.tone = succeeded ? .info : .error
                    $0.durationMs = duration
                    $0.callUsage = usage
                }
            } else {
                upsert(ActivityRow(
                    key: key,
                    kind: .llm,
                    label: label,
                    detail: detail,
                    status: succeeded ? .done : .failed,
                    tone: succeeded ? .info : .error,
                    durationMs: duration
                ))
                patch(key) { $0.callUsage = usage }
            }
            return
        }

        // ─── LLM lifecycle (coalesced by trace_id or iteration) ───
        if eventType.hasPrefix("llm.") {
            let trace = (payload["trace_id"] as? String)
                ?? (payload["iteration"] as? Int).map(String.init) ?? "llm"
            let key = chatLLMKey(eventType, payload: payload, traceKey: "\(normalizedAgentId)::llm::\(trace)",
                                 agentId: normalizedAgentId)
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
                // Task-run calls report `latency_ms`; chat calls `duration_ms`.
                let ms = doubleVal(payload["duration_ms"]) ?? doubleVal(payload["latency_ms"])
                let ttft = doubleVal(payload["ttft_ms"])
                let usage = ActivityCallUsage.read(payload)
                // The model that actually answered: an external harness can
                // take a turn the native profile requested, so the completion's
                // `model` wins over the request-time label.
                let answeredModel = nonEmptyString(payload["model"])
                if rowsByKey[key] != nil {
                    patch(key) { $0.status = .done; $0.tone = .info
                        if let answeredModel { $0.label = "\(agentPrefix)Thinking with \(answeredModel)" }
                        $0.detail = formatLatencyPair(ttft, ms); $0.durationMs = ms; $0.callUsage = usage }
                } else {
                    let model = (payload["model"] as? String)
                        ?? (payload["capability"] as? String) ?? "model"
                    upsert(ActivityRow(key: key, kind: .llm,
                                       label: "\(agentPrefix)Thinking with \(model)",
                                       detail: formatLatencyPair(ttft, ms), status: .done,
                                       tone: .info, durationMs: ms))
                    patch(key) { $0.callUsage = usage }
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
            // What the call actually did (a remote MCP tool and its query),
            // remembered per row because the finish rarely repeats the args.
            if let fresh = activityToolDisplay(tool: tool, payload: payload) {
                toolDisplayByKey[key] = fresh
            }
            let display = toolDisplayByKey[key]
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
                if rowsByKey[key] != nil {
                    // A later `.args` may be the first to name the call.
                    if let display, rowsByKey[key]?.status == .running {
                        patch(key) { $0.label = "\(agentPrefix)\(display.label)"; $0.detail = display.target }
                    }
                    return
                }
                var label = "\(agentPrefix)Calling \(tool)"
                var detail: String?
                if let display {
                    label = "\(agentPrefix)\(display.label)"
                    detail = display.target
                } else if tool == "delegate_to_agent", eventType.hasSuffix(".started"),
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
                let spawned = eventType == "tool.call.finished"
                    ? activitySpawnedTaskIds(payload, toolName: tool) : []
                let doneLabel = display.map { "\(agentPrefix)\($0.label)" } ?? "\(agentPrefix)\(tool) returned"
                // A named call keeps its query as the target; a raw result
                // preview never shows more than ~100 chars there.
                let doneDetail = display.map { $0.target ?? preview.map { trimTo($0, 100) } } ?? preview
                if rowsByKey[key] != nil {
                    patch(key) { $0.label = doneLabel; $0.status = .done; $0.tone = .tool
                        $0.detail = doneDetail; $0.durationMs = ms
                        if !spawned.isEmpty { $0.spawnedTaskIds = spawned } }
                } else {
                    upsert(ActivityRow(key: key, kind: .tool,
                                       label: doneLabel, detail: doneDetail,
                                       status: .done, tone: .tool, durationMs: ms))
                    if !spawned.isEmpty { patch(key) { $0.spawnedTaskIds = spawned } }
                }
            } else if eventType.hasSuffix(".failed") {
                let err = (payload["error"] as? String) ?? "tool failed"
                let failedLabel = "\(agentPrefix)\(display?.label ?? tool) failed"
                if rowsByKey[key] != nil {
                    patch(key) { $0.label = failedLabel; $0.status = .failed; $0.tone = .error; $0.detail = trimTo(err, 200) }
                } else {
                    upsert(ActivityRow(key: key, kind: .tool,
                                       label: failedLabel,
                                       detail: trimTo(err, 200), status: .failed,
                                       tone: .error, durationMs: nil))
                }
            }
            if tool != "tool", rowsByKey[key] != nil { patch(key) { $0.toolName = tool } }
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

/// An answer to a pause: the realtime events that resolve a HITL ask.
/// Durable replay can miss a child tool/LLM terminal event even though the
/// task itself is authoritative and terminal. Match web by settling those
/// stale running leaves so the card cannot spin or offer Stop forever.
func settleLeavesOfTerminalTasks(_ input: [ActivityRow]) -> [ActivityRow] {
    var rows = input
    let taskRows = rows.filter { $0.key.hasPrefix("task::") }
    guard !taskRows.isEmpty, !taskRows.contains(where: { $0.status == .running }) else {
        return rows
    }
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

/// The display label of a `task::` row, whose label is "<display> <status>"
/// or "<display> preparing final result".
private func activityTaskDisplayName(_ label: String) -> String? {
    let pending = " preparing final result"
    if label.hasSuffix(pending) { return String(label.dropLast(pending.count)) }
    guard let space = label.range(of: " ", options: .backwards) else { return nil }
    let name = String(label[..<space.lowerBound]).trimmingCharacters(in: .whitespaces)
    return name.isEmpty ? nil : name
}

/// Safety net for a delegated task whose step strip never settles: the
/// transcript's latest `task_status_update` is authoritative, so a terminal
/// card settles that task's still-running `task::<id>` row (and, once no task
/// row is running, its stale leaves). `terminalTaskStatuses` maps task id to
/// the card's lowercased terminal status.
func settleActivityRows(
    _ input: [ActivityRow],
    terminalTaskStatuses: [String: String]
) -> [ActivityRow] {
    guard !terminalTaskStatuses.isEmpty else { return input }
    var rows = input
    var changed = false
    for index in rows.indices where rows[index].key.hasPrefix("task::")
        && (rows[index].status == .running || rows[index].status == .waiting) {
        let keyTaskId = String(rows[index].key.dropFirst("task::".count))
        guard let status = terminalTaskStatuses[keyTaskId]
            ?? rows[index].taskId.flatMap({ terminalTaskStatuses[$0] }) else { continue }
        let failed = ["failed", "error", "cancelled", "canceled"].contains(status)
        let name = activityTaskDisplayName(rows[index].label) ?? rows[index].agentId ?? "Task"
        rows[index].status = failed ? .failed : .done
        rows[index].tone = failed ? .error : .tool
        rows[index].label = "\(name) \(status)"
        changed = true
    }
    return changed ? settleLeavesOfTerminalTasks(rows) : rows
}

func isPauseResolvedEvent(outer: String, event: String) -> Bool {
    if outer == "HitlResolved" { return true }
    return event == "hitl.resolved" || event == "input.received" || event == "input.resolved"
}

func isPauseEvent(outer: String, event: String) -> Bool {
    if outer == "HitlRequested" { return true }
    return event == "waiting_for_confirmation"
        || event == "execution.waiting_for_user"
        || event == "input.requested"
        || event == "hitl.requested"
        || event == "clarification.queued"
}

/// A tool row's human name and target, when the event says more than its raw
/// tool name.
struct ActivityToolDisplay: Equatable {
    let label: String
    let target: String?
}

private let activityCallToolSuffix = "__call_tool"
private let activityQueryKeys = ["sql", "query", "q", "term", "search", "question", "prompt"]

/// What a tool row should read instead of its raw tool name:
/// - the event's `detail` (the server's human summary, e.g.
///   "metabase-mcp · execute_sql — SELECT …"), split at " — " into label and target;
/// - for a pack's generic `<pack>__call_tool`, "<pack> · <name>" from the call's
///   args (`name`), with the first ~80 chars of a sql/query/term field of
///   `arguments_json` as the target.
/// Nil when neither applies (the row keeps "Calling <tool>" / "<tool> returned").
/// Never returns a raw JSON blob longer than ~100 chars.
func activityToolDisplay(tool: String, payload: [String: Any]) -> ActivityToolDisplay? {
    if let detail = payload["detail"] as? String,
       !detail.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
        if let split = detail.range(of: " — ") {
            let head = detail[..<split.lowerBound].trimmingCharacters(in: .whitespacesAndNewlines)
            let tail = detail[split.upperBound...].trimmingCharacters(in: .whitespacesAndNewlines)
            if !head.isEmpty {
                return ActivityToolDisplay(label: trimTo(head, 100), target: tail.isEmpty ? nil : trimTo(tail, 100))
            }
        }
        return ActivityToolDisplay(label: trimTo(detail, 100), target: nil)
    }
    guard let args = activityJSONObject(payload["args"] ?? payload["arguments"] ?? payload["input"]) else {
        return nil
    }
    if let suffix = ["__run", "__exec", "__execute"].first(where: { tool.hasSuffix($0) }),
       let argv = (args["args"] as? [String]) ?? (args["argv"] as? [String]),
       let command = argv.first,
       command.range(of: "^[A-Za-z][A-Za-z0-9_-]{0,63}$", options: .regularExpression) != nil {
        let verbs: Set<String> = ["list", "get", "show", "search", "create", "generate", "update", "delete", "remove", "send", "upload", "download", "status", "info", "help", "login", "logout", "run", "start", "stop", "wait", "select", "export", "import", "query", "render", "convert", "inspect"]
        let operation = argv.count > 1 && verbs.contains(argv[1]) ? "\(command) \(argv[1])" : command
        return ActivityToolDisplay(label: trimTo("\(tool.dropLast(suffix.count)) · \(operation)", 100), target: nil)
    }
    guard tool.hasSuffix(activityCallToolSuffix), tool.count > activityCallToolSuffix.count else { return nil }
    let pack = String(tool.dropLast(activityCallToolSuffix.count))
    let name = (args["tool_name"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        ?? (args["name"] as? String).flatMap { $0.isEmpty ? nil : $0 }
    let rawArguments = args["arguments_json"] ?? args["arguments"]
    var target: String?
    if let arguments = activityJSONObject(rawArguments) {
        for key in activityQueryKeys {
            if let value = arguments[key] as? String,
               !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                target = trimTo(value, 80)
                break
            }
        }
        if target == nil,
           let data = try? JSONSerialization.data(withJSONObject: arguments, options: [.sortedKeys]),
           let compact = String(data: data, encoding: .utf8), compact != "{}", compact.count <= 100 {
            target = compact
        }
    } else if let text = rawArguments as? String {
        let collapsed = trimTo(text, 1_000)
        if !collapsed.isEmpty, collapsed.count <= 100 { target = collapsed }
    }
    guard name != nil || target != nil else { return nil }
    return ActivityToolDisplay(label: "\(pack) · \(name ?? "call_tool")", target: target)
}

/// A dictionary, or a JSON object string decoded into one.
private func activityJSONObject(_ any: Any?) -> [String: Any]? {
    if let object = any as? [String: Any] { return object }
    guard let text = any as? String, let data = text.data(using: .utf8),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
    return object
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

/// A finite JSON number, never a boolean.
private func usageNumber(_ any: Any?) -> Double? {
    guard let number = any as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
    let value = number.doubleValue
    return value.isFinite ? value : nil
}

private func nonEmptyString(_ any: Any?) -> String? {
    guard let value = any as? String else { return nil }
    let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.isEmpty ? nil : trimmed
}

/// A typed LLM event's call id: `correlation.llm_call_id`, else a top-level
/// `llm_call_id`. Nil on servers that predate it.
private func activityLLMCallId(_ payload: [String: Any]) -> String? {
    nonEmptyString((payload["correlation"] as? [String: Any])?["llm_call_id"])
        ?? nonEmptyString(payload["llm_call_id"])
}

/// The model an LLM event names (web `answeredModelLabel`): `model`, else
/// `harness_model`, else `provider`.
private func activityAnsweredModel(_ payload: [String: Any]) -> String? {
    nonEmptyString(payload["model"]) ?? nonEmptyString(payload["harness_model"])
        ?? nonEmptyString(payload["provider"])
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
/// The turn reads "Waiting on you" while any pause row still waits; an
/// answered ask (settled to "Answered") clears it.
func activityAwaitsUser(_ rows: [ActivityRow]) -> Bool {
    rows.contains { $0.kind == .pause && $0.status == .waiting }
}

func activityPreviewRows(_ rows: [ActivityRow], limit: Int = 5) -> [ActivityRow] {
    guard limit > 0 else { return [] }
    return Array(rows.suffix(limit))
}

/// Copy and spacing for the chat turn's Steps header row. The row carries no
/// "Steps" word — the chevron, count and dots already say it, and the word made
/// the row wrap once usage and actions joined it.
enum ActivityStepsHeader {
    /// Opens the run in the task detail (Run act open).
    static let inspectTitle = "Inspect"
    static let inspectAccessibilityLabel = "Inspect run"
    /// Vertical padding around the header row: collapsed keeps its 7 pt;
    /// expanded is a little tighter (6/4) so the row sits close to its list.
    static func verticalPadding(expanded: Bool) -> (top: Double, bottom: Double) {
        expanded ? (6, 4) : (7, 7)
    }

    /// Space after the expand/collapse caret.
    static let chevronTrailingSpace: Double = 5

    /// What the header draws to say "still running". There is no separate
    /// spinner: while the run is active the latest step dot breathes and the
    /// caret wears an indeterminate ring; once it ends, neither.
    /// While active the total step count is hidden too: the row reads only
    /// "+N" and the latest dots.
    struct LiveIndicators: Equatable {
        let spinner: Bool
        let breathingDot: Bool
        let chevronRing: Bool
        let stepCount: Bool
    }

    static func liveIndicators(isRunning: Bool) -> LiveIndicators {
        LiveIndicators(spinner: false, breathingDot: isRunning, chevronRing: isRunning, stepCount: !isRunning)
    }

    /// One breath of the latest dot, in seconds.
    static let breathPeriod: Double = 1.4
    /// One turn of the caret's loading ring, in seconds.
    static let ringPeriod: Double = 1.0

    /// Breath phase 0...1 at time `t`: a cosine ease-in-out, 0 at each
    /// period's start, 1 at its middle.
    static func breathPhase(at t: Double) -> Double {
        let cycle = (t / breathPeriod).truncatingRemainder(dividingBy: 1)
        return (1 - cos(2 * Double.pi * cycle)) / 2
    }

    /// Ring rotation in turns 0..<1 at time `t`.
    static func ringTurns(at t: Double) -> Double {
        let turns = (t / ringPeriod).truncatingRemainder(dividingBy: 1)
        return turns < 0 ? turns + 1 : turns
    }
}

/// How many settled steps get a dot in the live Steps header; the rest fold
/// into "+N". Kept small so dots, count, usage and actions fit one line on a
/// narrow iPhone.
let activityStageDotLimit = 3

/// The Steps header's dot strip (web TurnStage): one dot per settled row, the
/// latest `limit` only; earlier settled rows fold into `hidden` ("+N").
/// Running rows get no dot — the header's spinner already says so.
func activityStageDots(
    _ rows: [ActivityRow],
    limit: Int = activityStageDotLimit
) -> (shown: [ActivityRow], hidden: Int) {
    let settled = rows.filter { $0.status != .running }
    let hidden = max(0, settled.count - max(0, limit))
    return (Array(settled.dropFirst(hidden)), hidden)
}

/// Stable tint slot 1...5 for a key — the same FNV-1a over UTF-16 code units
/// as web `tintFor`, so a tool keeps its colour on every client.
func activityTintSlot(_ key: String) -> Int {
    var hash: UInt32 = 0x811c9dc5
    for unit in key.utf16 {
        hash ^= UInt32(unit)
        hash = hash &* 0x01000193
    }
    return Int(hash % 5) + 1
}

/// The key a row's dot tint hashes: its tool, else its label (web `tool ?? label`).
func activityTintKey(_ row: ActivityRow) -> String { row.toolName ?? row.label }

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
