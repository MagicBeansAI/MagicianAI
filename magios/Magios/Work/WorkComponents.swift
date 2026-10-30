import SwiftUI

/// A separate tap target: reading help never navigates, saves or toggles work.
struct WorkHelpButton: View {
    let topic: WorkHelpTopic
    var compact = false
    @State private var showing = false
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        Button { showing.toggle() } label: {
            Image(systemName: "info.circle").font(.themed(16))
                .foregroundColor(theme.secondaryTextColor)
                .frame(width: compact ? 32 : 44, height: compact ? 32 : 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("About \(topic.title)")
        .accessibilityHint("Shows an explanation without changing your work.")
        .help(topic.explanation)
        .popover(isPresented: $showing) {
            VStack(alignment: .leading, spacing: 10) {
                Text(topic.title).font(.themed(17, weight: .semibold))
                Text(topic.explanation).font(.themed(15)).fixedSize(horizontal: false, vertical: true)
                Button("Done") { showing = false }.font(.themed(15, weight: .semibold)).tint(theme.accentColor)
            }
            .foregroundColor(theme.textColor).padding(16).frame(idealWidth: 300, maxWidth: 340)
            .presentationBackground(theme.cardColor)
            .presentationCompactAdaptation(.popover)
        }
    }
}

/// Compact human-readable values first; nested records expand on demand and
/// remain selectable. No field is silently discarded from an operator record.
/// Fields read as rows: a caption above the value, hairlines between rows,
/// nested records as disclosure rows that say how much they hold.
struct WorkRecordView: View {
    let value: WorkJSON
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        switch value {
        case .object(let fields):
            VStack(alignment: .leading, spacing: 0) {
                if fields.isEmpty { WorkRecordView.empty("No details reported.") }
                ForEach(Array(fields.keys.sorted().enumerated()), id: \.element) { index, key in
                    if let child = fields[key] {
                        if index > 0 { WorkRowDivider() }
                        if child.object != nil || child.array != nil {
                            DisclosureGroup(WorkRecordView.label(key)) {
                                WorkRecordView(value: child)
                                    .padding(.leading, 12)
                                    .overlay(alignment: .leading) { Rectangle().fill(theme.cardBorderColor).frame(width: 2) }
                            }
                            .disclosureGroupStyle(WorkDisclosureStyle(value: WorkRecordView.summary(child)))
                        } else {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(Self.label(key)).font(.themed(12, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                                Text(child.display).font(.themed(15)).foregroundColor(theme.textColor).textSelection(.enabled)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .padding(.vertical, 9)
                            .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
                        }
                    }
                }
            }
        case .array(let rows):
            VStack(alignment: .leading, spacing: 0) {
                if rows.isEmpty { WorkRecordView.empty("None") }
                ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                    if index > 0 { WorkRowDivider() }
                    WorkRecordView(value: row)
                }
            }
        default:
            Text(value.display).font(.themed(15)).foregroundColor(theme.textColor).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.vertical, 9)
                .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
        }
    }
    static func label(_ key: String) -> String { key.replacingOccurrences(of: "_", with: " ").capitalized }
    static func summary(_ value: WorkJSON) -> String {
        if let rows = value.array { return rows.isEmpty ? "None" : rows.count == 1 ? "1 item" : "\(rows.count) items" }
        if let fields = value.object { return fields.isEmpty ? "Empty" : fields.count == 1 ? "1 field" : "\(fields.count) fields" }
        return ""
    }
    private static func empty(_ text: String) -> some View {
        Text(text).font(.themed(15)).foregroundColor(ThemeManager.shared.secondaryTextColor)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
    }
}

