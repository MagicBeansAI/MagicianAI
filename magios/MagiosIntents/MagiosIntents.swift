import AppIntents
import Combine
import Foundation

// MARK: - Runtime primary-agent identity for Siri

/// The canonical primary-agent identity returned by the scoped Crew API.
/// Keeping this value in the App Group lets App Intents resolve it even when
/// Siri runs the intent outside the foreground app process.
struct PrimaryAgentSiriIdentity: Codable, Equatable, Sendable {
    let agentID: String
    let name: String
    let aliases: [String]
    /// In-lexicon spellings the on-device wake spotter arms INSTEAD of `name`,
    /// for names the speech model has no vocabulary entry for. Empty means the
    /// name is armable as spelled. Never used for display or for Siri, whose
    /// recognition is open-vocabulary.
    let wakeSpellings: [String]

    /// Defaulted so call sites predating wake spellings keep compiling, and so
    /// an App Group cache written before this field decodes rather than throws.
    init(agentID: String, name: String, aliases: [String], wakeSpellings: [String] = []) {
        self.agentID = agentID
        self.name = name
        self.aliases = aliases
        self.wakeSpellings = wakeSpellings
    }

    enum CodingKeys: String, CodingKey {
        case agentID, name, aliases, wakeSpellings
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        agentID = try c.decode(String.self, forKey: .agentID)
        name = try c.decode(String.self, forKey: .name)
        aliases = try c.decodeIfPresent([String].self, forKey: .aliases) ?? []
        wakeSpellings = try c.decodeIfPresent([String].self, forKey: .wakeSpellings) ?? []
    }

    /// Canonical name first, then every distinct alias. Matching is
    /// case-insensitive, while the backend's display casing is preserved.
    var advertisedNames: [String] {
        var seen = Set<String>()
        return ([name] + aliases).compactMap { raw in
            let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !value.isEmpty else { return nil }
            let key = value.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
            return seen.insert(key).inserted ? value : nil
        }
    }

    /// What the on-device spotter arms. Falls back to the advertised names when
    /// no spelling override exists; an override replaces them because the
    /// shipped identity's canonical name cannot be armed by this recogniser.
    var armingNames: [String] {
        let spellings = wakeSpellings
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        return spellings.isEmpty ? advertisedNames : spellings
    }

    var entities: [PrimaryAgentAliasEntity] {
        advertisedNames.map { name in
            PrimaryAgentAliasEntity(
                id: "\(agentID):\(name.lowercased())",
                agentID: agentID,
                spokenName: name
            )
        }
    }

    /// Decode the actual primary record instead of assuming a fixed agent id,
    /// position in the list, display name, or alias.
    static func decodePrimary(from data: Data) throws -> PrimaryAgentSiriIdentity {
        let response = try JSONDecoder().decode(AgentListEnvelope.self, from: data)
        guard let definition = response.agents.lazy.map(\.definition).first(where: \.isPrimary) else {
            throw PrimaryAgentSiriIdentityError.primaryAgentMissing
        }
        let agentID = definition.agentID.trimmingCharacters(in: .whitespacesAndNewlines)
        let name = definition.name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !agentID.isEmpty, !name.isEmpty else {
            throw PrimaryAgentSiriIdentityError.invalidPrimaryAgent
        }
        return PrimaryAgentSiriIdentity(
            agentID: agentID,
            name: name,
            aliases: definition.aliases,
            wakeSpellings: definition.wakeSpellings
        )
    }

    private struct AgentListEnvelope: Decodable {
        let agents: [AgentRecord]
    }

    private struct AgentRecord: Decodable {
        let definition: AgentDefinition
    }

    private struct AgentDefinition: Decodable {
        let agentID: String
        let name: String
        let aliases: [String]
        let wakeSpellings: [String]
        let isPrimary: Bool

        enum CodingKeys: String, CodingKey {
            case agentID = "agent_id"
            case name, aliases
            case wakeSpellings = "wake_spellings"
            case isPrimary = "is_primary"
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            agentID = try container.decode(String.self, forKey: .agentID)
            name = try container.decode(String.self, forKey: .name)
            aliases = try container.decodeIfPresent([String].self, forKey: .aliases) ?? []
            wakeSpellings = try container.decodeIfPresent([String].self, forKey: .wakeSpellings) ?? []
            isPrimary = try container.decodeIfPresent(Bool.self, forKey: .isPrimary) ?? false
        }
    }
}

