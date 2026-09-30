import SwiftUI
import MarkdownUI

// Native, read-only rendering for published GAUI/MUIJ surfaces. Published
// surfaces are display artifacts: controls are deliberately not dispatched
// from this renderer until mobile has the same authority-bound interaction
// contract as the web client.

enum MuijDocumentError: Error, Equatable, LocalizedError {
    case invalidEnvelope
    case unsupportedVersion(String)
    case tooManyComponents
    case nestingTooDeep
    case duplicateID(String)
    case invalidComponent(String)

    var errorDescription: String? {
        switch self {
        case .invalidEnvelope: return "This briefing has an invalid dashboard document."
        case .unsupportedVersion(let version): return "Dashboard version \(version) is not supported yet."
        case .tooManyComponents: return "This dashboard is too large to render safely."
        case .nestingTooDeep: return "This dashboard is nested too deeply to render safely."
        case .duplicateID(let id): return "This dashboard contains a duplicate component (\(id))."
        case .invalidComponent(let id): return "Dashboard component \(id) is malformed."
        }
    }
}

struct MuijDocumentModel: Equatable {
    static let maximumDepth = 32
    static let maximumTopLevelComponents = 500
    static let maximumTotalComponents = 2_000

    let version: String
    let agentID: String
    let layout: [MuijComponentModel]

    static func parse(_ raw: JSONValue) -> Result<MuijDocumentModel, MuijDocumentError> {
        guard let envelope = raw.objectValue,
              let version = envelope["muij_version"]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines),
              let agentID = envelope["agent_id"]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines),
              !agentID.isEmpty,
              let layout = envelope["layout"]?.arrayValue else {
            return .failure(.invalidEnvelope)
        }
        guard version == "1.0" else { return .failure(.unsupportedVersion(version)) }
        guard layout.count <= maximumTopLevelComponents else { return .failure(.tooManyComponents) }

        var stack = layout.map { ($0, 0) }
        var seen = Set<String>()
        var count = 0
        while let (rawComponent, depth) = stack.popLast() {
            guard depth < maximumDepth else { return .failure(.nestingTooDeep) }
            guard let component = rawComponent.objectValue,
                  let id = component["id"]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !id.isEmpty,
                  let type = component["component_type"]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !type.isEmpty,
                  component["label"]?.stringValue != nil,
                  component["children"] == nil || component["children"]?.arrayValue != nil,
                  component["props"] == nil || component["props"]?.objectValue != nil else {
                return .failure(.invalidComponent(rawComponent.objectValue?["id"]?.stringValue ?? "unknown"))
            }
            guard seen.insert(id).inserted else { return .failure(.duplicateID(id)) }
            count += 1
            guard count <= maximumTotalComponents else { return .failure(.tooManyComponents) }
            for child in component["children"]?.arrayValue ?? [] { stack.append((child, depth + 1)) }
        }

        return .success(MuijDocumentModel(
            version: version,
            agentID: agentID,
            layout: layout.compactMap { MuijComponentModel.decode($0, depth: 0) }
        ))
    }
}

struct MuijComponentModel: Identifiable, Equatable {
    let id: String
    let type: String
    let label: String
    let props: [String: JSONValue]
    let staticSnapshot: JSONValue?
    let children: [MuijComponentModel]

    fileprivate static func decode(_ raw: JSONValue, depth: Int) -> MuijComponentModel? {
        guard depth < MuijDocumentModel.maximumDepth,
              let value = raw.objectValue,
              let id = value["id"]?.stringValue,
              let type = value["component_type"]?.stringValue else { return nil }
        return MuijComponentModel(
            id: id,
            type: type,
            label: value["label"]?.stringValue ?? "",
            props: value["props"]?.objectValue ?? [:],
            staticSnapshot: value["static_snapshot"],
            children: (value["children"]?.arrayValue ?? []).compactMap { decode($0, depth: depth + 1) }
        )
    }

    func string(_ key: String, fallback: String = "") -> String {
        props[key]?.stringValue ?? fallback
    }

