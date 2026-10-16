import SwiftUI

/// A preference alignment hold, answered on iOS (P6.2) — the port of the
/// web's `AlignmentAskFields.svelte`.
///
/// iOS is an owner-private surface, so this reads why the act was held and
/// keeps three things visibly apart:
/// 1. what to do with **this act** (allow once, something else, cancel);
/// 2. what Magician **remembers** (change or remember a preference) — never
///    the act's answer, never preselected;
/// 3. a **reply** in the owner's own words — an instruction, not consent.
///
/// A lasting change is two steps: the first answer asks the server to read
/// the exact wording back; the second confirms that readback with its token.
struct AlignmentAskView: View {
    let item: AttentionItem
    let projection: AlignmentProjection
    @ObservedObject var viewModel: AttentionViewModel
    /// Called after an answer is posted, so the modal can close.
    let onAnswered: () -> Void
    @StateObject private var themeManager = ThemeManager.shared

    @State private var outcome: AttentionViewModel.AlignmentDetailOutcome?
    @State private var replyText = ""
    @State private var wordingText = ""
    @State private var changing = false

    private var detail: AlignmentDetail? {
        if case .ready(let detail) = outcome, detail.isPrivate { return detail }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch outcome {
            case .none:
                note("Loading why this was held…")
            case .gone:
                note("This review was already answered or replaced. Open the current one to continue.")
            case .unavailable, .ready:
                if detail == nil {
                    note("The reason could not be loaded here. Your answer still works.")
                }
            }
            if outcome != .gone {
                if let reason = detail?.retryReason, !reason.isEmpty {
                    Text(reason).font(.themed(14)).foregroundColor(themeManager.textColor)
                }
                if let detail, detail.alignment.isWordingStage || projection.isWordingStage {
                    wordingStage(detail)
                } else {
                    if let detail { reason(detail.alignment) }
                    actChoices
                    if let detail, !detail.alignment.lastingChoices.isEmpty {
                        lastingSection(detail.alignment)
                    }
                }
                if projection.freeText != false { replySection }
                if let line = AlignmentAsk.deadlineLine(
                    deadlineAtMs: detail?.alignment.deadlineAtMs ?? projection.deadlineAtMs) {
                    note(line)
                }
            }
        }
        .onAppear {
            viewModel.fetchAlignmentDetail(item) { outcome = $0 }
        }
    }

    // MARK: pieces

    private func note(_ text: String) -> some View {
        Text(text).font(.themed(13)).foregroundColor(themeManager.secondaryTextColor)
    }

    @ViewBuilder
    private func reason(_ alignment: AlignmentPrivateProjection) -> some View {
        if let explanation = alignment.explanation, !explanation.isEmpty {
            Text(explanation).font(.themed(15)).foregroundColor(themeManager.textColor)
        }
        if let act = alignment.actSummary, !act.isEmpty {
            note("Held: \(act)")
        }
        if !alignment.alsoCovers.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                note("It also needs your approval:")
                ForEach(alignment.alsoCovers) { covered in
                    Text("• \(covered.line)").font(.themed(14)).foregroundColor(themeManager.textColor)
                }
                note("Allowing it approves it too. Any other answer leaves it unapproved.")
            }
            .accessibilityIdentifier("alignment-also-covers")
        }
        ForEach(alignment.intentions) { intention in
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(intention.sourceLabel)
                    .font(.themed(11, weight: .medium))
                    .foregroundColor(themeManager.secondaryTextColor)
                Text(intention.statement).font(.themed(14)).foregroundColor(themeManager.textColor)
            }
        }
    }

    private var actChoices: some View {
        let choices: [AlignmentChoice] = detail.map { detail in
            detail.alignment.actChoices.map { AlignmentChoice(id: $0.id, label: $0.label) }
        } ?? projection.actChoices
        return VStack(spacing: 8) {
            ForEach(choices) { choice in
                // "Cancel the action", so it never reads as closing the sheet,
                // which leaves the hold waiting.
                choiceButton(choice.id == "cancel" ? "Cancel the action" : choice.label) {
                    answer(choice.id)
                }
            }
        }
    }

    @ViewBuilder
    private func lastingSection(_ alignment: AlignmentPrivateProjection) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Change what Magician remembers")
                .font(.themed(13, weight: .semibold))
                .foregroundColor(themeManager.textColor)
            ForEach(alignment.lastingChoices) { choice in
                if choice.needsWording {
                    if changing {
                        TextField("New wording", text: $wordingText, axis: .vertical)
                            .lineLimit(2...4)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityLabel("New wording for the preference")
                        HStack(spacing: 8) {
                            choiceButton("Back") { changing = false }
                            choiceButton("Read it back") { answer("change_preference", input: wordingText) }
                                .disabled(wordingText.trimmingCharacters(in: .whitespaces).isEmpty)
                        }
                    } else {
                        choiceButton(choice.label) { changing = true }
                    }
                } else {
                    if choice.id == "remember_preference", let remember = alignment.remember {
                        note("“\(remember.statement)”")
                    }
                    choiceButton(choice.label) { answer(choice.id) }
                }
            }
        }
        .padding(10)
        .overlay(RoundedRectangle(cornerRadius: 10)
            .stroke(themeManager.secondaryTextColor.opacity(0.4), style: StrokeStyle(lineWidth: 1, dash: [4])))
    }

    @ViewBuilder
    private func wordingStage(_ detail: AlignmentDetail) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let readback = detail.alignment.readback {
                Text("Save exactly this as your preference?")
                    .font(.themed(15, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                Text(readback.wording)
                    .font(.themed(15))
                    .foregroundColor(themeManager.textColor)
                    .padding(.leading, 8)
                    .overlay(alignment: .leading) {
                        Rectangle().fill(themeManager.secondaryTextColor.opacity(0.4)).frame(width: 2)
                    }
                if let superseded = readback.superseded {
                    note("Replaces: \(superseded)")
                }
                note("This changes what Magician remembers. It does not run the held action; that is checked again afterwards.")
                choiceButton("Save this wording") { answer("confirm_wording", input: readback.token) }
            }
            choiceButton("Don't change it") { answer("keep_as_is") }
            choiceButton("Cancel the action") { answer("cancel") }
        }
    }

    private var replySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextField("Or reply with what to do instead", text: $replyText, axis: .vertical)
                .lineLimit(2...4)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("Reply with what to do instead")
            choiceButton("Send reply") { answer(AlignmentAsk.replyChoiceId, input: replyText) }
                .disabled(replyText.trimmingCharacters(in: .whitespaces).isEmpty)
        }
    }

    private func choiceButton(_ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .frame(maxWidth: .infinity)
                .padding(12)
                .foregroundColor(themeManager.textColor)
                .overlay(RoundedRectangle(cornerRadius: 10)
                    .stroke(themeManager.secondaryTextColor.opacity(0.5), lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    private func answer(_ choiceId: String, input: String? = nil) {
        guard AlignmentAsk.answerValue(choiceId: choiceId, input: input) != nil else { return }
        viewModel.submitAlignment(item, choiceId: choiceId, input: input)
        onAnswered()
    }
}

/// Settings → Notes from recent checks (P7.1 on iOS) — the port of the web's
/// "Notes from recent checks" in `PreferenceChecksPanel.svelte`.
///
/// A check that let an act through with a nudge leaves a private note, and
/// sometimes an offer to remember an inferred preference as the owner's own.
/// Saving it sends the token the private read showed, which binds the answer
/// to exactly the wording on screen; if the preference changed since, the
/// server saves nothing and the note reads stale.
struct PreferenceNotesView: View {
    @ObservedObject private var theme = ThemeManager.shared
    @State private var notices: [PreferenceNotice] = []
    @State private var loading = true
    @State private var failed = false
    @State private var busyId: String?
    @State private var notice: String?

    private var base: String { "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2" }

    var body: some View {
        Form {
            if loading {
                Section { ProgressView() }
            } else if failed {
                Section {
                    Text("Notes from recent checks are unavailable.")
                        .foregroundColor(theme.secondaryTextColor)
                    Button("Try again") { load() }
                }
            } else if notices.isEmpty {
                Section {
                    Text("Nothing to review. When a check lets an action through with a note, it shows here.")
                        .foregroundColor(theme.secondaryTextColor)
                }
            } else {
                ForEach(notices) { row($0) }
            }
            if let notice {
                Section { Text(notice).font(.footnote).foregroundColor(theme.secondaryTextColor) }
            }
        }
        .navigationTitle("Notes from recent checks")
        .onAppear(perform: load)
        .refreshable { load() }
    }

    private func row(_ item: PreferenceNotice) -> some View {
        Section(header: Text(item.actSummary).foregroundColor(theme.secondaryTextColor)) {
            if !item.explanation.isEmpty {
                Text(item.explanation).foregroundColor(theme.textColor)
            }
            ForEach(item.intentions) { intention in
                Text(intention.statement).font(.footnote).foregroundColor(theme.secondaryTextColor)
            }
            if let remember = item.remember {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Remember this as your preference?")
                        .font(.footnote).foregroundColor(theme.secondaryTextColor)
                    Text("“\(remember.wording)”").foregroundColor(theme.textColor)
                }
                Button("Save this wording") { answer(item, remember: true, token: remember.token) }
                    .disabled(busyId != nil)
                    .accessibilityIdentifier("preference-note-remember")
            }
            Button("Dismiss") { answer(item, remember: false, token: nil) }
                .disabled(busyId != nil)
                .foregroundColor(theme.secondaryTextColor)
        }
    }

    private func load() {
        guard let url = PreferenceNotices.listURL(base: base) else { return }
        var req = URLRequest(url: url)
        req.cachePolicy = .reloadIgnoringLocalCacheData
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&req)
        URLSession.shared.dataTask(with: req) { data, response, _ in
            let ok = (200..<300).contains((response as? HTTPURLResponse)?.statusCode ?? 0)
            let parsed = ok ? data.map(PreferenceNotices.parse) : nil
            DispatchQueue.main.async {
                loading = false
                failed = parsed == nil
                notices = parsed ?? []
            }
        }.resume()
    }

    private func answer(_ item: PreferenceNotice, remember: Bool, token: String?) {
        guard let url = PreferenceNotices.answerURL(base: base, id: item.id),
              let body = PreferenceNotices.answerBody(remember: remember, token: token),
              let payload = try? JSONSerialization.data(withJSONObject: body) else { return }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.httpBody = payload
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&req)
        busyId = item.id
        URLSession.shared.dataTask(with: req) { data, response, _ in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let state = data
                .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                .flatMap { ($0["state"] as? String) ?? (($0["notice"] as? [String: Any])?["state"] as? String) }
            DispatchQueue.main.async {
                busyId = nil
                if status == 409 && state == nil {
                    // The note changed since it was read: read it again.
                    notice = "This note changed. Check it again before saving."
                    load()
                } else if (200..<300).contains(status) || status == 409 {
                    notices.removeAll { $0.id == item.id }
                    switch state {
                    case "remembered": notice = "Saved. Future checks use it."
                    case "stale": notice = "Your preference changed since this note, so nothing was saved."
                    default: notice = nil
                    }
                } else {
                    notice = "The note could not be answered. Try again."
                }
            }
        }.resume()
    }
}

