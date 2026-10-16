import SwiftUI

// A reusable unified-diff renderer: the parser turns `git diff` text into
// hunks with old/new line numbers; the view tints added and removed lines
// with the theme's success/danger tokens, folds long unchanged runs, lets
// each hunk collapse, and wraps or scrolls long lines. Nothing here knows
// about VibeDev, so MUIJ's DiffViewer can reuse it.

// MARK: - Model

struct UnifiedDiffLine: Identifiable, Equatable {
    enum Kind: Equatable { case context, added, removed, note }
    let id: Int
    let kind: Kind
    let oldNumber: Int?
    let newNumber: Int?
    let text: String

    /// The marker the gutter shows before the text.
    var marker: String {
        switch kind {
        case .context: return " "
        case .added: return "+"
        case .removed: return "−"
        case .note: return "\\"
        }
    }
}

struct UnifiedDiffHunk: Identifiable, Equatable {
    let id: Int
    let header: String
    let oldStart: Int
    let oldCount: Int
    let newStart: Int
    let newCount: Int
    /// The function or section after the second `@@`.
    let section: String
    var lines: [UnifiedDiffLine]
    /// Unchanged lines between the previous hunk (or the file start) and this one.
    var unchangedBefore: Int

    var additions: Int { lines.filter { $0.kind == .added }.count }
    var deletions: Int { lines.filter { $0.kind == .removed }.count }

    enum Segment: Equatable, Identifiable {
        case lines([UnifiedDiffLine])
        /// A long unchanged run, hidden until asked for.
        case fold(id: Int, lines: [UnifiedDiffLine])

        var id: String {
            switch self {
            case .lines(let lines): return "l\(lines.first?.id ?? -1)"
            case .fold(let id, _): return "f\(id)"
            }
        }
    }

    /// The hunk with every context run longer than `threshold` folded down to
    /// `keep` lines on each side of the hidden part.
    func segments(threshold: Int = 8, keep: Int = 3) -> [Segment] {
        var segments: [Segment] = []
        var shown: [UnifiedDiffLine] = []
        var index = 0
        while index < lines.count {
            guard lines[index].kind == .context else {
                shown.append(lines[index]); index += 1; continue
            }
            var end = index
            while end < lines.count, lines[end].kind == .context { end += 1 }
            let run = Array(lines[index..<end])
            let atStart = index == 0
            let atEnd = end == lines.count
            let lead = atStart ? 0 : keep
            let trail = atEnd ? 0 : keep
            if run.count > threshold, run.count - lead - trail > 0 {
                shown.append(contentsOf: run.prefix(lead))
                if !shown.isEmpty { segments.append(.lines(shown)); shown = [] }
                let hidden = Array(run.dropFirst(lead).dropLast(trail))
                segments.append(.fold(id: hidden.first?.id ?? index, lines: hidden))
                shown.append(contentsOf: run.suffix(trail))
            } else {
                shown.append(contentsOf: run)
            }
            index = end
        }
        if !shown.isEmpty { segments.append(.lines(shown)) }
        return segments
    }
}

struct UnifiedDiff: Equatable {
    var oldPath: String?
    var newPath: String?
    var isBinary = false
    var hunks: [UnifiedDiffHunk] = []

    var additions: Int { hunks.reduce(0) { $0 + $1.additions } }
    var deletions: Int { hunks.reduce(0) { $0 + $1.deletions } }
    var lineCount: Int { hunks.reduce(0) { $0 + $1.lines.count } }
}