enum PrimaryAgentSiriIdentityError: Error, Equatable {
    case primaryAgentMissing
    case invalidPrimaryAgent
    case invalidResponse
}

enum PrimaryAgentSiriIdentityStore {
    private static let key = "siri.primary-agent-identity.v1"

    static func load(from store: UserDefaults = MagicianAccess.store) -> PrimaryAgentSiriIdentity? {
        guard let data = store.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(PrimaryAgentSiriIdentity.self, from: data)
    }

    /// Returns true only when Siri's parameter inventory actually changed.
    @discardableResult
    static func save(
        _ identity: PrimaryAgentSiriIdentity,
        to store: UserDefaults = MagicianAccess.store
    ) -> Bool {
        guard load(from: store) != identity,
              let data = try? JSONEncoder().encode(identity) else { return false }
        store.set(data, forKey: key)
        return true
    }
}

struct PrimaryAgentAliasEntity: AppEntity, Equatable {
    static var typeDisplayRepresentation: TypeDisplayRepresentation = "Assistant name"
    static var defaultQuery = PrimaryAgentAliasQuery()

    let id: String
    let agentID: String
    let spokenName: String

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(spokenName)", subtitle: "Primary Magican assistant")
    }
}

struct PrimaryAgentAliasQuery: EntityStringQuery {
    func entities(for identifiers: [PrimaryAgentAliasEntity.ID]) async throws -> [PrimaryAgentAliasEntity] {
        let requested = Set(identifiers)
        return availableEntities.filter { requested.contains($0.id) }
    }

    /// App Shortcut phrases interpolate this query into
    /// `"Ask \(\.$assistant) using \(.applicationName)"`. When an advertised
    /// name equals the app name the expansion reads "Ask Magican using
    /// Magican", which duplicates the plain `"Ask \(.applicationName)"`
    /// shortcut. Suppress it here so Siri registers each phrase once; the
    /// name still RESOLVES because `entities(matching:)` stays unfiltered.
    func suggestedEntities() async throws -> [PrimaryAgentAliasEntity] {
        availableEntities.filter {
            $0.spokenName.caseInsensitiveCompare(MagicianAccess.productName) != .orderedSame
        }
    }

    func entities(matching string: String) async throws -> [PrimaryAgentAliasEntity] {
        let query = string.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return availableEntities }
        return availableEntities.filter {
            $0.spokenName.localizedCaseInsensitiveContains(query)
        }
    }

    private var availableEntities: [PrimaryAgentAliasEntity] {
        PrimaryAgentSiriIdentityStore.load()?.entities ?? []
    }
}

/// Refreshes the primary-agent identity on every foreground activation and
/// re-advertises App Shortcut parameter values. Cached values are advertised
/// immediately; a changed backend definition is advertised again after fetch.
@MainActor
final class PrimaryAgentSiriAdvertiser: ObservableObject {
    static let shared = PrimaryAgentSiriAdvertiser()

    @Published private(set) var identity = PrimaryAgentSiriIdentityStore.load()
    private var refreshTask: Task<Void, Never>?
    private var lastRefreshStartedAt = Date.distantPast

    var preferredName: String? { identity?.advertisedNames.first }
    var alternateNames: [String] {
        identity.map { Array($0.advertisedNames.dropFirst()) } ?? []
    }

    func refreshAndAdvertise(session: URLSession = .shared) {
        // Apple recommends refreshing stored App Shortcut parameters from app
        // initialization. Doing this on every activation also evicts stale
        // aliases after an out-of-band Crew definition change.
        MagiosShortcuts.updateAppShortcutParameters()

        // SwiftUI may deliver onAppear and scene-active back-to-back. Both calls
        // advertise the cache, but only one network refresh is useful.
        guard refreshTask == nil,
              Date().timeIntervalSince(lastRefreshStartedAt) >= 2 else { return }
        lastRefreshStartedAt = Date()
        refreshTask = Task { [weak self] in
            defer { self?.refreshTask = nil }
            do {
                let identity = try await Self.fetchIdentity(session: session)
                let changed = PrimaryAgentSiriIdentityStore.save(identity)
                self?.identity = identity
                if changed { MagiosShortcuts.updateAppShortcutParameters() }
            } catch {
                // Keep the last known identity. Siri remains usable offline and
                // the next foreground activation retries the canonical source.
            }
        }
    }