    func number(_ key: String) -> Double? {
        switch props[key] {
        case .number(let value): return value.isFinite ? value : nil
        case .string(let value): return Double(value)
        default: return nil
        }
    }

    func bool(_ key: String, fallback: Bool = false) -> Bool {
        switch props[key] {
        case .bool(let value): return value
        case .number(let value): return value != 0
        case .string(let value): return ["true", "1", "yes", "on"].contains(value.lowercased())
        default: return fallback
        }
    }

    var displayLabel: String { string("title", fallback: string("label", fallback: label)) }
}

struct MuijDocumentView: View {
    let document: JSONValue
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        switch MuijDocumentModel.parse(document) {
        case .success(let model):
            LazyVStack(alignment: .leading, spacing: 12) {
                ForEach(model.layout) { component in
                    MuijComponentView(component: component, depth: 0)
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Dashboard")
        case .failure(let error):
            Label(error.localizedDescription, systemImage: "exclamationmark.triangle.fill")
                .font(.themed(13))
                .foregroundColor(theme.warningColor)
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(theme.elevatedColor)
                .clipShape(RoundedRectangle(cornerRadius: 11))
        }
    }
}

struct MuijJSONContentView: View {
    let value: JSONValue
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(value.presentationRows(limit: 100).enumerated()), id: \.offset) { _, row in
                VStack(alignment: .leading, spacing: 3) {
                    if !row.key.isEmpty {
                        Text(row.key).font(.themed(11, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                    }
                    Text(row.value).font(.themed(14)).foregroundColor(theme.textColor).textSelection(.enabled)
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(theme.elevatedColor)
                .clipShape(RoundedRectangle(cornerRadius: 9))
            }
        }
    }
}

private struct MuijComponentView: View {
    let component: MuijComponentModel
    let depth: Int
    @ObservedObject private var theme = ThemeManager.shared
    @State private var selectedGraphNodeID = ""

