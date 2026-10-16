import CoreGraphics
import Foundation

/// The task plan graph the Plan Inspector shows and edits — the iOS port of
/// `ui/unified-ui/src/lib/types/plangraph.ts` and the graph helpers inside
/// `routes/(app)/PlanGraphView.svelte` (levels, waterfall order, cycle-safe
/// dependency editing, `requestSave`'s edge rebuild). Pure: no SwiftUI, no
/// networking.
///
/// **Nothing the server sent is dropped on save.** The Rust `PlanGraph` and
/// `PlanStep` (`magician/src/magician_v2/strategy/plan.rs`) carry more than
/// the web type knows (`session_policy`, `planning_metadata`,
/// `response_contract`, `success_criteria`, `parameter_provenance`, `role`,
/// `readiness`, `sub_steps`, …), and parameters are JSON values, not strings.
/// Every step and the graph keep the object they were decoded from, and the
/// save payload is that object with only the edited fields written over it.

// MARK: - JSON

/// A JSON value that round-trips through `JSONSerialization` untouched.
indirect enum PlanJSON: Equatable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case null
    case array([PlanJSON])
    case object([String: PlanJSON])

    init(_ any: Any?) {
        switch any {
        case nil, is NSNull: self = .null
        case let value as String: self = .string(value)
        case let value as NSNumber:
            // `JSONSerialization` hands booleans back as NSNumber too.
            if CFGetTypeID(value) == CFBooleanGetTypeID() { self = .bool(value.boolValue) }
            else { self = .number(value.doubleValue) }
        case let value as Bool: self = .bool(value)
        case let value as Int: self = .number(Double(value))
        case let value as Double: self = .number(value)
        case let value as [Any]: self = .array(value.map { PlanJSON($0) })
        case let value as [String: Any]: self = .object(value.mapValues { PlanJSON($0) })
        default: self = .null
        }
    }

    /// Back to Foundation objects for `JSONSerialization`.
    var foundation: Any {
        switch self {
        case .string(let value): return value
        case .number(let value):
            // Whole numbers go out as integers (epoch ms, counts).
            if value.rounded() == value, abs(value) < 9_007_199_254_740_992 { return Int64(value) }
            return value
        case .bool(let value): return value
        case .null: return NSNull()
        case .array(let values): return values.map(\.foundation)
        case .object(let values): return values.mapValues(\.foundation)
        }
    }

    var stringValue: String? { if case .string(let value) = self { return value } else { return nil } }
    var doubleValue: Double? { if case .number(let value) = self { return value } else { return nil } }
    var objectValue: [String: PlanJSON]? { if case .object(let value) = self { return value } else { return nil } }
    var arrayValue: [PlanJSON]? { if case .array(let value) = self { return value } else { return nil } }
    var isNull: Bool { self == .null }

    subscript(key: String) -> PlanJSON? { objectValue?[key] }

    /// One-line text for a pill or an editor: strings bare, the rest as JSON.
    var displayText: String {
        switch self {
        case .string(let value): return value
        case .null: return "null"
        default: return compactJSON
        }
    }

    var compactJSON: String { Self.serialize(self, pretty: false) }
    var prettyJSON: String { Self.serialize(self, pretty: true) }

    private static func serialize(_ value: PlanJSON, pretty: Bool) -> String {
        let object = value.foundation
        var options: JSONSerialization.WritingOptions = [.sortedKeys, .fragmentsAllowed, .withoutEscapingSlashes]
        if pretty { options.insert(.prettyPrinted) }
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: options),
              let text = String(data: data, encoding: .utf8) else { return "\(object)" }
        return text
    }

    /// Parse JSON text (any fragment); nil when it is not JSON.
    static func parse(_ text: String) -> PlanJSON? {
        guard let data = text.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
        else { return nil }
        return PlanJSON(object)
    }

    static func parse(data: Data) -> PlanJSON? {
        guard let object = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) else { return nil }
        return PlanJSON(object)
    }
}

// MARK: - Types

/// One parameter of a step, in display order. The value keeps its JSON type.
struct PlanParameter: Equatable, Identifiable {
    var key: String
    var value: PlanJSON
    var id: String { key }
}

/// One metadata entry (the server types metadata as string → string).
struct PlanKeyValue: Equatable, Identifiable {
    var key: String
    var value: String
    var id: String { key }
}

struct PlanEdge: Equatable, Hashable {
    let from: String
    let to: String
    var reason: String
}