/// Settings → Preferences → Your preferences: the owner's preferences exactly
/// as preference checks see them — rules, goals and tastes, how strong the
/// evidence is — with the owner's own edits and each one's history.
///
/// Every edit names the revision on screen; when a preference changed in the
/// meantime the screen reloads and says so rather than overwriting it.
/// Forgetting asks first, because it also erases the wording from the history.
struct OwnerPreferencesView: View {
    @ObservedObject private var theme = ThemeManager.shared
    @State private var preferences: [OwnerPreference] = []
    @State private var loading = true
    @State private var failed = false
    @State private var busy = false
    @State private var notice: String?
    @State private var editing: OwnerPreference?
    @State private var draft = ""
    @State private var forgetting: OwnerPreference?
    @State private var history: OwnerPreference?
    @State private var adding = false
    /// Rendered from fixtures (tests): no network load on appear.
    private let seeded: Bool

    init() {
        seeded = false
    }

    /// The screen with `preferences` already loaded, for render tests.
    init(seededWith preferences: [OwnerPreference]) {
        seeded = true
        _preferences = State(initialValue: preferences)
        _loading = State(initialValue: false)
    }

    private var base: String { "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2" }

    private let groups: [(kind: String, title: String)] = [
        ("standing_instruction", "Rules you set"),
        ("goal", "Goals"),
        ("taste", "Tastes"),
    ]