    @ViewBuilder var body: some View {
        if depth >= MuijDocumentModel.maximumDepth {
            EmptyView()
        } else {
            switch component.type {
            case "Stack", "Container", "ScrollArea": children
            case "Grid":
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 145), spacing: 10)], spacing: 10) {
                    ForEach(component.children) { MuijComponentView(component: $0, depth: depth + 1) }
                }
            case "SplitPanel": children
            case "Card", "Panel": card
            case "Text": text
            case "Markdown": markdown
            case "MetricCard", "Gauge": metric
            case "Table", "EntityGrid": table
            case "DataList": dataList
            case "Progress", "ProgressBar": progress
            case "Badge", "Tag": badge
            case "ActivityFeed": activityFeed
            case "Alert", "Toast", "Notification": notice
            case "EmptyState": emptyState
            case "Divider": Divider()
            case "CodeBlock", "DiffViewer", "TerminalTransient": code
            case "PieChart", "BarChart", "LineChart", "AreaChart", "ScatterChart", "TrendChart", "Sparkline", "Heatmap": chart
            case "Tree", "TreeNode": tree
            case "Graph": graph
            case "Image": image
            case "Button", "TextField", "Select", "Form", "TextArea", "NumberField", "Slider", "Checkbox", "RadioGroup", "MultiSelect", "DatePicker", "Toggle", "SearchInput", "ActionBus", "ApprovalFlow", "ConfirmDialog": readOnlyControl
            default: fallback
            }
        }
    }

    private var children: some View {
        LazyVStack(alignment: .leading, spacing: 10) {
            ForEach(component.children) { MuijComponentView(component: $0, depth: depth + 1) }
        }
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !component.displayLabel.isEmpty {
                Text(component.displayLabel).font(.themed(16, weight: .semibold)).foregroundColor(theme.textColor)
            }
            children
        }
        .padding(13).frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.elevatedColor)
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(theme.cardBorderColor, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

    private var text: some View {
        Text(component.string("children", fallback: component.string("text", fallback: component.label)))
            .font(component.string("variant") == "title" ? .themed(22, weight: .bold) : .themed(14))
            .foregroundColor(theme.textColor).textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var markdown: some View {
        Markdown(component.string("content", fallback: component.label))
            .markdownTextStyle { ForegroundColor(theme.textColor) }
            .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
    }

    private var metric: some View {
        let value = component.metricValue
        let trend = component.string("trend")
        return VStack(alignment: .leading, spacing: 5) {
            Text(value).font(.system(size: 23, weight: .bold, design: .rounded)).foregroundColor(theme.textColor)
            Text(component.displayLabel.isEmpty ? "Metric" : component.displayLabel)
                .font(.themed(11, weight: .medium)).foregroundColor(theme.secondaryTextColor)
            if !trend.isEmpty || !component.string("trendLabel").isEmpty {
                Label(component.string("trendLabel", fallback: trend.capitalized),
                      systemImage: trend == "up" ? "arrow.up.right" : trend == "down" ? "arrow.down.right" : "minus")
                    .font(.themed(10, weight: .semibold))
                    .foregroundColor(trend == "down" ? theme.dangerColor : theme.accentColor)
            }
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.surfaceColor).overlay(RoundedRectangle(cornerRadius: 11).stroke(theme.cardBorderColor))
        .clipShape(RoundedRectangle(cornerRadius: 11))
    }

    private var table: some View {
        let columns = component.tableColumns
        let rows = component.tableRows
        return VStack(alignment: .leading, spacing: 7) {
            if !component.displayLabel.isEmpty { Text(component.displayLabel).font(.themed(14, weight: .semibold)) }
            if columns.isEmpty || rows.isEmpty { Text("No data").foregroundColor(theme.secondaryTextColor) }
            else {
                ScrollView(.horizontal, showsIndicators: true) {
                    Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 7) {
                        GridRow { ForEach(columns, id: \.key) { Text($0.label).font(.themed(11, weight: .bold)).foregroundColor(theme.secondaryTextColor) } }
                        Divider()
                        ForEach(Array(rows.prefix(200).enumerated()), id: \.offset) { _, row in
                            GridRow { ForEach(columns, id: \.key) { Text(row[$0.key]?.compactDisplay ?? "—").font(.themed(12)).foregroundColor(theme.textColor) } }
                        }
                    }.padding(.vertical, 3)
                }
            }
        }
        .padding(11).background(theme.surfaceColor).clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private var dataList: some View {
        let items = component.props["items"]?.arrayValue ?? []
        return VStack(alignment: .leading, spacing: 7) {
            ForEach(Array(items.prefix(200).enumerated()), id: \.offset) { _, item in
                let record = item.objectValue ?? [:]
                HStack(alignment: .firstTextBaseline) {
                    Text(record["key"]?.compactDisplay ?? "").font(.themed(11, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                    Spacer()
                    Text(record["value"]?.compactDisplay ?? "—").font(.themed(12)).foregroundColor(theme.textColor).multilineTextAlignment(.trailing)
                }
            }
        }
    }

    private var progress: some View {
        let percent = min(100, max(0, component.number("percent") ?? 0))
        return VStack(alignment: .leading, spacing: 5) {
            if !component.displayLabel.isEmpty { Text(component.displayLabel).font(.themed(12, weight: .medium)) }
            ProgressView(value: percent, total: 100).tint(theme.accentColor)
            if component.bool("showPercent", fallback: true) { Text("\(Int(percent.rounded()))%").font(.themed(10)).foregroundColor(theme.secondaryTextColor) }
        }
    }

    private var badge: some View {
        Text(component.string("text", fallback: component.displayLabel))
            .font(.themed(11, weight: .semibold)).foregroundColor(theme.accentColor)
            .padding(.horizontal, 9).padding(.vertical, 5).background(theme.accentColor.opacity(0.12)).clipShape(Capsule())
    }

    private var activityFeed: some View {
        let items = component.props["items"]?.arrayValue ?? []
        return VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(items.prefix(100).enumerated()), id: \.offset) { _, item in
                let record = item.objectValue ?? [:]
                HStack(alignment: .top, spacing: 9) {
                    Circle().fill(theme.accentColor).frame(width: 7, height: 7).padding(.top, 5)
                    VStack(alignment: .leading, spacing: 2) {
                        Text([record["actor"]?.compactDisplay, record["action"]?.compactDisplay, record["target"]?.compactDisplay].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " "))
                            .font(.themed(12)).foregroundColor(theme.textColor)
                        if let timestamp = record["timestamp"]?.compactDisplay { Text(timestamp).font(.themed(9)).foregroundColor(theme.secondaryTextColor) }
                    }
                }
            }
        }
    }

    private var notice: some View {
        Label(component.string("message", fallback: component.string("body", fallback: component.displayLabel)), systemImage: "info.circle.fill")
            .font(.themed(12)).foregroundColor(theme.textColor).padding(11).frame(maxWidth: .infinity, alignment: .leading)
            .background(theme.accentColor.opacity(0.10)).clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private var emptyState: some View {
        ContentUnavailableView(component.string("title", fallback: component.displayLabel.isEmpty ? "No results" : component.displayLabel),
                               systemImage: "tray", description: Text(component.string("description")))
    }

    private var code: some View {
        ScrollView(.horizontal) {
            Text(component.string("code", fallback: component.string("content", fallback: component.string("text", fallback: component.label))))
                .font(.themedMono(.caption)).foregroundColor(theme.textColor).textSelection(.enabled).padding(10)
        }.background(theme.surfaceColor).clipShape(RoundedRectangle(cornerRadius: 9))
    }

    private var chart: some View {
        let data = component.chartData
        let maximum = data.map(\.value).map(abs).max() ?? 1
        return VStack(alignment: .leading, spacing: 8) {
            if !component.displayLabel.isEmpty { Text(component.displayLabel).font(.themed(14, weight: .semibold)) }
            if data.isEmpty { Text("No chart data").font(.themed(12)).foregroundColor(theme.secondaryTextColor) }
            ForEach(data.prefix(30)) { point in
                HStack(spacing: 8) {
                    Text(point.label).font(.themed(10)).foregroundColor(theme.secondaryTextColor).frame(width: 82, alignment: .leading).lineLimit(1)
                    GeometryReader { proxy in
                        RoundedRectangle(cornerRadius: 3).fill(theme.accentColor.opacity(0.75))
                            .frame(width: max(2, proxy.size.width * CGFloat(abs(point.value) / max(maximum, 0.000_001))))
                    }.frame(height: 8)
                    Text(point.value.formatted()).font(.themedMono(10)).foregroundColor(theme.textColor)
                }
            }
        }.padding(11).background(theme.surfaceColor).clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private var tree: some View {
        VStack(alignment: .leading, spacing: 5) {
            if !component.displayLabel.isEmpty { Label(component.displayLabel, systemImage: "point.3.connected.trianglepath.dotted") }
            children.padding(.leading, component.children.isEmpty ? 0 : 12)
        }.font(.themed(12)).foregroundColor(theme.textColor)
    }

    /// Graph family (plan 1.5): deterministic layered/radial/list layouts with
    /// local-only select/expand. Reveal order renders statically on iOS; the
    /// renderer keeps its read-only contract (no dispatched controls).
    private var graph: some View {
        let model = component.graphModel
        let effectiveSelection = selectedGraphNodeID.isEmpty ? (model.focusNodeID ?? "") : selectedGraphNodeID
        return VStack(alignment: .leading, spacing: 8) {
            if !component.displayLabel.isEmpty {
                Label(component.displayLabel, systemImage: "point.3.connected.trianglepath.dotted")
                    .font(.themed(14, weight: .semibold)).foregroundColor(theme.textColor)
            }
            if model.nodes.isEmpty {
                Text("No items").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            } else {
                switch model.layout {
                case .layered:
                    MuijGraphTieredView(model: model, selectedNodeID: effectiveSelection,
                                        onSelect: { selectedGraphNodeID = $0 })
                case .radial:
                    MuijGraphRadialView(model: model, selectedNodeID: effectiveSelection,
                                        onSelect: { selectedGraphNodeID = $0 })
                case .list:
                    MuijGraphListView(model: model, selectedNodeID: effectiveSelection,
                                      onSelect: { selectedGraphNodeID = $0 })
                }
                if let selected = model.nodes.first(where: { $0.id == effectiveSelection }) {
                    MuijGraphNodeDetailView(node: selected,
                                            incoming: model.edges.filter { $0.to == selected.id }.count,
                                            outgoing: model.edges.filter { $0.from == selected.id }.count)
                }
            }
        }
    }

    @ViewBuilder private var image: some View {
        if let url = component.safeMediaURL {
            AsyncImage(url: url) { image in image.resizable().scaledToFit() } placeholder: { ProgressView() }
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .accessibilityLabel(component.displayLabel.isEmpty ? "Dashboard image" : component.displayLabel)
        } else { fallback }
    }

    private var readOnlyControl: some View {
        HStack {
            Image(systemName: "lock.fill")
            Text(component.displayLabel.isEmpty ? component.type : component.displayLabel)
            Spacer()
            Text("Read only")
        }
        .font(.themed(11)).foregroundColor(theme.secondaryTextColor).padding(10)
        .overlay(RoundedRectangle(cornerRadius: 9).stroke(theme.cardBorderColor))
        .accessibilityHint("Published briefing controls cannot be changed from this view")
    }

    private var fallback: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack { Text(component.type).font(.themedMono(10)); if !component.displayLabel.isEmpty { Text(component.displayLabel).font(.themed(12)) } }
            children
        }
        .foregroundColor(theme.secondaryTextColor).padding(10)
        .overlay(RoundedRectangle(cornerRadius: 9).stroke(theme.cardBorderColor, style: StrokeStyle(lineWidth: 1, dash: [4])))
    }
}