/// Loading, failure and success notices for one resource, drawn the same
/// way on every Work screen.
struct WorkResourceStatus: View {
    @ObservedObject var resource: WorkResource
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        if resource.loading && resource.value == nil {
            HStack(spacing: 10) {
                ProgressView().tint(theme.accentColor)
                Text("Loading…").font(.themed(15)).foregroundColor(theme.secondaryTextColor)
            }
            .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight)
            .padding(.vertical, 8)
            .accessibilityElement(children: .combine)
        }
        if let error = resource.error {
            WorkGroupCard {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Image(systemName: "exclamationmark.triangle.fill").font(.themed(15, weight: .semibold))
                        .foregroundColor(theme.dangerColor).accessibilityHidden(true)
                    Text(error).font(.themed(15)).foregroundColor(theme.textColor)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(.vertical, 12)
                WorkRowDivider()
                WorkButtonRow(title: "Retry", systemImage: "arrow.clockwise") { resource.refresh() }
            }
        }
        if let notice = resource.notice {
            Label(notice, systemImage: "checkmark.circle.fill")
                .font(.themed(13, weight: .semibold)).foregroundColor(theme.successColor)
                .padding(.horizontal, WorkMetrics.cardInset)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

struct WorkEndpointView: View {
    let title: String
    let path: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: resource)
            if let value = resource.value { WorkGroupCard { WorkRecordView(value: value) } }
        }
        .navigationTitle(title).navigationBarTitleDisplayMode(.inline)
        .task(id: path) { resource.load(path) }
        .refreshable { resource.refresh() }
        .toolbar { Button("Refresh", systemImage: "arrow.clockwise") { resource.refresh() } }
    }
}

// MARK: - Grouped rows
//
// Crew member screens group their options the way Settings does: a caption
// header above a themed card, rows separated by hairlines, labels leading and
// values, controls or chevrons trailing, every row at least 44 pt tall.

enum WorkMetrics {
    static let screenPadding: CGFloat = 16
    static let sectionSpacing: CGFloat = 20
    static let cardRadius: CGFloat = 14
    static let cardInset: CGFloat = 16
    static let rowMinHeight: CGFloat = 44
    static let rowVerticalPadding: CGFloat = 10
    static let fieldRadius: CGFloat = 8
    static let panelRadius: CGFloat = 10
}

/// Caption above a grouped card ("MEMORY").
struct WorkSectionHeader: View {
    let title: String
    @ObservedObject private var theme = ThemeManager.shared
    init(_ title: String) { self.title = title }
    var body: some View {
        HStack(spacing: 4) {
        Text(title).textCase(.uppercase)
            .font(.themed(12, weight: .semibold)).tracking(0.6)
            .foregroundColor(theme.secondaryTextColor)
            .accessibilityAddTraits(.isHeader)
        if let topic = WorkHelpTopic.forLabel(title) { WorkHelpButton(topic: topic, compact: true) }
        Spacer(minLength: 0)
        }
        .padding(.horizontal, WorkMetrics.cardInset)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A titled group of rows in one themed card. Rows place `WorkRowDivider`
/// between themselves.
struct WorkGroupCard<Content: View>: View {
    let title: String?
    let footer: String?
    let content: Content
    @ObservedObject private var theme = ThemeManager.shared
    init(_ title: String? = nil, footer: String? = nil, @ViewBuilder content: () -> Content) {
        self.title = title; self.footer = footer; self.content = content()
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let title { WorkSectionHeader(title) }
            VStack(alignment: .leading, spacing: 0) { content }
                .padding(.horizontal, WorkMetrics.cardInset)
                .frame(maxWidth: .infinity, alignment: .leading)
                .workCardSurface()
            if let footer {
                Text(footer).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, WorkMetrics.cardInset)
            }
        }
    }
}

private struct WorkCardSurface: ViewModifier {
    @ObservedObject private var theme = ThemeManager.shared
    func body(content: Content) -> some View {
        content
            .background(theme.cardColor, in: RoundedRectangle(cornerRadius: WorkMetrics.cardRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: WorkMetrics.cardRadius, style: .continuous).strokeBorder(theme.cardBorderColor, lineWidth: 1))
    }
}

