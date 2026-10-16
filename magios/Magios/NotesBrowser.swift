import MarkdownUI
import SwiftUI

struct NoteTreeEntry: Decodable, Identifiable {
    let name: String
    let relativePath: String
    let kind: String
    let hasChildren: Bool?

    var id: String { relativePath }

    enum CodingKeys: String, CodingKey {
        case name
        case relativePath = "relative_path"
        case kind
        case hasChildren = "has_children"
    }
}

struct NoteTreePage: Decodable {
    let entries: [NoteTreeEntry]
}

private func noteFileName(_ path: String) -> String {
    (path as NSString).lastPathComponent
}

private func noteFolder(_ path: String) -> String {
    let folder = (path as NSString).deletingLastPathComponent
    return folder == "." || folder.isEmpty ? "" : folder
}

private func highlightedTerms(_ text: String, _ terms: [String]) -> AttributedString {
    var attr = AttributedString(text)
    let usable = terms.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
    guard !usable.isEmpty, !text.isEmpty else { return attr }
    let lowered = text.lowercased()
    for term in usable {
        let needle = term.lowercased()
        var search = lowered.startIndex
        while let range = lowered.range(of: needle, range: search..<lowered.endIndex) {
            let start = attr.characters.index(attr.startIndex, offsetBy: lowered.distance(from: lowered.startIndex, to: range.lowerBound))
            let end = attr.characters.index(start, offsetBy: needle.count)
            if start < end, end <= attr.endIndex {
                attr[start..<end].backgroundColor = .yellow.opacity(0.45)
            }
            search = range.upperBound
        }
    }
    return attr
}

private func noteMatchCount(_ text: String, _ query: String) -> Int {
    let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !needle.isEmpty else { return 0 }
    var count = 0
    var rest = text.lowercased()[...]
    let look = needle.lowercased()
    while let range = rest.range(of: look) {
        count += 1
        rest = rest[range.upperBound...]
    }
    return count
}

private func highlightedNote(_ markdown: String, query: String) -> AttributedString {
    var attr = (try? AttributedString(markdown: markdown)) ?? AttributedString(markdown)
    let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !needle.isEmpty else { return attr }
    let plain = String(attr.characters).lowercased()
    let look = needle.lowercased()
    var search = plain.startIndex
    while let range = plain.range(of: look, range: search..<plain.endIndex) {
        let start = attr.characters.index(attr.startIndex, offsetBy: plain.distance(from: plain.startIndex, to: range.lowerBound))
        let end = attr.characters.index(start, offsetBy: look.count)
        if start < end, end <= attr.endIndex {
            attr[start..<end].backgroundColor = .yellow.opacity(0.45)
        }
        search = range.upperBound
    }
    return attr
}

struct NoteSearchHit: Decodable, Identifiable {
    let title: String
    let relativePath: String
    let matches: [NoteSearchMatch]
    var id: String { relativePath }

    enum CodingKeys: String, CodingKey {
        case title
        case relativePath = "relative_path"
        case matches
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        title = try container.decodeIfPresent(String.self, forKey: .title) ?? ""
        relativePath = try container.decodeIfPresent(String.self, forKey: .relativePath) ?? ""
        matches = try container.decodeIfPresent([NoteSearchMatch].self, forKey: .matches) ?? []
    }
}

struct NoteSearchMatch: Decodable {
    let line: Int
    let text: String

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        line = try container.decodeIfPresent(Int.self, forKey: .line) ?? 0
        text = try container.decodeIfPresent(String.self, forKey: .text) ?? ""
    }

    enum CodingKeys: String, CodingKey { case line, text }
}

struct NoteSearchPage: Decodable {
    let hits: [NoteSearchHit]
    let queryTerms: [String]
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        hits = try container.decodeIfPresent([NoteSearchHit].self, forKey: .hits) ?? []
        queryTerms = try container.decodeIfPresent([String].self, forKey: .queryTerms) ?? []
    }
    enum CodingKeys: String, CodingKey {
        case hits
        case queryTerms = "query_terms"
    }
}