// MARK: - Graph family (plan 1.5)

/// Bounded, fail-soft graph model. Mirrors the Rust validator caps
/// (200 nodes / 400 edges); malformed members and dangling edges are skipped
/// so a hostile document degrades to a smaller graph instead of failing the
/// whole briefing.
struct MuijGraphModel: Equatable {
    enum Layout: Equatable { case layered, radial, list }

    struct MetaEntry: Identifiable, Equatable {
        let key: String
        let value: String
        var id: String { key }
    }

    struct Node: Identifiable, Equatable {
        let id: String
        let label: String
        let kind: String?
        let metadata: [MetaEntry]
        let tier: Int
    }

    struct Edge: Equatable {
        let from: String
        let to: String
        let label: String?
    }

    let nodes: [Node]
    let edges: [Edge]
    let layout: Layout
    let focusNodeID: String?
    let revealOrder: [String]
}

extension MuijComponentModel {
    static let graphMaximumNodes = 200
    static let graphMaximumEdges = 400
    static let graphMaximumNodeIdCharacters = 128
    static let graphMaximumTextCharacters = 200
    static let graphMaximumMetadataEntries = 8

    var graphModel: MuijGraphModel {
        var seen = Set<String>()
        var nodes: [MuijGraphModel.Node] = []
        for record in (props["nodes"]?.arrayValue ?? []).compactMap(\.objectValue) {
            // Ids compare exactly like the Rust validator (no trim), so a
            // valid document's edges always resolve; the id-length cap drops
            // over-long ids like every other cap violation. Caps count
            // Unicode scalars (matching the Rust validator's chars()),
            // not composed grapheme clusters.
            guard nodes.count < MuijComponentModel.graphMaximumNodes,
                  let id = record["id"]?.stringValue,
                  !id.isEmpty,
                  id.unicodeScalars.count <= MuijComponentModel.graphMaximumNodeIdCharacters,
                  seen.insert(id).inserted else { continue }
            let label = record["label"]?.stringValue ?? id
            guard label.unicodeScalars.count <= MuijComponentModel.graphMaximumTextCharacters else { continue }
            let kind = (record["kind"]?.stringValue)
                .flatMap {
                    $0.unicodeScalars.count <= MuijComponentModel.graphMaximumTextCharacters ? $0 : nil
                }
            var metadata: [MuijGraphModel.MetaEntry] = []
            if let meta = record["metadata"]?.objectValue {
                for key in meta.keys.sorted() where metadata.count < MuijComponentModel.graphMaximumMetadataEntries {
                    guard key.unicodeScalars.count <= 64,
                          let display = meta[key]?.compactDisplay,
                          display.unicodeScalars.count <= 200 else { continue }
                    metadata.append(MuijGraphModel.MetaEntry(key: key, value: display))
                }
            }
            nodes.append(MuijGraphModel.Node(id: id, label: label, kind: kind, metadata: metadata, tier: 0))
        }

        let ids = Set(nodes.map(\.id))
        var edges: [MuijGraphModel.Edge] = []
        for record in (props["edges"]?.arrayValue ?? []).compactMap(\.objectValue) {
            guard edges.count < MuijComponentModel.graphMaximumEdges,
                  let from = record["from"]?.stringValue,
                  let to = record["to"]?.stringValue,
                  ids.contains(from), ids.contains(to) else { continue }
            let label = (record["label"]?.stringValue)
                .flatMap {
                    $0.unicodeScalars.count <= MuijComponentModel.graphMaximumTextCharacters ? $0 : nil
                }
            edges.append(MuijGraphModel.Edge(from: from, to: to, label: label))
        }

        let layout: MuijGraphModel.Layout
        switch props["layout"]?.stringValue {
        case "radial": layout = .radial
        case "list": layout = .list
        default: layout = .layered
        }

        let focus = props["focus_node_id"]?.stringValue ?? props["focusNodeId"]?.stringValue
        let reveal = ((props["reveal_order"] ?? props["revealOrder"])?.arrayValue ?? [])
            .compactMap(\.stringValue)
            .filter(ids.contains)
            .prefix(MuijComponentModel.graphMaximumNodes)

        return MuijGraphModel(nodes: MuijComponentModel.assignGraphTiers(nodes: nodes, edges: edges),
                              edges: edges,
                              layout: layout,
                              focusNodeID: focus.flatMap { ids.contains($0) ? $0 : nil },
                              revealOrder: Array(reveal))
    }

