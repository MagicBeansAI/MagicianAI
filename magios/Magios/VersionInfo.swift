import Foundation

/// Local app identity, read from the bundle (Info.plist is generated from
/// project.yml → MARKETING_VERSION / CURRENT_PROJECT_VERSION).
enum AppInfo {
    static var version: String { Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "—" }
    static var build: String { Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "—" }
    /// e.g. "0.1.46 (46)"
    static var versionAndBuild: String { build == version ? version : "\(version) (\(build))" }
}

/// Backend component versions, fetched from the magician `/health` endpoint.
///
/// Today `/health` reports only the magician version (`{status, service, version}`).
/// The magicutor / supervisor / tauri fields are parsed opportunistically — the
/// app can only reach its runtime-enrolled Magician origin, so those populate once the
/// backend aggregates and exposes them (e.g. `magicutor_version`); until then they
/// read as "not reported".
final class VersionInfoModel: ObservableObject {
    @Published var magician: String?
    @Published var magicutor: String?
    @Published var supervisor: String?
    @Published var tauri: String?
    @Published var loaded = false

    func load() {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/health") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        request.httpMethod = "GET"
        URLSession.shared.dataTask(with: request) { [weak self] data, _, _ in
            var m: String?, mu: String?, sv: String?, tv: String?
            if let data,
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                m = json["version"] as? String
                mu = (json["magicutor_version"] ?? json["magicutor"]) as? String
                sv = (json["supervisor_version"] ?? json["supervisor"]) as? String
                tv = (json["tauri_version"] ?? json["tauri"]) as? String
            }
            DispatchQueue.main.async {
                guard let self = self else { return }
                self.magician = m
                self.magicutor = mu
                self.supervisor = sv
                self.tauri = tv
                self.loaded = true
            }
        }.resume()
    }
}