    var body: some View {
        Form {
            if loading {
                Section { ProgressView().listRowBackground(theme.surfaceColor) }
            } else if failed {
                Section {
                    Text("Your preferences are unavailable.").foregroundColor(theme.secondaryTextColor)
                    Button("Try again") { load() }
                }
                .listRowBackground(theme.surfaceColor)
            } else {
                Section {
                    Text("Swipe a preference to edit, confirm, retire or forget it. Tap it for its history.")
                        .font(.footnote)
                        .foregroundColor(theme.secondaryTextColor)
                }
                .listRowBackground(theme.surfaceColor)
                ForEach(groups, id: \.kind) { group in
                    let items = preferences.filter { $0.kind == group.kind }
                    Section(header: Text(group.title).foregroundColor(theme.secondaryTextColor)) {
                        if items.isEmpty {
                            Text("None yet.").foregroundColor(theme.secondaryTextColor)
                        }
                        ForEach(items) { row($0) }
                    }
                    .listRowBackground(theme.surfaceColor)
                }
            }
            if let notice {
                Section { Text(notice).font(.footnote).foregroundColor(theme.secondaryTextColor) }
                    .listRowBackground(theme.surfaceColor)
            }
        }
        // The app's palette, as Settings does: theme rows on the theme
        // background, never the system grouped grey.
        .scrollContentBackground(.hidden)
        .background(theme.backgroundColor.ignoresSafeArea())
        .navigationTitle("Your preferences")
        .toolbar {
            Button { adding = true } label: { Image(systemName: "plus") }
                .accessibilityIdentifier("owner-preferences-add")
        }
        .onAppear { if !seeded { load() } }
        .refreshable { load() }
        .sheet(item: $history) { preference in
            NavigationView { PreferenceHistoryList(preference: preference, base: base) }
        }
        .sheet(isPresented: $adding) {
            NavigationView { AddOwnerPreferenceForm { body in send("POST", OwnerPreferences.listURL(base: base), body, done: "Added. The next check uses it.") } }
        }
        .alert("Edit preference", isPresented: Binding(get: { editing != nil }, set: { if !$0 { editing = nil } })) {
            TextField("Wording", text: $draft)
            Button("Save") {
                if let preference = editing {
                    send(
                        "PATCH",
                        OwnerPreferences.itemURL(base: base, id: preference.id),
                        OwnerPreferences.patchBody(action: "edit", expectedRevision: preference.revision, wording: draft),
                        done: "Reworded. The old wording is kept in its history."
                    )
                }
                editing = nil
            }
            Button("Cancel", role: .cancel) { editing = nil }
        }
        .confirmationDialog(
            "Forget this preference? Magician stops using it, and its wording is erased from the history too.",
            isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
            titleVisibility: .visible
        ) {
            Button("Forget it", role: .destructive) {
                if let preference = forgetting {
                    send(
                        "DELETE",
                        OwnerPreferences.itemURL(base: base, id: preference.id, expectedRevision: preference.revision),
                        nil,
                        done: "Forgotten, and its wording erased from the history."
                    )
                }
                forgetting = nil
            }
        }
    }