    /// Deterministic topological tiers (Kahn). Cycle members share the final
    /// tier so the layout never loops or deadlocks on cyclic input.
    static func assignGraphTiers(nodes: [MuijGraphModel.Node], edges: [MuijGraphModel.Edge]) -> [MuijGraphModel.Node] {
        let position = Dictionary(uniqueKeysWithValues: nodes.enumerated().map { ($1.id, $0) })
        var incoming = [Int](repeating: 0, count: nodes.count)
        var outgoing: [Int: [Int]] = [:]
        for edge in edges {
            guard let from = position[edge.from], let to = position[edge.to], from != to else { continue }
            incoming[to] += 1
            outgoing[from, default: []].append(to)
        }

        var tiers = [Int](repeating: 0, count: nodes.count)
        var frontier = incoming.indices.filter { incoming[$0] == 0 }.sorted()
        var tier = 0
        var placed = Set<Int>()
        while !frontier.isEmpty {
            for index in frontier {
                tiers[index] = tier
                placed.insert(index)
            }
            var next = [Int]()
            for index in frontier {
                for target in outgoing[index] ?? [] where incoming[target] > 0 {
                    incoming[target] -= 1
                    if incoming[target] == 0, !placed.contains(target), !next.contains(target) {
                        next.append(target)
                    }
                }
            }
            frontier = next.sorted()
            tier += 1
        }
        for index in tiers.indices where !placed.contains(index) { tiers[index] = tier }
        return nodes.enumerated().map { index, node in
            MuijGraphModel.Node(id: node.id, label: node.label, kind: node.kind,
                                metadata: node.metadata, tier: tiers[index])
        }
    }
}