struct PlanProvenance: Equatable {
    var strategy: String?
    var generator: String?
    var notes: String?
}

struct PlanStep: Equatable, Identifiable {
    var id: String
    var task: String
    var tool: String?
    var parameters: [PlanParameter]
    var expectedOutputs: [String]
    var confidence: Double
    var metadata: [PlanKeyValue]
    var providingAgentId: String?
    var stepHash: String?
    /// The steps this one waits for — read from the edges; saved back as edges.
    var dependsOn: [String]
    /// The object this step was decoded from (empty for a new step).
    var raw: [String: PlanJSON] = [:]

    var hasResolvedTool: Bool { !(tool ?? "").trimmingCharacters(in: .whitespaces).isEmpty }
    var isDelegated: Bool { !(providingAgentId ?? "").isEmpty }
    var isStart: Bool { dependsOn.isEmpty }

    /// A new step, as the web's Add step makes it.
    static func new(id: String) -> PlanStep {
        PlanStep(id: id, task: "New step", tool: nil, parameters: [], expectedOutputs: [],
                 confidence: 0.5, metadata: [], providingAgentId: nil, stepHash: nil, dependsOn: [])
    }
}

struct PlanGraph: Equatable {
    var steps: [PlanStep]
    var edges: [PlanEdge]
    /// Opaque: shown as a count, saved back as received.
    var unresolvedInputs: [PlanJSON]
    var confidence: Double?
    var provenance: PlanProvenance
    /// The graph object as received.
    var raw: [String: PlanJSON]

    static let empty = PlanGraph(steps: [], edges: [], unresolvedInputs: [], confidence: nil,
                                 provenance: PlanProvenance(), raw: [:])

    /// Decode a `plan_graph` object. Missing fields take empty values; nil
    /// when the value is not an object.
    init?(json: PlanJSON?) {
        guard let object = json?.objectValue else { return nil }
        raw = object
        edges = (object["edges"]?.arrayValue ?? []).compactMap { value in
            guard let from = value["from"]?.stringValue, let to = value["to"]?.stringValue else { return nil }
            return PlanEdge(from: from, to: to, reason: value["reason"]?.stringValue ?? "")
        }
        var seen = Set<String>()
        var decoded: [PlanStep] = []
        for (index, value) in (object["steps"]?.arrayValue ?? []).enumerated() {
            guard let stepObject = value.objectValue else { continue }
            var id = stepObject["id"]?.stringValue ?? ""
            if id.isEmpty || seen.contains(id) { id = "step_\(index + 1)" }
            seen.insert(id)
            decoded.append(PlanStep(
                id: id,
                task: stepObject["task"]?.stringValue ?? "",
                tool: stepObject["tool"]?.stringValue,
                parameters: (stepObject["parameters"]?.objectValue ?? [:])
                    .sorted { $0.key < $1.key }.map { PlanParameter(key: $0.key, value: $0.value) },
                expectedOutputs: (stepObject["expected_outputs"]?.arrayValue ?? []).compactMap(\.stringValue),
                confidence: stepObject["confidence"]?.doubleValue ?? 0,
                metadata: (stepObject["metadata"]?.objectValue ?? [:])
                    .sorted { $0.key < $1.key }.map { PlanKeyValue(key: $0.key, value: $0.value.displayText) },
                providingAgentId: stepObject["providing_agent_id"]?.stringValue,
                stepHash: stepObject["step_hash"]?.stringValue,
                dependsOn: [],
                raw: stepObject))
        }
        let ids = Set(decoded.map(\.id))
        // Edges are the truth (the web reads only them); a step's own
        // `depends_on` (the planner derives it from the edges) fills in only
        // dependencies the edges do not already name.
        for index in decoded.indices {
            var deps: [String] = []
            for edge in edges where edge.to == decoded[index].id && !deps.contains(edge.from) {
                deps.append(edge.from)
            }
            for dep in (decoded[index].raw["depends_on"]?.arrayValue ?? []).compactMap(\.stringValue)
            where ids.contains(dep) && dep != decoded[index].id && !deps.contains(dep) {
                deps.append(dep)
            }
            decoded[index].dependsOn = deps
        }
        steps = decoded
        unresolvedInputs = object["unresolved_inputs"]?.arrayValue ?? []
        confidence = object["confidence"]?.doubleValue
        let provenanceObject = object["provenance"]?.objectValue ?? [:]
        provenance = PlanProvenance(
            strategy: provenanceObject["strategy"]?.stringValue,
            generator: provenanceObject["generator"]?.stringValue,
            notes: provenanceObject["notes"]?.stringValue)
    }