/// An inset panel inside a card (an opened focus area).
private struct WorkInsetPanel: ViewModifier {
    @ObservedObject private var theme = ThemeManager.shared
    func body(content: Content) -> some View {
        content
            .padding(.horizontal, 12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(theme.backgroundColor, in: RoundedRectangle(cornerRadius: WorkMetrics.panelRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: WorkMetrics.panelRadius, style: .continuous).strokeBorder(theme.cardBorderColor, lineWidth: 1))
    }
}

/// A bordered text input surface, the same box `ModelCatalogPicker` draws.
private struct WorkFieldSurface: ViewModifier {
    @ObservedObject private var theme = ThemeManager.shared
    func body(content: Content) -> some View {
        content
            .padding(.horizontal, 12).padding(.vertical, 10)
            .frame(minHeight: WorkMetrics.rowMinHeight)
            .background(theme.controlColor, in: RoundedRectangle(cornerRadius: WorkMetrics.fieldRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: WorkMetrics.fieldRadius, style: .continuous).strokeBorder(theme.controlBorderColor, lineWidth: 1))
    }
}

extension View {
    func workCardSurface() -> some View { modifier(WorkCardSurface()) }
    func workInsetPanel() -> some View { modifier(WorkInsetPanel()) }
    func workFieldSurface() -> some View { modifier(WorkFieldSurface()) }
}

/// One-pixel separator between rows of a group.
struct WorkRowDivider: View {
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.displayScale) private var scale
    var body: some View {
        Rectangle().fill(theme.cardBorderColor).frame(height: 1 / max(scale, 1))
            .accessibilityHidden(true)
    }
}

enum WorkRowAccessory { case none, chevron, external, menu, expanded(Bool) }

/// Icon · title · value · accessory, used by every tappable row.
struct WorkRowLabel: View {
    let title: String
    var systemImage: String? = nil
    var value: String? = nil
    var accessory: WorkRowAccessory = .chevron
    var tint: Color? = nil
    /// A second line under the title (a path, a count or a short status).
    var subtitle: String? = nil
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dynamicTypeSize) private var typeSize
    @ScaledMetric(relativeTo: .body) private var iconWidth: CGFloat = 24
    private var accessoryImage: String? {
        switch accessory {
        case .none: return nil
        case .chevron: return "chevron.right"
        case .external: return "arrow.up.right"
        case .menu: return "chevron.up.chevron.down"
        case .expanded(let open): return open ? "chevron.down" : "chevron.right"
        }
    }
    var body: some View {
        HStack(spacing: 12) {
            if let systemImage {
                Image(systemName: systemImage).font(.themed(16, weight: .medium))
                    .foregroundColor(tint ?? theme.accentColor)
                    .frame(width: iconWidth).accessibilityHidden(true)
            }
            let stacked = typeSize.isAccessibilitySize
            (stacked ? AnyLayout(VStackLayout(alignment: .leading, spacing: 2)) : AnyLayout(HStackLayout(spacing: 8))) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.themed(16)).foregroundColor(tint ?? theme.textColor)
                        .multilineTextAlignment(.leading).fixedSize(horizontal: false, vertical: true)
                    if let subtitle, !subtitle.isEmpty {
                        Text(subtitle).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                            .multilineTextAlignment(.leading).fixedSize(horizontal: false, vertical: true)
                    }
                }
                if !stacked { Spacer(minLength: 0) }
                if let value, !value.isEmpty {
                    Text(value).font(.themed(15)).foregroundColor(theme.secondaryTextColor)
                        .lineLimit(stacked ? nil : 1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let accessoryImage {
                Image(systemName: accessoryImage).font(.themed(13, weight: .semibold))
                    .foregroundColor(theme.secondaryTextColor.opacity(0.75))
                    .accessibilityHidden(true)
            }
        }
        .padding(.vertical, WorkMetrics.rowVerticalPadding)
        .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
        .contentShape(Rectangle())
    }
}