/// Layered layout: one horizontal tier row per topological level.
private struct MuijGraphTieredView: View {
    let model: MuijGraphModel
    let selectedNodeID: String
    let onSelect: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        let tiers = Dictionary(grouping: model.nodes, by: \.tier).sorted { $0.key < $1.key }
        return VStack(alignment: .leading, spacing: 10) {
            ForEach(tiers, id: \.key) { _, tierNodes in
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 88), spacing: 6)], spacing: 6) {
                    ForEach(tierNodes) { node in
                        MuijGraphNodeChip(node: node, isSelected: node.id == selectedNodeID,
                                          theme: theme, onSelect: onSelect)
                    }
                }
            }
        }
    }
}

/// Radial layout: one ring in declaration order with edges drawn as lines.
private struct MuijGraphRadialView: View {
    let model: MuijGraphModel
    let selectedNodeID: String
    let onSelect: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        GeometryReader { proxy in
            let center = CGPoint(x: proxy.size.width / 2, y: proxy.size.height / 2)
            let radius = max(64, min(proxy.size.width, proxy.size.height) / 2 - 44)
            ZStack {
                Path { path in
                    for edge in model.edges {
                        guard let from = nodePosition(edge.from, center: center, radius: radius),
                              let to = nodePosition(edge.to, center: center, radius: radius) else { continue }
                        path.move(to: from)
                        path.addLine(to: to)
                    }
                }
                .stroke(theme.cardBorderColor, lineWidth: 1)
                ForEach(model.nodes) { node in
                    MuijGraphNodeChip(node: node, isSelected: node.id == selectedNodeID,
                                      theme: theme, onSelect: onSelect)
                        .position(nodePosition(node.id, center: center, radius: radius) ?? center)
                }
            }
        }
        .frame(height: 290)
    }

    private func nodePosition(_ id: String, center: CGPoint, radius: CGFloat) -> CGPoint? {
        guard let index = model.nodes.firstIndex(where: { $0.id == id }) else { return nil }
        let angle = (2 * .pi * Double(index) / Double(max(model.nodes.count, 1))) - .pi / 2
        return CGPoint(x: center.x + radius * CGFloat(cos(angle)),
                       y: center.y + radius * CGFloat(sin(angle)))
    }
}