struct NoteDocument: Decodable {
    let relativePath: String
    let title: String
    let markdown: String

    enum CodingKeys: String, CodingKey {
        case relativePath = "relative_path"
        case title
        case markdown
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        relativePath = try container.decode(String.self, forKey: .relativePath)
        title = try container.decodeIfPresent(String.self, forKey: .title) ?? ""
        markdown = try container.decodeIfPresent(String.self, forKey: .markdown) ?? ""
    }
}

final class NoteNode: Identifiable {
    let entry: NoteTreeEntry
    var children: [NoteNode]?
    var open = false
    var empty: Bool
    var id: String { entry.id }
    init(_ entry: NoteTreeEntry) {
        self.entry = entry
        empty = entry.kind == "dir" && entry.hasChildren == false
    }
}

@MainActor
final class NotesBrowserModel: ObservableObject {
    @Published var roots: [NoteNode] = []
    @Published var selectedFolder = ""
    @Published var document: NoteDocument?
    @Published var status = ""
    @Published var draftName = ""
    @Published var query = ""
    @Published var results: [NoteSearchHit] = []
    @Published var queryTerms: [String] = []
    @Published var searching = false
    @Published var tookMs = 0

    func loadRoot() async {
        do {
            roots = try await get("notes/tree?path=", as: NoteTreePage.self).entries.map(NoteNode.init)
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    func toggle(_ node: NoteNode) async {
        if node.entry.kind == "dir" {
            selectedFolder = node.entry.relativePath
            if node.empty {
                roots = Array(roots)
                return
            }
            if node.open {
                node.open = false
            } else if node.children == nil {
                do {
                    let kids = try await get(
                        "notes/tree?path=\(encoded(node.entry.relativePath))",
                        as: NoteTreePage.self
                    ).entries.map(NoteNode.init)
                    node.children = kids
                    node.empty = kids.isEmpty
                    node.open = !kids.isEmpty
                    status = ""
                } catch {
                    status = error.localizedDescription
                }
            } else {
                node.open = node.children?.isEmpty == false
            }
            roots = Array(roots)
            return
        }
        await open(path: node.entry.relativePath)
    }

    func refresh(_ path: String) async {
        do {
            let kids = try await get("notes/tree?path=\(encoded(path))", as: NoteTreePage.self).entries.map(NoteNode.init)
            if path.isEmpty {
                roots = kids
            } else if let node = find(roots, path) {
                node.children = kids
                node.open = !kids.isEmpty
                roots = Array(roots)
            }
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    private func find(_ nodes: [NoteNode], _ path: String) -> NoteNode? {
        for node in nodes {
            if node.entry.relativePath == path { return node }
            if let nested = node.children, let found = find(nested, path) { return found }
        }
        return nil
    }

    func search(_ query: String) async {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            results = []
            queryTerms = []
            searching = false
            return
        }
        searching = true
        let started = Date()
        do {
            var call = request("notes/search", method: "POST", body: nil)
            call.setValue("application/json", forHTTPHeaderField: "content-type")
            call.httpBody = try JSONSerialization.data(withJSONObject: ["query": trimmed, "limit": 20])
            let (data, response) = try await URLSession.shared.data(for: call)
            try Self.check(response)
            let page = try JSONDecoder().decode(NoteSearchPage.self, from: data)
            results = page.hits
            queryTerms = page.queryTerms
            tookMs = Int(Date().timeIntervalSince(started) * 1000)
            status = ""
        } catch {
            status = error.localizedDescription
        }
        searching = false
    }

    func save(path: String, markdown: String) async -> NoteDocument? {
        do {
            let saved = try await send(
                "notes/file",
                method: "PUT",
                body: ["path": path, "markdown": markdown],
                as: NoteDocument.self
            )
            document = saved
            status = ""
            return saved
        } catch {
            status = error.localizedDescription
            return nil
        }
    }

    func open(path: String) async {
        do {
            document = try await get("notes/file?path=\(encoded(path))", as: NoteDocument.self)
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    func create() async {
        let name = draftName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else {
            status = "Enter a name."
            return
        }
        do {
            let created = try await send(
                "notes/file",
                method: "POST",
                body: ["folder": selectedFolder, "name": name],
                as: NoteDocument.self
            )
            draftName = ""
            await refresh(selectedFolder)
            document = created
        } catch {
            status = error.localizedDescription
        }
    }

    func createFolder() async {
        let name = draftName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else {
            status = "Enter a name."
            return
        }
        do {
            let created = try await send(
                "notes/tree",
                method: "POST",
                body: ["folder": selectedFolder, "name": name],
                as: NoteDocument.self
            )
            draftName = ""
            await refresh(selectedFolder)
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    func deleteFile() async {
        guard let path = document?.relativePath else { return }
        await remove("notes/file?path=\(encoded(path))")
        document = nil
        await refresh(selectedFolder)
    }

    func deleteFolder(_ path: String) async {
        guard !path.isEmpty else { return }
        await remove("notes/tree?path=\(encoded(path))")
        if document?.relativePath.hasPrefix(path) == true { document = nil }
        let parent = path.split(separator: "/").dropLast().map(String.init).joined(separator: "/")
        selectedFolder = parent
        await refresh(parent)
    }

    private func get<T: Decodable>(_ path: String, as type: T.Type) async throws -> T {
        let (data, response) = try await URLSession.shared.data(for: request(path, method: "GET", body: nil))
        try Self.check(response)
        return try JSONDecoder().decode(type, from: data)
    }

    private func send<T: Decodable>(_ path: String, method: String, body: [String: String], as type: T.Type) async throws -> T {
        let (data, response) = try await URLSession.shared.data(for: request(path, method: method, body: body))
        try Self.check(response)
        return try JSONDecoder().decode(type, from: data)
    }

    private func remove(_ path: String) async {
        do {
            let (_, response) = try await URLSession.shared.data(for: request(path, method: "DELETE", body: nil))
            try Self.check(response)
            status = ""
        } catch {
            status = error.localizedDescription
        }
    }

    private func request(_ path: String, method: String, body: [String: String]?) -> URLRequest {
        let root = MagicianAccess.baseURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard let url = URL(string: "\(root)/api/magician/v2/\(path)") else {
            return URLRequest(url: MagicianAccess.baseURL)
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 20
        MagicianAccess.authorize(&request)
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "content-type")
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        }
        return request
    }

    private static func check(_ response: URLResponse) throws {
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        if !(200..<300).contains(status) {
            throw URLError(.badServerResponse)
        }
    }

    private func encoded(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? value
    }
}

private struct NoteBranch: View {
    @ObservedObject private var theme = ThemeManager.shared
    let nodes: [NoteNode]
    let depth: Int
    let selectedPath: String
    let onTap: (NoteNode) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(nodes.enumerated()), id: \.element.id) { index, node in
                let last = index == nodes.count - 1
                HStack(alignment: .top, spacing: 0) {
                    if depth > 0 {
                        ZStack(alignment: .topLeading) {
                            Rectangle()
                                .fill(theme.cardBorderColor)
                                .frame(width: 1, height: last ? 16 : nil)
                                .frame(maxHeight: last ? nil : .infinity, alignment: .top)
                            Rectangle()
                                .fill(theme.cardBorderColor)
                                .frame(width: 12, height: 1)
                                .offset(y: 16)
                        }
                        .frame(width: 14)
                    }
                    VStack(alignment: .leading, spacing: 0) {
                        let selected = !selectedPath.isEmpty && node.entry.relativePath == selectedPath
                        Button { onTap(node) } label: {
                            HStack(spacing: 4) {
                                Text(node.entry.kind == "dir" ? (node.empty ? "–" : (node.open ? "▾" : "▸")) : " ")
                                    .font(.system(size: 11, weight: .semibold))
                                    .foregroundStyle(node.empty ? theme.secondaryTextColor : theme.accentColor)
                                    .frame(width: 14, alignment: .center)
                                Text(node.entry.name)
                                    .foregroundStyle(node.empty ? theme.secondaryTextColor : theme.textColor)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .padding(.vertical, 8)
                            .padding(.horizontal, 4)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(
                                RoundedRectangle(cornerRadius: 8, style: .continuous)
                                    .fill(selected ? theme.accentColor.opacity(0.16) : Color.clear)
                            )
                        }
                        .buttonStyle(.plain)
                        if node.open, let children = node.children, !children.isEmpty {
                            NoteBranch(nodes: children, depth: depth + 1, selectedPath: selectedPath, onTap: onTap)
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private enum NotesFocus: Hashable {
    case search, name, find
}

struct NotesBrowserView: View {
    var initialPath: String?
    @ObservedObject private var theme = ThemeManager.shared
    @StateObject private var model = NotesBrowserModel()
    @State private var drawer = true
    @State private var confirmFile = false
    @State private var confirmFolder = false
    @State private var editing = false
    @State private var draft = ""
    @State private var find = ""
    @State private var naming: String?
    @State private var searchTask: Task<Void, Never>?
    @FocusState private var notesFocus: NotesFocus?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    Button {
                        drawer.toggle()
                        if drawer { notesFocus = nil }
                    } label: {
                        Image(systemName: "line.3.horizontal")
                            .foregroundStyle(theme.textColor)
                            .frame(width: 32, height: 36)
                    }
                    .accessibilityLabel("Folders")
                    searchField
                        .frame(maxWidth: .infinity)
                    Button { dismiss() } label: {
                        Image(systemName: "xmark")
                            .foregroundStyle(theme.textColor)
                            .frame(width: 32, height: 36)
                    }
                    .accessibilityLabel("Close")
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(theme.backgroundColor)
                ZStack(alignment: .leading) {
                    reader
                    if drawer {
                        HStack(spacing: 0) {
                            tree
                                .frame(width: 300)
                                .frame(maxHeight: .infinity, alignment: .top)
                                .background(theme.elevatedColor)
                                .shadow(color: theme.textColor.opacity(0.12), radius: 8)
                            Color.black.opacity(0.32)
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                                .contentShape(Rectangle())
                                .onTapGesture {
                                    notesFocus = nil
                                    drawer = false
                                }
                        }
                    }
                }
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationBarHidden(true)
        }
        .preferredColorScheme(theme.colorScheme)
        .confirmationDialog("Delete this note?", isPresented: $confirmFile, titleVisibility: .visible) {
            Button("Delete", role: .destructive) { Task { await model.deleteFile() } }
        }
        .confirmationDialog("Delete this folder and the notes inside it?", isPresented: $confirmFolder, titleVisibility: .visible) {
            Button("Delete folder", role: .destructive) { Task { await model.deleteFolder(model.selectedFolder) } }
        }
        .onChange(of: drawer) { _, isOpen in
            if isOpen { notesFocus = nil }
        }
        .onChange(of: model.query) { _, newValue in
            if !newValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                drawer = false
            }
            searchTask?.cancel()
            searchTask = Task {
                try? await Task.sleep(nanoseconds: 200_000_000)
                if Task.isCancelled { return }
                await model.search(newValue)
            }
        }
        .task {
            await model.loadRoot()
            if let initialPath, !initialPath.isEmpty {
                await model.open(path: initialPath)
            }
        }
    }

    private func searchHit(_ hit: NoteSearchHit) -> some View {
        let fileName = noteFileName(hit.relativePath)
        let folder = noteFolder(hit.relativePath)
        let title = hit.title.trimmingCharacters(in: .whitespacesAndNewlines)
        let showTitle = !title.isEmpty
            && title.caseInsensitiveCompare(fileName) != .orderedSame
            && title.caseInsensitiveCompare(hit.relativePath) != .orderedSame
        return VStack(alignment: .leading, spacing: 6) {
            if showTitle {
                Text(highlightedTerms(title, model.queryTerms))
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Button {
                notesFocus = nil
                Task {
                    await model.open(path: hit.relativePath)
                    draft = model.document?.markdown ?? ""
                    editing = false
                    model.query = ""
                    model.results = []
                    drawer = false
                }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 0) {
                    if !folder.isEmpty {
                        Text(folder + "/")
                            .foregroundStyle(theme.secondaryTextColor)
                    }
                    Text(fileName)
                        .foregroundStyle(theme.accentColor)
                        .underline()
                }
                .font(.system(size: 15, weight: .semibold))
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Open \(fileName)")
            ForEach(Array(hit.matches.prefix(3).enumerated()), id: \.offset) { _, match in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text("\(match.line)")
                        .font(.footnote.monospacedDigit())
                        .foregroundStyle(theme.secondaryTextColor)
                        .frame(width: 32, alignment: .trailing)
                    Text(highlightedTerms(match.text, model.queryTerms))
                        .font(.footnote)
                        .foregroundStyle(theme.textColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    private var searchField: some View {
        noteField("Search Notes", text: $model.query, clears: true, focus: .search)
    }

    private func noteField(_ title: String, text: Binding<String>, clears: Bool = false, focus: NotesFocus) -> some View {
        HStack(spacing: 0) {
            TextField(title, text: text)
                .focused($notesFocus, equals: focus)
                .textFieldStyle(.plain)
                .font(.system(size: 15))
                .foregroundStyle(theme.textColor)
                .padding(.leading, 10)
                .padding(.trailing, clears && !text.wrappedValue.isEmpty ? 0 : 10)
            if clears && !text.wrappedValue.isEmpty {
                Button {
                    text.wrappedValue = ""
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(theme.secondaryTextColor)
                        .frame(width: 28, height: 28)
                }
                .buttonStyle(.plain)
                .padding(.trailing, 4)
                .accessibilityLabel("Clear search")
            }
        }
        .frame(maxWidth: .infinity, minHeight: 36, maxHeight: 36, alignment: .leading)
        .background(theme.controlColor)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(theme.controlBorderColor, lineWidth: 1)
        )
    }

    private func noteAction(_ title: String, action: @escaping () -> Void) -> some View {
        Button {
            notesFocus = nil
            action()
        } label: {
            Text(title)
                .font(.system(size: 12, weight: .medium))
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                .padding(.horizontal, 8)
                .frame(height: 32)
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.plain)
        .foregroundStyle(theme.textColor)
        .background(theme.controlColor)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(theme.controlBorderColor, lineWidth: 1)
        )
    }

    private var tree: some View {
        ThinScroll {
            VStack(alignment: .leading, spacing: 8) {
                Text(model.selectedFolder.isEmpty ? "In the notes root" : "In \(model.selectedFolder)")
                    .font(.footnote)
                    .foregroundStyle(theme.secondaryTextColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                HStack(spacing: 6) {
                    noteAction("New note") { naming = "note"; model.draftName = "" }
                    noteAction("New folder") { naming = "folder"; model.draftName = "" }
                    if !model.selectedFolder.isEmpty {
                        noteAction("Delete") { confirmFolder = true }
                    }
                }
                if let naming {
                    noteField(naming == "folder" ? "Folder name" : "Note name", text: $model.draftName, focus: .name)
                    noteAction(naming == "folder" ? "Create folder" : "Create") {
                        Task {
                            if naming == "folder" {
                                await model.createFolder()
                            } else {
                                await model.create()
                                if model.document != nil { drawer = false }
                            }
                            self.naming = nil
                        }
                    }
                }
                NoteBranch(nodes: model.roots, depth: 0, selectedPath: model.selectedFolder) { node in
                    notesFocus = nil
                    Task {
                        await model.toggle(node)
                        if node.entry.kind != "dir" { drawer = false }
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var reader: some View {
        ThinScroll {
            VStack(alignment: .leading, spacing: 12) {
                if !model.status.isEmpty {
                    Text(model.status).foregroundStyle(theme.secondaryTextColor)
                }
                if !model.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    if model.searching && model.results.isEmpty {
                        Text("Searching notes…").foregroundStyle(theme.secondaryTextColor)
                    } else {
                        Text("\(model.results.count) results · \(model.tookMs) ms")
                            .font(.footnote)
                            .foregroundStyle(theme.secondaryTextColor)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.bottom, 4)
                        if model.results.isEmpty {
                            Text("Nothing matched “\(model.query)”.")
                        }
                        ForEach(Array(model.results.enumerated()), id: \.element.id) { index, hit in
                            VStack(spacing: 0) {
                                if index > 0 {
                                    Rectangle()
                                        .fill(theme.cardBorderColor)
                                        .frame(height: 1)
                                        .frame(maxWidth: .infinity)
                                }
                                searchHit(hit)
                                    .padding(.vertical, 10)
                            }
                        }
                    }
                } else if let document = model.document {
                    HStack {
                        Text(document.relativePath).font(.footnote).foregroundStyle(theme.secondaryTextColor)
                        Spacer()
                        noteAction(editing ? "View" : "Edit") {
                            if editing {
                                editing = false
                            } else {
                                draft = document.markdown
                                editing = true
                            }
                        }
                        if editing {
                            noteAction("Save") {
                                Task {
                                    if await model.save(path: document.relativePath, markdown: draft) != nil {
                                        editing = false
                                    }
                                }
                            }
                        }
                        noteAction("Delete") { confirmFile = true }
                    }
                    if !editing {
                        noteField("Find in this note", text: $find, focus: .find)
                        if !find.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            Text("\(noteMatchCount(document.markdown, find)) matches")
                                .font(.footnote)
                                .foregroundStyle(theme.secondaryTextColor)
                        }
                    }
                    if editing {
                        TextEditor(text: $draft)
                            .font(.body)
                            .frame(minHeight: 320)
                            .padding(8)
                            .foregroundStyle(theme.textColor)
                            .background(theme.controlColor)
                            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                            .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).stroke(theme.controlBorderColor))
                    } else if find.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Markdown(document.markdown)
                    } else {
                        Text(highlightedNote(document.markdown, query: find))
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                } else {
                    Text("Your notes")
                        .font(.title2)
                        .foregroundStyle(theme.textColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Text("The folders are open beside this page. Pick a note, or add one from that list.")
                        .foregroundStyle(theme.secondaryTextColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct NoteScrollMetrics: Equatable {
    var offset: CGFloat = 0
    var height: CGFloat = 1
}

private struct NoteScrollKey: PreferenceKey {
    static var defaultValue = NoteScrollMetrics()
    static func reduce(value: inout NoteScrollMetrics, nextValue: () -> NoteScrollMetrics) {
        let next = nextValue()
        if next.height > 1 { value = next }
    }
}

private struct ThinScroll<Content: View>: View {
    @State private var offset: CGFloat = 0
    @State private var contentHeight: CGFloat = 1
    private let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        GeometryReader { geo in
            ScrollView {
                content
                    .background(
                        GeometryReader { inner in
                            Color.clear.preference(
                                key: NoteScrollKey.self,
                                value: NoteScrollMetrics(
                                    offset: -inner.frame(in: .named("noteScroll")).minY,
                                    height: inner.size.height
                                )
                            )
                        }
                    )
            }
            .scrollDismissesKeyboard(.immediately)
            .scrollIndicators(.hidden)
            .coordinateSpace(name: "noteScroll")
            .onPreferenceChange(NoteScrollKey.self) { metrics in
                offset = metrics.offset
                contentHeight = metrics.height
            }
            .overlay(alignment: .topTrailing) {
                let viewport = geo.size.height
                if contentHeight > viewport + 8 {
                    let thumb = max(28, viewport * viewport / contentHeight)
                    let travel = max(0, viewport - thumb)
                    let span = max(1, contentHeight - viewport)
                    let progress = min(1, max(0, offset / span))
                    Capsule()
                        .fill(ThemeManager.shared.secondaryTextColor.opacity(0.4))
                        .frame(width: 3, height: thumb)
                        .padding(.trailing, 2)
                        .offset(y: travel * progress)
                }
            }
        }
    }
}
