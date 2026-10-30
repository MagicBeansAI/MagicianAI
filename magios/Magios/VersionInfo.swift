import Foundation

/// Local app identity, read from the bundle (Info.plist is generated from
/// project.yml → MARKETING_VERSION / CURRENT_PROJECT_VERSION).
enum AppInfo {
    static var version: String { Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "—" }
    static var build: String { Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "—" }
    /// e.g. "0.1.46 (46)"
    static var versionAndBuild: String { build == version ? version : "\(version) (\(build))" }
}

/// Component versions read from the paired machine's `/health` body.
///
/// Current servers send `version` (the Magician core version), `api_version`
/// (the magician-api crate) and a `versions` object
/// (`magician`, `magician_api`, `magicutor`, `desktop`; the last two may be
/// null or absent when that component is offline or unknown). Older servers
/// send only `version`, which was the API crate's version. Every field is
/// optional: missing, null, empty or non-string values read as nil, and the
/// status keys (`magicutor`, `tauri`, `*_status`) are never read as versions.
struct HealthVersions: Equatable {
    var magician: String?
    var magicianAPI: String?
    var magicutor: String?
    var desktop: String?
    var supervisor: String?

    init(magician: String? = nil, magicianAPI: String? = nil, magicutor: String? = nil,
         desktop: String? = nil, supervisor: String? = nil) {
        self.magician = magician
        self.magicianAPI = magicianAPI
        self.magicutor = magicutor
        self.desktop = desktop
        self.supervisor = supervisor
    }

    init(json: [String: Any]) {
        let nested = json["versions"] as? [String: Any] ?? [:]
        func text(_ object: [String: Any], _ key: String) -> String? {
            guard let raw = object[key] as? String else { return nil }
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }
        magician = text(nested, "magician") ?? text(json, "version")
        magicianAPI = text(json, "api_version") ?? text(nested, "magician_api")
        magicutor = text(nested, "magicutor") ?? text(json, "magicutor_version")
        desktop = text(nested, "desktop") ?? text(json, "desktop_version") ?? text(json, "tauri_version")
        supervisor = text(nested, "supervisor") ?? text(json, "supervisor_version")
    }

    /// Nil when the body is not a JSON object.
    init?(data: Data) {
        guard let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return nil }
        self.init(json: json)
    }
}

/// Backend component versions, fetched from the magician `/health` endpoint
/// and decoded by `HealthVersions`. The app can only reach its
/// runtime-enrolled Magician origin, so other components appear only when
/// that machine reports them.
final class VersionInfoModel: ObservableObject {
    @Published var magician: String?
    @Published var magicianAPI: String?
    @Published var magicutor: String?
    @Published var supervisor: String?
    @Published var desktop: String?
    @Published var loaded = false

    func load() {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/health") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        request.httpMethod = "GET"
        URLSession.shared.dataTask(with: request) { [weak self] data, _, _ in
            let versions = data.flatMap(HealthVersions.init(data:)) ?? HealthVersions()
            DispatchQueue.main.async {
                guard let self = self else { return }
                self.magician = versions.magician
                self.magicianAPI = versions.magicianAPI
                self.magicutor = versions.magicutor
                self.supervisor = versions.supervisor
                self.desktop = versions.desktop
                self.loaded = true
            }
        }.resume()
    }
}