    private func row(_ preference: OwnerPreference) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let presented = preference.presented {
                Text(presented.title).font(.body.weight(.semibold)).foregroundColor(theme.textColor)
                ForEach(presented.fields, id: \.label) { field in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(field.label).font(.footnote).foregroundColor(theme.secondaryTextColor)
                        Text(field.value).font(.footnote).foregroundColor(theme.textColor)
                    }
                }
            } else {
                Text(preference.statement).foregroundColor(theme.textColor)
            }
            if let conditions = preference.conditions {
                Text("Applies \(conditions)").font(.footnote).foregroundColor(theme.secondaryTextColor)
            }
            Text(preference.statusWords).font(.footnote).foregroundColor(theme.secondaryTextColor)
        }
        .contentShape(Rectangle())
        .onTapGesture { history = preference }
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            if preference.editable {
                Button("Forget", role: .destructive) { forgetting = preference }
                Button("Retire") {
                    send(
                        "PATCH",
                        OwnerPreferences.itemURL(base: base, id: preference.id),
                        OwnerPreferences.patchBody(action: "retire", expectedRevision: preference.revision),
                        done: "Retired. Checks no longer use it."
                    )
                }
            }
        }
        .swipeActions(edge: .leading) {
            if preference.editable {
                Button("Edit") {
                    draft = preference.wording
                    editing = preference
                }
                if preference.status == "inferred" {
                    Button("Confirm") {
                        send(
                            "PATCH",
                            OwnerPreferences.itemURL(base: base, id: preference.id),
                            OwnerPreferences.patchBody(action: "confirm", expectedRevision: preference.revision),
                            done: "Confirmed."
                        )
                    }
                }
            }
        }
    }

    private func load() {
        guard let url = OwnerPreferences.listURL(base: base) else { return }
        var req = URLRequest(url: url)
        req.cachePolicy = .reloadIgnoringLocalCacheData
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&req)
        URLSession.shared.dataTask(with: req) { data, response, _ in
            let ok = (200..<300).contains((response as? HTTPURLResponse)?.statusCode ?? 0)
            let parsed = ok ? data.map(OwnerPreferences.parse) : nil
            DispatchQueue.main.async {
                loading = false
                failed = parsed == nil
                preferences = parsed ?? []
            }
        }.resume()
    }

    private func send(_ method: String, _ url: URL?, _ body: [String: Any]?, done: String) {
        guard let url, !busy else { return }
        var req = URLRequest(url: url)
        req.httpMethod = method
        if let body {
            guard let payload = try? JSONSerialization.data(withJSONObject: body) else { return }
            req.httpBody = payload
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        } else if method != "DELETE" {
            return
        }
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&req)
        busy = true
        URLSession.shared.dataTask(with: req) { data, response, _ in
            let outcome = OwnerPreferences.outcome(status: (response as? HTTPURLResponse)?.statusCode ?? 0, data: data)
            DispatchQueue.main.async {
                busy = false
                switch outcome {
                case .applied: notice = done
                case .stale: notice = "This preference changed since you opened it. The current version is shown."
                case .failed: notice = "The change was not saved. Try again."
                }
                load()
            }
        }.resume()
    }
}