/// A whole-row navigation link with a trailing chevron. VoiceOver and UI
/// tests see the title as the link's label and the value as its value.
struct WorkNavigationRow<Destination: View>: View {
    let title: String
    var systemImage: String? = nil
    var value: String? = nil
    var subtitle: String? = nil
    @ViewBuilder let destination: () -> Destination
    var body: some View {
        NavigationLink { destination() } label: {
            WorkRowLabel(title: title, systemImage: systemImage, value: value, subtitle: subtitle)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .accessibilityValue(value ?? "")
    }
}

/// A row that shows text without acting (an empty state inside a group).
struct WorkInfoRow: View {
    let title: String
    var systemImage: String? = nil
    var value: String? = nil
    var body: some View {
        WorkRowLabel(title: title, systemImage: systemImage, value: value, accessory: .none, tint: nil)
            .accessibilityElement(children: .combine)
    }
}

/// Label leading, control trailing; the label stacks above the control at
/// accessibility text sizes so neither clips.
struct WorkFormRow<Control: View>: View {
    let title: String
    var detail: String? = nil
    /// Keep the visible title as its own text for VoiceOver (a heading the
    /// control does not repeat).
    var exposesTitle = false
    @ViewBuilder let control: Control
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dynamicTypeSize) private var typeSize
    init(_ title: String, detail: String? = nil, exposesTitle: Bool = false, @ViewBuilder control: () -> Control) {
        self.title = title; self.detail = detail; self.exposesTitle = exposesTitle; self.control = control()
    }
    private var titleView: some View {
        HStack(spacing: 4) {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.themed(16)).foregroundColor(theme.textColor)
            if let detail {
                Text(detail).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            }
        }
        // The control carries the same label for VoiceOver.
        .accessibilityHidden(!exposesTitle)
        if let topic = WorkHelpTopic.forLabel(title) { WorkHelpButton(topic: topic, compact: true) }
        }
    }
    private var stackedRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            titleView.fixedSize(horizontal: false, vertical: true)
            control
        }
    }
    var body: some View {
        Group {
            if typeSize.isAccessibilitySize {
                stackedRow
            } else {
                // One line when label and control both fit unwrapped;
                // otherwise the control moves under the label.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 12) {
                        titleView.fixedSize()
                        Spacer(minLength: 8)
                        control.fixedSize()
                    }
                    stackedRow
                }
            }
        }
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
    }
}

/// A labelled input: caption above a bordered field.
struct WorkFieldRow<Field: View>: View {
    let label: String
    @ViewBuilder let field: Field
    @ObservedObject private var theme = ThemeManager.shared
    init(_ label: String, @ViewBuilder field: () -> Field) { self.label = label; self.field = field() }
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 4) {
            Text(label).font(.themed(13, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityHidden(true)
            if let topic = WorkHelpTopic.forLabel(label) { WorkHelpButton(topic: topic, compact: true) }
            }
            field
        }
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A DisclosureGroup drawn as a row: title leading, optional value, and a
/// chevron that turns down when open. The content follows inside the group.
struct WorkDisclosureStyle: DisclosureGroupStyle {
    var systemImage: String? = nil
    var value: String? = nil
    func makeBody(configuration: Configuration) -> some View {
        WorkDisclosureBody(configuration: configuration, systemImage: systemImage, value: value)
    }
}

private struct WorkDisclosureBody: View {
    let configuration: DisclosureGroupStyleConfiguration
    let systemImage: String?
    let value: String?
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dynamicTypeSize) private var typeSize
    @ScaledMetric(relativeTo: .body) private var iconWidth: CGFloat = 24
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { configuration.isExpanded.toggle() }
            } label: {
                HStack(spacing: 12) {
                    if let systemImage {
                        Image(systemName: systemImage).font(.themed(16, weight: .medium))
                            .foregroundColor(theme.accentColor).frame(width: iconWidth).accessibilityHidden(true)
                    }
                    configuration.label
                        .font(.themed(16)).foregroundColor(theme.textColor)
                        .multilineTextAlignment(.leading).fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 8)
                    if let value, !value.isEmpty, !typeSize.isAccessibilitySize {
                        Text(value).font(.themed(15)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
                            .accessibilityHidden(true)
                    }
                    Image(systemName: "chevron.right").font(.themed(13, weight: .semibold))
                        .foregroundColor(theme.secondaryTextColor.opacity(0.75))
                        .rotationEffect(.degrees(configuration.isExpanded ? 90 : 0))
                        .accessibilityHidden(true)
                }
                .padding(.vertical, WorkMetrics.rowVerticalPadding)
                .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityValue(configuration.isExpanded ? "Expanded" : "Collapsed")
            if configuration.isExpanded {
                configuration.content
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.bottom, 12)
            }
        }
    }
}

