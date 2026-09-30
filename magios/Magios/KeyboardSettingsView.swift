import SwiftUI
import UIKit

/// In-app home for the Magican Keyboard: guided enable (with live status), and a full
/// editor for the skill chips (stored in the App Group; the keyboard picks up
/// changes on its next launch).
struct KeyboardSettingsView: View {
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var scenePhase
    @State private var skills: [KeyboardSkill] = KeyboardSkillStore.load()
    @State private var editing: KeyboardSkill?
    @State private var addingNew = false
    @State private var installed = KeyboardInstall.hasRun
    @State private var fullAccess = KeyboardInstall.hasFullAccess
    @State private var language = KeyboardLanguageStore.current

    var body: some View {
        Form {
            Section(header: Text("Status").foregroundColor(theme.secondaryTextColor)) {
                statusRow(installed, on: "Keyboard added & used", off: "Not added yet")
                statusRow(fullAccess, on: "Full Access on", off: "Full Access off",
                          warnWhenOff: installed)
                if !installed || !fullAccess {
                    Button { openURL(URL(string: UIApplication.openSettingsURLString)!) } label: {
                        Label("Open iOS Settings", systemImage: "arrow.up.forward.app")
                            .foregroundColor(theme.accentColor)
                    }
                }
            }

            Section(header: Text("Enable").foregroundColor(theme.secondaryTextColor)) {
                stepRow(1, "Open iOS Settings → General → Keyboard → Keyboards")
                stepRow(2, "Add New Keyboard → Magican")
                stepRow(3, "Tap Magican → Allow Full Access")
                Text("Typing works without Full Access. The agentic lanes (Write / Ask / Act, skill chips, haptics) need it — nothing leaves the device otherwise.")
                    .font(.caption).foregroundColor(theme.secondaryTextColor)
                NavigationLink(destination: KeyboardPlaygroundView()) {
                    Label("Try the interactive tutorial", systemImage: "sparkles")
                        .foregroundColor(theme.accentColor)
                }
            }

            Section(
                header: Text("Language").foregroundColor(theme.secondaryTextColor),
                footer: Text("Sets autocorrect + suggestions. iOS has no language picker for third-party keyboards, so it's set here; the keyboard picks it up on its next launch.")
            ) {
                Picker(selection: $language) {
                    ForEach(KeyboardLanguage.allCases) { lang in
                        Text(lang.displayName).tag(lang)
                    }
                } label: {
                    Label("Keyboard language", systemImage: "character.book.closed")
                        .foregroundColor(theme.textColor)
                }
                .onChange(of: language) { _, newValue in KeyboardLanguageStore.set(newValue) }
            }

            Section(header: Text("How to use Magican").foregroundColor(theme.secondaryTextColor)) {
                tipRow("sparkles", "Open Magican actions", "Tap ✦ at the right of the suggestion strip, or hold the spacebar still for about half a second.")
                tipRow("wand.and.stars", "Rewrite", "Type something or place the cursor after existing text, open Magican actions, then tap Rewrite. Review before Replace; Undo remains available.")
                tipRow("questionmark.bubble", "Ask", "Open Magican actions and tap Ask. The answer streams into the keyboard; tap Insert to add it to the field.")
                tipRow("checklist", "Run an action", "Swipe through the skill chips and tap an orange Act skill such as Add task. Magican always shows a confirmation card first.")
                tipRow("lock.fill", "Secure fields", "In password / OTP / number fields Magican captures nothing.")
            }

            Section(
                header: Text("Skills").foregroundColor(theme.secondaryTextColor),
                footer: Text("Open ✦ to see these in a horizontally scrollable row above the keys. Write = rewrite guidance; Ask / Act = a prompt where {text} is your text. Changes appear the next time the keyboard launches.")
            ) {
                ForEach(skills) { skill in
                    Button { editing = skill } label: { skillRow(skill) }
                        .buttonStyle(.plain)
                }
                .onDelete { skills.remove(atOffsets: $0); save() }
                .onMove { skills.move(fromOffsets: $0, toOffset: $1); save() }

                Button { addingNew = true } label: {
                    Label("Add skill", systemImage: "plus.circle.fill").foregroundColor(theme.accentColor)
                }
            }

            Section {
                Button(role: .destructive) {
                    KeyboardSkillStore.resetToDefault()
                    skills = KeyboardSkillStore.defaultPack
                } label: {
                    Text("Reset to default skills")
                }
            }
        }
        .navigationTitle("Magican Keyboard")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { EditButton() }
        .onAppear { refreshStatus() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { refreshStatus() }
        }
        .sheet(item: $editing) { skill in
            SkillEditorView(skill: skill) { updated in
                if let i = skills.firstIndex(where: { $0.id == updated.id }) { skills[i] = updated }
                save()
            }
        }
        .sheet(isPresented: $addingNew) {
            SkillEditorView(skill: KeyboardSkill(label: "", symbol: "sparkles", lane: .write, template: "")) { created in
                skills.append(created)
                save()
            }
        }
    }

    private func save() { KeyboardSkillStore.save(skills) }

    private func refreshStatus() {
        installed = KeyboardInstall.hasRun
        fullAccess = KeyboardInstall.hasFullAccess
    }

    private func statusRow(_ ok: Bool, on: String, off: String, warnWhenOff: Bool = false) -> some View {
        HStack(spacing: 12) {
            Image(systemName: ok ? "checkmark.circle.fill" : (warnWhenOff ? "exclamationmark.circle.fill" : "circle"))
                .font(.system(size: 18))
                .foregroundColor(ok ? theme.successColor : (warnWhenOff ? theme.warningColor : theme.secondaryTextColor))
            Text(ok ? on : off).font(.subheadline).foregroundColor(theme.textColor)
        }
    }