    static func fetchIdentity(session: URLSession) async throws -> PrimaryAgentSiriIdentity {
        var components = URLComponents(
            url: MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/agents"),
            resolvingAgainstBaseURL: false
        )
        components?.queryItems = [
            URLQueryItem(name: "offset", value: "0"),
            // The server's documented maximum. Fetching the complete inventory
            // guarantees the primary definition cannot sit on a later page.
            URLQueryItem(name: "limit", value: "500")
        ]
        guard let url = components?.url else { throw PrimaryAgentSiriIdentityError.invalidResponse }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        MagicianAccess.authorize(&request)
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse,
              (200...299).contains(http.statusCode) else {
            throw PrimaryAgentSiriIdentityError.invalidResponse
        }
        return try PrimaryAgentSiriIdentity.decodePrimary(from: data)
    }
}

/// Sends free-form work to whichever scoped Crew definition is currently
/// marked primary. Its Siri-facing names come from the dynamic entity above;
/// this intent contains no product-specific assistant name or alias.
struct AskMagicianIntent: AppIntent {
    static var title: LocalizedStringResource = "Ask Your Assistant"
    static var description = IntentDescription("Sends a request to your primary Magican assistant.")

    @Parameter(title: "Assistant name")
    var assistant: PrimaryAgentAliasEntity?

    @Parameter(title: "Prompt")
    var prompt: String

    func perform() async throws -> some IntentResult & ReturnsValue<String> {
        // Start a Live Activity and, only when that visible surface exists, its
        // bounded local keepalive. Bound to the task id below so it tracks the
        // right run; task dispatch itself does not depend on ActivityKit.
        let activityLifecycleID = await BackgroundEngine.shared.start(taskName: prompt)

        let url = MagicianAccess.baseURL.appendingPathComponent("api/magician/v3/tasks")

        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let payload: [String: Any] = [
            "prompt": prompt,
            "source": "ios_siri_intent"
        ]

        request.httpBody = try? JSONSerialization.data(withJSONObject: payload)

        let assistantName = assistant?.spokenName
            ?? PrimaryAgentSiriIdentityStore.load()?.advertisedNames.first
            ?? "your assistant"

        do {
            // Send the request to the Magican backend via the Cloudflare tunnel
            let (data, response) = try await URLSession.shared.data(for: request)

            if let httpResponse = response as? HTTPURLResponse, (200...299).contains(httpResponse.statusCode) {
                // Correlate the Live Activity / Dynamic Island to THIS task so it
                // tracks the right run out of the shared realtime stream. Response
                // shape is { "task": { "id": … } }.
                if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                   let task = json["task"] as? [String: Any],
                   let taskId = task["id"] as? String,
                   !taskId.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    if let activityLifecycleID {
                        if await BackgroundEngine.shared.bindTask(
                            taskId,
                            lifecycleID: activityLifecycleID
                        ) {
                            return .result(value: "I have successfully sent that to \(assistantName).")
                        }
                        return .result(value: "That task was sent, but a newer request replaced its progress card.")
                    }
                    return .result(value: "I have successfully sent that to \(assistantName).")
                }
                if let activityLifecycleID {
                    await BackgroundEngine.shared.stop(lifecycleID: activityLifecycleID)
                }
                return .result(value: "I reached \(assistantName), but could not track the task it created.")
            } else {
                if let activityLifecycleID {
                    await BackgroundEngine.shared.stop(lifecycleID: activityLifecycleID)
                }
                return .result(value: "I reached \(assistantName), but received an error response.")
            }
        } catch {
            if let activityLifecycleID {
                await BackgroundEngine.shared.stop(lifecycleID: activityLifecycleID)
            }
            return .result(value: "I could not connect to \(assistantName). Please check your connection.")
        }
    }
}

/// Opens the app straight into a fresh chat. `openAppWhenRun` launches Magican;
/// the pending action is left in the App Group and consumed by `AppActions` on
/// foreground (which switches to the Chat tab and starts a new session).
struct NewChatIntent: AppIntent {
    static var title: LocalizedStringResource = "New Magican Chat"
    static var description = IntentDescription("Opens Magican to a fresh chat.")
    static var openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        SharedActions.setPending("new-chat")
        return .result()
    }
}