/// List layout: a vertical stack with a bounded adjacency summary per node.
private struct MuijGraphListView: View {
    let model: MuijGraphModel
    let selectedNodeID: String
    let onSelect: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(model.nodes) { node in
                let targets = model.edges.filter { $0.from == node.id }
                    .compactMap { edge in model.nodes.first(where: { $0.id == edge.to })?.label }
                Button {
                    onSelect(node.id)
                } label: {
                    HStack(spacing: 8) {
                        MuijGraphNodeChip(node: node, isSelected: node.id == selectedNodeID,
                                          theme: theme, onSelect: nil)
                        Spacer(minLength: 8)
                        Text(targets.isEmpty ? "—" : "→ \(targets.prefix(3).joined(separator: ", "))")
                            .font(.themed(10)).foregroundColor(theme.secondaryTextColor)
                            .lineLimit(1)
                    }
                }
                .buttonStyle(.plain)
            }
        }
    }
}

private struct MuijGraphNodeChip: View {
    let node: MuijGraphModel.Node
    let isSelected: Bool
    let theme: ThemeManager
    let onSelect: ((String) -> Void)?

    var body: some View {
        VStack(spacing: 1) {
            Text(node.label).lineLimit(1)
            if let kind = node.kind {
                Text(kind).font(.themed(9)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
            }
        }
        .font(.themed(11)).foregroundColor(theme.textColor)
        .padding(.horizontal, 8).padding(.vertical, 5)
        .frame(maxWidth: .infinity)
        .background(isSelected ? theme.accentColor.opacity(0.14) : theme.surfaceColor)
        .overlay(RoundedRectangle(cornerRadius: 7)
            .stroke(isSelected ? theme.accentColor : theme.cardBorderColor, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 7))
        .onTapGesture { onSelect?(node.id) }
    }
}

private struct MuijGraphNodeDetailView: View {
    let node: MuijGraphModel.Node
    let incoming: Int
    let outgoing: Int
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(node.label).font(.themed(13, weight: .semibold)).foregroundColor(theme.textColor)
            if let kind = node.kind {
                Text(kind).font(.themed(10)).foregroundColor(theme.secondaryTextColor)
            }
            ForEach(node.metadata) { entry in
                HStack(alignment: .firstTextBaseline) {
                    Text(entry.key).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                    Spacer()
                    Text(entry.value).font(.themed(11)).foregroundColor(theme.textColor)
                        .multilineTextAlignment(.trailing)
                }
            }
            Text("→ \(outgoing)   ← \(incoming)")
                .font(.themedMono(10))
                .foregroundColor(theme.secondaryTextColor)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.elevatedColor)
        .clipShape(RoundedRectangle(cornerRadius: 9))
    }
}