    private func stepRow(_ n: Int, _ text: String) -> some View {
        HStack(spacing: 12) {
            Text("\(n)").font(.footnote.bold()).foregroundColor(theme.onAccentColor)
                .frame(width: 22, height: 22).background(Circle().fill(theme.accentColor))
            Text(text).font(.subheadline).foregroundColor(theme.textColor)
        }
    }

    private func tipRow(_ symbol: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: symbol).font(.system(size: 16)).foregroundColor(theme.accentColor).frame(width: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.semibold)).foregroundColor(theme.textColor)
                Text(detail).font(.caption).foregroundColor(theme.secondaryTextColor).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func skillRow(_ skill: KeyboardSkill) -> some View {
        HStack(spacing: 12) {
            Image(systemName: skill.symbol.isEmpty ? "sparkles" : skill.symbol)
                .font(.system(size: 15)).foregroundColor(theme.accentColor).frame(width: 26)
            VStack(alignment: .leading, spacing: 2) {
                Text(skill.label).font(.subheadline.weight(.medium)).foregroundColor(theme.textColor)
                Text(skill.template).font(.caption).foregroundColor(theme.secondaryTextColor).lineLimit(1)
            }
            Spacer()
            Text(skill.lane.rawValue.capitalized)
                .font(.caption2.weight(.semibold))
                .padding(.horizontal, 7).padding(.vertical, 3)
                .background(Capsule().fill(laneColor(skill.lane).opacity(0.18)))
                .foregroundColor(laneColor(skill.lane))
        }
    }

    private func laneColor(_ lane: KeyboardSkill.Lane) -> Color {
        switch lane {
        case .write: return theme.accentColor
        case .ask: return theme.infoColor
        case .act: return theme.warningColor
        }
    }
}

/// The live tutorial playground: a real field where the Magican keyboard coaches the
/// user through each feature as they do it. Arming the coach (App Group) makes the
/// keyboard show its step-by-step banner here.
struct KeyboardPlaygroundView: View {
    @ObservedObject private var theme = ThemeManager.shared
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        ScrollView {
            VStack(spacing: 18) {
                VStack(spacing: 8) {
                    Image(systemName: "keyboard.badge.ellipsis")
                        .font(.system(size: 38)).foregroundColor(theme.accentColor)
                    Text("Learn by doing")
                        .font(.title3.bold()).foregroundColor(theme.textColor)
                    Text("Tap the box below, switch to the **Magican** keyboard with the 🌐 key, and follow the tips that appear right on the keyboard.")
                        .font(.subheadline).foregroundColor(theme.secondaryTextColor)
                        .multilineTextAlignment(.center)
                }
                .padding(.top, 10)

                TextField("Type here to start…", text: $text, axis: .vertical)
                    .focused($focused)
                    .font(.body)
                    .foregroundColor(theme.textColor)
                    .padding(14)
                    .frame(minHeight: 130, alignment: .topLeading)
                    .background(RoundedRectangle(cornerRadius: 14).fill(theme.surfaceColor))
                    .overlay(RoundedRectangle(cornerRadius: 14).stroke(theme.accentColor.opacity(0.3), lineWidth: 1))

                VStack(alignment: .leading, spacing: 10) {
                    tip("1", "Type a few words in the field above.")
                    tip("2", "Tap ✦ at the right of the suggestion strip, or hold the spacebar still.")
                    tip("3", "Choose Rewrite or Ask from the Magican action row.")
                    tip("4", "Swipe the skill chips; orange Add task is the default Act action.")
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(RoundedRectangle(cornerRadius: 14).fill(theme.surfaceColor.opacity(0.6)))

                Text("The agentic tips need Full Access (Settings → Keyboards → Magican → Allow Full Access).")
                    .font(.caption).foregroundColor(theme.secondaryTextColor).multilineTextAlignment(.center)
            }
            .padding(20)
        }
        .background(theme.backgroundColor.ignoresSafeArea())
        .navigationTitle("Tutorial")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { KeyboardCoach.arm() }
        .onDisappear { KeyboardCoach.disarm() }
    }

    private func tip(_ n: String, _ text: String) -> some View {
        HStack(spacing: 10) {
            Text(n).font(.footnote.bold()).foregroundColor(theme.onAccentColor)
                .frame(width: 20, height: 20).background(Circle().fill(theme.accentColor))
            Text(text).font(.subheadline).foregroundColor(theme.textColor)
        }
    }
}

/// Add / edit one skill chip.
private struct SkillEditorView: View {
    @State var skill: KeyboardSkill
    let onSave: (KeyboardSkill) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            Form {
                TextField("Label", text: $skill.label)
                Picker("Lane", selection: $skill.lane) {
                    Text("Write — rewrite in place").tag(KeyboardSkill.Lane.write)
                    Text("Ask — stream an answer").tag(KeyboardSkill.Lane.ask)
                    Text("Act — run a task (confirmed)").tag(KeyboardSkill.Lane.act)
                }
                TextField("SF Symbol name (e.g. wand.and.stars)", text: $skill.symbol)
                    .autocorrectionDisabled().textInputAutocapitalization(.never)
                Section(
                    header: Text("Template"),
                    footer: Text(skill.lane == .write
                        ? "Rewrite guidance, e.g. “Make this professional.”"
                        : "A prompt. Use {text} where your typed / field text should go.")
                ) {
                    TextEditor(text: $skill.template).frame(minHeight: 100)
                }
            }
            .navigationTitle("Skill")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { onSave(skill); dismiss() }
                        .disabled(skill.label.trimmingCharacters(in: .whitespaces).isEmpty
                            || skill.template.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
    }
}
