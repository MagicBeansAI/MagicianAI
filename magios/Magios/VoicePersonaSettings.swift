import Foundation
import SwiftUI
import AVFoundation

struct VoicePersonaChoice: Identifiable, Equatable {
    let id: String
    let label: String
}

struct VoicePersonaBinding: Identifiable, Equatable {
    let key: String
    let realtime: Bool
    let label: String
    let defaultVoice: String?
    let choices: [VoicePersonaChoice]
    let available: Bool
    var id: String { "\(realtime ? "realtime" : "tts"):\(key)" }
    var field: String { realtime ? "realtime_voices" : "tts_voices" }
}

@MainActor
final class VoicePersonaSettingsModel: ObservableObject {
    @Published private(set) var bindings: [VoicePersonaBinding] = []
    @Published private(set) var selections: [String: [String: String]] = [:]
    @Published private(set) var loading = false
    @Published private(set) var saving = false
    @Published var error: String?
    private let loadObject: (URLRequest) async throws -> [String: Any]
    private var loadedScope: String?

    init(loadObject: @escaping (URLRequest) async throws -> [String: Any] = VoicePersonaSettingsModel.requestObject) {
        self.loadObject = loadObject
    }

    static func catalog(_ object: [String: Any]) -> [VoicePersonaBinding] {
        let stages = object["stages"] as? [String: Any] ?? [:]
        let tts = (stages["tts"] as? [[String: Any]] ?? []).compactMap { raw -> VoicePersonaBinding? in
            guard let key = raw["provider_id"] as? String,
                  let capabilities = raw["capabilities"] as? [String: Any],
                  let voices = capabilities["voices"] as? [String], !voices.isEmpty else { return nil }
            return VoicePersonaBinding(
                key: key, realtime: false, label: raw["label"] as? String ?? key,
                defaultVoice: raw["variant"] as? String,
                choices: voices.map { VoicePersonaChoice(id: $0, label: $0.replacingOccurrences(of: "_", with: " ")) },
                available: raw["availability"] as? String == "available"
            )
        }
        let realtime = (object["realtime_voice_profiles"] as? [[String: Any]] ?? []).compactMap { raw -> VoicePersonaBinding? in
            guard raw["mode"] as? String != "translation",
                  let key = raw["profile_id"] as? String else { return nil }
            let choices = (raw["voices"] as? [[String: String]] ?? []).compactMap { voice -> VoicePersonaChoice? in
                guard let id = voice["id"] else { return nil }
                return VoicePersonaChoice(id: id, label: voice["label"] ?? id)
            }
            guard !choices.isEmpty else { return nil }
            return VoicePersonaBinding(
                key: key, realtime: true, label: raw["label"] as? String ?? key,
                defaultVoice: raw["voice"] as? String, choices: choices,
                available: raw["available"] as? Bool ?? false
            )
        }
        return tts + realtime
    }

    private func request(_ path: String) -> URLRequest {
        var request = URLRequest(url: MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/media/\(path)"))
        MagicianAccess.authorize(&request)
        return request
    }

    static func requestObject(_ request: URLRequest) async throws -> [String: Any] {
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode),
              let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw URLError(.badServerResponse)
        }
        return object
    }

    private func scopeKey() -> String {
        let authorized = request("providers")
        return MagicianAccess.baseURL.absoluteString + (authorized.allHTTPHeaderFields ?? [:]).sorted { $0.key < $1.key }.map { "|\($0.key)=\($0.value)" }.joined()
    }

    func refresh(force: Bool = false) async {
        guard !loading, !saving else { return }
        let scope = scopeKey()
        guard force || loadedScope != scope else { return }
        if loadedScope != nil && loadedScope != scope { bindings = []; selections = [:]; loadedScope = nil }
        loading = true
        error = nil
        defer { loading = false }
        do {
            let catalog = try await loadObject(request("providers"))
            let preferences = try await loadObject(request("preferences"))
            try Task.checkCancellation()
            guard scopeKey() == scope else { return }
            bindings = Self.catalog(catalog)
            selections = [
                "tts_voices": preferences["tts_voices"] as? [String: String] ?? [:],
                "realtime_voices": preferences["realtime_voices"] as? [String: String] ?? [:]
            ]
            loadedScope = scope
        } catch {
            // A disappearing/replaced panel cancels its task; that is not a
            // provider failure and must not leave an error on the next panel.
            if Task.isCancelled || (error as? URLError)?.code == .cancelled || error is CancellationError { return }
            self.error = "Could not load voice settings. Check your connection and retry."
        }
    }

    func choose(_ voice: String, for binding: VoicePersonaBinding) async -> Bool {
        guard !saving else { return false }
        saving = true
        defer { saving = false }
        do {
            var update = request("preferences")
            update.httpMethod = "PUT"
            update.setValue("application/json", forHTTPHeaderField: "Content-Type")
            update.httpBody = try JSONSerialization.data(withJSONObject: [binding.field: [binding.key: voice]])
            let confirmed = try await loadObject(update)
            selections[binding.field] = confirmed[binding.field] as? [String: String] ?? [:]
            error = nil
            return true
        } catch {
            self.error = "Could not save voice. Your previous choice is still selected."
            return false
        }
    }
}