    init(steps: [PlanStep], edges: [PlanEdge], unresolvedInputs: [PlanJSON], confidence: Double?,
         provenance: PlanProvenance, raw: [String: PlanJSON]) {
        self.steps = steps
        self.edges = edges
        self.unresolvedInputs = unresolvedInputs
        self.confidence = confidence
        self.provenance = provenance
        self.raw = raw
    }

    func step(_ id: String) -> PlanStep? { steps.first { $0.id == id } }

    /// Header chips: steps · edges · unresolved · confidence · strategy.
    var summary: PlanSummary {
        PlanSummary(steps: steps.count, edges: steps.reduce(0) { $0 + $1.dependsOn.count },
                    unresolved: unresolvedInputs.count, confidence: confidence,
                    strategy: provenance.strategy.flatMap { $0.isEmpty ? nil : $0 })
    }
}

struct PlanSummary: Equatable {
    let steps: Int
    let edges: Int
    let unresolved: Int
    let confidence: Double?
    let strategy: String?
}

/// The confidence band a node's fill and a step's badge use (web
/// `getNodeColor`): high ≥ 0.7, medium ≥ 0.4, else low.
enum PlanConfidenceBand: Equatable {
    case high, medium, low

    init(_ confidence: Double) {
        if confidence >= 0.7 { self = .high } else if confidence >= 0.4 { self = .medium } else { self = .low }
    }
}

// MARK: - Dependencies

extension PlanGraph {
    /// Steps that wait for `id` (outgoing edges).
    func dependents(of id: String) -> [String] {
        steps.filter { $0.dependsOn.contains(id) }.map(\.id)
    }

    /// No cycle among the dependencies (web `isPlanAcyclic`).
    var isAcyclic: Bool {
        let deps = Dictionary(steps.map { ($0.id, $0.dependsOn) }, uniquingKeysWith: { first, _ in first })
        var done = Set<String>()
        var onPath = Set<String>()
        func cycle(from id: String) -> Bool {
            if onPath.contains(id) { return true }
            if done.contains(id) { return false }
            onPath.insert(id)
            defer { onPath.remove(id); done.insert(id) }
            return (deps[id] ?? []).contains { cycle(from: $0) }
        }
        return !steps.contains { cycle(from: $0.id) }
    }

    /// Making `dep` a dependency of `stepId` would close a cycle: `dep`
    /// already waits (transitively) for `stepId`. Web `wouldCreateCycle`.
    func wouldCreateCycle(stepId: String, dependingOn dep: String) -> Bool {
        if stepId == dep { return true }
        var visited = Set<String>()
        var stack = [dep]
        while let next = stack.popLast() {
            if next == stepId { return true }
            guard visited.insert(next).inserted, let step = step(next) else { continue }
            stack.append(contentsOf: step.dependsOn)
        }
        return false
    }

    /// Steps `stepId` may start depending on: not itself, not one it already
    /// depends on, and none of its transitive dependents (any would make a
    /// cycle). Web `getAvailableDeps`, in plan order.
    func dependencyCandidates(for stepId: String) -> [PlanStep] {
        guard let step = step(stepId) else { return [] }
        var dependents = Set<String>()
        var frontier = [stepId]
        while let current = frontier.popLast() {
            for other in steps where other.dependsOn.contains(current) && dependents.insert(other.id).inserted {
                frontier.append(other.id)
            }
        }
        let existing = Set(step.dependsOn)
        return steps.filter { $0.id != stepId && !existing.contains($0.id) && !dependents.contains($0.id) }
    }

    /// The reason recorded on the edge `from → to`, if any.
    func reason(from: String, to: String) -> String? {
        edges.first { $0.from == from && $0.to == to }?.reason
    }
}

// MARK: - Levels and waterfall order

extension PlanGraph {
    /// Each step's level: 0 for a start step, else 1 + the deepest of its
    /// dependencies. A cycle is cut where it closes (web `calculateStepLevel`).
    var levels: [String: Int] {
        let deps = Dictionary(steps.map { ($0.id, $0.dependsOn) }, uniquingKeysWith: { first, _ in first })
        var memo: [String: Int] = [:]
        func level(_ id: String, _ path: Set<String>) -> Int {
            if let known = memo[id] { return known }
            if path.contains(id) { return 0 }
            let known = (deps[id] ?? []).filter { deps[$0] != nil }
            guard !known.isEmpty else { memo[id] = 0; return 0 }
            var next = path
            next.insert(id)
            let value = 1 + (known.map { level($0, next) }.max() ?? 0)
            memo[id] = value
            return value
        }
        var result: [String: Int] = [:]
        for step in steps { result[step.id] = level(step.id, []) }
        return result
    }