enum UnifiedDiffParser {
    /// Parse one file's unified diff. Lines before the first hunk (`diff
    /// --git`, `index`, `---`, `+++`) only name the paths; a `Binary files`
    /// line marks a binary change with no hunks.
    static func parse(_ text: String) -> UnifiedDiff {
        var diff = UnifiedDiff()
        var current: UnifiedDiffHunk?
        var oldLine = 0
        var newLine = 0
        var lineID = 0
        var previousOldEnd = 1

        func finish() {
            if let hunk = current { diff.hunks.append(hunk) }
            current = nil
        }

        var rawLines = text.components(separatedBy: "\n")
        if rawLines.last == "" { rawLines.removeLast() }
        for raw in rawLines {
            let line = raw.hasSuffix("\r") ? String(raw.dropLast()) : raw
            if line.hasPrefix("@@") {
                finish()
                guard let header = parseHeader(line) else { continue }
                let unchanged = max(0, header.oldStart - previousOldEnd)
                current = UnifiedDiffHunk(
                    id: diff.hunks.count, header: line, oldStart: header.oldStart,
                    oldCount: header.oldCount, newStart: header.newStart,
                    newCount: header.newCount, section: header.section, lines: [],
                    unchangedBefore: unchanged
                )
                oldLine = header.oldStart
                newLine = header.newStart
                previousOldEnd = header.oldStart + header.oldCount
                continue
            }
            guard current != nil else {
                if line.hasPrefix("--- ") { diff.oldPath = path(line.dropFirst(4)) }
                else if line.hasPrefix("+++ ") { diff.newPath = path(line.dropFirst(4)) }
                else if line.hasPrefix("Binary files") || line.hasPrefix("GIT binary patch") { diff.isBinary = true }
                continue
            }
            if line.hasPrefix("diff --git") {
                // A second file: this view shows one file at a time.
                finish()
                break
            }
            let marker = line.first
            let body = line.isEmpty ? "" : String(line.dropFirst())
            let kind: UnifiedDiffLine.Kind
            var old: Int?
            var new: Int?
            switch marker {
            case "+":
                kind = .added; new = newLine; newLine += 1
            case "-":
                kind = .removed; old = oldLine; oldLine += 1
            case "\\":
                kind = .note
            default:
                // A context line (an empty one may have lost its leading space).
                kind = .context; old = oldLine; new = newLine; oldLine += 1; newLine += 1
            }
            current?.lines.append(UnifiedDiffLine(
                id: lineID, kind: kind, oldNumber: old, newNumber: new,
                text: kind == .note ? String(line.dropFirst()).trimmingCharacters(in: .whitespaces) : body
            ))
            lineID += 1
        }
        finish()
        return diff
    }

    struct Header: Equatable {
        let oldStart: Int, oldCount: Int, newStart: Int, newCount: Int
        let section: String
    }

    /// `@@ -10,6 +10,8 @@ export function search()`
    static func parseHeader(_ line: String) -> Header? {
        let parts = line.components(separatedBy: "@@")
        guard parts.count >= 3 else { return nil }
        let ranges = parts[1].split(separator: " ")
        guard ranges.count >= 2, ranges[0].hasPrefix("-"), ranges[1].hasPrefix("+") else { return nil }
        func range(_ text: Substring) -> (Int, Int)? {
            let pieces = text.dropFirst().split(separator: ",", omittingEmptySubsequences: false)
            guard let start = Int(pieces[0]) else { return nil }
            let count = pieces.count > 1 ? Int(pieces[1]) ?? 1 : 1
            return (start, count)
        }
        guard let old = range(ranges[0]), let new = range(ranges[1]) else { return nil }
        let section = parts.dropFirst(2).joined(separator: "@@").trimmingCharacters(in: .whitespaces)
        return Header(oldStart: old.0, oldCount: old.1, newStart: new.0, newCount: new.1, section: section)
    }

    private static func path(_ raw: Substring) -> String? {
        let value = raw.trimmingCharacters(in: .whitespaces)
        if value == "/dev/null" { return nil }
        if value.hasPrefix("a/") || value.hasPrefix("b/") { return String(value.dropFirst(2)) }
        return value
    }
}

// MARK: - View

struct UnifiedDiffView: View {
    let diff: UnifiedDiff
    /// Wrap long lines (phone default) or scroll them sideways.
    var wrap: Bool = true
    /// Start with every hunk collapsed (a big file shows its hunk list first).
    var startCollapsed: Bool = false

    @ObservedObject private var theme = ThemeManager.shared
    @State private var collapsed: Set<Int> = []
    @State private var openedFolds: Set<Int> = []
    @State private var didSeed = false