// Match the model picker: compact themed rows open a searchable choice page
// in the current NavigationStack, including when already presented as a sheet.
struct VoiceSelectionOption: Identifiable {
    let id: String
    let label: String
    var detail: String? = nil
    var available = true
}

struct VoiceSelectionPicker: View {
    let title: String
    let selected: String
    let options: [VoiceSelectionOption]
    var enabled = true
    let onSelect: (String) async throws -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        NavigationLink {
            VoiceSelectionList(title: title, selected: selected, options: options, onSelect: onSelect)
        } label: {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.themed(13, weight: .semibold)).foregroundColor(theme.textColor)
                    Text(options.first(where: { $0.id == selected })?.label ?? (selected.isEmpty ? "Choose…" : selected))
                        .font(.themed(11)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
                }
                Spacer(minLength: 8)
                Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
            }
            .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 12).padding(.vertical, 10)
            .background(theme.surfaceColor).clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.cardBorderColor, lineWidth: 1))
            .contentShape(Rectangle())
        }.buttonStyle(.plain).disabled(!enabled).opacity(enabled ? 1 : 0.55)
    }
}

struct VoiceSelectionList: View {
    let title: String
    let selected: String
    let options: [VoiceSelectionOption]
    var dismissOnSelect = true
    let onSelect: (String) async throws -> Void
    @State private var query = ""
    @State private var saving = false
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                if let error { Text(error).font(.themed(12)).foregroundColor(.red).padding(12) }
                let filtered = options.filter { query.isEmpty || "\($0.label) \($0.id) \($0.detail ?? "")".localizedCaseInsensitiveContains(query) }
                ForEach(filtered) { option in
                    Button {
                        saving = true
                        Task {
                            defer { saving = false }
                            do { try await onSelect(option.id); if dismissOnSelect { dismiss() } }
                            catch { self.error = "Could not save. Your previous choice is still selected." }
                        }
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(option.label).font(.themed(14, weight: .semibold)).foregroundColor(theme.textColor)
                                if let detail = option.detail { Text(detail).font(.themed(11)).foregroundColor(theme.secondaryTextColor) }
                            }
                            Spacer(minLength: 8)
                            if selected == option.id { Image(systemName: "checkmark.circle.fill").foregroundColor(theme.accentColor) }
                        }.padding(12).frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                    }.buttonStyle(.plain).disabled(saving || !option.available).opacity(option.available ? 1 : 0.5)
                    Divider().overlay(theme.cardBorderColor)
                }
                if filtered.isEmpty { Text("No matches").font(.themed(13)).foregroundColor(theme.secondaryTextColor).padding(16) }
            }
            .background(theme.surfaceColor).clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.cardBorderColor, lineWidth: 1))
            .padding(16)
        }
        .background(theme.backgroundColor)
        .navigationTitle(title).navigationBarTitleDisplayMode(.inline)
        .searchable(text: $query, prompt: "Search choices")
        .overlay { if saving { ProgressView().tint(theme.accentColor) } }
    }
}

struct VoicePersonaControls: View {
    var showTTS = true
    var showRealtime = true
    var realtimeProfileID: String? = nil
    var ttsProviderID: String? = nil
    @StateObject private var model = VoicePersonaSettingsModel()
    @ObservedObject private var theme = ThemeManager.shared
    private var bindings: [VoicePersonaBinding] {
        model.bindings.filter { $0.realtime ? showRealtime && (realtimeProfileID == nil || $0.key == realtimeProfileID) : showTTS && (ttsProviderID == nil || $0.key == ttsProviderID) }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if ttsProviderID == nil {
                Text("Speaking voices").font(.themed(12, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                Text("Shared across your devices. Live voices apply to the next call.").font(.themed(11)).foregroundColor(theme.secondaryTextColor)
            }
            if model.loading { ProgressView("Loading voices…") }
            ForEach(bindings) { binding in
                VoiceSelectionPicker(title: ttsProviderID == nil ? binding.label : "Speaking voice", selected: model.selections[binding.field]?[binding.key] ?? "",
                    options: [VoiceSelectionOption(id: "", label: "Provider default", detail: binding.defaultVoice)] + binding.choices.map { VoiceSelectionOption(id: $0.id, label: $0.label) },
                    enabled: binding.available && !model.saving && !model.loading) { id in
                    if !(await model.choose(id, for: binding)) { throw URLError(.badServerResponse) }
                }
            }
            if let error = model.error {
                Text(error).font(.themed(12)).foregroundColor(.red)
                Button("Retry") { Task { await model.refresh(force: true) } }
            } else if !model.loading && bindings.isEmpty {
                Text(ttsProviderID == nil ? "No selectable voices are available for these models." : "Uses the provider’s default voice. No voice choices are listed by this server.").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            }
        }.task { await model.refresh() }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            Task { await model.refresh(force: true) }
        }
    }
}

struct DeviceSpeechVoicePicker: View {
    @ObservedObject private var settings = AudioSettings.shared
    var body: some View {
        VoiceSelectionPicker(title: "This iPhone · speaking voice", selected: settings.deviceTTSVoiceID,
            options: [VoiceSelectionOption(id: "", label: "System default")] + AVSpeechSynthesisVoice.speechVoices().sorted { $0.name < $1.name }.map {
                VoiceSelectionOption(id: $0.identifier, label: $0.name, detail: $0.language)
            }) { id in settings.deviceTTSVoiceID = id }
    }
}