    /// The waterfall's order: Kahn with depth-first child placement — a step
    /// is placed as soon as all its dependencies are, children visited
    /// shallowest first; whatever a cycle strands is appended in plan order.
    /// Port of the web `topoSortIds`.
    var waterfallOrder: [String] {
        let ids = steps.map(\.id)
        let known = Set(ids)
        let deps = Dictionary(steps.map { ($0.id, $0.dependsOn.filter { known.contains($0) }) },
                              uniquingKeysWith: { first, _ in first })
        let levels = self.levels
        var children: [String: [String]] = [:]
        for step in steps {
            for dep in step.dependsOn where known.contains(dep) { children[dep, default: []].append(step.id) }
        }
        var ordered: [String] = []
        var visited = Set<String>()
        func visit(_ id: String) {
            guard visited.insert(id).inserted else { return }
            ordered.append(id)
            let sortedChildren = (children[id] ?? []).enumerated()
                .sorted { lhs, rhs in
                    let left = levels[lhs.element] ?? 0, right = levels[rhs.element] ?? 0
                    return left == right ? lhs.offset < rhs.offset : left < right
                }
                .map(\.element)
            for child in sortedChildren where !visited.contains(child)
                && (deps[child] ?? []).allSatisfy({ visited.contains($0) }) {
                visit(child)
            }
        }
        for id in ids where (deps[id] ?? []).isEmpty { visit(id) }
        var remaining = ids.filter { !visited.contains($0) }
        while !remaining.isEmpty {
            let before = visited.count
            for id in remaining where (deps[id] ?? []).allSatisfy({ visited.contains($0) }) { visit(id) }
            remaining = ids.filter { !visited.contains($0) }
            if visited.count == before {
                for id in remaining where visited.insert(id).inserted { ordered.append(id) }
                break
            }
        }
        return ordered
    }

    /// Steps in waterfall order with their levels.
    var waterfall: [(step: PlanStep, level: Int)] {
        let levels = self.levels
        return waterfallOrder.compactMap { id in step(id).map { ($0, levels[id] ?? 0) } }
    }
}

// MARK: - Layered layout

/// Where each node sits on the graph canvas: a top-to-bottom layered DAG.
struct PlanGraphLayout: Equatable {
    struct Metrics: Equatable {
        var nodeSize = CGSize(width: 168, height: 76)
        var horizontalGap: CGFloat = 28
        var verticalGap: CGFloat = 56
        var padding: CGFloat = 24
    }

    /// Node centres, in canvas points.
    let positions: [String: CGPoint]
    let size: CGSize
    /// Node ids per layer, left to right.
    let layers: [[String]]
    let metrics: Metrics

    /// Level = 1 + the deepest dependency (0 for a start step). Layer 0 keeps
    /// waterfall order; every later layer is ordered by the barycenter (mean
    /// x) of each node's parents, which pulls children under their parents
    /// and cuts edge crossings. Ties keep waterfall order.
    init(graph: PlanGraph, metrics: Metrics = Metrics()) {
        self.metrics = metrics
        let levels = graph.levels
        let order = graph.waterfallOrder
        let rank = Dictionary(order.enumerated().map { ($0.element, $0.offset) }, uniquingKeysWith: { first, _ in first })
        let depth = (levels.values.max() ?? -1) + 1
        var layers: [[String]] = Array(repeating: [], count: max(depth, 0))
        for id in order { layers[levels[id] ?? 0].append(id) }

        let pitchX = metrics.nodeSize.width + metrics.horizontalGap
        let pitchY = metrics.nodeSize.height + metrics.verticalGap
        let widest = layers.map(\.count).max() ?? 0
        let span = CGFloat(max(widest - 1, 0)) * pitchX
        var x: [String: CGFloat] = [:]
        for index in layers.indices {
            if index > 0 {
                func barycenter(_ id: String) -> CGFloat? {
                    let parents = (graph.step(id)?.dependsOn ?? []).compactMap { x[$0] }
                    return parents.isEmpty ? nil : parents.reduce(0, +) / CGFloat(parents.count)
                }
                layers[index] = layers[index].enumerated()
                    .map { (id: $0.element, offset: $0.offset, center: barycenter($0.element)) }
                    .sorted { lhs, rhs in
                        switch (lhs.center, rhs.center) {
                        case let (left?, right?) where left != right: return left < right
                        default: return (rank[lhs.id] ?? lhs.offset) < (rank[rhs.id] ?? rhs.offset)
                        }
                    }
                    .map(\.id)
            }
            let count = layers[index].count
            let start = (span - CGFloat(max(count - 1, 0)) * pitchX) / 2
            for (slot, id) in layers[index].enumerated() { x[id] = start + CGFloat(slot) * pitchX }
        }
        var positions: [String: CGPoint] = [:]
        let originX = metrics.padding + metrics.nodeSize.width / 2
        let originY = metrics.padding + metrics.nodeSize.height / 2
        for (index, layer) in layers.enumerated() {
            for id in layer {
                positions[id] = CGPoint(x: originX + (x[id] ?? 0), y: originY + CGFloat(index) * pitchY)
            }
        }
        self.positions = positions
        self.layers = layers
        size = layers.isEmpty ? .zero : CGSize(
            width: span + metrics.nodeSize.width + metrics.padding * 2,
            height: CGFloat(layers.count - 1) * pitchY + metrics.nodeSize.height + metrics.padding * 2)
    }