/// A small status chip ("active", "Primary"): a 6 pt rounded square, like the web's chips.
struct WorkChip: View {
    let title: String
    var systemImage: String? = nil
    var tone: Color
    var body: some View {
        HStack(spacing: 4) {
            if let systemImage { Image(systemName: systemImage).accessibilityHidden(true) }
            Text(title).lineLimit(1).minimumScaleFactor(0.7)
        }
        .font(.themed(12, weight: .semibold)).foregroundColor(tone)
        .padding(.horizontal, 8).padding(.vertical, 3)
        .background(tone.opacity(0.12), in: RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).strokeBorder(tone.opacity(0.35), lineWidth: 1))
    }
}

// MARK: - Screens, actions and lists
//
// Every Work page is a scroll of grouped cards on the theme background with
// the same gutters; actions are rows, lists are rows with chips.

/// The scrolling page every Work screen sits in.
struct WorkPage<Content: View>: View {
    var spacing: CGFloat = WorkMetrics.sectionSpacing
    let content: Content
    @ObservedObject private var theme = ThemeManager.shared
    init(spacing: CGFloat = WorkMetrics.sectionSpacing, @ViewBuilder content: () -> Content) {
        self.spacing = spacing; self.content = content()
    }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: spacing) { content }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(WorkMetrics.screenPadding)
        }
        .foregroundColor(theme.textColor)
        .background(theme.backgroundColor.ignoresSafeArea())
    }
}

/// An action drawn as a row: icon and title in the accent colour, or in the
/// danger colour for destructive actions; no chevron.
struct WorkButtonRow: View {
    let title: String
    var systemImage: String? = nil
    var role: ButtonRole? = nil
    var value: String? = nil
    var accessory: WorkRowAccessory = .none
    let action: () -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.isEnabled) private var isEnabled
    var body: some View {
        Button(role: role, action: action) {
            WorkRowLabel(title: title, systemImage: systemImage, value: value, accessory: accessory,
                         tint: role == .destructive ? theme.dangerColor : theme.accentColor)
        }
        .buttonStyle(.plain)
        .opacity(isEnabled ? 1 : 0.45)
        .accessibilityLabel(title)
    }
}

/// A chip's text, tone and optional icon, for rows that list several.
struct WorkChipSpec: Hashable {
    let title: String
    var systemImage: String? = nil
    let tone: Color
}

/// Chips on one line, or stacked when they do not fit.
struct WorkChipRow: View {
    let chips: [WorkChipSpec]
    var body: some View {
        if !chips.isEmpty {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 6) { ForEach(chips, id: \.self) { WorkChip(title: $0.title, systemImage: $0.systemImage, tone: $0.tone) } }
                VStack(alignment: .leading, spacing: 6) { ForEach(chips, id: \.self) { WorkChip(title: $0.title, systemImage: $0.systemImage, tone: $0.tone) } }
            }
        }
    }
}