extension MuijComponentModel {
    struct TableColumn { let key: String; let label: String }
    struct ChartPoint: Identifiable { let id: Int; let label: String; let value: Double }

    var metricValue: String {
        let explicit = string("value")
        if !explicit.isEmpty { return explicit }
        if let record = staticSnapshot?.arrayValue?.first?.objectValue,
           let key = string("valueField").nilIfEmpty ?? record.keys.sorted().first,
           let value = record[key]?.compactDisplay { return value }
        return "—"
    }

    var tableColumns: [TableColumn] {
        let explicit = (props["columns"]?.arrayValue ?? []).compactMap { value -> TableColumn? in
            if let key = value.stringValue { return TableColumn(key: key, label: key) }
            guard let object = value.objectValue else { return nil }
            let key = object["key"]?.stringValue ?? object["label"]?.stringValue ?? ""
            guard !key.isEmpty else { return nil }
            return TableColumn(key: key, label: object["label"]?.stringValue ?? key)
        }
        if !explicit.isEmpty { return Array(explicit.prefix(12)) }
        return Array((tableRows.first?.keys.sorted() ?? []).prefix(12)).map { TableColumn(key: $0, label: $0) }
    }

    var tableRows: [[String: JSONValue]] {
        let raw = props["rows"]?.arrayValue ?? staticSnapshot?.arrayValue ?? []
        return raw.prefix(200).compactMap(\.objectValue)
    }

    var chartData: [ChartPoint] {
        let raw = props["data"]?.arrayValue ?? staticSnapshot?.arrayValue ?? []
        let labelField = string("labelField", fallback: string("xField", fallback: "label"))
        let valueField = string("valueField", fallback: string("yField", fallback: "value"))
        return raw.prefix(30).enumerated().compactMap { index, value in
            guard let record = value.objectValue else { return nil }
            let numeric: Double?
            switch record[valueField] {
            case .number(let value): numeric = value
            case .string(let value): numeric = Double(value)
            default: numeric = nil
            }
            guard let numeric, numeric.isFinite else { return nil }
            return ChartPoint(id: index, label: record[labelField]?.compactDisplay ?? "Item \(index + 1)", value: numeric)
        }
    }

    var safeMediaURL: URL? {
        guard let raw = props["src"]?.stringValue ?? props["url"]?.stringValue,
              let url = URL(string: raw), ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { return nil }
        return url
    }
}

extension JSONValue {
    var compactDisplay: String? {
        switch self {
        case .string(let value): return value
        case .number(let value): return value.formatted()
        case .bool(let value): return value ? "Yes" : "No"
        case .null: return "—"
        case .array(let value): return "\(value.count) items"
        case .object(let value): return "\(value.count) fields"
        }
    }

    struct PresentationRow { let key: String; let value: String }
    func presentationRows(limit: Int) -> [PresentationRow] {
        switch self {
        case .object(let object):
            return object.keys.sorted().prefix(limit).map { PresentationRow(key: $0.humanized, value: object[$0]?.compactDisplay ?? "—") }
        case .array(let array):
            return array.prefix(limit).enumerated().map { index, value in PresentationRow(key: "Item \(index + 1)", value: value.compactDisplay ?? "—") }
        default: return [PresentationRow(key: "", value: compactDisplay ?? "—")]
        }
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
    var humanized: String {
        replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ").capitalized
    }
}