    /// Edges as drawable segments: bottom-centre of the parent to top-centre
    /// of the child.
    func segments(for graph: PlanGraph) -> [(edge: PlanEdge, from: CGPoint, to: CGPoint)] {
        graph.saveEdges().compactMap { edge in
            guard let from = positions[edge.from], let to = positions[edge.to] else { return nil }
            let half = metrics.nodeSize.height / 2
            return (edge, CGPoint(x: from.x, y: from.y + half), CGPoint(x: to.x, y: to.y - half))
        }
    }

    /// The zoom that fits the whole graph in `viewport`, clamped to 0.3–3×
    /// and never above 1× (a small graph stays at its natural size).
    func fitScale(in viewport: CGSize) -> CGFloat {
        guard size.width > 0, size.height > 0, viewport.width > 0, viewport.height > 0 else { return 1 }
        let scale = min(viewport.width / size.width, viewport.height / size.height, 1)
        return PlanGraphLayout.clampScale(scale)
    }

    static let scaleRange: ClosedRange<CGFloat> = 0.3...3
    static func clampScale(_ scale: CGFloat) -> CGFloat {
        min(max(scale, scaleRange.lowerBound), scaleRange.upperBound)
    }
}

// MARK: - Editing

extension PlanGraph {
    /// Add a step (`step_<epochms>`, confidence 0.5, no tool, no
    /// dependencies) at the end. Returns its id.
    @discardableResult
    mutating func addStep(now: Date = Date()) -> String {
        let base = "step_\(Int64((now.timeIntervalSince1970 * 1000).rounded()))"
        var id = base
        var suffix = 2
        while step(id) != nil { id = "\(base)_\(suffix)"; suffix += 1 }
        steps.append(.new(id: id))
        return id
    }

    /// Remove a step and every dependency on it.
    mutating func removeStep(_ id: String) {
        steps.removeAll { $0.id == id }
        for index in steps.indices { steps[index].dependsOn.removeAll { $0 == id } }
    }

    mutating func removeSteps(atOffsets offsets: IndexSet) {
        let ids = offsets.compactMap { steps.indices.contains($0) ? steps[$0].id : nil }
        ids.forEach { removeStep($0) }
    }

    /// Reorder (Steps edit mode drag). Same semantics as `Array.move`.
    mutating func moveSteps(fromOffsets source: IndexSet, toOffset destination: Int) {
        let moving = source.sorted().filter { steps.indices.contains($0) }.map { steps[$0] }
        guard !moving.isEmpty else { return }
        let before = source.filter { $0 < destination }.count
        for index in source.sorted(by: >) where steps.indices.contains(index) { steps.remove(at: index) }
        let insertAt = min(max(destination - before, 0), steps.count)
        steps.insert(contentsOf: moving, at: insertAt)
    }

    /// Add (`true`) or remove a dependency. Adding one that would close a
    /// cycle, or that already exists, changes nothing and returns false.
    @discardableResult
    mutating func setDependency(_ stepId: String, on dep: String, _ enabled: Bool) -> Bool {
        guard let index = steps.firstIndex(where: { $0.id == stepId }) else { return false }
        if enabled {
            guard step(dep) != nil, !steps[index].dependsOn.contains(dep),
                  !wouldCreateCycle(stepId: stepId, dependingOn: dep) else { return false }
            steps[index].dependsOn.append(dep)
        } else {
            guard steps[index].dependsOn.contains(dep) else { return false }
            steps[index].dependsOn.removeAll { $0 == dep }
        }
        return true
    }