/// A list entry: icon, title, secondary lines and chips, chevron trailing.
/// Lists place these in a `WorkGroupCard` separated by `WorkRowDivider`.
struct WorkItemLabel: View {
    let title: String
    var subtitle: String? = nil
    var detail: String? = nil
    var systemImage: String? = nil
    var iconTint: Color? = nil
    var chips: [WorkChipSpec] = []
    var accessory: WorkRowAccessory = .chevron
    @ObservedObject private var theme = ThemeManager.shared
    @ScaledMetric(relativeTo: .body) private var iconWidth: CGFloat = 24
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            if let systemImage {
                Image(systemName: systemImage).font(.themed(16, weight: .medium))
                    .foregroundColor(iconTint ?? theme.accentColor)
                    .frame(width: iconWidth).padding(.top, 1).accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.themed(16, weight: .semibold)).foregroundColor(theme.textColor)
                    .multilineTextAlignment(.leading).fixedSize(horizontal: false, vertical: true)
                if let subtitle, !subtitle.isEmpty {
                    Text(subtitle).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        .multilineTextAlignment(.leading).fixedSize(horizontal: false, vertical: true)
                }
                if let detail, !detail.isEmpty {
                    Text(detail).font(.themed(14)).foregroundColor(theme.textColor.opacity(0.85))
                        .lineLimit(3).multilineTextAlignment(.leading)
                }
                if !chips.isEmpty { WorkChipRow(chips: chips).padding(.top, 2) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if case .chevron = accessory {
                Image(systemName: "chevron.right").font(.themed(13, weight: .semibold))
                    .foregroundColor(theme.secondaryTextColor.opacity(0.75))
                    .frame(maxHeight: .infinity).accessibilityHidden(true)
            }
        }
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
        .contentShape(Rectangle())
    }
}

/// The summary card at the top of a detail screen: title, secondary lines,
/// chips, then optional rows (actions) under a hairline.
struct WorkHeaderCard<Rows: View>: View {
    let title: String
    var subtitle: String? = nil
    var detail: String? = nil
    var chips: [WorkChipSpec] = []
    let rows: Rows
    @ObservedObject private var theme = ThemeManager.shared
    init(_ title: String, subtitle: String? = nil, detail: String? = nil, chips: [WorkChipSpec] = [], @ViewBuilder rows: () -> Rows) {
        self.title = title; self.subtitle = subtitle; self.detail = detail; self.chips = chips; self.rows = rows()
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 8) {
                Text(title).font(.themed(22, weight: .semibold)).foregroundColor(theme.textColor)
                    .fixedSize(horizontal: false, vertical: true).accessibilityAddTraits(.isHeader)
                if let subtitle, !subtitle.isEmpty {
                    Text(subtitle).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                }
                if let detail, !detail.isEmpty {
                    Text(detail).font(.themed(15)).foregroundColor(theme.textColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if !chips.isEmpty { WorkChipRow(chips: chips).padding(.top, 2) }
            }
            .padding(.vertical, WorkMetrics.cardInset)
            if Rows.self != EmptyView.self {
                WorkRowDivider()
                rows
            }
        }
        .padding(.horizontal, WorkMetrics.cardInset)
        .frame(maxWidth: .infinity, alignment: .leading)
        .workCardSurface()
    }
}

extension WorkHeaderCard where Rows == EmptyView {
    init(_ title: String, subtitle: String? = nil, detail: String? = nil, chips: [WorkChipSpec] = []) {
        self.init(title, subtitle: subtitle, detail: detail, chips: chips) { EmptyView() }
    }
}