/// One preference's history, newest first.
private struct PreferenceHistoryList: View {
    let preference: OwnerPreference
    let base: String
    @ObservedObject private var theme = ThemeManager.shared
    @State private var entries: [PreferenceHistoryEntry] = []
    @State private var loading = true
    @Environment(\.dismiss) private var dismiss

    private static let opWords: [String: String] = [
        "added": "Added", "edited": "Reworded", "confirmed": "Confirmed", "superseded": "Replaced",
        "retired": "Retired", "restored": "Restored", "scope_changed": "Scope changed", "forgotten": "Forgotten",
    ]

    var body: some View {
        List {
            if loading {
                ProgressView()
            } else if entries.isEmpty {
                Text("No recorded changes yet.").foregroundColor(theme.secondaryTextColor)
            }
            ForEach(entries) { entry in
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(Self.opWords[entry.op] ?? entry.op) · \(Date(timeIntervalSince1970: entry.atMs / 1000).formatted(date: .abbreviated, time: .shortened))")
                        .foregroundColor(theme.textColor)
                    if entry.wordingErased {
                        Text("Wording erased").italic().font(.footnote).foregroundColor(theme.secondaryTextColor)
                    } else if let after = entry.wordingAfter {
                        Text(after).font(.footnote).foregroundColor(theme.secondaryTextColor)
                    }
                }
                .listRowBackground(theme.surfaceColor)
            }
        }
        .scrollContentBackground(.hidden)
        .background(theme.backgroundColor.ignoresSafeArea())
        .navigationTitle("History")
        .toolbar { Button("Done") { dismiss() } }
        .onAppear {
            guard let url = OwnerPreferences.historyURL(base: base, id: preference.id) else { return }
            var req = URLRequest(url: url)
            req.setValue("application/json", forHTTPHeaderField: "Accept")
            MagicianAccess.authorize(&req)
            URLSession.shared.dataTask(with: req) { data, _, _ in
                let parsed = data.map(OwnerPreferences.parseHistory) ?? []
                DispatchQueue.main.async {
                    loading = false
                    entries = parsed
                }
            }.resume()
        }
    }
}

/// Add a preference in the owner's own words.
private struct AddOwnerPreferenceForm: View {
    let submit: ([String: Any]?) -> Void
    @State private var kind = "standing_instruction"
    @State private var wording = ""
    @State private var label = ""
    @State private var conditions = ""
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Form {
            Group {
                Picker("Kind", selection: $kind) {
                    Text("A rule (\"check with me before…\")").tag("standing_instruction")
                    Text("A goal").tag("goal")
                    Text("A taste").tag("taste")
                }
                TextField("In your words", text: $wording, axis: .vertical)
                TextField("Short label (optional)", text: $label)
                TextField("Only when (optional)", text: $conditions)
            }
            .listRowBackground(theme.surfaceColor)
        }
        .scrollContentBackground(.hidden)
        .background(theme.backgroundColor.ignoresSafeArea())
        .navigationTitle("Add a preference")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Add") {
                    submit(OwnerPreferences.addBody(kind: kind, wording: wording, label: label, conditions: conditions))
                    dismiss()
                }
                .disabled(wording.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }
}