    /// Make a step a start step: clear its dependencies.
    mutating func makeStart(_ stepId: String) {
        guard let index = steps.firstIndex(where: { $0.id == stepId }) else { return }
        steps[index].dependsOn = []
    }

    /// Replace a step's editable fields (its id, raw object and dependencies
    /// are kept).
    mutating func updateStep(_ stepId: String, _ change: (inout PlanStep) -> Void) {
        guard let index = steps.firstIndex(where: { $0.id == stepId }) else { return }
        var step = steps[index]
        change(&step)
        step.id = steps[index].id
        step.raw = steps[index].raw
        steps[index] = step
    }

    /// Re-sort steps into waterfall order (entering edit mode, and after a
    /// dependency change — web `sortEditableStepsByLevel`).
    mutating func sortByWaterfall() {
        let rank = Dictionary(waterfallOrder.enumerated().map { ($0.element, $0.offset) }, uniquingKeysWith: { first, _ in first })
        steps = steps.enumerated()
            .sorted { (rank[$0.element.id] ?? Int.max, $0.offset) < (rank[$1.element.id] ?? Int.max, $1.offset) }
            .map(\.element)
    }
}

// MARK: - Save payload

extension PlanGraph {
    /// Edges rebuilt from the steps' dependencies, in step order; each keeps
    /// the reason its old edge had, else "dependency" (web `rebuildEdges`).
    func saveEdges() -> [PlanEdge] {
        steps.flatMap { step in
            step.dependsOn.map { dep in
                let reason = reason(from: dep, to: step.id).flatMap { $0.isEmpty ? nil : $0 } ?? "dependency"
                return PlanEdge(from: dep, to: step.id, reason: reason)
            }
        }
    }

    /// The `plan_graph` object to PUT: the graph as received, with steps and
    /// edges written over it. `unresolved_inputs`, `confidence`, `provenance`
    /// and every field this app does not know are carried through.
    func savePayload() -> [String: PlanJSON] {
        var object = raw
        object["steps"] = .array(steps.map { .object($0.savePayload()) })
        object["edges"] = .array(saveEdges().map {
            .object(["from": .string($0.from), "to": .string($0.to), "reason": .string($0.reason)])
        })
        if object["unresolved_inputs"] == nil { object["unresolved_inputs"] = .array(unresolvedInputs) }
        if object["confidence"] == nil { object["confidence"] = .number(confidence ?? 0) }
        if object["provenance"] == nil {
            object["provenance"] = .object([
                "strategy": provenance.strategy.map(PlanJSON.string) ?? .string("manual"),
                "generator": provenance.generator.map(PlanJSON.string) ?? .null,
                "notes": provenance.notes.map(PlanJSON.string) ?? .null
            ])
        }
        return object
    }
}

extension PlanStep {
    /// The step object to save: the received object with the editable fields
    /// written over it. `depends_on` mirrors the rebuilt edges so the two
    /// never disagree (omitted when empty, as the server serialises it).
    func savePayload() -> [String: PlanJSON] {
        var object = raw
        object["id"] = .string(id)
        object["task"] = .string(task)
        let trimmedTool = tool?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        object["tool"] = trimmedTool.isEmpty ? .null : .string(trimmedTool)
        object["parameters"] = .object(Dictionary(parameters.map { ($0.key, $0.value) }, uniquingKeysWith: { _, last in last }))
        object["expected_outputs"] = .array(expectedOutputs.map(PlanJSON.string))
        object["confidence"] = .number(confidence)
        object["metadata"] = .object(Dictionary(metadata.map { ($0.key, PlanJSON.string($0.value)) }, uniquingKeysWith: { _, last in last }))
        if dependsOn.isEmpty { object.removeValue(forKey: "depends_on") }
        else { object["depends_on"] = .array(dependsOn.map(PlanJSON.string)) }
        return object
    }
}

extension PlanParameter {
    /// Parse editor text back to a value: a parameter that was not a string
    /// keeps its JSON type when the text is valid JSON; anything else is a
    /// string.
    static func value(fromText text: String, previous: PlanJSON?) -> PlanJSON {
        if let previous, previous.stringValue == nil, let parsed = PlanJSON.parse(text) { return parsed }
        return .string(text)
    }
}