    private let numberWidth: CGFloat = 30

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if diff.isBinary {
                note("Binary file — no line diff to show.")
            } else if diff.hunks.isEmpty {
                note("No line changes.")
            }
            ForEach(diff.hunks) { hunk in
                if hunk.unchangedBefore > 0 && hunk.id > 0 {
                    Text("\(hunk.unchangedBefore) unchanged lines")
                        .font(.themed(11, weight: .semibold))
                        .foregroundStyle(theme.secondaryTextColor)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 5)
                }
                hunkHeader(hunk)
                if !collapsed.contains(hunk.id) {
                    hunkBody(hunk)
                }
            }
        }
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
                .strokeBorder(theme.cardBorderColor, lineWidth: 1)
        )
        .onAppear {
            guard !didSeed else { return }
            didSeed = true
            if startCollapsed { collapsed = Set(diff.hunks.map(\.id)) }
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(.themed(13))
            .foregroundStyle(theme.secondaryTextColor)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
    }

    private func hunkHeader(_ hunk: UnifiedDiffHunk) -> some View {
        Button {
            if collapsed.contains(hunk.id) { collapsed.remove(hunk.id) } else { collapsed.insert(hunk.id) }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: collapsed.contains(hunk.id) ? "chevron.right" : "chevron.down")
                    .font(.system(size: 10, weight: .semibold))
                Text(hunk.header)
                    .font(.themedMono(11, weight: .medium))
                    .vibeDevLineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 4)
                if collapsed.contains(hunk.id) {
                    Text("+\(hunk.additions) −\(hunk.deletions)")
                        .font(.themedMono(11))
                }
            }
            .foregroundStyle(theme.secondaryTextColor)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .frame(maxWidth: .infinity, minHeight: Lively.tapMin, alignment: .leading)
            .background(theme.softBackgroundColor)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Hunk \(hunk.header)")
        .accessibilityHint(collapsed.contains(hunk.id) ? "Shows the lines" : "Hides the lines")
    }

    @ViewBuilder
    private func hunkBody(_ hunk: UnifiedDiffHunk) -> some View {
        let rows = VStack(alignment: .leading, spacing: 0) {
            ForEach(hunk.segments()) { segment in
                switch segment {
                case .lines(let lines):
                    ForEach(lines) { line in lineRow(line) }
                case .fold(let id, let lines):
                    if openedFolds.contains(id) {
                        ForEach(lines) { line in lineRow(line) }
                    } else {
                        Button { openedFolds.insert(id) } label: {
                            Text("\(lines.count) unchanged lines")
                                .font(.themed(11, weight: .semibold))
                                .foregroundStyle(theme.accentColor)
                                .frame(maxWidth: .infinity, minHeight: Lively.tapMin)
                        }
                        .buttonStyle(.plain)
                        .accessibilityHint("Shows the unchanged lines")
                    }
                }
            }
        }
        if wrap {
            rows
        } else {
            ScrollView(.horizontal, showsIndicators: true) {
                rows.fixedSize(horizontal: true, vertical: false)
            }
        }
    }

    private func lineRow(_ line: UnifiedDiffLine) -> some View {
        let tint: Color? = {
            switch line.kind {
            case .added: return theme.successColor
            case .removed: return theme.dangerColor
            default: return nil
            }
        }()
        return HStack(alignment: .top, spacing: 0) {
            Rectangle()
                .fill(tint ?? theme.cardColor.opacity(0))
                .frame(width: 3)
            Text(line.oldNumber.map(String.init) ?? "")
                .frame(width: numberWidth, alignment: .trailing)
                .foregroundStyle(theme.secondaryTextColor.opacity(0.7))
            Text(line.newNumber.map(String.init) ?? "")
                .frame(width: numberWidth, alignment: .trailing)
                .foregroundStyle(theme.secondaryTextColor.opacity(0.7))
            Text(line.marker)
                .frame(width: 14, alignment: .center)
                .foregroundStyle(tint ?? theme.secondaryTextColor)
            Text(line.text.isEmpty ? " " : line.text)
                .foregroundStyle(line.kind == .note ? theme.secondaryTextColor : theme.textColor)
                .lineLimit(wrap ? nil : 1)
                .fixedSize(horizontal: !wrap, vertical: wrap)
                .frame(maxWidth: wrap ? .infinity : nil, alignment: .leading)
                .padding(.trailing, 8)
        }
        .font(.themedMono(11.5))
        .padding(.vertical, 1)
        .background(tint.map { $0.opacity(0.12) } ?? theme.cardColor.opacity(0))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibility(line))
    }

    private func accessibility(_ line: UnifiedDiffLine) -> String {
        switch line.kind {
        case .added: return "Added line \(line.newNumber ?? 0): \(line.text)"
        case .removed: return "Removed line \(line.oldNumber ?? 0): \(line.text)"
        case .context: return "Line \(line.newNumber ?? 0): \(line.text)"
        case .note: return line.text
        }
    }
}