/// Opens the app straight into a blackboard (source-free) Tutor — "explain a
/// concept" from anywhere, no screenshot needed. `openAppWhenRun` launches Magican;
/// the pending action is consumed by `AppActions` on foreground, which presents the
/// Tutor overlay in blackboard mode ready for the concept. Assignable to the Action
/// Button / Back Tap / Control Center via Shortcuts.
struct BlackboardTutorIntent: AppIntent {
    static var title: LocalizedStringResource = "Ask Tutor (Blackboard)"
    static var description = IntentDescription("Opens Magican and starts a blackboard Tutor to explain a concept.")
    static var openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        SharedActions.setPending("tutor-blackboard")
        return .result()
    }
}

/// Opens Magican and starts an in-app "Listen here" room capture — "start listening"
/// from Siri, the Action Button, Back Tap, Shortcuts, or Spotlight. `AppActions`
/// consumes the pending action on foreground, switches to Observe, and starts the
/// session.
struct StartListeningIntent: AppIntent {
    static var title: LocalizedStringResource = "Start Listening"
    static var description = IntentDescription("Opens Magican and starts listening to the room / meeting.")
    static var openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        SharedActions.setPending("observe")
        return .result()
    }
}

/// Donated Shortcuts action for lock-screen Talk. Distinct from
/// `ArmAmbientIntent` (Control Center / Lock Screen `OpenIntent`) so it
/// appears in Magican's Shortcuts folder without a destination parameter.
struct TalkToMagicanIntent: AppIntent {
    static var title: LocalizedStringResource = "Talk to Magican"
    static var description = IntentDescription(
        "Starts talking immediately, then keeps Magican available for wake-word follow-ups."
    )
    static var openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        SharedActions.setPending(SharedActions.PendingAction.ambientArm)
        return .result()
    }
}

struct MagiosShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        // Free-text (String) parameters can't be interpolated into phrases —
        // AppIntents only allows AppEntity/AppEnum there. Siri prompts for the
        // prompt value via the @Parameter title instead.
        // AppIntents requires every utterance to contain exactly one
        // `\(.applicationName)`. The entity interpolation expands this phrase
        // for the canonical primary-agent name and every current alias.
        AppShortcut(
            intent: AskMagicianIntent(),
            phrases: [
                "Ask \(.applicationName)",
                // Natural, app-qualified form. The application token keeps the
                // request inside Magican even when the assistant alias is a contact.
                "Ask \(\.$assistant) using \(.applicationName)",
                // Put the application name first so Siri enters Magican's
                // shortcut vocabulary before resolving an assistant alias.
                // "Ask Sam ..." otherwise competes with Messages contacts.
                "\(.applicationName) ask \(\.$assistant)",
                "Tell \(.applicationName) something"
            ],
            shortTitle: "Ask Assistant",
            systemImageName: "wand.and.stars",
            parameterPresentation: ParameterPresentation(
                for: \.$assistant,
                summary: Summary("Ask \(\.$assistant)")
            ) {
                OptionsCollection(
                    PrimaryAgentAliasQuery(),
                    title: "Primary assistant names",
                    systemImageName: "person.wave.2.fill"
                )
            }
        )
        AppShortcut(
            intent: NewChatIntent(),
            phrases: [
                "New chat in \(.applicationName)",
                "Start a \(.applicationName) chat"
            ],
            shortTitle: "New Chat",
            systemImageName: "bubble.left.and.bubble.right.fill"
        )
        AppShortcut(
            intent: BlackboardTutorIntent(),
            phrases: [
                "Ask Tutor on \(.applicationName)",
                "Explain a concept on \(.applicationName)",
                "Start a \(.applicationName) blackboard"
            ],
            shortTitle: "Ask Tutor",
            systemImageName: "graduationcap.fill"
        )
        AppShortcut(
            intent: StartListeningIntent(),
            phrases: [
                "Start listening on \(.applicationName)",
                "Listen to this meeting on \(.applicationName)",
                "\(.applicationName) start listening"
            ],
            shortTitle: "Start Listening",
            systemImageName: "waveform.badge.mic"
        )
        AppShortcut(
            intent: TalkToMagicanIntent(),
            phrases: [
                "Talk to \(.applicationName)",
                "Start talking on \(.applicationName)",
                "\(.applicationName) start talking"
            ],
            shortTitle: "Talk to Magican",
            systemImageName: "waveform"
        )
    }
}