/// Secondary explanatory text inside a card, between rows.
struct WorkNote: View {
    let text: String
    var tone: Color? = nil
    @ObservedObject private var theme = ThemeManager.shared
    init(_ text: String, tone: Color? = nil) { self.text = text; self.tone = tone }
    var body: some View {
        Text(text).font(.themed(13)).foregroundColor(tone ?? theme.secondaryTextColor)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Previous · Page n · Next for server-paged lists.
struct WorkPager: View {
    @Binding var page: Int
    let hasMore: Bool
    var disabled = false
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dynamicTypeSize) private var typeSize
    var body: some View {
        // At accessibility sizes the arrows stand alone so the words never
        // break; VoiceOver still reads Previous and Next.
        let compact = typeSize.isAccessibilitySize
        HStack(spacing: 12) {
            Button { page = max(0, page - 1) } label: {
                HStack(spacing: 6) { Image(systemName: "chevron.left").accessibilityHidden(true); if !compact { Text("Previous") } }
                    .frame(minWidth: WorkMetrics.rowMinHeight, minHeight: WorkMetrics.rowMinHeight, alignment: .leading).contentShape(Rectangle())
            }
            .disabled(page == 0 || disabled)
            .accessibilityLabel("Previous")
            Spacer(minLength: 8)
            Text("Page \(page + 1)").font(.themed(13, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
            Spacer(minLength: 8)
            Button { page += 1 } label: {
                HStack(spacing: 6) { if !compact { Text("Next") }; Image(systemName: "chevron.right").accessibilityHidden(true) }
                    .frame(minWidth: WorkMetrics.rowMinHeight, minHeight: WorkMetrics.rowMinHeight, alignment: .trailing).contentShape(Rectangle())
            }
            .disabled(!hasMore || disabled)
            .accessibilityLabel("Next")
        }
        .font(.themed(15, weight: .medium))
        .buttonStyle(.borderless)
        .tint(theme.accentColor)
        .padding(.horizontal, WorkMetrics.cardInset)
    }
}

/// A themed in-page search box (the same field surface as form inputs).
struct WorkSearchField: View {
    let prompt: String
    @Binding var text: String
    @ObservedObject private var theme = ThemeManager.shared
    init(_ prompt: String, text: Binding<String>) { self.prompt = prompt; self._text = text }
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").font(.themed(15, weight: .medium))
                .foregroundColor(theme.secondaryTextColor).accessibilityHidden(true)
            TextField(prompt, text: $text)
                .font(.themed(16)).foregroundColor(theme.textColor)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .submitLabel(.search)
            if !text.isEmpty {
                Button { text = "" } label: {
                    Image(systemName: "xmark.circle.fill").foregroundColor(theme.secondaryTextColor.opacity(0.7))
                        .frame(minWidth: 28, minHeight: 28)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear search")
            }
        }
        .padding(.horizontal, 12)
        .frame(minHeight: WorkMetrics.rowMinHeight)
        .background(theme.controlColor, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(theme.controlBorderColor, lineWidth: 1))
    }
}

/// A centred empty, error or loading message for a whole list.
struct WorkEmptyState: View {
    let title: String
    var systemImage: String = "tray"
    var message: String? = nil
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: systemImage).font(.themed(28, weight: .regular))
                .foregroundColor(theme.secondaryTextColor.opacity(0.6)).accessibilityHidden(true)
            Text(title).font(.themed(16, weight: .semibold)).foregroundColor(theme.textColor)
                .multilineTextAlignment(.center)
            if let message {
                Text(message).font(.themed(14)).foregroundColor(theme.secondaryTextColor)
                    .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 28).padding(.horizontal, 16)
        .accessibilityElement(children: .combine)
    }
}

extension Color {
    /// Chip tone for a runtime status word (running, failed, paused…).
    static func workStatusTone(_ status: String, theme: ThemeManager = .shared) -> Color {
        switch status.lowercased() {
        case "active", "running", "triggered", "accepted", "complete", "completed", "captured", "healthy", "approved", "succeeded": return theme.successColor
        case "error", "failed", "partial", "rejected", "revoked", "cancelled", "budget_exhausted", "needs_attention": return theme.dangerColor
        case "paused", "pending", "review", "awaiting_review", "waiting_owner", "attention", "plateau", "drafting": return theme.warningColor
        default: return theme.secondaryTextColor
        }
    }
}

/// Label, current value and −/+ as one form row.
struct WorkStepperRow: View {
    let title: String
    @Binding var value: Int
    let range: ClosedRange<Int>
    var display: String? = nil
    @ObservedObject private var theme = ThemeManager.shared
    init(_ title: String, value: Binding<Int>, in range: ClosedRange<Int>, display: String? = nil) {
        self.title = title; self._value = value; self.range = range; self.display = display
    }
    var body: some View {
        Stepper(value: $value, in: range) {
            HStack(spacing: 8) {
                Text(title).font(.themed(16)).foregroundColor(theme.textColor).fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Text(display ?? "\(value)").font(.themed(16, weight: .semibold)).monospacedDigit().foregroundColor(theme.secondaryTextColor)
            }
        }
        .accessibilityLabel(title)
        .accessibilityValue(display ?? "\(value)")
        .padding(.vertical, 6).frame(minHeight: WorkMetrics.rowMinHeight)
    }
}
